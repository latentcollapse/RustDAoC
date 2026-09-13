//! Addon host: discovery, per-addon VMs, dispatch, hot reload, fault isolation.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use mlua::Table;

use crate::error::HostError;
use crate::event::AddonEvent;
use crate::intent::AddonIntent;
use crate::manifest::{discover, load_manifest, version_major, AddonManifest, HOST_API_VERSION};
use crate::sandbox::{
    create_sandbox, exec_chunk, restore_saved, snapshot_saved, AddonRuntime, HostLimits,
};

/// Directory of in-tree example addons (HelloCAER, CombatMeter). No retail client assets.
///
/// Cargo normally embeds the package directory in `CARGO_MANIFEST_DIR`.  CAER's shared target
/// directory can, however, legitimately contain an artifact built from a short-lived hygiene
/// copy of the tree.  That old absolute path disappears after the check, so retain the compiled
/// path when it is valid and otherwise recover the checked-out workspace from the process cwd.
/// This keeps developer examples test-only and avoids a stale build artifact turning a healthy
/// product test into a fake missing-addon failure.
pub fn in_tree_addons_dir() -> PathBuf {
    resolve_in_tree_addons_dir(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("addons"),
        std::env::current_dir().ok(),
    )
}

fn resolve_in_tree_addons_dir(compiled: PathBuf, cwd: Option<PathBuf>) -> PathBuf {
    if is_in_tree_addons_dir(&compiled) {
        return compiled;
    }
    cwd.as_deref()
        .and_then(find_workspace_addons_dir)
        .unwrap_or(compiled)
}

fn find_workspace_addons_dir(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .map(|root| root.join("crates/caer-script/addons"))
        .find(|candidate| is_in_tree_addons_dir(candidate))
}

