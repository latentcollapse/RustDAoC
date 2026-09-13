//! `caer-protocol` — the DoL-lineage DAoC wire protocol, headless.
//!
//! This crate is the **mirror image** of the DOLSharp / OpenDAoC server-side packet code:
//! where the server *reads* a client packet, we *write* it, and vice versa. Every wire fact
//! in this crate is traceable to the oracle sources (cloned under `oracle/`) and — the higher
//! bar — to golden packet traces captured from an original client against a local server.
//!
//! It has **no rendering, engine, or OS dependencies on purpose**: this crate alone is the
//! headless protocol client that powers (a) SoloDAoC AI bots and (b) shard load-testing, and
//! it is the foundation the full `game.dll` and any engine-hosted client build on top of.
//!
//! ## Byte-order convention (from the oracle)
//! DAoC's game-server protocol is **big-endian** by default (`PacketIn.ReadShort` reads the
//! first byte as the high byte). A handful of fields are explicitly little-endian
//! (`ReadShortLowEndian`); those are marked at each call site, never assumed.
//!
//! ## Module map
//! - [`checksum`] — the exact server checksum algorithm (`PacketProcessor.CalculateChecksum`).
//! - [`codec`]    — [`PacketReader`]/[`PacketWriter`] primitives (BE/LE ints, pascal strings).
//! - [`framing`]  — the two header layouts (client→server 12B incl. checksum; server→client 3B).
//! - [`codes`]    — packet-code enums + the 0xA8 client-code obfuscation.
//! - [`crypto`]   — the RSA→RC4 handshake interface and a pure-Rust RC4.
//! - [`entities`] — NPC/object create decoders (the visible-area population stream).
//! - [`overview`] — the CharacterOverview (0xFC) decoder (the char-select screen).
//! - [`region`]   — RegionChanged (0xB7) decoder (in-world region transition).
//! - [`session`]  — the connection state machine (login → char select → in-world).
//! - [`status`]   — the player's own vitals (0xAD), the source of the HUD bars.

pub mod career;
pub mod charcreate;
pub mod charsheet;
pub mod checksum;
pub mod class_catalog;
pub mod codec;
pub mod codes;
pub mod combat_anim;
pub mod coverage;
pub mod create_validity;
pub mod creation_adapters;
pub mod crypto;
pub mod customization;
pub mod death;
pub mod effects;
pub mod emblem;
pub mod emote;
pub mod encumberance;
pub mod entities;
pub mod equipment;
pub mod error;
pub mod findgroup;
pub mod framing;
pub mod inventory;
pub mod invverb;
pub mod login;
pub mod market;
pub mod merchant;
pub mod money;
pub mod overview;
pub mod packet_telemetry;
pub mod pets;
pub mod points;
pub mod quest;
pub mod region;
pub mod session;
pub mod shape2_loop;
pub mod siege;
pub mod skills;
pub mod social;
pub mod spells;
pub mod starting_stats;
pub mod stats_update;
pub mod status;
pub mod trainer;
pub mod transition;
pub mod typed_s2c_registry;
pub mod view_control;
pub mod weapon_armor;
pub mod worldverb;

pub use codec::{PacketReader, PacketWriter};
pub use error::{ProtocolError, Result};
pub use framing::{ClientPacketHeader, ServerPacketHeader};
pub use session::{SessionPhase, SessionState};
