//! Per-addon Lua 5.1 VM: curated globals, no FS/network/FFI, instruction + memory caps.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};

use mlua::{ChunkMode, Function, HookTriggers, Lua, LuaOptions, StdLib, Table, Value, VmState};

use crate::error::{from_mlua, Capability, HostError};
use crate::event::AddonEvent;
use crate::intent::{validate_intent, AddonIntent, IntentValue};

/// Default production budgets (per callback / extra heap).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostLimits {
    pub instruction_budget: u32,
    pub hook_interval: u32,
    pub memory_limit_bytes: usize,
}

impl Default for HostLimits {
    fn default() -> Self {
        Self::production()
    }
}

impl HostLimits {
    pub const fn production() -> Self {
        Self {
            instruction_budget: 250_000,
            hook_interval: 1_000,
            memory_limit_bytes: 8 * 1024 * 1024,
        }
    }

    /// Tight enough that `while true do end` trips in unit tests.
    pub const fn tight() -> Self {
        Self {
            instruction_budget: 8_000,
            hook_interval: 64,
            memory_limit_bytes: 256 * 1024,
        }
    }
}

/// Mutable VM-side bookkeeping. Lives in `Lua` app data (not Send — one VM per addon).
pub(crate) struct AddonRuntime {
    pub addon_id: String,
    pub generation: u64,
    pub logs: RefCell<Vec<String>>,
    pub intents: RefCell<Vec<AddonIntent>>,
    pub handlers: RefCell<BTreeMap<AddonEvent, Vec<RegisteredHandler>>>,
    pub deferred_unload: Cell<bool>,
    pub instructions: AtomicU32,
    pub instruction_limit: AtomicU32,
}

#[derive(Clone)]
pub(crate) struct RegisteredHandler {
    pub generation: u64,
    pub func: Function,
}

impl AddonRuntime {
    fn new(addon_id: String, generation: u64, limits: HostLimits) -> Self {
        Self {
            addon_id,
            generation,
            logs: RefCell::new(Vec::new()),
            intents: RefCell::new(Vec::new()),
            handlers: RefCell::new(BTreeMap::new()),
            deferred_unload: Cell::new(false),
            instructions: AtomicU32::new(0),
            instruction_limit: AtomicU32::new(limits.instruction_budget),
        }
    }

    pub(crate) fn reset_budget(&self, budget: u32) {
        self.instructions.store(0, Ordering::Relaxed);
        self.instruction_limit.store(budget, Ordering::Relaxed);
    }
}

const SAFE_GLOBALS: &[&str] = &[
    "assert", "error", "ipairs", "next", "pairs", "pcall", "rawequal", "rawget", "rawset",
    "select", "tonumber", "tostring", "type", "unpack", "xpcall", "_VERSION",
];

const SAFE_LIBS: &[&str] = &["table", "string", "math", "coroutine"];

pub(crate) fn create_sandbox(
    addon_id: &str,
    generation: u64,
    limits: HostLimits,
) -> Result<(Lua, Table), HostError> {
    // Lua 5.1 always opens `base` (includes `coroutine`). Do not load io/package/debug.
    let libs = StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::OS;
    let lua = Lua::new_with(libs, LuaOptions::default()).map_err(|e| from_mlua(addon_id, e))?;

    let used = lua.used_memory();
    lua.set_memory_limit(used.saturating_add(limits.memory_limit_bytes))
        .map_err(|e| from_mlua(addon_id, e))?;

    lua.set_app_data(AddonRuntime::new(addon_id.to_string(), generation, limits));

    let interval = limits.hook_interval.max(1);
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(interval),
        move |lua, _debug| {
            if let Some(rt) = lua.app_data_ref::<AddonRuntime>() {
                let used = rt.instructions.fetch_add(interval, Ordering::Relaxed);
                let limit = rt.instruction_limit.load(Ordering::Relaxed);
                if used.saturating_add(interval) > limit {
                    return Err(mlua::Error::runtime(format!(
                        "instruction budget exceeded ({limit})"
                    )));
                }
            }
            Ok(VmState::Continue)
        },
    )
    .map_err(|e| from_mlua(addon_id, e))?;
    lua.set_global_hook(
        HookTriggers::new().every_nth_instruction(interval),
        move |lua, _debug| {
            if let Some(rt) = lua.app_data_ref::<AddonRuntime>() {
                let used = rt.instructions.fetch_add(interval, Ordering::Relaxed);
                let limit = rt.instruction_limit.load(Ordering::Relaxed);
                if used.saturating_add(interval) > limit {
                    return Err(mlua::Error::runtime(format!(
                        "instruction budget exceeded ({limit})"
                    )));
                }
            }
            Ok(VmState::Continue)
        },
    )
    .map_err(|e| from_mlua(addon_id, e))?;

    let env = build_env(&lua, addon_id)?;
    install_caer(&lua, &env, addon_id, generation)?;
    Ok((lua, env))
}