fn is_in_tree_addons_dir(path: &Path) -> bool {
    path.join("HelloCAER/HelloCAER.toc").is_file()
        && path.join("CombatMeter/CombatMeter.toc").is_file()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddonStatus {
    Loaded,
    Faulted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReloadPolicy {
    /// Keep SavedVariables when TOC major version is unchanged.
    #[default]
    PreserveIfCompatible,
    AlwaysDrop,
    AlwaysPreserve,
}

/// Opaque handle to a previously registered Lua callback. Invalid after unload/reload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackHandle {
    pub addon_id: String,
    pub generation: u64,
    pub event: AddonEvent,
    pub index: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DispatchReport {
    pub delivered: usize,
    pub faults: Vec<String>,
    pub unloaded: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReloadReport {
    pub addon_id: String,
    pub generation: u64,
    pub preserved_saved_variables: bool,
}

enum AddonSource {
    Directory(PathBuf),
    Memory {
        manifest: AddonManifest,
        files: BTreeMap<String, String>,
    },
}

struct LoadedAddon {
    lua: mlua::Lua,
    env: Table,
    manifest: AddonManifest,
    source: AddonSource,
    generation: u64,
    status: AddonStatus,
    last_error: Option<String>,
}

/// Multi-addon mlua host. One Lua state per addon. No WorldState access.
pub struct AddonHost {
    limits: HostLimits,
    reload_policy: ReloadPolicy,
    addons: BTreeMap<String, LoadedAddon>,
    intents: Vec<AddonIntent>,
    logs: Vec<HostLog>,
    next_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostLog {
    pub addon_id: String,
    pub line: String,
}

impl AddonHost {
    pub fn new(limits: HostLimits) -> Self {
        Self {
            limits,
            reload_policy: ReloadPolicy::PreserveIfCompatible,
            addons: BTreeMap::new(),
            intents: Vec::new(),
            logs: Vec::new(),
            next_generation: 1,
        }
    }

    pub fn with_reload_policy(mut self, policy: ReloadPolicy) -> Self {
        self.reload_policy = policy;
        self
    }

    pub fn limits(&self) -> HostLimits {
        self.limits
    }

    pub fn discover(root: impl AsRef<Path>) -> Result<Vec<AddonManifest>, HostError> {
        discover(root)
    }

    pub fn loaded_ids(&self) -> Vec<String> {
        self.addons.keys().cloned().collect()
    }

    pub fn status(&self, id: &str) -> Option<AddonStatus> {
        self.addons.get(id).map(|a| a.status)
    }

    pub fn generation(&self, id: &str) -> Option<u64> {
        self.addons.get(id).map(|a| a.generation)
    }

    pub fn last_error(&self, id: &str) -> Option<&str> {
        self.addons.get(id).and_then(|a| a.last_error.as_deref())
    }

    pub fn take_intents(&mut self) -> Vec<AddonIntent> {
        std::mem::take(&mut self.intents)
    }

    pub fn take_logs(&mut self) -> Vec<HostLog> {
        std::mem::take(&mut self.logs)
    }

    pub fn load_dir(&mut self, addon_dir: impl AsRef<Path>) -> Result<String, HostError> {
        let manifest = load_manifest(addon_dir.as_ref())?;
        let root = manifest.root.clone();
        let files = read_scripts(&manifest)?;
        self.load_inner(manifest, AddonSource::Directory(root), files)
    }

    pub fn load_tree(&mut self, addons_root: impl AsRef<Path>) -> Result<Vec<String>, HostError> {
        let manifests = discover(addons_root)?;
        let mut ids = Vec::new();
        for m in manifests {
            ids.push(self.load_dir(&m.root)?);
        }
        Ok(ids)
    }

    /// Load from in-memory sources (unit tests / tooling). `files` maps TOC script names to Lua.
    pub fn load_memory(
        &mut self,
        manifest: AddonManifest,
        files: BTreeMap<String, String>,
    ) -> Result<String, HostError> {
        self.load_inner(
            manifest.clone(),
            AddonSource::Memory {
                manifest,
                files: files.clone(),
            },
            files,
        )
    }

    pub fn unload(&mut self, id: &str) -> Result<(), HostError> {
        if self.addons.remove(id).is_none() {
            return Err(HostError::UnknownAddon(id.to_string()));
        }
        Ok(())
    }

    pub fn reload(&mut self, id: &str) -> Result<ReloadReport, HostError> {
        let addon = self
            .addons
            .remove(id)
            .ok_or_else(|| HostError::UnknownAddon(id.to_string()))?;
        let old_major = addon.manifest.version_major();
        let saved_names = addon.manifest.saved_variables.clone();
        let snap = snapshot_saved(&addon.env, &saved_names).ok();
        let source = match &addon.source {
            AddonSource::Directory(p) => AddonSource::Directory(p.clone()),
            AddonSource::Memory { manifest, files } => AddonSource::Memory {
                manifest: manifest.clone(),
                files: files.clone(),
            },
        };

        let (manifest, files) = match &source {
            AddonSource::Directory(p) => match load_manifest(p).and_then(|m| {
                let files = read_scripts(&m)?;
                Ok((m, files))
            }) {
                Ok(pair) => pair,
                Err(e) => {
                    self.addons.insert(id.to_string(), addon);
                    return Err(e);
                }
            },
            AddonSource::Memory { manifest, files } => (manifest.clone(), files.clone()),
        };

        let preserve = match self.reload_policy {
            ReloadPolicy::AlwaysDrop => false,
            ReloadPolicy::AlwaysPreserve => true,
            ReloadPolicy::PreserveIfCompatible => version_major(&manifest.version) == old_major,
        };

        match self.load_inner(manifest, source, files) {
            Ok(new_id) => {
                if preserve {
                    if let Some(snap) = snap {
                        let restore_err = self.addons.get(&new_id).and_then(|loaded| {
                            restore_saved(&loaded.lua, &loaded.env, &snap, &new_id).err()
                        });
                        if let Some(e) = restore_err {
                            self.addons.remove(&new_id);
                            self.addons.insert(id.to_string(), addon);
                            return Err(e);
                        }
                    }
                }
                let generation = self.addons.get(&new_id).map(|a| a.generation).unwrap_or(0);
                Ok(ReloadReport {
                    addon_id: new_id,
                    generation,
                    preserved_saved_variables: preserve,
                })
            }
            Err(e) => {
                self.addons.insert(id.to_string(), addon);
                Err(e)
            }
        }
    }

    pub fn dispatch(&mut self, event: AddonEvent, payload: &EventPayload) -> DispatchReport {
        let mut report = DispatchReport::default();
        let ids: Vec<String> = self.addons.keys().cloned().collect();
        for id in ids {
            match self.dispatch_one(&id, event, payload) {
                Ok(n) => report.delivered += n,
                Err(e) => {
                    report.faults.push(format!("{id}: {e}"));
                    if let Some(addon) = self.addons.get_mut(&id) {
                        addon.status = AddonStatus::Faulted;
                        addon.last_error = Some(e.to_string());
                    }
                }
            }
            if self
                .addons
                .get(&id)
                .map(|a| {
                    a.lua
                        .app_data_ref::<AddonRuntime>()
                        .map(|rt| rt.deferred_unload.get())
                        .unwrap_or(false)
                })
                .unwrap_or(false)
            {
                let _ = self.unload(&id);
                report.unloaded.push(id);
            }
        }
        report
    }

    pub fn handler_handles(&self, id: &str, event: AddonEvent) -> Vec<CallbackHandle> {
        let Some(addon) = self.addons.get(id) else {
            return Vec::new();
        };
        let Some(rt) = addon.lua.app_data_ref::<AddonRuntime>() else {
            return Vec::new();
        };
        let handlers = rt.handlers.borrow();
        let Some(list) = handlers.get(&event) else {
            return Vec::new();
        };
        list.iter()
            .enumerate()
            .map(|(index, h)| CallbackHandle {
                addon_id: id.to_string(),
                generation: h.generation,
                event,
                index,
            })
            .collect()
    }

    /// Invoke a previously issued handle. Must fail after unload/reload (stale callback).
    pub fn invoke_handle(
        &mut self,
        handle: &CallbackHandle,
        payload: &EventPayload,
    ) -> Result<(), HostError> {
        let addon = self
            .addons
            .get(&handle.addon_id)
            .ok_or_else(|| HostError::StaleCallback {
                addon: handle.addon_id.clone(),
                generation: handle.generation,
                current: None,
            })?;
        if addon.generation != handle.generation {
            return Err(HostError::StaleCallback {
                addon: handle.addon_id.clone(),
                generation: handle.generation,
                current: Some(addon.generation),
            });
        }
        self.dispatch_one(&handle.addon_id, handle.event, payload)
            .map(|_| ())
    }

    fn load_inner(
        &mut self,
        manifest: AddonManifest,
        source: AddonSource,
        files: BTreeMap<String, String>,
    ) -> Result<String, HostError> {
        crate::check_catalog()?;
        if manifest.interface > HOST_API_VERSION {
            return Err(HostError::ApiVersion {
                addon: manifest.id.clone(),
                want: HOST_API_VERSION,
                have: manifest.interface,
            });
        }
        if self.addons.contains_key(&manifest.id) {
            return Err(HostError::DuplicateAddon(manifest.id));
        }

        let generation = self.next_generation;
        self.next_generation += 1;
        let (lua, env) = create_sandbox(&manifest.id, generation, self.limits)?;
        if let Some(rt) = lua.app_data_ref::<AddonRuntime>() {
            rt.reset_budget(self.limits.instruction_budget);
        }

        for script in &manifest.scripts {
            let src = files.get(script).ok_or_else(|| HostError::Manifest {
                path: script.clone(),
                detail: format!(
                    "TOC lists `{script}` but the file is missing; put it next to the .toc"
                ),
            })?;
            exec_chunk(&lua, &env, &manifest.id, script, src)?;
        }

        self.drain_runtime(&lua, &manifest.id);
        let id = manifest.id.clone();
        self.addons.insert(
            id.clone(),
            LoadedAddon {
                lua,
                env,
                manifest,
                source,
                generation,
                status: AddonStatus::Loaded,
                last_error: None,
            },
        );
        Ok(id)
    }

    fn dispatch_one(
        &mut self,
        id: &str,
        event: AddonEvent,
        payload: &EventPayload,
    ) -> Result<usize, HostError> {
        let addon = self
            .addons
            .get(id)
            .ok_or_else(|| HostError::UnknownAddon(id.to_string()))?;
        if addon.status != AddonStatus::Loaded {
            return Ok(0);
        }
        let lua = addon.lua.clone();
        let generation = addon.generation;

        let handlers = {
            let rt = lua
                .app_data_ref::<AddonRuntime>()
                .ok_or_else(|| HostError::UnknownAddon(id.to_string()))?;
            let borrowed = rt.handlers.borrow();
            borrowed.get(&event).cloned().unwrap_or_default()
        };

        let mut delivered = 0;
        for h in handlers {
            if h.generation != generation {
                return Err(HostError::StaleCallback {
                    addon: id.to_string(),
                    generation: h.generation,
                    current: Some(generation),
                });
            }
            if lua
                .app_data_ref::<AddonRuntime>()
                .map(|rt| rt.deferred_unload.get())
                .unwrap_or(false)
            {
                break;
            }
            if let Some(rt) = lua.app_data_ref::<AddonRuntime>() {
                rt.reset_budget(self.limits.instruction_budget);
            }
            let table =
                payload_table(&lua, event, payload).map_err(|e| crate::error::from_mlua(id, e))?;
            match h.func.call::<()>(table) {
                Ok(()) => delivered += 1,
                Err(e) => {
                    self.drain_runtime(&lua, id);
                    return Err(crate::error::from_mlua(id, e));
                }
            }
            self.drain_runtime(&lua, id);
        }
        Ok(delivered)
    }

    fn drain_runtime(&mut self, lua: &mlua::Lua, addon_id: &str) {
        if let Some(rt) = lua.app_data_ref::<AddonRuntime>() {
            for line in rt.logs.borrow_mut().drain(..) {
                self.logs.push(HostLog {
                    addon_id: addon_id.to_string(),
                    line,
                });
            }
            self.intents.extend(rt.intents.borrow_mut().drain(..));
        }
    }
}

fn read_scripts(manifest: &AddonManifest) -> Result<BTreeMap<String, String>, HostError> {
    let mut files = BTreeMap::new();
    for script in &manifest.scripts {
        let path = manifest.root.join(script);
        let src = fs::read_to_string(&path).map_err(|e| HostError::Manifest {
            path: path.display().to_string(),
            detail: e.to_string(),
        })?;
        files.insert(script.clone(), src);
    }
    Ok(files)
}

/// Addon-visible event body. Only scalars; never a WorldState handle.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EventPayload {
    pub fields: BTreeMap<String, IntentValueLite>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IntentValueLite {
    Bool(bool),
    Int(i64),
    Number(f64),
    Str(String),
}

impl EventPayload {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn with(mut self, key: impl Into<String>, value: IntentValueLite) -> Self {
        self.fields.insert(key.into(), value);
        self
    }
}

fn payload_table(
    lua: &mlua::Lua,
    event: AddonEvent,
    payload: &EventPayload,
) -> mlua::Result<Table> {
    let data = lua.create_table()?;
    data.set("name", event.as_str())?;
    data.set("category", event.category().as_str())?;
    for (k, v) in &payload.fields {
        match v {
            IntentValueLite::Bool(b) => data.set(k.as_str(), *b)?,
            IntentValueLite::Int(i) => data.set(k.as_str(), *i)?,
            IntentValueLite::Number(n) => data.set(k.as_str(), *n)?,
            IntentValueLite::Str(s) => data.set(k.as_str(), s.as_str())?,
        }
    }
    // Empty proxy + __index: assignment hits __newindex even for published keys.
    let proxy = lua.create_table()?;
    let mt = lua.create_table()?;
    mt.set("__index", data)?;
    let deny = lua.create_function(|_, _: mlua::MultiValue| -> mlua::Result<()> {
        Err(mlua::Error::runtime(
            "event payload is a read-only projection; addons cannot mutate WorldState",
        ))
    })?;
    mt.set("__newindex", deny)?;
    mt.set("__metatable", "read-only")?;
    proxy.set_metatable(Some(mt))?;
    Ok(proxy)
}

/// In-memory TOC helper for tests and `caer addon test` snippets.
pub fn memory_manifest(id: &str, version: &str, saved: &[&str], scripts: &[&str]) -> AddonManifest {
    AddonManifest {
        id: id.to_string(),
        title: id.to_string(),
        version: version.to_string(),
        interface: HOST_API_VERSION,
        saved_variables: saved.iter().map(|s| (*s).to_string()).collect(),
        scripts: scripts.iter().map(|s| (*s).to_string()).collect(),
        root: PathBuf::from(id),
    }
}

/// Capability suite used by `caer addon test` (same checks as the crate tests).
///
/// Directory-driven: every in-tree addon under [`in_tree_addons_dir`] is loaded. HelloCAER and
/// CombatMeter are required. rustdaoc does not project live session events into this suite.
pub fn run_capability_suite() -> Result<CapabilitySuiteReport, HostError> {
    let mut host = AddonHost::new(HostLimits::tight());
    let loaded = host.load_tree(in_tree_addons_dir())?;
    require_loaded(&loaded, "HelloCAER")?;
    require_loaded(&loaded, "CombatMeter")?;

    let hello_n = host
        .handler_handles("HelloCAER", AddonEvent::CombatSwing)
        .len();
    let meter_n = host
        .handler_handles("CombatMeter", AddonEvent::CombatSwing)
        .len();
    if hello_n == 0 {
        return Err(suite_lua(
            "HelloCAER",
            "HelloCAER registered no combat.swing handler",
        ));
    }
    if meter_n == 0 {
        return Err(suite_lua(
            "CombatMeter",
            "CombatMeter registered no combat.swing handler",
        ));
    }

    let payload = EventPayload::empty()
        .with("damage", IntentValueLite::Int(25))
        .with("result", IntentValueLite::Str("hit".into()))
        .with("hp_pct", IntentValueLite::Int(80));
    let report = host.dispatch(AddonEvent::CombatSwing, &payload);
    if report.delivered < hello_n + meter_n {
        return Err(suite_lua(
            "CombatMeter",
            format!(
                "expected >= {} combat.swing deliveries (HelloCAER+CombatMeter), got {}",
                hello_n + meter_n,
                report.delivered
            ),
        ));
    }
    let logs = host.take_logs();
    if !logs
        .iter()
        .any(|l| l.addon_id == "CombatMeter" && l.line.contains("damage=25"))
    {
        return Err(suite_lua(
            "CombatMeter",
            "CombatMeter did not log typed damage=25 from the combat.swing projection",
        ));
    }

    let mut deny = AddonHost::new(HostLimits::production());
    let files = BTreeMap::from([("main.lua".into(), r#"io.open("/etc/passwd", "r")"#.into())]);
    let err = deny
        .load_memory(
            memory_manifest("DenyFS", "1.0.0", &[], &["main.lua"]),
            files,
        )
        .expect_err("io.open must hard-error");
    match err {
        HostError::SandboxDenied {
            capability: crate::error::Capability::Filesystem,
            ..
        } => {}
        other => {
            return Err(suite_lua(
                "DenyFS",
                format!("expected filesystem deny, got {other}"),
            ));
        }
    }

    Ok(CapabilitySuiteReport {
        hello_id: "HelloCAER".into(),
        hello_delivered: hello_n,
        combat_meter_id: "CombatMeter".into(),
        combat_meter_delivered: meter_n,
        loaded,
        sandbox_denied_fs: true,
    })
}

fn require_loaded(loaded: &[String], id: &str) -> Result<(), HostError> {
    if loaded.iter().any(|x| x == id) {
        Ok(())
    } else {
        Err(suite_lua(
            id,
            format!("in-tree addons dir did not load `{id}` (got {loaded:?})"),
        ))
    }
}

fn suite_lua(addon: &str, message: impl Into<String>) -> HostError {
    HostError::Lua {
        addon: addon.to_string(),
        message: message.into(),
        file: Some("main.lua".into()),
        line: None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilitySuiteReport {
    pub hello_id: String,
    pub hello_delivered: usize,
    pub combat_meter_id: String,
    pub combat_meter_delivered: usize,
    pub loaded: Vec<String>,
    pub sandbox_denied_fs: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::AddonCommand;
    use crate::error::Capability;
    use crate::intent::IntentValue;

    #[test]
    fn stale_compiled_manifest_dir_recovers_checked_out_examples() {
        let unique = format!(
            "caer-script-addon-path-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock before Unix epoch")
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let checkout = root.join("checkout");
        let addons = checkout.join("crates/caer-script/addons");
        for toc in ["HelloCAER/HelloCAER.toc", "CombatMeter/CombatMeter.toc"] {
            let path = addons.join(toc);
            fs::create_dir_all(path.parent().expect("toc parent")).expect("create addon parent");
            fs::write(path, "## Interface: 1\n").expect("write addon toc");
        }

        let resolved =
            resolve_in_tree_addons_dir(root.join("vanished-build/addons"), Some(checkout));
        assert_eq!(resolved, addons);

        fs::remove_dir_all(root).expect("remove fixture");
    }

    fn files(src: &str) -> BTreeMap<String, String> {
        BTreeMap::from([("main.lua".into(), src.into())])
    }

    fn swing_payload(damage: i64, result: &str, hp_pct: i64) -> EventPayload {
        EventPayload::empty()
            .with("damage", IntentValueLite::Int(damage))
            .with("result", IntentValueLite::Str(result.into()))
            .with("hp_pct", IntentValueLite::Int(hp_pct))
    }

    #[test]
    fn hello_caer_loads_from_tree_and_handles_combat_swing() {
        let mut host = AddonHost::new(HostLimits::production());
        let id = host
            .load_dir(in_tree_addons_dir().join("HelloCAER"))
            .unwrap();
        assert_eq!(id, "HelloCAER");
        let report = host.dispatch(AddonEvent::CombatSwing, &EventPayload::empty());
        assert!(
            report.delivered >= 1,
            "HelloCAER must register combat.swing; got {report:?}"
        );
        let logs = host.take_logs();
        assert!(
            logs.iter().any(|l| l.line.contains("HelloCAER")),
            "expected HelloCAER print, got {logs:?}"
        );
    }

    #[test]
    fn discover_finds_in_tree_examples() {
        let found = AddonHost::discover(in_tree_addons_dir()).unwrap();
        for id in ["HelloCAER", "CombatMeter"] {
            assert!(
                found.iter().any(|m| m.id == id),
                "in-tree addons must include {id}, got {found:?}"
            );
        }
    }

    #[test]
    fn combat_meter_receives_typed_read_only_combat_projection() {
        let mut host = AddonHost::new(HostLimits::production());
        let id = host
            .load_dir(in_tree_addons_dir().join("CombatMeter"))
            .unwrap();
        assert_eq!(id, "CombatMeter");
        let payload = swing_payload(25, "hit", 80);
        let before = payload.clone();
        let report = host.dispatch(AddonEvent::CombatSwing, &payload);
        assert!(
            report.delivered >= 1,
            "CombatMeter must register combat.swing; got {report:?}"
        );
        assert_eq!(
            payload, before,
            "dispatch must not mutate the caller's EventPayload"
        );
        host.dispatch(AddonEvent::PlayerDied, &EventPayload::empty());
        host.dispatch(AddonEvent::PlayerRevived, &EventPayload::empty());
        let logs = host.take_logs();
        assert!(
            logs.iter()
                .any(|l| l.addon_id == "CombatMeter" && l.line.contains("damage=25")),
            "CombatMeter must observe typed damage: {logs:?}"
        );
        assert!(
            logs.iter()
                .any(|l| l.line.contains("result=hit") && l.line.contains("hp_pct=80")),
            "CombatMeter must observe result/hp_pct projection: {logs:?}"
        );
        assert!(logs.iter().any(|l| l.line.contains("CombatMeter death")));
        assert!(logs.iter().any(|l| l.line.contains("CombatMeter revive")));
    }

    #[test]
    fn combat_meter_commands_queue_intents_not_world_state() {
        let mut host = AddonHost::new(HostLimits::production());
        host.load_dir(in_tree_addons_dir().join("CombatMeter"))
            .unwrap();
        host.dispatch(AddonEvent::CombatSwing, &swing_payload(12, "hit", 90));
        assert!(
            host.take_intents().is_empty(),
            "meter aggregation must not emit intents on swing"
        );
        host.dispatch(
            AddonEvent::ChatMessage,
            &EventPayload::empty().with("text", IntentValueLite::Str("/cm".into())),
        );
        let intents = host.take_intents();
        assert_eq!(intents.len(), 1, "expected one /cm say intent: {intents:?}");
        assert_eq!(intents[0].addon_id, "CombatMeter");
        assert_eq!(intents[0].command, AddonCommand::Say);
        let text = intents[0]
            .args
            .get("text")
            .and_then(IntentValue::as_str)
            .unwrap_or("");
        assert!(
            text.contains("swings=") && text.contains("damage="),
            "say intent must report meter totals, got {text:?}"
        );
        // Discriminating: the only host-visible effect is the drained queue. caer-script has
        // no WorldState apply path; rustdaoc projection is still missing.
        assert!(host.take_intents().is_empty());
        assert!(host.status("CombatMeter") == Some(AddonStatus::Loaded));
    }

    #[test]
    fn event_payload_write_faults_and_neighbor_still_dispatches() {
        let mut host = AddonHost::new(HostLimits::production());
        host.load_dir(in_tree_addons_dir().join("CombatMeter"))
            .unwrap();
        host.load_memory(
            memory_manifest("Mutator", "1.0.0", &[], &["main.lua"]),
            files(
                r#"
                CAER.register("combat.swing", function(ev)
                    ev.damage = 999
                end)
                "#,
            ),
        )
        .unwrap();
        let report = host.dispatch(AddonEvent::CombatSwing, &swing_payload(7, "hit", 50));
        assert!(
            report
                .faults
                .iter()
                .any(|f| f.contains("Mutator")
                    && (f.contains("read-only") || f.contains("WorldState"))),
            "write to projection must fault Mutator: {report:?}"
        );
        assert_eq!(host.status("CombatMeter"), Some(AddonStatus::Loaded));
        assert_eq!(host.status("Mutator"), Some(AddonStatus::Faulted));
        let logs = host.take_logs();
        assert!(
            logs.iter()
                .any(|l| l.addon_id == "CombatMeter" && l.line.contains("damage=7")),
            "CombatMeter must still see original projection: {logs:?}"
        );
    }

    #[test]
    fn broken_addon_does_not_stall_neighbor() {
        let mut host = AddonHost::new(HostLimits::production());
        host.load_memory(
            memory_manifest("Good", "1.0.0", &[], &["main.lua"]),
            files(
                r#"
                CAER.register("combat.swing", function(ev)
                    CAER.print("good")
                end)
                "#,
            ),
        )
        .unwrap();
        host.load_memory(
            memory_manifest("Bad", "1.0.0", &[], &["main.lua"]),
            files(
                r#"
                CAER.register("combat.swing", function(ev)
                    error("boom from Bad")
                end)
                "#,
            ),
        )
        .unwrap();
        let report = host.dispatch(AddonEvent::CombatSwing, &EventPayload::empty());
        assert!(
            report.faults.iter().any(|f| f.contains("Bad")),
            "bad addon must be reported: {report:?}"
        );
        assert_eq!(host.status("Good"), Some(AddonStatus::Loaded));
        assert_eq!(host.status("Bad"), Some(AddonStatus::Faulted));
        let logs = host.take_logs();
        assert!(logs.iter().any(|l| l.line == "good"));
        assert!(host
            .last_error("Bad")
            .unwrap_or("")
            .contains("boom from Bad"));
    }

    #[test]
    fn unload_during_dispatch_skips_remaining_handlers() {
        let mut host = AddonHost::new(HostLimits::production());
        host.load_memory(
            memory_manifest("SelfUnload", "1.0.0", &[], &["main.lua"]),
            files(
                r#"
                CAER.register("combat.swing", function(ev)
                    CAER.print("first")
                    CAER.unload()
                end)
                CAER.register("combat.swing", function(ev)
                    CAER.print("second-must-not-run")
                end)
                "#,
            ),
        )
        .unwrap();
        host.load_memory(
            memory_manifest("Other", "1.0.0", &[], &["main.lua"]),
            files(
                r#"
                CAER.register("combat.swing", function(ev)
                    CAER.print("other")
                end)
                "#,
            ),
        )
        .unwrap();
        let report = host.dispatch(AddonEvent::CombatSwing, &EventPayload::empty());
        assert!(
            report.unloaded.iter().any(|id| id == "SelfUnload"),
            "expected deferred unload, got {report:?}"
        );
        assert!(host.status("SelfUnload").is_none());
        assert_eq!(host.status("Other"), Some(AddonStatus::Loaded));
        let logs = host.take_logs();
        assert!(logs.iter().any(|l| l.line == "first"));
        assert!(logs.iter().any(|l| l.line == "other"));
        assert!(
            !logs.iter().any(|l| l.line.contains("second-must-not-run")),
            "second handler must not run after unload: {logs:?}"
        );
    }

    #[test]
    fn stale_callback_after_reload_is_rejected() {
        let mut host = AddonHost::new(HostLimits::production());
        host.load_memory(
            memory_manifest("Stale", "1.0.0", &["DB"], &["main.lua"]),
            files(
                r#"
                DB = DB or { n = 0 }
                CAER.register("combat.swing", function(ev)
                    DB.n = DB.n + 1
                end)
                "#,
            ),
        )
        .unwrap();
        let handles = host.handler_handles("Stale", AddonEvent::CombatSwing);
        assert_eq!(handles.len(), 1);
        let old = handles[0].clone();
        let gen_before = host.generation("Stale").unwrap();
        host.reload("Stale").unwrap();
        let gen_after = host.generation("Stale").unwrap();
        assert_ne!(gen_before, gen_after);
        let err = host
            .invoke_handle(&old, &EventPayload::empty())
            .expect_err("stale callback must fail");
        match err {
            HostError::StaleCallback {
                generation,
                current,
                ..
            } => {
                assert_eq!(generation, old.generation);
                assert_eq!(current, Some(gen_after));
            }
            other => panic!("expected StaleCallback, got {other}"),
        }
        // Fresh generation still dispatches.
        let report = host.dispatch(AddonEvent::CombatSwing, &EventPayload::empty());
        assert!(report.delivered >= 1);
    }

    #[test]
    fn looping_addon_budget_trip_does_not_kill_host() {
        let mut host = AddonHost::new(HostLimits::tight());
        host.load_memory(
            memory_manifest("Loop", "1.0.0", &[], &["main.lua"]),
            files(
                r#"
                CAER.register("combat.swing", function(ev)
                    while true do end
                end)
                "#,
            ),
        )
        .unwrap();
        host.load_dir(in_tree_addons_dir().join("CombatMeter"))
            .unwrap();
        let report = host.dispatch(AddonEvent::CombatSwing, &swing_payload(3, "hit", 99));
        assert!(
            report
                .faults
                .iter()
                .any(|f| f.contains("Loop") && (f.contains("budget") || f.contains("instruction"))),
            "loop must trip budget: {report:?}"
        );
        assert_eq!(host.status("Loop"), Some(AddonStatus::Faulted));
        assert_eq!(host.status("CombatMeter"), Some(AddonStatus::Loaded));
        let logs = host.take_logs();
        assert!(
            logs.iter()
                .any(|l| l.addon_id == "CombatMeter" && l.line.contains("damage=3")),
            "CombatMeter must still dispatch after budget trip: {logs:?} {report:?}"
        );
    }

    #[test]
    fn sandbox_denies_filesystem() {
        let mut host = AddonHost::new(HostLimits::production());
        let err = host
            .load_memory(
                memory_manifest("Fs", "1.0.0", &[], &["main.lua"]),
                files(r#"io.open("/etc/passwd", "r")"#),
            )
            .expect_err("io.open must be denied");
        match err {
            HostError::SandboxDenied {
                capability: Capability::Filesystem,
                symbol,
                ..
            } => {
                assert!(
                    symbol.contains("io.open"),
                    "denial must name io.open, got {symbol}"
                );
            }
            other => panic!("expected SandboxDenied filesystem, got {other}"),
        }
        // Host remains usable.
        host.load_memory(
            memory_manifest("Ok", "1.0.0", &[], &["main.lua"]),
            files(r#"CAER.print("still-up")"#),
        )
        .unwrap();
        assert_eq!(host.status("Ok"), Some(AddonStatus::Loaded));
    }

    #[test]
    fn command_enters_validated_intent_path() {
        let mut host = AddonHost::new(HostLimits::production());
        host.load_memory(
            memory_manifest("Cmd", "1.0.0", &[], &["main.lua"]),
            files(
                r#"
                CAER.register("chat.message", function(ev)
                    CAER.command("chat.say", { text = "hi" })
                    CAER.command("inventory.move_item", { from = 1, to = 2 })
                end)
                "#,
            ),
        )
        .unwrap();
        host.dispatch(AddonEvent::ChatMessage, &EventPayload::empty());
        let intents = host.take_intents();
        assert_eq!(intents.len(), 2);
        assert_eq!(intents[0].command, AddonCommand::Say);
        assert_eq!(
            intents[0].args.get("text"),
            Some(&IntentValue::Str("hi".into()))
        );
        assert_eq!(intents[1].command, AddonCommand::MoveItem);
        // Discriminating: host has no world-apply side effect — only the queue.
        assert!(host.take_intents().is_empty());
    }

    #[test]
    fn syntax_error_is_contained_with_file() {
        let mut host = AddonHost::new(HostLimits::production());
        let err = host
            .load_memory(
                memory_manifest("Syn", "1.0.0", &[], &["main.lua"]),
                files("this is not lua {{{"),
            )
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Syn"));
        assert!(
            msg.contains("main.lua") || matches!(err, HostError::Lua { file: Some(_), .. }),
            "syntax error must name file/line: {err}"
        );
        assert!(host.loaded_ids().is_empty());
    }

    #[test]
    fn hot_reload_preserves_saved_variables_on_same_major() {
        let mut host = AddonHost::new(HostLimits::production())
            .with_reload_policy(ReloadPolicy::PreserveIfCompatible);
        host.load_memory(
            memory_manifest("Rel", "1.2.0", &["DB"], &["main.lua"]),
            files(
                r#"
                DB = DB or { n = 0 }
                CAER.register("combat.swing", function()
                    DB.n = (DB.n or 0) + 1
                    CAER.print("n=" .. DB.n)
                end)
                "#,
            ),
        )
        .unwrap();
        host.dispatch(AddonEvent::CombatSwing, &EventPayload::empty());
        let report = host.reload("Rel").unwrap();
        assert!(report.preserved_saved_variables);
        host.dispatch(AddonEvent::CombatSwing, &EventPayload::empty());
        let logs = host.take_logs();
        assert!(
            logs.iter().any(|l| l.line == "n=2"),
            "saved DB.n should survive reload: {logs:?}"
        );
    }

    #[test]
    fn hot_reload_drops_state_on_major_bump() {
        let mut host = AddonHost::new(HostLimits::production());
        let files_map = files(
            r#"
            DB = DB or { n = 0 }
            CAER.register("combat.swing", function()
                DB.n = (DB.n or 0) + 1
                CAER.print("n=" .. DB.n)
            end)
            "#,
        );
        let mut manifest = memory_manifest("Rel2", "1.0.0", &["DB"], &["main.lua"]);
        host.load_memory(manifest.clone(), files_map.clone())
            .unwrap();
        host.dispatch(AddonEvent::CombatSwing, &EventPayload::empty());
        // Mutate the memory source version to 2.x and reload.
        if let Some(addon) = host.addons.get_mut("Rel2") {
            manifest.version = "2.0.0".into();
            addon.source = AddonSource::Memory {
                manifest: manifest.clone(),
                files: files_map.clone(),
            };
            addon.manifest.version = "1.0.0".into();
        }
        // reload() re-reads Memory source manifest (2.0.0) vs old major 1.
        let report = host.reload("Rel2").unwrap();
        assert!(
            !report.preserved_saved_variables,
            "major bump must drop SavedVariables"
        );
        host.dispatch(AddonEvent::CombatSwing, &EventPayload::empty());
        let logs = host.take_logs();
        assert!(
            logs.iter().any(|l| l.line == "n=1"),
            "state must reset after major bump: {logs:?}"
        );
    }

    #[test]
    fn capability_suite_runs() {
        let report = run_capability_suite().unwrap();
        assert!(report.sandbox_denied_fs);
        assert!(report.hello_delivered >= 1);
        assert!(report.combat_meter_delivered >= 1);
        assert!(report.loaded.iter().any(|id| id == "CombatMeter"));
        assert!(report.loaded.iter().any(|id| id == "HelloCAER"));
    }

    #[test]
    fn failed_reload_rolls_back_previous_generation() {
        let mut host = AddonHost::new(HostLimits::production());
        host.load_memory(
            memory_manifest("RB", "1.0.0", &["DB"], &["main.lua"]),
            files(
                r#"
                DB = DB or { n = 0 }
                CAER.register("combat.swing", function()
                    DB.n = (DB.n or 0) + 1
                    CAER.print("ok")
                end)
                "#,
            ),
        )
        .unwrap();
        let gen = host.generation("RB").unwrap();
        host.dispatch(AddonEvent::CombatSwing, &EventPayload::empty());
        if let Some(addon) = host.addons.get_mut("RB") {
            addon.source = AddonSource::Memory {
                manifest: memory_manifest("RB", "1.0.0", &["DB"], &["main.lua"]),
                files: files("this is not lua {{{"),
            };
        }
        let err = host.reload("RB").expect_err("broken reload must fail");
        assert!(
            err.to_string().contains("RB"),
            "reload error must name addon: {err}"
        );
        assert_eq!(host.status("RB"), Some(AddonStatus::Loaded));
        assert_eq!(host.generation("RB"), Some(gen));
        host.take_logs();
        let report = host.dispatch(AddonEvent::CombatSwing, &EventPayload::empty());
        assert!(
            report.delivered >= 1 && report.faults.is_empty(),
            "rolled-back addon must still dispatch: {report:?}"
        );
        let logs = host.take_logs();
        assert!(
            logs.iter().any(|l| l.line == "ok"),
            "previous handlers must remain: {logs:?}"
        );
    }

    #[test]
    fn userdata_command_arg_is_rejected() {
        let mut host = AddonHost::new(HostLimits::production());
        host.load_memory(
            memory_manifest("Ud", "1.0.0", &[], &["main.lua"]),
            files(
                r#"
                CAER.register("chat.message", function()
                    CAER.command("chat.say", { text = function() end })
                end)
                "#,
            ),
        )
        .unwrap();
        let report = host.dispatch(AddonEvent::ChatMessage, &EventPayload::empty());
        assert!(
            !report.faults.is_empty() || host.status("Ud") == Some(AddonStatus::Faulted),
            "function arg must fault the addon: {report:?}"
        );
        assert!(host.take_intents().is_empty());
    }
}
