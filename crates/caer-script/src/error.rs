//! Host, sandbox, and intent errors. Messages name the fix (model-authorable API).

use core::fmt;

use crate::check::CatalogError;

/// Why the addon host refused a load, dispatch, command, or reload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostError {
    Catalog(CatalogError),
    Manifest {
        path: String,
        detail: String,
    },
    DuplicateAddon(String),
    UnknownAddon(String),
    UnknownEvent {
        name: String,
    },
    UnknownCommand {
        name: String,
    },
    InvalidIntent {
        addon: String,
        reason: String,
    },
    Lua {
        addon: String,
        message: String,
        file: Option<String>,
        line: Option<i32>,
    },
    BudgetExceeded {
        addon: String,
        budget: u32,
    },
    MemoryExceeded {
        addon: String,
    },
    SandboxDenied {
        addon: String,
        capability: Capability,
        symbol: String,
    },
    StaleCallback {
        addon: String,
        generation: u64,
        current: Option<u64>,
    },
    ApiVersion {
        addon: String,
        want: u32,
        have: u32,
    },
    UnloadDuringCall {
        addon: String,
    },
}

/// Capability classes addons do not receive by default (REQ-009).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Filesystem,
    Network,
    NativeFfi,
}

impl Capability {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Filesystem => "filesystem",
            Self::Network => "network",
            Self::NativeFfi => "native FFI",
        }
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Catalog(e) => write!(f, "{e}"),
            Self::Manifest { path, detail } => {
                write!(f, "addon manifest `{path}`: {detail}")
            }
            Self::DuplicateAddon(id) => {
                write!(f, "addon `{id}` is already loaded; unload or reload it")
            }
            Self::UnknownAddon(id) => write!(f, "no loaded addon named `{id}`"),
            Self::UnknownEvent { name } => write!(
                f,
                "unknown event `{name}`; use a catalog id such as `combat.swing` (caer addon check)"
            ),
            Self::UnknownCommand { name } => write!(
                f,
                "unknown command `{name}`; use a catalog id such as `chat.say` (caer addon check)"
            ),
            Self::InvalidIntent { addon, reason } => {
                write!(f, "addon `{addon}` command rejected: {reason}")
            }
            Self::Lua {
                addon,
                message,
                file,
                line,
            } => match (file, line) {
                (Some(file), Some(line)) => {
                    write!(f, "addon `{addon}` Lua error at {file}:{line}: {message}")
                }
                (Some(file), None) => {
                    write!(f, "addon `{addon}` Lua error in {file}: {message}")
                }
                _ => write!(f, "addon `{addon}` Lua error: {message}"),
            },
            Self::BudgetExceeded { addon, budget } => write!(
                f,
                "addon `{addon}` exceeded instruction budget ({budget}); looping scripts are stopped, the host continues"
            ),
            Self::MemoryExceeded { addon } => write!(
                f,
                "addon `{addon}` exceeded its memory cap; the host continues without that addon"
            ),
            Self::SandboxDenied {
                addon,
                capability,
                symbol,
            } => write!(
                f,
                "addon `{addon}` sandbox denied {capability} via `{symbol}`: no filesystem, network, or native FFI by default (REQ-009)"
            ),
            Self::StaleCallback {
                addon,
                generation,
                current,
            } => write!(
                f,
                "stale callback for addon `{addon}` (generation {generation}, current {}); reload dropped it",
                current
                    .map(|g| g.to_string())
                    .unwrap_or_else(|| "unloaded".into())
            ),
            Self::ApiVersion { addon, want, have } => write!(
                f,
                "addon `{addon}` Interface {have} is newer than host API {want}; update CAER or pin Interface: {want}"
            ),
            Self::UnloadDuringCall { addon } => {
                write!(f, "addon `{addon}` cannot be dropped while Lua is on the stack; unload is deferred")
            }
        }
    }
}

impl std::error::Error for HostError {}

impl From<CatalogError> for HostError {
    fn from(value: CatalogError) -> Self {
        Self::Catalog(value)
    }
}

/// Pull `file:line` out of an mlua error string when present (`[string "main.lua"]:3: ...`).
pub(crate) fn lua_location(message: &str) -> (Option<String>, Option<i32>) {
    // mlua/Lua 5.1: `main.lua:12: msg` or `[string "main.lua"]:12: msg`
    let rest = message.strip_prefix("[string \"").unwrap_or(message);
    let rest = rest.strip_prefix("\"").unwrap_or(rest);
    let Some((left, after_colon)) = rest.split_once(':') else {
        return (None, None);
    };
    let file = left.trim_end_matches('"').trim_end_matches(']');
    if file.is_empty() || file.contains('\n') {
        return (None, None);
    }
    let line_part = after_colon.trim_start();
    let line_digits: String = line_part
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    let line = line_digits.parse().ok();
    if line.is_none() && !file.ends_with(".lua") && !file.contains('/') {
        return (None, None);
    }
    (Some(file.to_string()), line)
}

pub(crate) fn from_mlua(addon: &str, err: mlua::Error) -> HostError {
    let message = err.to_string();
    let lower = message.to_lowercase();
    if lower.contains("not enough memory") || matches!(err, mlua::Error::MemoryError(_)) {
        return HostError::MemoryExceeded {
            addon: addon.to_string(),
        };
    }
    if lower.contains("instruction budget") {
        return HostError::BudgetExceeded {
            addon: addon.to_string(),
            budget: 0,
        };
    }
    if let Some(cap) = parse_sandbox_denial(&message) {
        return HostError::SandboxDenied {
            addon: addon.to_string(),
            capability: cap.0,
            symbol: cap.1,
        };
    }
    let (file, line) = lua_location(&message);
    HostError::Lua {
        addon: addon.to_string(),
        message,
        file,
        line,
    }
}

fn parse_sandbox_denial(message: &str) -> Option<(Capability, String)> {
    // "sandbox denied filesystem via `io.open`"
    let (_, rest) = message.split_once("sandbox denied ")?;
    let cap = if rest.starts_with("filesystem") {
        Capability::Filesystem
    } else if rest.starts_with("network") {
        Capability::Network
    } else if rest.starts_with("native FFI") || rest.starts_with("native ffi") {
        Capability::NativeFfi
    } else {
        return None;
    };
    let symbol = rest.split('`').nth(1).unwrap_or("unknown").to_string();
    Some((cap, symbol))
}