fn build_env(lua: &Lua, addon_id: &str) -> Result<Table, HostError> {
    let globals = lua.globals();
    let env = lua.create_table().map_err(|e| from_mlua(addon_id, e))?;

    for name in SAFE_GLOBALS {
        let v: Value = globals.get(*name).unwrap_or(Value::Nil);
        if !v.is_nil() {
            env.set(*name, v).map_err(|e| from_mlua(addon_id, e))?;
        }
    }
    for name in SAFE_LIBS {
        let v: Value = globals.get(*name).unwrap_or(Value::Nil);
        if !v.is_nil() {
            env.set(*name, v).map_err(|e| from_mlua(addon_id, e))?;
        }
    }

    // Clock-only os. Full os.execute / getenv / remove are FS/network adjacent.
    let os_src: Option<Table> = globals.get("os").ok();
    let os = lua.create_table().map_err(|e| from_mlua(addon_id, e))?;
    if let Some(src) = os_src {
        for key in ["clock", "time", "date", "difftime"] {
            let v: Value = src.get(key).unwrap_or(Value::Nil);
            if !v.is_nil() {
                os.set(key, v).map_err(|e| from_mlua(addon_id, e))?;
            }
        }
    }
    for (sym, cap) in [
        ("os.execute", Capability::Network),
        ("os.remove", Capability::Filesystem),
        ("os.rename", Capability::Filesystem),
        ("os.getenv", Capability::Filesystem),
        ("os.tmpname", Capability::Filesystem),
        ("os.exit", Capability::NativeFfi),
        ("os.setlocale", Capability::NativeFfi),
    ] {
        let f = deny_fn(lua, addon_id, cap, sym)?;
        let field = sym.rsplit('.').next().unwrap();
        os.set(field, f).map_err(|e| from_mlua(addon_id, e))?;
    }
    env.set("os", os.clone())
        .map_err(|e| from_mlua(addon_id, e))?;
    globals.set("os", os).map_err(|e| from_mlua(addon_id, e))?;

    let io = lua.create_table().map_err(|e| from_mlua(addon_id, e))?;
    for sym in [
        "open", "popen", "lines", "input", "output", "read", "write", "flush", "close", "tmpfile",
        "type", "stderr", "stdin", "stdout",
    ] {
        let symbol = format!("io.{sym}");
        let f = deny_fn(lua, addon_id, Capability::Filesystem, &symbol)?;
        io.set(sym, f).map_err(|e| from_mlua(addon_id, e))?;
    }
    env.set("io", io.clone())
        .map_err(|e| from_mlua(addon_id, e))?;
    globals.set("io", io).map_err(|e| from_mlua(addon_id, e))?;

    for (sym, cap) in [
        ("dofile", Capability::Filesystem),
        ("loadfile", Capability::Filesystem),
        ("load", Capability::NativeFfi),
        ("loadstring", Capability::NativeFfi),
        ("require", Capability::Filesystem),
        ("module", Capability::Filesystem),
        ("getfenv", Capability::NativeFfi),
        ("setfenv", Capability::NativeFfi),
        ("collectgarbage", Capability::NativeFfi),
    ] {
        env.set(sym, deny_fn(lua, addon_id, cap, sym)?)
            .map_err(|e| from_mlua(addon_id, e))?;
    }

    env.set(
        "package",
        deny_fn(lua, addon_id, Capability::Filesystem, "package")?,
    )
    .map_err(|e| from_mlua(addon_id, e))?;
    env.set(
        "debug",
        deny_fn(lua, addon_id, Capability::NativeFfi, "debug")?,
    )
    .map_err(|e| from_mlua(addon_id, e))?;
    env.set("ffi", deny_fn(lua, addon_id, Capability::NativeFfi, "ffi")?)
        .map_err(|e| from_mlua(addon_id, e))?;
    env.set("jit", deny_fn(lua, addon_id, Capability::NativeFfi, "jit")?)
        .map_err(|e| from_mlua(addon_id, e))?;

    env.set("_G", env.clone())
        .map_err(|e| from_mlua(addon_id, e))?;

    // Strip dangerous names from the real globals too (defense in depth).
    for name in [
        "dofile",
        "loadfile",
        "load",
        "loadstring",
        "require",
        "module",
        "package",
        "debug",
        "ffi",
        "jit",
        "getfenv",
        "setfenv",
    ] {
        globals
            .set(name, Value::Nil)
            .map_err(|e| from_mlua(addon_id, e))?;
    }

    Ok(env)
}

