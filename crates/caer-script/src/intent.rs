//! Typed command requests. Addons never apply world/server/persistence mutations.

use std::collections::BTreeMap;
use std::fmt;

use crate::command::{AddonCommand, AddonCommandCategory};
use crate::error::HostError;

/// Scalar argument on a validated intent. Nested tables, functions, and userdata are refused.
#[derive(Debug, Clone, PartialEq)]
pub enum IntentValue {
    Bool(bool),
    Int(i64),
    Number(f64),
    Str(String),
}

impl IntentValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// A command request that has passed catalog + payload validation.
///
/// The host **queues** this for a later integrator. It does not touch inventory, combat,
/// movement, social, server, or persistence itself. rustdaoc wiring is out of scope.
#[derive(Debug, Clone, PartialEq)]
pub struct AddonIntent {
    pub addon_id: String,
    pub command: AddonCommand,
    pub args: BTreeMap<String, IntentValue>,
}

impl AddonIntent {
    pub fn category(&self) -> AddonCommandCategory {
        self.command.category()
    }
}

impl fmt::Display for AddonIntent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.addon_id, self.command.as_str())
    }
}

/// Validate a Lua `CAER.command(name, args)` call into a queued intent.
pub fn validate_intent(
    addon_id: &str,
    name: &str,
    args: BTreeMap<String, IntentValue>,
) -> Result<AddonIntent, HostError> {
    let command = AddonCommand::parse(name).ok_or_else(|| HostError::UnknownCommand {
        name: name.to_string(),
    })?;

    match command {
        AddonCommand::Say | AddonCommand::SlashCommand => {
            match args.get("text").and_then(IntentValue::as_str) {
                Some(t) if !t.is_empty() => {}
                _ => {
                    return Err(HostError::InvalidIntent {
                        addon: addon_id.to_string(),
                        reason: format!(
                            "`{}` requires args.text (non-empty string)",
                            command.as_str()
                        ),
                    });
                }
            }
        }
        _ => {}
    }

    Ok(AddonIntent {
        addon_id: addon_id.to_string(),
        command,
        args,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_command_names_the_fix() {
        let err = validate_intent("x", "world.mutate", BTreeMap::new()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("world.mutate"));
        assert!(msg.contains("chat.say"));
    }

    #[test]
    fn say_without_text_is_rejected() {
        let err = validate_intent("x", "chat.say", BTreeMap::new()).unwrap_err();
        assert!(matches!(err, HostError::InvalidIntent { .. }));
    }

    #[test]
    fn catalog_command_queues_not_applies() {
        let mut args = BTreeMap::new();
        args.insert("text".into(), IntentValue::Str("hi".into()));
        let intent = validate_intent("HelloCAER", "chat.say", args).unwrap();
        assert_eq!(intent.command, AddonCommand::Say);
        assert_eq!(intent.category(), AddonCommandCategory::Chat);
    }
}
