//! `caer-script` — Rust-hosted Lua 5.1 addon platform (mlua).
//!
//! One VM per addon, curated globals (no FS/network/native FFI by default), instruction
//! budgets and memory caps, typed events/commands, and hot reload with a SavedVariables
//! major-version policy. Commands become [`AddonIntent`] values; this crate never mutates
//! WorldState, inventory, combat, movement, social, server, or persistence.
//!
//! rustdaoc product dispatch lives in `caer-render::addon_product`. This crate never mutates
//! WorldState. Not a CAER-1.0 claim.

mod catalog;
mod check;
mod command;
mod error;
mod event;
mod host;
mod intent;
mod manifest;
mod sandbox;

pub use catalog::{catalog_entries, AddonCatalogEntry, CatalogKind, Stability, CATALOG};
pub use check::{
    check_catalog, check_catalog_entries, CatalogError, REQUIRED_COMMAND_CATEGORIES,
    REQUIRED_EVENT_CATEGORIES,
};
pub use command::{AddonCommand, AddonCommandCategory};
pub use error::{Capability, HostError};
pub use event::{AddonEvent, AddonEventCategory};
pub use host::{
    in_tree_addons_dir, memory_manifest, run_capability_suite, AddonHost, AddonStatus,
    CallbackHandle, CapabilitySuiteReport, DispatchReport, EventPayload, HostLog, IntentValueLite,
    ReloadPolicy, ReloadReport,
};
pub use intent::{validate_intent, AddonIntent, IntentValue};
pub use manifest::{discover, load_manifest, AddonManifest, HOST_API_VERSION};
pub use sandbox::HostLimits;