fn deny_fn(
    lua: &Lua,
    addon_id: &str,
    cap: Capability,
    symbol: &str,
) -> Result<Function, HostError> {
    let symbol = symbol.to_string();
    lua.create_function(move |_, _: mlua::MultiValue| -> mlua::Result<()> {
        Err(mlua::Error::runtime(format!(
            "sandbox denied {cap} via `{symbol}`: addons have no filesystem, network, or native FFI by default (REQ-009)"
        )))
    })
    .map_err(|e| from_mlua(addon_id, e))
}

fn install_caer(lua: &Lua, env: &Table, addon_id: &str, generation: u64) -> Result<(), HostError> {
    let caer = lua.create_table().map_err(|e| from_mlua(addon_id, e))?;
    caer.set("api", crate::manifest::HOST_API_VERSION)
        .map_err(|e| from_mlua(addon_id, e))?;
    caer.set("addon", addon_id)
        .map_err(|e| from_mlua(addon_id, e))?;
    caer.set("generation", generation)
        .map_err(|e| from_mlua(addon_id, e))?;

    let print_fn = lua
        .create_function(move |lua, args: mlua::MultiValue| {
            let mut parts = Vec::new();
            for v in args {
                parts.push(value_to_string(v));
            }
            let line = parts.join("\t");
            if let Some(rt) = lua.app_data_ref::<AddonRuntime>() {
                rt.logs.borrow_mut().push(line);
            }
            Ok(())
        })
        .map_err(|e| from_mlua(addon_id, e))?;
    caer.set("print", print_fn.clone())
        .map_err(|e| from_mlua(addon_id, e))?;
    env.set("print", print_fn)
        .map_err(|e| from_mlua(addon_id, e))?;

    let register = lua
        .create_function(|lua, (name, func): (String, Function)| {
            let event = AddonEvent::parse(&name).ok_or_else(|| {
                mlua::Error::runtime(format!(
                    "unknown event `{name}`; use a catalog id such as `combat.swing` (caer addon check)"
                ))
            })?;
            let rt = lua
                .app_data_ref::<AddonRuntime>()
                .ok_or_else(|| mlua::Error::runtime("addon runtime missing"))?;
            let generation = rt.generation;
            rt.handlers
                .borrow_mut()
                .entry(event)
                .or_default()
                .push(RegisteredHandler { generation, func });
            Ok(())
        })
        .map_err(|e| from_mlua(addon_id, e))?;
    caer.set("register", register)
        .map_err(|e| from_mlua(addon_id, e))?;

    let command = lua
        .create_function(|lua, (name, args): (String, Option<Table>)| {
            let rt = lua
                .app_data_ref::<AddonRuntime>()
                .ok_or_else(|| mlua::Error::runtime("addon runtime missing"))?;
            let map = table_to_intent_args(args)?;
            match validate_intent(&rt.addon_id, &name, map) {
                Ok(intent) => {
                    rt.intents.borrow_mut().push(intent);
                    Ok(())
                }
                Err(e) => Err(mlua::Error::runtime(e.to_string())),
            }
        })
        .map_err(|e| from_mlua(addon_id, e))?;
    caer.set("command", command)
        .map_err(|e| from_mlua(addon_id, e))?;

    let unload = lua
        .create_function(|lua, (): ()| {
            if let Some(rt) = lua.app_data_ref::<AddonRuntime>() {
                rt.deferred_unload.set(true);
            }
            Ok(())
        })
        .map_err(|e| from_mlua(addon_id, e))?;
    caer.set("unload", unload)
        .map_err(|e| from_mlua(addon_id, e))?;

    env.set("CAER", caer).map_err(|e| from_mlua(addon_id, e))?;
    Ok(())
}

fn table_to_intent_args(args: Option<Table>) -> mlua::Result<BTreeMap<String, IntentValue>> {
    let mut map = BTreeMap::new();
    let Some(table) = args else {
        return Ok(map);
    };
    for pair in table.pairs::<Value, Value>() {
        let (k, v) = pair?;
        let key = match k {
            Value::String(s) => s.to_str()?.to_string(),
            Value::Integer(i) => i.to_string(),
            _ => {
                return Err(mlua::Error::runtime(
                    "command args keys must be strings; cannot pass userdata (no WorldState)",
                ));
            }
        };
        let iv = match v {
            Value::Boolean(b) => IntentValue::Bool(b),
            Value::Integer(i) => IntentValue::Int(i),
            Value::Number(n) => IntentValue::Number(n),
            Value::String(s) => IntentValue::Str(s.to_str()?.to_string()),
            Value::Nil => continue,
            Value::Table(_) => {
                return Err(mlua::Error::runtime(
                    "command args must be scalars; nested tables cannot carry WorldState or persistence handles",
                ));
            }
            Value::Function(_)
            | Value::Thread(_)
            | Value::UserData(_)
            | Value::LightUserData(_) => {
                return Err(mlua::Error::runtime(
                    "command args cannot be functions or userdata; addons cannot mutate inventory/combat/movement/social/server/persistence",
                ));
            }
            _ => {
                return Err(mlua::Error::runtime(
                    "command args must be bool/int/number/string",
                ));
            }
        };
        map.insert(key, iv);
    }
    Ok(map)
}

fn value_to_string(v: Value) -> String {
    match v {
        Value::Nil => "nil".into(),
        Value::Boolean(b) => b.to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.to_str().map(|s| s.to_string()).unwrap_or_default(),
        other => format!("{other:?}"),
    }
}

pub(crate) fn exec_chunk(
    lua: &Lua,
    env: &Table,
    addon_id: &str,
    name: &str,
    source: &str,
) -> Result<(), HostError> {
    if source.as_bytes().first() == Some(&0x1b) {
        return Err(HostError::SandboxDenied {
            addon: addon_id.to_string(),
            capability: Capability::NativeFfi,
            symbol: "bytecode".into(),
        });
    }
    lua.load(source)
        .set_name(format!("@{name}"))
        .set_mode(ChunkMode::Text)
        .set_environment(env.clone())
        .exec()
        .map_err(|e| from_mlua(addon_id, e))
}

pub(crate) fn snapshot_saved(
    env: &Table,
    names: &[String],
) -> Result<BTreeMap<String, SnapshotValue>, HostError> {
    let mut out = BTreeMap::new();
    for name in names {
        let v: Value = env.get(name.as_str()).unwrap_or(Value::Nil);
        out.insert(name.clone(), snapshot_value(v, 0)?);
    }
    Ok(out)
}

pub(crate) fn restore_saved(
    lua: &Lua,
    env: &Table,
    snap: &BTreeMap<String, SnapshotValue>,
    addon_id: &str,
) -> Result<(), HostError> {
    for (name, val) in snap {
        let v = restore_value(lua, val).map_err(|e| from_mlua(addon_id, e))?;
        env.set(name.as_str(), v)
            .map_err(|e| from_mlua(addon_id, e))?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SnapshotValue {
    Nil,
    Bool(bool),
    Int(i64),
    Number(f64),
    Str(String),
    Table(Vec<(SnapshotValue, SnapshotValue)>),
}

fn snapshot_value(v: Value, depth: usize) -> Result<SnapshotValue, HostError> {
    if depth > 8 {
        return Ok(SnapshotValue::Nil);
    }
    Ok(match v {
        Value::Nil => SnapshotValue::Nil,
        Value::Boolean(b) => SnapshotValue::Bool(b),
        Value::Integer(i) => SnapshotValue::Int(i),
        Value::Number(n) => SnapshotValue::Number(n),
        Value::String(s) => {
            SnapshotValue::Str(s.to_str().map(|s| s.to_string()).unwrap_or_default())
        }
        Value::Table(t) => {
            let mut pairs = Vec::new();
            for pair in t.pairs::<Value, Value>() {
                let Ok((k, val)) = pair else { break };
                pairs.push((
                    snapshot_value(k, depth + 1)?,
                    snapshot_value(val, depth + 1)?,
                ));
            }
            SnapshotValue::Table(pairs)
        }
        _ => SnapshotValue::Nil,
    })
}

fn restore_value(lua: &Lua, v: &SnapshotValue) -> mlua::Result<Value> {
    Ok(match v {
        SnapshotValue::Nil => Value::Nil,
        SnapshotValue::Bool(b) => Value::Boolean(*b),
        SnapshotValue::Int(i) => Value::Integer(*i),
        SnapshotValue::Number(n) => Value::Number(*n),
        SnapshotValue::Str(s) => Value::String(lua.create_string(s)?),
        SnapshotValue::Table(pairs) => {
            let t = lua.create_table()?;
            for (k, val) in pairs {
                t.set(restore_value(lua, k)?, restore_value(lua, val)?)?;
            }
            Value::Table(t)
        }
    })
}
