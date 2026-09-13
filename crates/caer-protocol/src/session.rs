//! The connection state machine: the P1 spine, login → character select → in-world.
//!
//! This is transport-agnostic on purpose: it consumes decoded server packets and produces
//! client packets to send, but it does not own the TCP/UDP socket. That keeps it trivially
//! testable (drive it with canned packets, assert the emitted replies) and lets the same
//! machine back a headless bot, the game.dll, or an engine-hosted client unchanged.
//!
//! Phase transitions mirror the order the oracle's client-status gate enforces
//! (`eClientStatus`: NotConnected → Connecting → LoggedIn → PlayerInGame). We model the
//! subset that the login→in-world walk actually traverses.

use crate::codes;
use crate::framing::{ClientPacketHeader, ServerPacketHeader};

/// Where in the login→in-world walk a connection is. Ordering matters — a packet valid in one
/// phase is a protocol error in another (the oracle rejects out-of-phase packets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    /// Fresh socket, nothing sent.
    Disconnected,
    /// Sent `CryptKeyRequest`, awaiting the server's crypt-key reply.
    CryptHandshake,
    /// Crypt settled; sent `LoginRequest`, awaiting grant.
    Authenticating,
    /// Login accepted; querying/awaiting realm (OPEN_ORACLE CharacterOverviewRequest byte 0 →
    /// SendRealm; realm 0 = show realm select, 1..3 = bound realm).
    RealmSelect,
    /// Logged in with a chosen/bound realm; at the character-selection screen.
    CharacterSelect,
    /// Sent character select + `GameOpenRequest`; entering the world.
    EnteringWorld,
    /// Fully in-world: movement, combat, chat flow.
    InWorld,
    /// Logged out cleanly (server confirmed Quit 0xA4 → saved + removed). The socket may be closed
    /// without triggering link-death.
    Closed,
}

/// An action the session wants the transport to perform.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)] // Event carries ServerEvent by value; boxing would change the public Action API.
pub enum Action {
    /// Send this fully-framed client→server packet.
    Send(Vec<u8>),
    /// The session advanced to a new phase (for observability/tests).
    Phase(SessionPhase),
    /// A decoded, in-world server event the higher layer (engine/bot) should act on.
    Event(ServerEvent),
}

/// Decoded server→client events the game layer cares about. Curated for P1; grows as decoders
/// are ported.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerEvent {
    CryptKeyReceived,
    LoginGranted,
    /// Login rejected (LoginDenied 0x2C). OPEN_ORACLE PacketLib168: first byte = eLoginError.
    LoginDenied {
        error: u8,
    },
    /// Server attack-mode sync (AttackMode 0x74). OPEN_ORACLE PacketLib168 `[state][3 pad]`.
    AttackMode {
        attacking: bool,
    },
    /// Max movement speed percent (MaxSpeed 0xB6). OPEN_ORACLE PacketLib168.
    MaxSpeed {
        /// Percent of base speed (100 = normal run).
        percent: u16,
        turning_disabled: bool,
        water_percent: u8,
    },
    /// Character attributes / resists (StatsUpdate 0xFB). OPEN_ORACLE PacketLib175.
    StatsUpdated(crate::stats_update::StatsUpdate),
    /// The server assigned our session id (SessionID 0x28, in reply to our 0x10).
    SessionAssigned(u16),
    /// The character-select screen arrived and decoded.
    CharacterOverview(crate::overview::CharacterOverview),
    /// The server named the region endpoint (RegionServer 0xB1) — world entry proceeds.
    RegionHandoff {
        ip: String,
        port: u32,
    },
    EnteredWorld,
    /// A chat/system message (0xAF, v1127 form `[type:1][text C-string]`). Location prefixes
    /// ("@@" chat window, "##" popup) are stripped into the text as-is-meaningful.
    ChatMessage {
        chat_type: u8,
        text: String,
    },
    /// Our own spawn position + object id (PositionAndObjectID 0x20, sent during world entry).
    PlayerPosition {
        x: f32,
        y: f32,
        z: f32,
        object_id: u16,
        heading: u16,
    },
    /// A Dialog box (0x81) — group invite `0x05`, CustomDialog `0x06`, quest subscribe, etc.
    /// Never auto-answered; the driver sends [`SessionState::dialog_response`].
    Dialog {
        /// `eDialogCode` (payload byte 1).
        code: u8,
        data1: u16,
        data2: u16,
        data3: u16,
        data4: u16,
        message: String,
    },
    /// A living NPC came into view (NPCCreate 0xDA).
    NpcInView(crate::entities::Npc),
    /// A static object came into view (ObjectCreate 0xD9).
    ObjectInView(crate::entities::StaticObject),
    /// A known entity moved / changed state (ObjectUpdate 0xA1) — the world's live heartbeat.
    EntityUpdated(crate::entities::EntityUpdate),
    /// A page of the player's usable skills (VariousUpdate 0x16 subcode 0x01). Pages accumulate —
    /// see [`crate::skills::apply_page`].
    SkillsPage(crate::skills::SkillPage),
    /// The player's own character sheet (0x16 subcode 0x03) — level, realm rank, titles, guild.
    CharacterSheet(crate::charsheet::CharacterSheet),
    /// What a living thing is visibly wearing/wielding (0x15) — armour tiers, weapons, dyes.
    EquipmentUpdated(crate::equipment::EquipmentUpdate),
    /// Another player came into view (PlayerCreate 0x4B).
    PlayerInView(crate::entities::Player),
    /// An entity left our view (ObjectDelete 0xE1) and must be culled from the world. Without
    /// this the world only ever grows — despawned mobs linger forever at their last position.
    ObjectRemoved {
        object_id: u16,
    },
    /// Server-driven remove (RemoveObject 0xA2). Same cull as ObjectDelete; carries oracle oType.
    RemoveObject(crate::view_control::RemoveObject),
    /// Server-driven target change (ChangeTarget 0xF6). oid 0 = clear.
    TargetChanged(crate::view_control::ChangeTarget),
    /// UDP init reply (UDPInitReply 0x2F) — region IP + UDP port for freeshard UDP path.
    UdpInitReply(crate::view_control::UdpInitReply),
    /// BadNameCheckReply 0xC3 — char-create name filter result.
    BadNameCheckReply(crate::shape2_loop::BadNameCheckReply),
    /// DupNameCheckReply 0xCC — duplicate/invalid name result.
    DupNameCheckReply(crate::shape2_loop::DupNameCheckReply),
    /// CharacterCreateReply 0xF0 — create ack with name.
    CharacterCreateReply(crate::shape2_loop::CharacterCreateReply),
    /// CheckLOSRequest 0xD0 — server asks client for LOS.
    CheckLosRequest(crate::shape2_loop::CheckLosRequest),
    /// TimerWindow 0xF3 open/close.
    TimerWindow(crate::shape2_loop::TimerWindow),
    /// DisableSkills 0xD6 skill/spell lockouts.
    DisableSkills(crate::shape2_loop::DisableSkills),
    /// PlaySound 0xD3 UI/combat cue.
    PlaySound(crate::shape2_loop::PlaySound),
    /// SoundEffect 0xC9 positional audio.
    SoundEffect(crate::shape2_loop::SoundEffect),
    /// ModelChange 0xDB model/size swap.
    ModelChange(crate::shape2_loop::ModelChange),
    /// MovingObjectCreate 0x12 boats/siege movers.
    MovingObjectCreate(crate::shape2_loop::MovingObjectCreate),
    /// ObjectDataUpdate 0xEE level/name/guild refresh.
    ObjectDataUpdate(crate::shape2_loop::ObjectDataUpdate),
    /// Riding 0xC8 mount/dismount.
    Riding(crate::shape2_loop::Riding),
    /// PlayerModelTypeChange 0x8D.
    PlayerModelTypeChange(crate::shape2_loop::PlayerModelTypeChange),
    /// DelveInfo 0xD8 (PacketLib1110 null-terminated text).
    DelveInfo(crate::shape2_loop::DelveInfo),
    /// ControlledHorse 0x4E mount control.
    ControlledHorse(crate::shape2_loop::ControlledHorse),
    /// Our own health/power/endurance (CharacterStatusUpdate 0xAD) — what the HUD bars display,
    /// and the only source of our own absolute HP (entity updates carry percentages for others).
    StatusUpdate(crate::status::PlayerStatus),
    /// Structured combat swing feedback (CombatAnimation 0xBC). Carries result + target HP%;
    /// does **not** carry damage amounts — those remain on Message 0xAF until a dedicated packet.
    CombatAnimation(crate::combat_anim::CombatAnimation),
    /// Local-player bag / worn / vault slot contents (InventoryUpdate 0x02). Does **not** carry
    /// an avatar object id — visible mesh still comes from EquipmentUpdate 0x15.
    InventoryUpdated(crate::inventory::InventoryUpdate),
    /// Local-player purse (MoneyUpdate 0xFA).
    MoneyUpdated(crate::money::MoneyUpdate),
    /// A living left the world as a corpse (PlayerDeath 0xAE). Carries victim + optional killer only.
    PlayerDied(crate::death::PlayerDeath),
    /// A dead living was released / revived (PlayerRevive 0x89).
    PlayerRevived(crate::death::PlayerRevive),
    /// Merchant offer page (MerchantWindow 0x17). One packet = one page of the catalogue.
    MerchantWindow(crate::merchant::MerchantWindow),
    /// Spell cast wind-up (SpellCastAnimation 0x72).
    SpellCast(crate::spells::SpellCastAnimation),
    /// Spell effect / bolt impact (SpellEffectAnimation 0x1B) — PacketLib174 layout.
    SpellEffect(crate::spells::SpellEffectAnimation),
    /// Cast interrupted (InterruptSpellCast 0x73).
    SpellInterrupted(crate::spells::InterruptSpellCast),
    /// Buff/debuff/CC icon list (UpdateIcons 0x7F, PacketLib1110).
    UpdateIcons(crate::effects::UpdateIcons),
    /// Maintained concentration effects (ConcentrationList 0x75).
    ConcentrationList(crate::effects::ConcentrationList),
    /// XP / realm / bounty points (CharacterPointsUpdate 0x91).
    CharacterPoints(crate::points::CharacterPoints),
    /// In-world region transition (RegionChanged 0xB7) — PacketLib174 / 1.127.
    RegionChanged(crate::region::RegionChanged),
    /// Door open/close confirm (DoorState 0x99 S2C). OPEN_ORACLE SendDoorState.
    DoorState(crate::worldverb::DoorState),
    /// Server-driven ground target change (ChangeGroundTarget 0xDF). Not an echo of 0xEC.
    GroundTargetChanged(crate::worldverb::ChangeGroundTarget),
    /// Teleport / MoveTo confirm (CharacterJump 0x04) — carries post-jump X/Y/Z.
    CharacterJump(crate::worldverb::CharacterJump),
    /// Group roster vitals (GroupMemberUpdate 0x70). Join evidence for Stream G.
    GroupMemberUpdate(crate::social::GroupMemberUpdate),
    /// Group window names / clear (`VariousUpdate` 0x16 subcode 0x06). Empty = left/disbanded.
    GroupWindow(crate::social::GroupWindow),
    /// Player trade window open/update/close (TradeWindow 0xEA). Close = 40 zero bytes.
    TradeWindow(crate::social::TradeWindow),
    /// Quest log slot update (QuestEntry 0x83). Accept-path provenance for quest subscribe.
    QuestEntry {
        entry: crate::quest::QuestEntry,
        /// Raw payload — decline falsifier compares byte-identical quest state.
        payload: Vec<u8>,
    },
    /// Controlled-pet window (PetWindow 0x88, PacketLib181 / 1.127). One oid per packet.
    PetWindow(crate::pets::PetWindow),
    /// GameOpenReply 0x2D — world-open handshake ack (PacketLib168 WriteByte(0)).
    GameOpenReply {
        flag: u8,
    },
    /// Account realm reply 0xFE (realm 0 = show realm select).
    Realm {
        realm: u8,
    },
    /// ConsignmentMerchantMoney 0x1E — house market purse (not player 0xFA).
    ConsignmentMerchantMoney(crate::money::ConsignmentMerchantMoney),
    /// MarketExplorerWindow 0x1F header / empty-close.
    MarketExplorer(crate::market::MarketExplorer),
    /// TrainerWindow 0x7B.
    TrainerWindow(crate::trainer::TrainerWindow),
    /// FindGroupUpdate 0x86 (LFG, not GroupWindow).
    FindGroupUpdate(crate::findgroup::FindGroupUpdate),
    /// Encumberance 0xBD.
    Encumberance(crate::encumberance::Encumberance),
    /// ObjectGuildID 0xDE.
    ObjectGuildId(crate::social::ObjectGuildId),
    /// EmblemDialogue 0xE2.
    EmblemDialogue(crate::emblem::EmblemDialogue),
    /// SiegeWeaponAnimation 0xE3 prefix.
    SiegeWeaponAnimation(crate::siege::SiegeWeaponAnimation),
    /// SiegeWeaponInterface 0xF5.
    SiegeWeaponInterface(crate::siege::SiegeWeaponInterface),
    /// EmoteAnimation 0xF9.
    EmoteAnimation(crate::emote::EmoteAnimation),
    /// The server confirmed our logout (Quit 0xA4, sent after a `/quit` countdown completes): it
    /// has saved the character and removed it from the world, so the socket can now be closed
    /// cleanly WITHOUT triggering link-death. `total_out` = whether this closes the whole client.
    LoggedOut {
        total_out: bool,
        level: u8,
    },
    /// Raw undecoded server packet (code + payload) — surfaced so nothing is silently dropped
    /// while the decoder table is still being filled in.
    Raw {
        code: u8,
        payload: Vec<u8>,
    },
}

impl ServerEvent {
    /// Unknown opcode: keep bytes as Raw and record a gap.
    #[must_use]
    pub fn raw_unknown(code: u8, payload: &[u8]) -> Self {
        crate::packet_telemetry::record_unknown(code);
        Self::Raw {
            code,
            payload: payload.to_vec(),
        }
    }

    /// Known opcode whose decoder failed: keep bytes as Raw and record a gap.
    #[must_use]
    pub fn raw_malformed(code: u8, payload: &[u8]) -> Self {
        crate::packet_telemetry::record_malformed(code);
        Self::Raw {
            code,
            payload: payload.to_vec(),
        }
    }

    /// Coverage-matrix event name. Distinct from packet enum names (`PlayerCreate` → `PlayerInView`).
    #[must_use]
    pub fn coverage_name(&self) -> &'static str {
        match self {
            Self::CryptKeyReceived => "CryptKeyReceived",
            Self::LoginGranted => "LoginGranted",
            Self::LoginDenied { .. } => "LoginDenied",
            Self::AttackMode { .. } => "AttackMode",
            Self::MaxSpeed { .. } => "MaxSpeed",
            Self::StatsUpdated(_) => "StatsUpdated",
            Self::SessionAssigned(_) => "SessionAssigned",
            Self::CharacterOverview(_) => "CharacterOverview",
            Self::RegionHandoff { .. } => "RegionHandoff",
            Self::EnteredWorld => "EnteredWorld",
            Self::ChatMessage { .. } => "ChatMessage",
            Self::PlayerPosition { .. } => "PlayerPosition",
            Self::Dialog { .. } => "Dialog",
            Self::NpcInView(_) => "NpcInView",
            Self::ObjectInView(_) => "ObjectInView",
            Self::EntityUpdated(_) => "EntityUpdated",
            Self::SkillsPage(_) => "SkillsPage",
            Self::CharacterSheet(_) => "CharacterSheet",
            Self::EquipmentUpdated(_) => "EquipmentUpdated",
            Self::PlayerInView(_) => "PlayerInView",
            Self::ObjectRemoved { .. } => "ObjectRemoved",
            Self::RemoveObject(_) => "RemoveObject",
            Self::TargetChanged(_) => "TargetChanged",
            Self::UdpInitReply(_) => "UdpInitReply",
            Self::BadNameCheckReply(_) => "BadNameCheckReply",
            Self::DupNameCheckReply(_) => "DupNameCheckReply",
            Self::CharacterCreateReply(_) => "CharacterCreateReply",
            Self::CheckLosRequest(_) => "CheckLosRequest",
            Self::TimerWindow(_) => "TimerWindow",
            Self::DisableSkills(_) => "DisableSkills",
            Self::PlaySound(_) => "PlaySound",
            Self::SoundEffect(_) => "SoundEffect",
            Self::ModelChange(_) => "ModelChange",
            Self::MovingObjectCreate(_) => "MovingObjectCreate",
            Self::ObjectDataUpdate(_) => "ObjectDataUpdate",
            Self::Riding(_) => "Riding",
            Self::PlayerModelTypeChange(_) => "PlayerModelTypeChange",
            Self::DelveInfo(_) => "DelveInfo",
            Self::ControlledHorse(_) => "ControlledHorse",
            Self::StatusUpdate(_) => "StatusUpdate",
            Self::CombatAnimation(_) => "CombatAnimation",
            Self::InventoryUpdated(_) => "InventoryUpdated",
            Self::MoneyUpdated(_) => "MoneyUpdated",
            Self::PlayerDied(_) => "PlayerDied",
            Self::PlayerRevived(_) => "PlayerRevived",
            Self::MerchantWindow(_) => "MerchantWindow",
            Self::SpellCast(_) => "SpellCast",
            Self::SpellEffect(_) => "SpellEffect",
            Self::SpellInterrupted(_) => "SpellInterrupted",
            Self::UpdateIcons(_) => "UpdateIcons",
            Self::ConcentrationList(_) => "ConcentrationList",
            Self::CharacterPoints(_) => "CharacterPoints",
            Self::RegionChanged(_) => "RegionChanged",
            Self::DoorState(_) => "DoorState",
            Self::GroundTargetChanged(_) => "GroundTargetChanged",
            Self::CharacterJump(_) => "CharacterJump",
            Self::GroupMemberUpdate(_) => "GroupMemberUpdate",
            Self::GroupWindow(_) => "GroupWindow",
            Self::TradeWindow(_) => "TradeWindow",
            Self::QuestEntry { .. } => "QuestEntry",
            Self::PetWindow(_) => "PetWindow",
            Self::GameOpenReply { .. } => "GameOpenReply",
            Self::Realm { .. } => "Realm",
            Self::ConsignmentMerchantMoney(_) => "ConsignmentMerchantMoney",
            Self::MarketExplorer(_) => "MarketExplorer",
            Self::TrainerWindow(_) => "TrainerWindow",
            Self::FindGroupUpdate(_) => "FindGroupUpdate",
            Self::Encumberance(_) => "Encumberance",
            Self::ObjectGuildId(_) => "ObjectGuildId",
            Self::EmblemDialogue(_) => "EmblemDialogue",
            Self::SiegeWeaponAnimation(_) => "SiegeWeaponAnimation",
            Self::SiegeWeaponInterface(_) => "SiegeWeaponInterface",
            Self::EmoteAnimation(_) => "EmoteAnimation",
            Self::LoggedOut { .. } => "LoggedOut",
            Self::Raw { .. } => "Raw",
        }
    }
}

/// Per-tick motion state that rides on a position update.
///
/// Separate from the position itself because these are things the SERVER derives behaviour from —
/// which zone you are in, whether you are jumping or strafing — rather than raw coordinates.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlayerMotion {
    /// Our own object id, echoed back from 1.127 onward.
    pub object_id: u16,
    /// The zone we are standing in. Zero is a real zone, so this must be filled in.
    pub zone_id: u16,
    /// Mid-jump. The server reads this as `playerAction & 0x40`.
    pub jumping: bool,
    /// Strafing left/right. The server reads any of `playerState & 0xE000`.
    pub strafing: bool,
}

impl PlayerMotion {
    /// The `playerAction` byte. 0x80 is what the reference client sends as its baseline; 0x40 adds
    /// the jump bit the server tests for.
    #[must_use]
    pub fn player_action(self) -> u8 {
        let mut v = 0x80;
        if self.jumping {
            v |= 0x40;
        }
        v
    }

    /// The `playerState` short. The server only tests `& 0xE000` for strafing.
    #[must_use]
    pub fn player_state(self) -> u16 {
        if self.strafing {
            0x2000
        } else {
            0
        }
    }
}

/// The connection state. Holds only protocol-level identity; game state lives above this.
#[derive(Debug, Clone)]
pub struct SessionState {
    phase: SessionPhase,
    session_id: u16,
    sequence: u16,
    account: String,
    password: String,
    /// Machine-specific 8-byte client id carried in the LoginRequest trailer (P0 finding).
    /// Defaults to zeros; set from a capture or a synthesized value.
    client_id: Vec<u8>,
    /// Realm whose characters to list (1 = Albion, 2 = Midgard, 3 = Hibernia). The overview
    /// request carries this byte; 0 would instead query the account's assigned realm.
    realm: u8,
    /// Guards against re-requesting the character overview on repeated session packets.
    overview_requested: bool,
    /// Realm-local slot of the character being played, set by [`select_character`].
    selected_slot: u8,
    /// Current heading (DAoC units, 0..4096), seeded from the spawn packet (0x20).
    heading: u16,
}

/// Convert the slot rendered in a realm-local 1.126 `CharacterOverview` into the account-wide
/// index DOL's v168 `CharacterSelectRequestHandler` and `RegionListRequestHandler` consume.
///
/// The overview decoder can only yield `0..10` slots, and `request_character_overview` clamps the
/// realm to `1..=3`; the debug assertions keep that source contract visible in development while
/// the saturating arithmetic avoids wrapping an invalid external caller into another character.
fn dol_character_wire_slot(realm: u8, realm_local_slot: u8) -> u8 {
    debug_assert!((1..=3).contains(&realm));
    debug_assert!(realm_local_slot < 10);
    realm_local_slot.saturating_add(realm.saturating_sub(1).min(2) * 10)
}

impl SessionState {
    #[must_use]
    pub fn new(account: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            phase: SessionPhase::Disconnected,
            session_id: 0,
            sequence: 0,
            account: account.into(),
            password: password.into(),
            client_id: vec![0u8; 8],
            realm: 1,
            overview_requested: false,
            selected_slot: 0,
            heading: 0,
        }
    }

    /// Set the 8-byte client id used in the LoginRequest trailer.
    pub fn with_client_id(mut self, id: impl Into<Vec<u8>>) -> Self {
        self.client_id = id.into();
        self
    }

    /// Set the realm whose character list to request (1 = Albion, 2 = Midgard, 3 = Hibernia).
    pub fn with_realm(mut self, realm: u8) -> Self {
        self.realm = realm;
        self
    }

    #[must_use]
    pub fn phase(&self) -> SessionPhase {
        self.phase
    }

    #[must_use]
    pub fn session_id(&self) -> u16 {
        self.session_id
    }

    fn next_sequence(&mut self) -> u16 {
        self.sequence = self.sequence.wrapping_add(1);
        self.sequence
    }

    /// Frame a client→server packet with the current session identity.
    fn frame(&mut self, code: u8, payload: &[u8]) -> Vec<u8> {
        let hdr = ClientPacketHeader {
            packet_size: 0, // encode derives it
            sequence: self.next_sequence(),
            session_id: self.session_id,
            parameter: 0,
            id: code,
        };
        // encode only errors on oversize; P1 control packets are tiny, so unwrap is safe here.
        hdr.encode(payload)
            .expect("control packet within size bound")
    }

    /// Kick off the connection: emit the crypt-key request with the real 7-byte version
    /// announce (the server replies with version + crypt key). We do NOT send the 256-byte RSA
    /// block — the server ignores it when encryption is disabled (oracle CryptKeyRequestHandler).
    pub fn begin(&mut self) -> Vec<Action> {
        let pkt = self.frame(
            codes::client::CryptKeyRequest,
            &crate::crypto::encode_crypt_key_request(),
        );
        self.phase = SessionPhase::CryptHandshake;
        vec![
            Action::Phase(SessionPhase::CryptHandshake),
            Action::Send(pkt),
        ]
    }

    /// Feed one decoded server packet (already de-framed and decrypted) and get the resulting
    /// actions. This is the heart of the machine; the transport loop calls it per packet.
    pub fn on_server_packet(&mut self, header: ServerPacketHeader, payload: &[u8]) -> Vec<Action> {
        match (self.phase, header.code) {
            (SessionPhase::CryptHandshake, code) if code == codes::server::CryptKey => {
                // Crypt settled → send login. Format verified against the golden trace: 4-byte-LE
                // length-prefixed account + password + client-id trailer (crate::login). The
                // client id is machine-specific; `self.client_id` carries it (captured or
                // synthesized). Game stream is plaintext on this server, so no encryption here.
                let body = crate::login::encode_login_request(
                    &self.account,
                    &self.password,
                    &self.client_id,
                );
                let pkt = self.frame(codes::client::LoginRequest, &body);
                self.phase = SessionPhase::Authenticating;
                vec![
                    Action::Event(ServerEvent::CryptKeyReceived),
                    Action::Phase(SessionPhase::Authenticating),
                    Action::Send(pkt),
                ]
            }
            (SessionPhase::Authenticating, code) if code == codes::server::LoginGranted => {
                // Login accepted. The server now WAITS (oracle: after SendLoginGranted nothing
                // more is pushed) — the CLIENT drives the next step. CharacterSelectRequest
                // (0x10, index 0) while ClientState is still Connecting yields SessionID only
                // (1126 handler does not LoadPlayer until CharScreen). Do **not** jump to
                // CharacterSelect or request a realm-1 overview here — that skips RealmSelect
                // and can bind an unbound account (OPEN_ORACLE CharacterOverviewRequestHandler).
                self.phase = SessionPhase::RealmSelect;
                let pkt = self.frame(codes::client::CharacterSelectRequest, &[0x00]);
                vec![
                    Action::Event(ServerEvent::LoginGranted),
                    Action::Phase(SessionPhase::RealmSelect),
                    Action::Send(pkt),
                ]
            }
            (SessionPhase::Authenticating, code) if code == codes::server::LoginDenied => {
                let error = payload.first().copied().unwrap_or(0);
                self.phase = SessionPhase::Closed;
                vec![
                    Action::Event(ServerEvent::LoginDenied { error }),
                    Action::Phase(SessionPhase::Closed),
                ]
            }
            // 1124+ create/customize refresh (OPEN_ORACLE CharacterCreateRequestHandler): on
            // success the server sends LoginGranted again — *not* CharacterOverview. Re-request
            // the overview directly. Do **not** re-send CharacterSelectRequest(0): after a create
            // into slot 0, the 1126 select handler treats index 0 as that character and LoadPlayer
            // leaves a half-initialized Player that OverviewRequest then NRE's clearing.
            (SessionPhase::CharacterSelect, code) if code == codes::server::LoginGranted => {
                let pkt = self.frame(codes::client::CharacterOverviewRequest, &[self.realm]);
                vec![Action::Event(ServerEvent::LoginGranted), Action::Send(pkt)]
            }
            (SessionPhase::RealmSelect, code)
                if code == codes::server::SessionID && !self.overview_requested =>
            {
                // Adopt the assigned session id (2-byte LE, oracle SendSessionID). Query the
                // account realm with CharacterOverviewRequest(0) — OPEN_ORACLE replies
                // SendRealm(None) for unbound accounts (show realm select) or SendRealm(N)
                // when already bound. Never invent realm 1 here.
                if payload.len() >= 2 {
                    self.session_id = u16::from(payload[0]) | (u16::from(payload[1]) << 8);
                }
                self.overview_requested = true;
                let pkt = self.frame(codes::client::CharacterOverviewRequest, &[0x00]);
                vec![
                    Action::Event(ServerEvent::SessionAssigned(self.session_id)),
                    Action::Send(pkt),
                ]
            }
            (SessionPhase::RealmSelect, code) if code == codes::server::Realm => {
                let realm = payload.first().copied().unwrap_or(0);
                if realm == 0 {
                    // Unbound / allow-all: player picks on RealmSelect. No overview yet.
                    vec![Action::Event(ServerEvent::Realm { realm: 0 })]
                } else {
                    // Bound account: server named the realm — request that overview and wait
                    // for CharacterOverview before leaving RealmSelect phase.
                    let realm = realm.clamp(1, 3);
                    self.realm = realm;
                    let pkt = self.frame(codes::client::CharacterOverviewRequest, &[realm]);
                    vec![
                        Action::Event(ServerEvent::Realm { realm }),
                        Action::Send(pkt),
                    ]
                }
            }
            (SessionPhase::RealmSelect | SessionPhase::CharacterSelect, code)
                if code == codes::server::CharacterOverview =>
            {
                // Decode failure surfaces the packet raw rather than killing the session — the
                // higher layer sees exactly what arrived either way.
                match crate::overview::decode_character_overview(payload) {
                    Ok(ov) => {
                        let mut out = Vec::new();
                        if self.phase == SessionPhase::RealmSelect {
                            self.phase = SessionPhase::CharacterSelect;
                            out.push(Action::Phase(SessionPhase::CharacterSelect));
                        }
                        out.push(Action::Event(ServerEvent::CharacterOverview(ov)));
                        out
                    }
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (SessionPhase::EnteringWorld, code) if code == codes::server::SessionID => {
                // The server re-sends the session id after every CharacterSelectRequest; keep
                // ours in sync but nothing to do.
                if payload.len() >= 2 {
                    self.session_id = u16::from(payload[0]) | (u16::from(payload[1]) << 8);
                }
                vec![]
            }
            (SessionPhase::EnteringWorld, code) if code == codes::server::RegionServer => {
                // Region handoff: `[ip: pstrIntLE(0x14)][port: u32 LE][port: u32 LE]` (oracle
                // SendRegions). On a DoL freeshard the world continues on THIS socket — the
                // capture's single connection carries login through in-world play — so we don't
                // reconnect; we proceed with the "Play"-button sequence the capture shows:
                // re-select the char (loads Player server-side), then WorldInit, PlayerInit,
                // GameOpen (flag 0 = TCP-only, no UDP channel).
                let mut r = crate::codec::PacketReader::new(payload);
                let ip = r.pascal_string_int_le().unwrap_or_default();
                let port = r.u32_le().unwrap_or(0);
                let select =
                    self.frame(codes::client::CharacterSelectRequest, &[self.selected_slot]);
                let world_init = self.frame(codes::client::WorldInitRequest, &[0x00]);
                let player_init = self.frame(codes::client::PlayerInitRequest, &[0x01]);
                let game_open = self.frame(codes::client::GameOpenRequest, &[0x00]);
                vec![
                    Action::Event(ServerEvent::RegionHandoff { ip, port }),
                    Action::Send(select),
                    Action::Send(world_init),
                    Action::Send(player_init),
                    Action::Send(game_open),
                ]
            }
            (SessionPhase::EnteringWorld, code) if code == codes::server::PositionAndObjectID => {
                // Our spawn: [x f32 LE][y f32 LE][z f32 LE][objectId u16 BE][heading u16 BE]…
                // (oracle SendPlayerPositionAndObjectID; the tail is zone/region/flag fields).
                let mut r = crate::codec::PacketReader::new(payload);
                let (x, y, z) = (
                    r.f32_le().unwrap_or(0.0),
                    r.f32_le().unwrap_or(0.0),
                    r.f32_le().unwrap_or(0.0),
                );
                let object_id = r.u16().unwrap_or(0);
                let heading = r.u16().unwrap_or(0);
                self.heading = heading;
                vec![Action::Event(ServerEvent::PlayerPosition {
                    x,
                    y,
                    z,
                    object_id,
                    heading,
                })]
            }
            (SessionPhase::EnteringWorld, code) if code == codes::server::CharacterInitFinished => {
                // The oracle's SendPlayerInitFinished — the definitive "you are in the world".
                self.phase = SessionPhase::InWorld;
                vec![
                    Action::Event(ServerEvent::EnteredWorld),
                    Action::Phase(SessionPhase::InWorld),
                ]
            }
            // The ping echo needs no action in any phase.
            (_, code) if code == codes::server::PingReply => vec![],
            // The visible-area population stream: NPCs and static objects. Arrives during
            // world entry and afterward as things come into range; a decode failure surfaces
            // the packet raw rather than dropping it.
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::NPCCreate =>
            {
                match crate::entities::decode_npc_create(payload) {
                    Ok(npc) => vec![Action::Event(ServerEvent::NpcInView(npc))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::ObjectCreate =>
            {
                match crate::entities::decode_object_create(payload) {
                    Ok(obj) => vec![Action::Event(ServerEvent::ObjectInView(obj))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::ObjectUpdate =>
            {
                match crate::entities::decode_object_update(payload) {
                    Ok(u) => vec![Action::Event(ServerEvent::EntityUpdated(u))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            // The player's usable skills (0x16 VariousUpdate, subcode 0x01). 0x16 is multiplexed;
            // other subcodes are legitimately not-skills and fall through to Raw rather than being
            // treated as a decode failure.
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::VariousUpdate =>
            {
                // Subcode 0x01 is the skill list; 0x03 is the character sheet (level, realm rank,
                // titles); 0x06 is the group window (PacketLib1125 SendGroupWindowUpdate).
                match crate::charsheet::decode(payload) {
                    Ok(Some(sheet)) => {
                        return vec![Action::Event(ServerEvent::CharacterSheet(sheet))];
                    }
                    Ok(None) => {}
                    Err(_) => {}
                }
                match crate::social::decode_group_window(payload) {
                    Ok(Some(gw)) => return vec![Action::Event(ServerEvent::GroupWindow(gw))],
                    Ok(None) => {}
                    Err(_) => {}
                }
                match crate::skills::decode_skills(payload) {
                    Ok(Some(page)) => vec![Action::Event(ServerEvent::SkillsPage(page))],
                    _ => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            // Visible equipment (0x15). Paired with each create packet, and the source of NPC
            // armour tiers, NPC faces and the player's own body textures.
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::EquipmentUpdate =>
            {
                match crate::equipment::decode(payload) {
                    Ok(e) => vec![Action::Event(ServerEvent::EquipmentUpdated(e))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            // CombatAnimation 0xBC — structured swing result (REQ-021 / SCN-05).
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::CombatAnimation =>
            {
                match crate::combat_anim::decode(payload) {
                    Ok(a) => vec![Action::Event(ServerEvent::CombatAnimation(a))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            // InventoryUpdate 0x02 — local player bags / worn gear (REQ-006 / SCN-06).
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::InventoryUpdate =>
            {
                match crate::inventory::decode(payload) {
                    Ok(u) => vec![Action::Event(ServerEvent::InventoryUpdated(u))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            // MoneyUpdate 0xFA — local player purse.
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::MoneyUpdate =>
            {
                match crate::money::decode(payload) {
                    Ok(m) => vec![Action::Event(ServerEvent::MoneyUpdated(m))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            // PlayerDeath 0xAE — victim + optional killer (PacketLib168).
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::PlayerDeath =>
            {
                match crate::death::decode_death(payload) {
                    Ok(d) => vec![Action::Event(ServerEvent::PlayerDied(d))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            // PlayerRevive 0x89 — release / revive.
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::PlayerRevive =>
            {
                match crate::death::decode_revive(payload) {
                    Ok(v) => vec![Action::Event(ServerEvent::PlayerRevived(v))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            // MerchantWindow 0x17 — one page of NPC offers (PacketLib1125 / SCN-08).
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::MerchantWindow =>
            {
                match crate::merchant::decode(payload) {
                    Ok(w) => vec![Action::Event(ServerEvent::MerchantWindow(w))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            // SpellCastAnimation 0x72 — cast wind-up (PacketLib168 / SCN-07).
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::SpellCastAnimation =>
            {
                match crate::spells::decode_cast(payload) {
                    Ok(c) => vec![Action::Event(ServerEvent::SpellCast(c))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            // SpellEffectAnimation 0x1B — impact / bolt (PacketLib174 / SCN-07).
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::SpellEffectAnimation =>
            {
                match crate::spells::decode_effect(payload) {
                    Ok(e) => vec![Action::Event(ServerEvent::SpellEffect(e))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            // InterruptSpellCast 0x73 — cancel active cast (PacketLib168 / SCN-07).
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::InterruptSpellCast =>
            {
                match crate::spells::decode_interrupt(payload) {
                    Ok(i) => vec![Action::Event(ServerEvent::SpellInterrupted(i))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::UpdateIcons =>
            {
                match crate::effects::decode_update_icons(payload) {
                    Ok(u) => vec![Action::Event(ServerEvent::UpdateIcons(u))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::ConcentrationList =>
            {
                match crate::effects::decode_concentration_list(payload) {
                    Ok(l) => vec![Action::Event(ServerEvent::ConcentrationList(l))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::CharacterPointsUpdate =>
            {
                match crate::points::decode_points(payload) {
                    Ok(p) => vec![Action::Event(ServerEvent::CharacterPoints(p))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            // RegionChanged 0xB7 — in-world region transition (PacketLib174 / System 3).
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::RegionChanged =>
            {
                match crate::region::decode(payload) {
                    Ok(r) => vec![Action::Event(ServerEvent::RegionChanged(r))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            // DoorState 0x99 S2C — open/close confirm after DoorRequest (System 5 Stream A).
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::DoorState =>
            {
                match crate::worldverb::decode_door_state(payload) {
                    Ok(d) => vec![Action::Event(ServerEvent::DoorState(d))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            // ChangeGroundTarget 0xDF — server-driven GT (GroundAssist); not an echo of 0xEC.
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::ChangeGroundTarget =>
            {
                match crate::worldverb::decode_change_ground_target(payload) {
                    Ok(g) => vec![Action::Event(ServerEvent::GroundTargetChanged(g))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            // CharacterJump 0x04 — MoveTo / jump teleport confirm (System 5 Stream A GT landing).
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::CharacterJump =>
            {
                match crate::worldverb::decode_character_jump(payload) {
                    Ok(j) => vec![Action::Event(ServerEvent::CharacterJump(j))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            // GroupMemberUpdate 0x70 — roster vitals (PacketLib1125; Stream G).
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::GroupMemberUpdate =>
            {
                match crate::social::decode_group_member_update(payload) {
                    Ok(g) => vec![Action::Event(ServerEvent::GroupMemberUpdate(g))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            // TradeWindow 0xEA — open/update or 40-zero close (Stream T).
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::TradeWindow =>
            {
                match crate::social::decode_trade_window(payload) {
                    Ok(t) => vec![Action::Event(ServerEvent::TradeWindow(t))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            // PetWindow 0x88 — controlled pet (PacketLib181 / 1.127 inherit).
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::PetWindow =>
            {
                match crate::pets::decode_pet_window(payload) {
                    Ok(p) => vec![Action::Event(ServerEvent::PetWindow(p))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            // QuestEntry 0x83 — quest-log slot (System 6 Stream Q). Never invent journal rows.
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::QuestEntry =>
            {
                match crate::quest::decode(payload) {
                    Ok(entry) => vec![Action::Event(ServerEvent::QuestEntry {
                        entry,
                        payload: payload.to_vec(),
                    })],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            // Another player came into view (0x4B — NOT the enum's 0xD4; see codes.rs).
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::PlayerCreate =>
            {
                match crate::entities::decode_player_create(payload) {
                    Ok(p) => vec![Action::Event(ServerEvent::PlayerInView(p))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            // An entity left our view (0xE1): [oid u16][unknown u16]. Only the id is acted on.
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::ObjectDelete && payload.len() >= 2 =>
            {
                let object_id = u16::from_be_bytes([payload[0], payload[1]]);
                vec![Action::Event(ServerEvent::ObjectRemoved { object_id })]
            }
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::RemoveObject =>
            {
                match crate::view_control::decode_remove_object(payload) {
                    Ok(o) => vec![Action::Event(ServerEvent::RemoveObject(o))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::ChangeTarget =>
            {
                match crate::view_control::decode_change_target(payload) {
                    Ok(t) => vec![Action::Event(ServerEvent::TargetChanged(t))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (_, code) if code == codes::server::UDPInitReply => {
                match crate::view_control::decode_udp_init_reply(payload) {
                    Ok(u) => vec![Action::Event(ServerEvent::UdpInitReply(u))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (_, code) if code == codes::server::BadNameCheckReply => {
                match crate::shape2_loop::decode_bad_name_check_reply(payload) {
                    Ok(r) => vec![Action::Event(ServerEvent::BadNameCheckReply(r))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (_, code) if code == codes::server::DupNameCheckReply => {
                match crate::shape2_loop::decode_dup_name_check_reply(payload) {
                    Ok(r) => vec![Action::Event(ServerEvent::DupNameCheckReply(r))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (_, code) if code == codes::server::CharacterCreateReply => {
                match crate::shape2_loop::decode_character_create_reply(payload) {
                    Ok(r) => vec![Action::Event(ServerEvent::CharacterCreateReply(r))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (_, code) if code == codes::server::CheckLosRequest => {
                match crate::shape2_loop::decode_check_los_request(payload) {
                    Ok(r) => vec![Action::Event(ServerEvent::CheckLosRequest(r))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (_, code) if code == codes::server::TimerWindow => {
                match crate::shape2_loop::decode_timer_window(payload) {
                    Ok(r) => vec![Action::Event(ServerEvent::TimerWindow(r))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (_, code) if code == codes::server::DisableSkills => {
                match crate::shape2_loop::decode_disable_skills(payload) {
                    Ok(r) => vec![Action::Event(ServerEvent::DisableSkills(r))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (_, code) if code == codes::server::PlaySound => {
                match crate::shape2_loop::decode_play_sound(payload) {
                    Ok(r) => vec![Action::Event(ServerEvent::PlaySound(r))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (_, code) if code == codes::server::SoundEffect => {
                match crate::shape2_loop::decode_sound_effect(payload) {
                    Ok(r) => vec![Action::Event(ServerEvent::SoundEffect(r))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (_, code) if code == codes::server::ModelChange => {
                match crate::shape2_loop::decode_model_change(payload) {
                    Ok(r) => vec![Action::Event(ServerEvent::ModelChange(r))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::MovingObjectCreate =>
            {
                match crate::shape2_loop::decode_moving_object_create(payload) {
                    Ok(r) => vec![Action::Event(ServerEvent::MovingObjectCreate(r))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (_, code) if code == codes::server::ObjectDataUpdate => {
                match crate::shape2_loop::decode_object_data_update(payload) {
                    Ok(r) => vec![Action::Event(ServerEvent::ObjectDataUpdate(r))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (_, code) if code == codes::server::Riding => {
                match crate::shape2_loop::decode_riding(payload) {
                    Ok(r) => vec![Action::Event(ServerEvent::Riding(r))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (_, code) if code == codes::server::PlayerModelTypeChange => {
                match crate::shape2_loop::decode_player_model_type_change(payload) {
                    Ok(r) => vec![Action::Event(ServerEvent::PlayerModelTypeChange(r))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (_, code) if code == codes::server::DelveInfo => {
                match crate::shape2_loop::decode_delve_info(payload) {
                    Ok(r) => vec![Action::Event(ServerEvent::DelveInfo(r))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (_, code) if code == codes::server::ControlledHorse => {
                match crate::shape2_loop::decode_controlled_horse(payload) {
                    Ok(r) => vec![Action::Event(ServerEvent::ControlledHorse(r))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            // Our own vitals (0xAD). Accepted from EnteringWorld onward: the server sends the first
            // status update as part of world entry, before CharacterInitFinished, so gating this on
            // InWorld alone would drop the very packet that fills the HUD on spawn.
            (SessionPhase::EnteringWorld | SessionPhase::InWorld, code)
                if code == codes::server::CharacterStatusUpdate =>
            {
                match crate::status::decode_status_update(payload) {
                    Ok(s) => vec![Action::Event(ServerEvent::StatusUpdate(s))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            // Dialog box (0x81): [00][code][data1 u16][data2][data3][data4][type][wrap][msg\0].
            // All dialog codes — including group invite 0x05 — surface structured and are never
            // auto-clicked. Player UI / live RTs own Yes/No via DialogResponse 0x82.
            (SessionPhase::InWorld, code)
                if code == codes::server::Dialog && payload.len() >= 12 =>
            {
                let dialog_code = payload[1];
                let data1 = u16::from_be_bytes([payload[2], payload[3]]);
                let data2 = u16::from_be_bytes([payload[4], payload[5]]);
                let data3 = u16::from_be_bytes([payload[6], payload[7]]);
                let data4 = u16::from_be_bytes([payload[8], payload[9]]);
                let msg_end = payload[12..]
                    .iter()
                    .position(|&c| c == 0)
                    .map_or(payload.len(), |p| 12 + p);
                let message: String = payload[12..msg_end].iter().map(|&b| b as char).collect();
                vec![Action::Event(ServerEvent::Dialog {
                    code: dialog_code,
                    data1,
                    data2,
                    data3,
                    data4,
                    message,
                })]
            }
            // Chat/system text (v1127): [type:1][text C-string], optional "@@"/"##" location
            // prefix. Decoded in any phase — the server talks from world entry onward.
            (_, code) if code == codes::server::Message && !payload.is_empty() => {
                let chat_type = payload[0];
                let raw = &payload[1..];
                let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
                let mut text: &[u8] = &raw[..end];
                if text.starts_with(b"@@") || text.starts_with(b"##") {
                    text = &text[2..];
                }
                let text: String = text.iter().map(|&b| b as char).collect();
                vec![Action::Event(ServerEvent::ChatMessage { chat_type, text })]
            }
            // Logout confirmation (0xA4): [totalOut u8][level u8]. The server sends this the moment
            // it has saved + removed our character (oracle QuitTimerCallback → SendPlayerQuit), so
            // it is the signal that the socket may be closed cleanly, with no link-death ghost.
            (_, code) if code == codes::server::Quit => {
                let total_out = payload.first().copied().unwrap_or(0) != 0;
                let level = payload.get(1).copied().unwrap_or(0);
                self.phase = SessionPhase::Closed;
                vec![
                    Action::Event(ServerEvent::LoggedOut { total_out, level }),
                    Action::Phase(SessionPhase::Closed),
                ]
            }
            // AttackMode 0x74 — authoritative melee stance (PacketLib168).
            (_, code) if code == codes::server::AttackMode => {
                let attacking = payload.first().copied().unwrap_or(0) != 0;
                vec![Action::Event(ServerEvent::AttackMode { attacking })]
            }
            // MaxSpeed 0xB6 — percent of base run speed (PacketLib168 WriteShort BE).
            (_, code) if code == codes::server::MaxSpeed && payload.len() >= 2 => {
                let percent = u16::from_be_bytes([payload[0], payload[1]]);
                let turning_disabled = payload.get(2).copied().unwrap_or(0) != 0;
                let water_percent = payload.get(3).copied().unwrap_or(0);
                vec![Action::Event(ServerEvent::MaxSpeed {
                    percent,
                    turning_disabled,
                    water_percent,
                })]
            }
            // StatsUpdate 0xFB — attributes (flag 0) / resists (flag 0xFF).
            (_, code) if code == codes::server::StatsUpdate => {
                match crate::stats_update::decode(payload) {
                    Ok(s) => vec![Action::Event(ServerEvent::StatsUpdated(s))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (_, code) if code == codes::server::GameOpenReply => {
                vec![Action::Event(ServerEvent::GameOpenReply {
                    flag: payload.first().copied().unwrap_or(0),
                })]
            }
            (_, code) if code == codes::server::Realm => {
                vec![Action::Event(ServerEvent::Realm {
                    realm: payload.first().copied().unwrap_or(0),
                })]
            }
            (_, code) if code == codes::server::ConsignmentMerchantMoney => {
                match crate::money::decode_consignment(payload) {
                    Ok(m) => vec![Action::Event(ServerEvent::ConsignmentMerchantMoney(m))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (_, code) if code == codes::server::MarketExplorerWindow => {
                match crate::market::decode(payload) {
                    Ok(m) => vec![Action::Event(ServerEvent::MarketExplorer(m))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (_, code) if code == codes::server::TrainerWindow => {
                match crate::trainer::decode(payload) {
                    Ok(w) => vec![Action::Event(ServerEvent::TrainerWindow(w))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (_, code) if code == codes::server::FindGroupUpdate => {
                match crate::findgroup::decode(payload) {
                    Ok(f) => vec![Action::Event(ServerEvent::FindGroupUpdate(f))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (_, code) if code == codes::server::Encumberance => {
                match crate::encumberance::decode(payload) {
                    Ok(e) => vec![Action::Event(ServerEvent::Encumberance(e))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (_, code) if code == codes::server::ObjectGuildID => {
                match crate::social::decode_object_guild_id(payload) {
                    Ok(g) => vec![Action::Event(ServerEvent::ObjectGuildId(g))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (_, code) if code == codes::server::EmblemDialogue => {
                match crate::emblem::decode(payload) {
                    Ok(e) => vec![Action::Event(ServerEvent::EmblemDialogue(e))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (_, code) if code == codes::server::SiegeWeaponAnimation => {
                match crate::siege::decode_animation(payload) {
                    Ok(a) => vec![Action::Event(ServerEvent::SiegeWeaponAnimation(a))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (_, code) if code == codes::server::SiegeWeaponInterface => {
                match crate::siege::decode_interface(payload) {
                    Ok(i) => vec![Action::Event(ServerEvent::SiegeWeaponInterface(i))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            (_, code) if code == codes::server::EmoteAnimation => {
                match crate::emote::decode(payload) {
                    Ok(e) => vec![Action::Event(ServerEvent::EmoteAnimation(e))],
                    Err(_) => vec![Action::Event(ServerEvent::raw_malformed(code, payload))],
                }
            }
            // Anything not yet decoded is surfaced raw, never dropped.
            (_, code) => vec![Action::Event(ServerEvent::raw_unknown(code, payload))],
        }
    }

    /// Keepalive ping (0xA3). The oracle's PingRequestHandler skips 4 bytes, reads a BE u32
    /// timestamp, and echoes it back in PingReply (0x29). The real client sends one every few
    /// seconds; a silent connection eventually link-deaths. There is no quit packet in the DoL
    /// handler table — a clean TCP close is the logout (server saves + removes the player).
    pub fn ping(&mut self, timestamp_ms: u32) -> Vec<Action> {
        let mut body = crate::codec::PacketWriter::new();
        body.bytes(&[0x00; 4]).u32(timestamp_ms).bytes(&[0x00; 4]);
        let pkt = self.frame(codes::client::PingRequest, body.as_slice());
        vec![Action::Send(pkt)]
    }

    /// Movement: a PositionUpdate (0xA9), v1127 40-byte form (oracle `_HandlePacket1124` +
    /// golden trace): `[x][y][z][speed][zSpeed]` as f32 LE, then BE u16s `[session][objectId]
    /// [zoneId][state][fallingDmg][heading]`, `[action u8][2 pad][health u8][4 pad]`. The real
    /// client sends objectId=0, zoneId=0, state=0, action=0x80 at steady state — mirrored here.
    /// `speed` is game units/sec and should match the actual displacement rate (the oracle has
    /// speedhack heuristics).
    pub fn position_update(
        &mut self,
        x: f32,
        y: f32,
        z: f32,
        speed: f32,
        health_pct: u8,
    ) -> Vec<Action> {
        self.position_update_full(x, y, z, speed, health_pct, PlayerMotion::default())
    }

    /// Full-fidelity position update, matching `PlayerPositionUpdateHandler`'s 1.127 layout:
    ///
    /// ```text
    /// [x f32][y f32][z f32][speed f32][zspeed f32]
    /// [sessionID u16][objectID u16][currentZoneID u16][playerState u16][fallingDMG u16][heading u16]
    /// [playerAction u8][unknown u16][health u8][trailing u32]
    /// ```
    ///
    /// Three fields used to go out as zero and shouldn't:
    /// * **currentZoneID** — the server does `WorldMgr.GetZone(currentZoneID)` and logs
    ///   "position in unknown zone" when it can't resolve it. Always sending 0 claims Camelot
    ///   Hills no matter where we are. The reference client varies it (0, 1, 6, 19, 26, 51 across
    ///   the capture), which is simply the zone it is standing in.
    /// * **objectID** — carried from 1.127 onward; the server reads it right after the session id.
    /// * **playerAction / playerState** — the server derives `IsJumping` from `playerAction & 0x40`
    ///   and `IsStrafing` from `playerState & 0xE000`. Without them a jump is invisible to the
    ///   server and to every other player.
    pub fn position_update_full(
        &mut self,
        x: f32,
        y: f32,
        z: f32,
        speed: f32,
        health_pct: u8,
        motion: PlayerMotion,
    ) -> Vec<Action> {
        let heading = self.heading;
        // (heading is set via `set_heading` before a move; falls back to the last spawn/update value)
        let mut body = crate::codec::PacketWriter::new();
        body.f32_le(x).f32_le(y).f32_le(z).f32_le(speed).f32_le(0.0);
        body.u16(self.session_id)
            .u16(motion.object_id)
            .u16(motion.zone_id)
            .u16(motion.player_state())
            .u16(0) // fallingDMG
            .u16(heading);
        body.u8(motion.player_action())
            .bytes(&[0x00; 2])
            .u8(health_pct)
            .bytes(&[0x00; 4]);
        let pkt = self.frame(codes::client::PlayerPositionUpdate, body.as_slice());
        vec![Action::Send(pkt)]
    }

    /// Set the facing (DAoC heading units, 0..4096) sent by the next [`position_update`]. The client
    /// is authoritative for its own heading; callers driving movement set this from the travel
    /// direction each tick. Wraps into range so callers can pass raw computed values.
    ///
    /// [`position_update`]: Self::position_update
    pub fn set_heading(&mut self, heading: u16) {
        self.heading = heading % 4096;
    }

    /// Request a clean logout: the `/quit` slash command (oracle `QuitCommandHandler` →
    /// `Player.Quit(false)`). The server sits the character, runs its quit timer (instant if the
    /// server has `disable_quit_timer` and we're out of combat; otherwise a countdown, longer if
    /// recently in combat), then sends Quit (0xA4) once it has saved + removed us — that packet
    /// (decoded as [`ServerEvent::LoggedOut`]) is the signal to close the socket without a
    /// link-death ghost. Caller must be stationary for a timer server to accept the quit.
    pub fn quit(&mut self) -> Vec<Action> {
        self.command("quit")
    }

    /// Send a slash command (0xAF CommandHandler) — e.g. `command("say Hello")` for "/say
    /// Hello". v1127 wire form from oracle + capture: `[1 skipped byte][cmd, NUL-terminated]`,
    /// with `&` where the user typed `/`. Only meaningful in-world (the handler is gated on
    /// PlayerInGame).
    pub fn command(&mut self, cmd: &str) -> Vec<Action> {
        let mut body = crate::codec::PacketWriter::new();
        body.u8(0x00);
        body.bytes(format!("&{}\0", cmd.trim_start_matches('/')).as_bytes());
        let pkt = self.frame(codes::client::Command, body.as_slice());
        vec![Action::Send(pkt)]
    }

    /// Tell the server which object we have targeted (`0xB0 PlayerTarget`).
    ///
    /// Wire form (DOL `PlayerTargetHandler`): BE `u16` target object id, then a BE `u16` flag word
    /// the client uses for the "target changed by me" bit. `0` clears the target, which is what
    /// Escape sends.
    ///
    /// **The server does not acknowledge this** — targeting is client-authoritative display state
    /// that the server merely records for subsequent attack/spell/interact requests. So there is no
    /// reply to wait for and no event to decode; correctness shows up later, when an attack aimed
    /// at this target resolves against the right mob.
    pub fn target(&mut self, object_id: u16) -> Vec<Action> {
        let mut body = crate::codec::PacketWriter::new();
        // [✓cap] The flag short is NOT zero. `PlayerTargetHandler` documents it as:
        //   0x8000 examine · 0x4000 LOS1 · 0x2000 LOS2 · 0x0001 attack mode
        // and gates the target change on `(flags & (0x4000 | 0x2000)) != 0` — i.e. on the client
        // asserting line of sight. Sending 0 told the server we had NO line of sight, which is why
        // targeting appeared to do nothing. The reference client sends 0xE000 (examine + both LOS
        // bits), confirmed across all 80 captured samples: `335ae000`, `18f0e001`, `00000001`.
        const TARGET_FLAGS: u16 = 0x8000 | 0x4000 | 0x2000;
        body.u16(object_id).u16(TARGET_FLAGS);
        let pkt = self.frame(codes::client::PlayerTarget, body.as_slice());
        vec![Action::Send(pkt)]
    }

    /// Ask the server to (re)send the create packet for `object_id` (`CreateNPCRequest` 0xBE).
    ///
    /// [✓cap] This is how the reference client resolves an entity it only knows a POSITION for —
    /// it is the second most frequent packet it sends (1406 in a 15-minute session). Without it a
    /// client that misses a create never learns what the object is, which is exactly what left
    /// unidentified entities lingering in our world model.
    ///
    /// The id is **little-endian** (`ReadShortLowEndian` from client 1.126), which is unusual in
    /// this protocol — nearly every other short is big-endian. The oracle even flags the oddity in
    /// a comment. The server answers an id it cannot resolve with `ObjectDelete`, so a stale
    /// request cleans itself up rather than hanging.
    pub fn request_npc(&mut self, object_id: u16) -> Vec<Action> {
        let mut body = crate::codec::PacketWriter::new();
        body.u16_le(object_id).u16(0);
        let pkt = self.frame(codes::client::CreateNPCRequest, body.as_slice());
        vec![Action::Send(pkt)]
    }

    /// Start or stop melee attack mode (`PlayerAttackRequest` 0x74).
    ///
    /// [✓cap] `PlayerAttackRequestHandler` reads exactly two bytes: `[mode u8][userAction u8]` — a
    /// non-zero mode starts the attack, zero stops it, and `userAction == 0` means the player
    /// pressed the button (1 means the client decided by itself). The reference client sends 4
    /// bytes; the trailing two are padding it never reads, and they are visibly uninitialised in
    /// the capture (`00015006`, `00017003`, `00017c24` — identical first two bytes, junk after).
    ///
    /// This is the packet B.2 was blocked on. It is no longer a guess.
    pub fn attack(&mut self, start: bool) -> Vec<Action> {
        let mut body = crate::codec::PacketWriter::new();
        // userAction = 0: on our side this is always a deliberate press, never a client heuristic.
        body.u8(u8::from(start)).u8(0).u16(0);
        let pkt = self.frame(codes::client::PlayerAttackRequest, body.as_slice());
        vec![Action::Send(pkt)]
    }

    /// Use a skill, style or spell from the player's usable-skill list (`UseSkill` 0xBB).
    ///
    /// [✓cap] `UseSkillHandler` reads
    /// `[x f32 LE][y f32 LE][z f32 LE][speed f32 LE][heading u16][flagSpeedData u16][index u8][type u8]`.
    ///
    /// **`index` is the position in the list `SendUpdatePlayerSkills` sent — not an internal id.**
    /// That is what makes [`crate::skills`] load-bearing rather than cosmetic: the order those
    /// pages arrive in IS the addressing scheme, so a quickbar must remember where a skill sat in
    /// the list. `skill_type` is the `eSkillPage` byte carried with each entry. A captured sample
    /// ends `12 01` — index 18, type 1 (Abilities).
    pub fn use_skill(&mut self, index: u8, skill_type: u8, x: f32, y: f32, z: f32) -> Vec<Action> {
        let heading = self.heading;
        let mut body = crate::codec::PacketWriter::new();
        body.f32_le(x).f32_le(y).f32_le(z).f32_le(0.0);
        body.u16(heading).u16(0).u8(index).u8(skill_type).u16(0);
        let pkt = self.frame(codes::client::UseSkill, body.as_slice());
        vec![Action::Send(pkt)]
    }

    /// Select a character by realm-local overview slot and start world entry. Called by the higher
    /// layer once it has the character overview. v1126+ DOL does **not** put that local slot
    /// straight on the wire: its handlers convert account-wide indices `0..9`, `10..19`, and
    /// `20..29` into Albion, Midgard, and Hibernia account slots respectively.  Both
    /// `CharacterSelectRequest` and `RegionListRequest` therefore carry the same realm-offset
    /// index, while `selected_slot` remains local for the UI.
    pub fn select_character(&mut self, slot: u8) -> Vec<Action> {
        self.selected_slot = slot;
        let wire_slot = dol_character_wire_slot(self.realm, slot);
        let select = self.frame(codes::client::CharacterSelectRequest, &[wire_slot]);
        let region_list = self.frame(codes::client::RegionListRequest, &[wire_slot]);
        self.phase = SessionPhase::EnteringWorld;
        vec![
            Action::Phase(SessionPhase::EnteringWorld),
            Action::Send(select),
            Action::Send(region_list),
        ]
    }

    /// Submit a v1126+ CharacterCreateRequest (0xFF). Stays in [`SessionPhase::CharacterSelect`];
    /// the server answers with a refreshed overview / login-granted path.
    pub fn create_character(
        &mut self,
        draft: &crate::charcreate::CharacterCreateDraft,
    ) -> Vec<Action> {
        let body = crate::charcreate::encode_create_request_1126(
            draft,
            crate::charcreate::CreateOp::Create,
        );
        let pkt = self.frame(codes::client::CharacterCreateRequest, &body);
        vec![Action::Send(pkt)]
    }

    /// Delete the character in `slot` of `realm`.
    ///
    /// The **same** `CharacterCreateRequest` the create form sends, with `CreateOp::Delete` and an
    /// empty name — there is no dedicated delete packet. `OPEN_ORACLE`
    /// `CharacterCreateRequestHandler._HandlePacket1124`: it switches on `pakdata.Operation`, and
    /// `case 3` acts **only when `CharName` is empty**, then resolves the target as
    /// `CharacterSlot + Realm * 100`. A delete carrying a name is silently ignored by the server.
    ///
    /// Version-routed on purpose. The pre-1124 handler uses magic operation values
    /// (`Delete = 0x12345678`); the 1124+ path this client targets uses the small integers 1/2/3,
    /// and reading the wrong branch of that file is a documented trap.
    pub fn delete_character(&mut self, realm: u8, slot: u8) -> Vec<Action> {
        let draft = crate::charcreate::CharacterCreateDraft::for_delete(realm, slot);
        let body = crate::charcreate::encode_create_request_1126(
            &draft,
            crate::charcreate::CreateOp::Delete,
        );
        let pkt = self.frame(codes::client::CharacterCreateRequest, &body);
        vec![Action::Send(pkt)]
    }

    pub fn bad_name_check(&mut self, name: &str) -> Vec<Action> {
        let pkt = self.frame(
            codes::client::BadNameCheck,
            &crate::shape2_loop::encode_bad_name_check(name),
        );
        vec![Action::Send(pkt)]
    }

    pub fn dup_name_check(&mut self, name: &str) -> Vec<Action> {
        let pkt = self.frame(
            codes::client::DupNameCheck,
            &crate::shape2_loop::encode_dup_name_check_1126(name),
        );
        vec![Action::Send(pkt)]
    }

    pub fn udp_init_request(&mut self, local_ip: &str, local_port: u16) -> Vec<Action> {
        let body = crate::shape2_loop::encode_udp_init_request_1124(local_ip, local_port);
        let pkt = self.frame(codes::client::UDPInitRequest, &body);
        vec![Action::Send(pkt)]
    }

    pub fn udp_ping(&mut self) -> Vec<Action> {
        let pkt = self.frame(
            codes::client::UDPPing,
            &crate::shape2_loop::encode_udp_ping(),
        );
        vec![Action::Send(pkt)]
    }

    pub fn check_los_response(
        &mut self,
        checker_oid: u16,
        target_oid: u16,
        response: u16,
    ) -> Vec<Action> {
        let body = crate::shape2_loop::encode_check_los_response(checker_oid, target_oid, response);
        let pkt = self.frame(codes::client::CheckLosResponse, &body);
        vec![Action::Send(pkt)]
    }

    pub fn disband_from_group(&mut self) -> Vec<Action> {
        let pkt = self.frame(
            codes::client::DisbandFromGroup,
            &crate::shape2_loop::encode_disband_from_group(),
        );
        vec![Action::Send(pkt)]
    }

    pub fn dismount(&mut self) -> Vec<Action> {
        let pkt = self.frame(
            codes::client::Dismount,
            &crate::shape2_loop::encode_dismount(),
        );
        vec![Action::Send(pkt)]
    }

    pub fn object_update_request(&mut self) -> Vec<Action> {
        let pkt = self.frame(
            codes::client::ObjectUpdateRequest,
            &crate::shape2_loop::encode_object_update_request(),
        );
        vec![Action::Send(pkt)]
    }

    pub fn create_player_request(&mut self, client_id: u32) -> Vec<Action> {
        let body = crate::shape2_loop::encode_create_player_request_1126(client_id);
        let pkt = self.frame(codes::client::CreatePlayerRequest, &body);
        vec![Action::Send(pkt)]
    }

    pub fn remove_quest(&mut self, quest_index: u16) -> Vec<Action> {
        let body = crate::shape2_loop::encode_remove_quest_request(quest_index);
        let pkt = self.frame(codes::client::RemoveQuestRequest, &body);
        vec![Action::Send(pkt)]
    }

    pub fn quest_reward_chosen(
        &mut self,
        count_chosen: u8,
        items_chosen: &[u8; 8],
        quest_id: u16,
        quest_giver_id: u16,
    ) -> Vec<Action> {
        let body = crate::shape2_loop::encode_quest_reward_chosen(
            count_chosen,
            items_chosen,
            quest_id,
            quest_giver_id,
        );
        let pkt = self.frame(codes::client::QuestRewardChosen, &body);
        vec![Action::Send(pkt)]
    }

    pub fn remove_concentration_effect(&mut self, index: u8) -> Vec<Action> {
        let body = crate::shape2_loop::encode_remove_concentration_effect(index);
        let pkt = self.frame(codes::client::RemoveConcentrationEffect, &body);
        vec![Action::Send(pkt)]
    }

    pub fn cancel_effect(&mut self, effect_id: u16) -> Vec<Action> {
        let body = crate::shape2_loop::encode_cancels_effect(effect_id);
        let pkt = self.frame(codes::client::CancelsEffect, &body);
        vec![Action::Send(pkt)]
    }

    pub fn detail_request(
        &mut self,
        object_type: u16,
        extra_id: u32,
        object_id: u16,
    ) -> Vec<Action> {
        let body = crate::shape2_loop::encode_detail_request(object_type, extra_id, object_id);
        let pkt = self.frame(codes::client::DetailRequest, &body);
        vec![Action::Send(pkt)]
    }

    pub fn appraise_item(
        &mut self,
        player_x: u32,
        player_y: u32,
        id: u16,
        item_slot: u16,
    ) -> Vec<Action> {
        let body = crate::shape2_loop::encode_appraise_item(player_x, player_y, id, item_slot);
        let pkt = self.frame(codes::client::AppraiseItem, &body);
        vec![Action::Send(pkt)]
    }

    /// Re-request the character overview for `realm` (1 Albion / 2 Midgard / 3 Hibernia).
    ///
    /// Used when the player picks a realm on the pre-world screen: the local draft alone is not
    /// enough — the session's overview byte must change and `CharacterOverviewRequest` (0xFC) must
    /// fire again, or the char list stays on realm 1 forever. Stays in [`SessionPhase::RealmSelect`]
    /// until `CharacterOverview` arrives (then advances to CharacterSelect).
    pub fn request_character_overview(&mut self, realm: u8) -> Vec<Action> {
        let realm = realm.clamp(1, 3);
        self.realm = realm;
        self.overview_requested = true;
        let pkt = self.frame(codes::client::CharacterOverviewRequest, &[realm]);
        vec![Action::Send(pkt)]
    }

    /// Current overview-request realm byte (1..3).
    #[must_use]
    pub fn realm(&self) -> u8 {
        self.realm
    }

    /// Move an inventory item (0xDD) — equip is backpack → worn / paperdoll (100).
    ///
    /// Does **not** mutate local equipment optimistically. Self confirmation is InventoryUpdate
    /// 0x02 on worn slots (DOL broadcasts LivingEquipmentUpdate 0x15 to *other* players only).
    pub fn move_item(&mut self, to_slot: u16, from_slot: u16, count: u16) -> Vec<Action> {
        let body = crate::invverb::encode_move_item(to_slot, from_slot, count);
        let pkt = self.frame(codes::client::PlayerMoveItem, &body);
        vec![Action::Send(pkt)]
    }

    /// Interact with a world object (0x7A) — opens merchant windows among other uses.
    pub fn interact(&mut self, player_x: u32, player_y: u32, target_oid: u16) -> Vec<Action> {
        let body =
            crate::invverb::encode_object_interact(player_x, player_y, self.session_id, target_oid);
        let pkt = self.frame(codes::client::ObjectInteractRequest, &body);
        vec![Action::Send(pkt)]
    }

    /// Pick up the currently targeted ground object (0xB5 PickUpRequest).
    ///
    /// OPEN_ORACLE uses `TargetObject`, not the wire oid — call [`Self::target`] first.
    pub fn pickup(&mut self, player_x: u32, player_y: u32, object_id: u16) -> Vec<Action> {
        let body =
            crate::invverb::encode_pickup_request(player_x, player_y, self.session_id, object_id);
        let pkt = self.frame(codes::client::PickUpRequest, &body);
        vec![Action::Send(pkt)]
    }

    /// Buy from the targeted merchant (0x78). Requires a prior [`Self::target`] on the merchant.
    pub fn buy_item(
        &mut self,
        player_x: u32,
        player_y: u32,
        merchant_id: u16,
        item_slot: u16,
        item_count: u8,
    ) -> Vec<Action> {
        let body = crate::invverb::encode_buy_request(
            player_x,
            player_y,
            merchant_id,
            item_slot,
            item_count,
        );
        let pkt = self.frame(codes::client::BuyRequest, &body);
        vec![Action::Send(pkt)]
    }

    /// DialogResponse (0x82) — reply to a server Dialog (0x81).
    ///
    /// For CustomDialog (`message_type = 0x06`), echo the server's `data1`/`data2` and set
    /// `response = 0x01` for Yes (OPEN_ORACLE `DialogResponseHandler`).
    ///
    /// Group invite Yes (`message_type = 0x05`) is local intent only: DialogResponse on the
    /// wire. There is no synthetic accept event — Dialog `data1` is the inviter's SessionID
    /// (OPEN_ORACLE `SendGroupInviteCommand` / `GetClientFromID`) while GroupWindow members
    /// carry `GameLiving.ObjectID` (`SendGroupWindowUpdate`). Those namespaces are not
    /// interchangeable. Authoritative membership is GroupWindow / GroupMemberUpdate.
    pub fn dialog_response(
        &mut self,
        data1: u16,
        data2: u16,
        data3: u16,
        message_type: u8,
        response: u8,
    ) -> Vec<Action> {
        let body =
            crate::invverb::encode_dialog_response(data1, data2, data3, message_type, response);
        let pkt = self.frame(codes::client::DialogResponse, &body);
        vec![Action::Send(pkt)]
    }

    /// DoorRequest (0x99) — ask the server to open/close a door by InternalID.
    ///
    /// Does **not** mutate local door state. Confirm is DoorState 0x99 S2C (`open` byte).
    pub fn door_request(&mut self, door_id: u32, door_state: u8) -> Vec<Action> {
        let body = crate::worldverb::encode_door_request(door_id, door_state);
        let pkt = self.frame(codes::client::DoorRequest, &body);
        vec![Action::Send(pkt)]
    }

    /// PlayerGroundTarget (0xEC) — set the cast/siege ground point.
    ///
    /// Does **not** apply a local "landed" marker. Area-spell landing is confirmed by
    /// SpellEffectAnimation (0x1B) after the server accepts the cast (reject-equip rule).
    pub fn ground_target(&mut self, x: i32, y: i32, z: i32, flag: u16) -> Vec<Action> {
        let body = crate::worldverb::encode_ground_target(x, y, z, flag);
        let pkt = self.frame(codes::client::PlayerGroundTarget, &body);
        vec![Action::Send(pkt)]
    }

    /// InviteToGroup (0x87) — empty body; server uses current `TargetObject`.
    ///
    /// Does **not** invent a roster. Confirm is GroupMemberUpdate 0x70 / GroupWindow 0x16:0x06
    /// after the invitee sends DialogResponse Yes for Dialog code 0x05.
    pub fn invite_to_group(&mut self) -> Vec<Action> {
        let body = crate::social::encode_invite_to_group();
        let pkt = self.frame(codes::client::InviteToGroup, &body);
        vec![Action::Send(pkt)]
    }

    /// ModifyTrade (0xEB) — cancel / update offers / accept. Confirm via TradeWindow 0xEA and,
    /// on accept, InventoryUpdate 0x02 + MoneyUpdate 0xFA on **both** clients.
    pub fn modify_trade(
        &mut self,
        action: crate::social::ModifyTradeAction,
        repair: bool,
        combine: bool,
        slots: &[u8; 10],
        money: crate::social::TradeMoney,
    ) -> Vec<Action> {
        let body = crate::social::encode_modify_trade(action, repair, combine, slots, money);
        let pkt = self.frame(codes::client::ModifyTrade, &body);
        vec![Action::Send(pkt)]
    }

    /// SellRequest (0x79). Does **not** remove the item or add money locally.
    pub fn sell_item(
        &mut self,
        player_x: u32,
        player_y: u32,
        merchant_id: u16,
        item_slot: u16,
    ) -> Vec<Action> {
        let body = crate::invverb::encode_sell_request(player_x, player_y, merchant_id, item_slot);
        let pkt = self.frame(codes::client::SellRequest, &body);
        vec![Action::Send(pkt)]
    }

    /// UseSlot (0x71) 1.124+ layout. Does **not** consume/equip locally.
    pub fn use_slot(
        &mut self,
        x: f32,
        y: f32,
        z: f32,
        speed: f32,
        heading: u16,
        flag_speed_data: u16,
        slot: u8,
        use_type: u8,
    ) -> Vec<Action> {
        let body = crate::invverb::encode_use_slot_1124(
            x,
            y,
            z,
            speed,
            heading,
            flag_speed_data,
            slot,
            use_type,
        );
        let pkt = self.frame(codes::client::UseSlot, &body);
        vec![Action::Send(pkt)]
    }

    /// CraftRequest (0xED). Does **not** invent a crafted item locally.
    pub fn craft_item(&mut self, item_id: u16) -> Vec<Action> {
        let body = crate::invverb::encode_craft_request(item_id);
        let pkt = self.frame(codes::client::CraftRequest, &body);
        vec![Action::Send(pkt)]
    }

    /// DestroyItemRequest (0x80). Does **not** remove the item locally.
    pub fn destroy_item(&mut self, slot: u16) -> Vec<Action> {
        let body = crate::invverb::encode_destroy_item(slot);
        let pkt = self.frame(codes::client::DestroyItemRequest, &body);
        vec![Action::Send(pkt)]
    }

    /// TrainWindowHandler 0x7B. Empty body; does not invent spec levels.
    pub fn train_window(&mut self) -> Vec<Action> {
        let pkt = self.frame(
            codes::client::TrainWindowHandler,
            &crate::trainer::encode_train_window(),
        );
        vec![Action::Send(pkt)]
    }

    /// TrainRequest 0x7C. Does not award spec levels locally.
    pub fn train_request(
        &mut self,
        player_x: u32,
        player_y: u32,
        id_line: u8,
        unk: u8,
        row: u8,
        skill_index: u8,
    ) -> Vec<Action> {
        let body = crate::trainer::encode_train_request(
            player_x,
            player_y,
            id_line,
            unk,
            row,
            skill_index,
        );
        let pkt = self.frame(codes::client::TrainRequest, &body);
        vec![Action::Send(pkt)]
    }

    /// SiegeCommandRequest 0xF5. Does not invent siege state locally.
    pub fn siege_command(&mut self, action: u8, ammo: u8) -> Vec<Action> {
        let body = crate::siege::encode_command(action, ammo);
        let pkt = self.frame(codes::client::SiegeCommandRequest, &body);
        vec![Action::Send(pkt)]
    }

    /// PlayerSitRequest 0xC7. Does not invent a local sit pose.
    pub fn sit(&mut self, sit: bool) -> Vec<Action> {
        let pkt = self.frame(
            codes::client::PlayerSitRequest,
            &crate::worldverb::encode_sit_request(sit),
        );
        vec![Action::Send(pkt)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framing::ClientPacketHeader;

    fn srv(code: u8) -> ServerPacketHeader {
        ServerPacketHeader { size: 0, code }
    }

    fn enter_world(s: &mut SessionState) {
        s.begin();
        s.on_server_packet(srv(codes::server::CryptKey), &[]);
        s.on_server_packet(srv(codes::server::LoginGranted), &[]);
        s.on_server_packet(srv(codes::server::SessionID), &[0x07, 0x00]);
        // Unbound query path: Realm(0) then ChooseRealm(1) before overview.
        s.on_server_packet(srv(codes::server::Realm), &[0x00]);
        let _ = s.request_character_overview(1);
        let mut empty = vec![0u8; 16];
        empty[0] = 1;
        s.on_server_packet(srv(codes::server::CharacterOverview), &empty);
        s.select_character(0);
        s.on_server_packet(srv(codes::server::CharacterInitFinished), &[0x00]);
        assert_eq!(s.phase(), SessionPhase::InWorld);
    }

    fn group_window_body(members: &[(&str, u16)]) -> Vec<u8> {
        let mut w = crate::codec::PacketWriter::new();
        w.u8(0x06).u8(members.len() as u8);
        for (n, oid) in members {
            w.pascal_string(n).pascal_string("").u16(*oid).u8(50);
        }
        w.into_bytes()
    }

    /// Extract the (code, payload-framed) sends from an action list.
    fn sends(actions: &[Action]) -> Vec<&Vec<u8>> {
        actions
            .iter()
            .filter_map(|a| {
                if let Action::Send(b) = a {
                    Some(b)
                } else {
                    None
                }
            })
            .collect()
    }

    /// SCN-01 anti-fake: overview slots are realm-local, while DOL 1.126 character selection
    /// uses the account-wide wire index (Albion +0, Midgard +10, Hibernia +20).  Both select and
    /// region-list packets must use that same source-defined index — otherwise the server loads
    /// no player and the next RegionListRequest dereferences a missing character.
    #[test]
    fn scn01_selection_packets_encode_realm_local_ui_slot_for_dol() {
        for (realm, realm_offset) in [(1u8, 0u8), (2, 10), (3, 20)] {
            for slot in [0u8, 1, 3, 7, 9] {
                let mut s = SessionState::new("a", "b");
                let _ = s.begin();
                let _ = s.on_server_packet(srv(codes::server::CryptKey), &[]);
                let _ = s.on_server_packet(srv(codes::server::LoginGranted), &[]);
                let _ = s.on_server_packet(srv(codes::server::SessionID), &[0x01, 0x00]);
                let _ = s.on_server_packet(srv(codes::server::Realm), &[0x00]);
                let _ = s.request_character_overview(realm);
                let mut empty = vec![0u8; 16];
                empty[0] = 1;
                let _ = s.on_server_packet(srv(codes::server::CharacterOverview), &empty);
                assert_eq!(s.phase(), SessionPhase::CharacterSelect);
                let a = s.select_character(slot);
                let entry = sends(&a);
                let (hdr, body) = ClientPacketHeader::decode(entry[0]).unwrap();
                assert_eq!(hdr.id, codes::client::CharacterSelectRequest);
                let expected = slot + realm_offset;
                assert_eq!(
                body,
                &[expected],
                "SCN-01: realm {realm} UI slot {slot} must map to DOL wire slot {expected}, got {body:?}"
            );
                let (hdr, body) = ClientPacketHeader::decode(entry[1]).unwrap();
                assert_eq!(hdr.id, codes::client::RegionListRequest);
                assert_eq!(body, &[expected]);
            }
        }
    }

    /// Drive unbound login through RealmSelect → ChooseRealm(1) → CharacterSelect.
    fn reach_char_select(s: &mut SessionState) {
        let _ = s.begin();
        let _ = s.on_server_packet(srv(codes::server::CryptKey), &[]);
        let _ = s.on_server_packet(srv(codes::server::LoginGranted), &[]);
        let _ = s.on_server_packet(srv(codes::server::SessionID), &[0x01, 0x00]);
        let _ = s.on_server_packet(srv(codes::server::Realm), &[0x00]);
        let _ = s.request_character_overview(1);
        let mut empty = vec![0u8; 16];
        empty[0] = 1;
        let _ = s.on_server_packet(srv(codes::server::CharacterOverview), &empty);
        assert_eq!(s.phase(), SessionPhase::CharacterSelect);
    }

    /// Bound-account path: Realm(N) after query auto-requests that realm's overview.
    #[test]
    fn login_bound_realm_auto_requests_overview() {
        let mut s = SessionState::new("bound", "secret");
        let _ = s.begin();
        let _ = s.on_server_packet(srv(codes::server::CryptKey), &[]);
        let _ = s.on_server_packet(srv(codes::server::LoginGranted), &[]);
        assert_eq!(s.phase(), SessionPhase::RealmSelect);
        let _ = s.on_server_packet(srv(codes::server::SessionID), &[0x02, 0x00]);
        let a = s.on_server_packet(srv(codes::server::Realm), &[0x02]);
        assert!(a
            .iter()
            .any(|x| matches!(x, Action::Event(ServerEvent::Realm { realm: 2 }))));
        let ov_req = sends(&a);
        assert_eq!(ov_req.len(), 1);
        let (hdr, body) = ClientPacketHeader::decode(ov_req[0]).unwrap();
        assert_eq!(hdr.id, codes::client::CharacterOverviewRequest);
        assert_eq!(body, &[0x02]);
        assert_eq!(s.realm(), 2);
        assert_eq!(
            s.phase(),
            SessionPhase::RealmSelect,
            "stay RealmSelect until overview lands"
        );
    }

    /// A malformed overview must not visually advance RealmSelect. Advancing first produced a
    /// convincing but empty character-select plate even though the server reply was undecodable.
    #[test]
    fn malformed_overview_stays_on_realm_select_and_surfaces_raw() {
        let mut s = SessionState::new("unbound", "x");
        let _ = s.begin();
        let _ = s.on_server_packet(srv(codes::server::CryptKey), &[]);
        let _ = s.on_server_packet(srv(codes::server::LoginGranted), &[]);
        let _ = s.on_server_packet(srv(codes::server::SessionID), &[0x01, 0x00]);
        let _ = s.on_server_packet(srv(codes::server::Realm), &[0x00]);
        let _ = s.request_character_overview(1);

        let a = s.on_server_packet(srv(codes::server::CharacterOverview), &[0x01]);
        assert_eq!(s.phase(), SessionPhase::RealmSelect);
        assert!(a.iter().any(|action| matches!(
            action,
            Action::Event(ServerEvent::Raw { code, payload })
                if *code == codes::server::CharacterOverview && payload == &[0x01]
        )));
        assert!(!a
            .iter()
            .any(|action| matches!(action, Action::Phase(SessionPhase::CharacterSelect))));
    }

    /// Falsifier: LoginGranted must not emit CharacterOverviewRequest for realm 1.
    #[test]
    fn login_granted_does_not_auto_bind_albion() {
        let mut s = SessionState::new("unbound", "x");
        let _ = s.begin();
        let _ = s.on_server_packet(srv(codes::server::CryptKey), &[]);
        let a = s.on_server_packet(srv(codes::server::LoginGranted), &[]);
        for pkt in sends(&a) {
            let (hdr, body) = ClientPacketHeader::decode(pkt).unwrap();
            assert_ne!(
                hdr.id,
                codes::client::CharacterOverviewRequest,
                "LoginGranted must not request overview (would bind unbound accounts); body={body:?}"
            );
        }
        let a = s.on_server_packet(srv(codes::server::SessionID), &[0x01, 0x00]);
        let (hdr, body) = ClientPacketHeader::decode(sends(&a)[0]).unwrap();
        assert_eq!(hdr.id, codes::client::CharacterOverviewRequest);
        assert_eq!(body, &[0x00], "must query realm, not invent Albion");
    }

    /// LoginDenied must close the session and surface the error byte (PacketLib168).
    #[test]
    fn login_denied_closes_session_with_error() {
        let mut s = SessionState::new("a", "b");
        let _ = s.begin();
        let _ = s.on_server_packet(srv(codes::server::CryptKey), &[]);
        let a = s.on_server_packet(
            srv(codes::server::LoginDenied),
            &[0x06, 0x01, 0x7f, 0x00, 0x00],
        );
        assert!(a
            .iter()
            .any(|x| matches!(x, Action::Event(ServerEvent::LoginDenied { error: 0x06 }))));
        assert_eq!(s.phase(), SessionPhase::Closed);
    }

    /// AttackMode / MaxSpeed must not remain Raw when payloads match OPEN_ORACLE.
    #[test]
    fn attack_mode_and_max_speed_are_typed() {
        let mut s = SessionState::new("a", "b");
        let _ = s.begin();
        let _ = s.on_server_packet(srv(codes::server::CryptKey), &[]);
        let _ = s.on_server_packet(srv(codes::server::LoginGranted), &[]);
        let a = s.on_server_packet(srv(codes::server::AttackMode), &[0x01, 0, 0, 0]);
        assert!(matches!(
            &a[0],
            Action::Event(ServerEvent::AttackMode { attacking: true })
        ));
        let a = s.on_server_packet(srv(codes::server::MaxSpeed), &[0x00, 0x64, 0x00, 0x32]);
        assert!(matches!(
            &a[0],
            Action::Event(ServerEvent::MaxSpeed {
                percent: 100,
                turning_disabled: false,
                water_percent: 0x32
            })
        ));
        // Empty MaxSpeed stays Raw (malformed negative).
        let a = s.on_server_packet(srv(codes::server::MaxSpeed), &[0x00]);
        assert!(matches!(
            &a[0],
            Action::Event(ServerEvent::Raw { code: 0xB6, .. })
        ));
    }

    /// Walk the full login→realm→char-select spine with canned server replies (unbound path).
    #[test]
    fn login_to_charselect_walk() {
        let mut s = SessionState::new("tester", "secret");
        assert_eq!(s.phase(), SessionPhase::Disconnected);

        let a = s.begin();
        assert!(matches!(a[0], Action::Phase(SessionPhase::CryptHandshake)));
        assert!(matches!(a[1], Action::Send(_)));

        // server crypt key → we send login
        let a = s.on_server_packet(srv(codes::server::CryptKey), &[]);
        assert_eq!(s.phase(), SessionPhase::Authenticating);
        assert!(a
            .iter()
            .any(|x| matches!(x, Action::Event(ServerEvent::CryptKeyReceived))));

        // login granted → RealmSelect + CharacterSelectRequest(0) for SessionID only
        let a = s.on_server_packet(srv(codes::server::LoginGranted), &[]);
        assert_eq!(s.phase(), SessionPhase::RealmSelect);
        assert!(a
            .iter()
            .any(|x| matches!(x, Action::Event(ServerEvent::LoginGranted))));
        assert!(a
            .iter()
            .any(|x| matches!(x, Action::Phase(SessionPhase::RealmSelect))));
        let sel = sends(&a);
        assert_eq!(sel.len(), 1);
        let (hdr, body) = ClientPacketHeader::decode(sel[0]).unwrap();
        assert_eq!(hdr.id, codes::client::CharacterSelectRequest);
        assert_eq!(body, &[0x00]);

        // SessionID → query account realm with overview request byte 0 (not invent realm 1)
        let a = s.on_server_packet(srv(codes::server::SessionID), &[0x01, 0x00]);
        assert_eq!(s.session_id(), 1);
        assert!(a
            .iter()
            .any(|x| matches!(x, Action::Event(ServerEvent::SessionAssigned(1)))));
        let ov_req = sends(&a);
        assert_eq!(ov_req.len(), 1);
        let (hdr, body) = ClientPacketHeader::decode(ov_req[0]).unwrap();
        assert_eq!(hdr.id, codes::client::CharacterOverviewRequest);
        assert_eq!(
            hdr.session_id, 1,
            "overview request must carry the assigned session id"
        );
        assert_eq!(body, &[0x00], "realm query byte must be 0");

        // Unbound: SendRealm(0) — stay on RealmSelect, no overview yet
        let a = s.on_server_packet(srv(codes::server::Realm), &[0x00]);
        assert_eq!(s.phase(), SessionPhase::RealmSelect);
        assert!(a
            .iter()
            .any(|x| matches!(x, Action::Event(ServerEvent::Realm { realm: 0 }))));
        assert!(
            sends(&a).is_empty(),
            "Realm(0) must not auto-request overview"
        );

        // ChooseRealm(1) → CharacterOverviewRequest(1)
        let a = s.request_character_overview(1);
        let ov_req = sends(&a);
        assert_eq!(ov_req.len(), 1);
        let (hdr, body) = ClientPacketHeader::decode(ov_req[0]).unwrap();
        assert_eq!(hdr.id, codes::client::CharacterOverviewRequest);
        assert_eq!(body, &[0x01], "realm byte (Albion)");

        // char overview arrives → CharacterSelect
        let mut empty_overview = vec![0u8; 16];
        empty_overview[0] = 0x01;
        empty_overview.extend_from_slice(&[0u8; 10]);
        let a = s.on_server_packet(srv(codes::server::CharacterOverview), &empty_overview);
        assert_eq!(s.phase(), SessionPhase::CharacterSelect);
        assert!(a
            .iter()
            .any(|x| matches!(x, Action::Phase(SessionPhase::CharacterSelect))));
        let ov = a
            .iter()
            .find_map(|x| {
                if let Action::Event(ServerEvent::CharacterOverview(ov)) = x {
                    Some(ov)
                } else {
                    None
                }
            })
            .expect("overview event");
        assert!(ov.characters.is_empty());

        // pick a character slot → char select + region-list request, entering world
        let a = s.select_character(3);
        assert_eq!(s.phase(), SessionPhase::EnteringWorld);
        let entry = sends(&a);
        assert_eq!(entry.len(), 2);
        let (hdr, body) = ClientPacketHeader::decode(entry[0]).unwrap();
        assert_eq!(
            (hdr.id, body),
            (codes::client::CharacterSelectRequest, &[3u8][..])
        );
        let (hdr, body) = ClientPacketHeader::decode(entry[1]).unwrap();
        assert_eq!(
            (hdr.id, body),
            (codes::client::RegionListRequest, &[3u8][..])
        );

        // the server re-sends the session id after the char select — absorbed, no action
        let a = s.on_server_packet(srv(codes::server::SessionID), &[0x01, 0x00]);
        assert!(sends(&a).is_empty());

        // region handoff (payload shaped like the capture) → Play sequence:
        // re-select, WorldInit, PlayerInit, GameOpen
        let mut handoff = crate::codec::PacketWriter::new();
        handoff.u32_le(10); // "127.0.0.1" + NUL
        handoff.bytes(b"127.0.0.1\0").u32_le(10400).u32_le(10400);
        let a = s.on_server_packet(srv(codes::server::RegionServer), handoff.as_slice());
        let ev = a
            .iter()
            .find_map(|x| {
                if let Action::Event(ServerEvent::RegionHandoff { ip, port }) = x {
                    Some((ip, port))
                } else {
                    None
                }
            })
            .expect("handoff event");
        assert_eq!((ev.0.as_str(), *ev.1), ("127.0.0.1", 10400));
        let play = sends(&a);
        let ids: Vec<u8> = play
            .iter()
            .map(|b| ClientPacketHeader::decode(b).unwrap().0.id)
            .collect();
        assert_eq!(
            ids,
            vec![
                codes::client::CharacterSelectRequest,
                codes::client::WorldInitRequest,
                codes::client::PlayerInitRequest,
                codes::client::GameOpenRequest,
            ]
        );

        // CharacterInitFinished → in world
        let a = s.on_server_packet(srv(codes::server::CharacterInitFinished), &[0x00]);
        assert_eq!(s.phase(), SessionPhase::InWorld);
        assert!(a
            .iter()
            .any(|x| matches!(x, Action::Event(ServerEvent::EnteredWorld))));
    }

    /// §14A: after create the 1124+ oracle re-sends LoginGranted; re-request overview in-place.
    #[test]
    fn post_create_login_granted_rerequests_overview() {
        let mut s = SessionState::new("a", "b");
        reach_char_select(&mut s);
        assert_eq!(s.phase(), SessionPhase::CharacterSelect);
        assert_eq!(s.session_id(), 1);

        let a = s.on_server_packet(srv(codes::server::LoginGranted), &[]);
        let req = sends(&a);
        assert_eq!(req.len(), 1);
        let (hdr, body) = ClientPacketHeader::decode(req[0]).unwrap();
        assert_eq!(hdr.id, codes::client::CharacterOverviewRequest);
        assert_eq!(
            hdr.session_id, 1,
            "post-create overview must keep session id"
        );
        assert_eq!(body, &[0x01]);
        // Must not emit CharacterSelectRequest — that would LoadPlayer(slot 0).
        assert!(!req.iter().any(|b| {
            ClientPacketHeader::decode(b)
                .map(|(h, _)| h.id == codes::client::CharacterSelectRequest)
                .unwrap_or(false)
        }));
    }

    #[test]
    fn request_character_overview_updates_realm_byte() {
        let mut s = SessionState::new("a", "b");
        let _ = s.begin();
        let _ = s.on_server_packet(srv(codes::server::CryptKey), &[]);
        let _ = s.on_server_packet(srv(codes::server::LoginGranted), &[]);
        let _ = s.on_server_packet(srv(codes::server::SessionID), &[0x02, 0x00]);
        assert_eq!(s.realm(), 1);

        let a = s.request_character_overview(2);
        assert_eq!(s.realm(), 2);
        let req = sends(&a);
        assert_eq!(req.len(), 1);
        let (hdr, body) = ClientPacketHeader::decode(req[0]).unwrap();
        assert_eq!(hdr.id, codes::client::CharacterOverviewRequest);
        assert_eq!(body, &[0x02], "Midgard realm byte on the wire");

        let a = s.request_character_overview(3);
        assert_eq!(s.realm(), 3);
        let (hdr, body) = ClientPacketHeader::decode(sends(&a)[0]).unwrap();
        assert_eq!(hdr.id, codes::client::CharacterOverviewRequest);
        assert_eq!(body, &[0x03]);
    }

    #[test]
    fn target_packet_carries_the_object_id_big_endian() {
        let mut s = SessionState::new("acct", "pw");
        let a = s.target(0x1234);
        let pkt = sends(&a).pop().expect("one packet");
        let (hdr, body) = ClientPacketHeader::decode(pkt).unwrap();
        assert_eq!(hdr.id, codes::client::PlayerTarget);
        // BE object id then the LOS/examine flag word — matching the reference client byte for
        // byte (captured: `335ae000`). A zero flag word means "no line of sight" to the server.
        assert_eq!(
            body,
            &[0x12, 0x34, 0xE0, 0x00],
            "unexpected PlayerTarget body"
        );

        // Object id 0 is the "clear target" form (what Escape sends), not a malformed packet.
        let a = s.target(0);
        let (_, body) = ClientPacketHeader::decode(sends(&a).pop().unwrap()).unwrap();
        assert_eq!(body, &[0x00, 0x00, 0xE0, 0x00]);
    }

    /// The 0xA9 position update must match the capture's 40-byte v1127 layout exactly (LE
    /// floats, BE shorts, action 0x80, health, zero tail) — the shape the live server verified
    /// by applying and persisting a walk.
    #[test]
    fn position_update_matches_captured_layout() {
        let mut s = SessionState::new("a", "b");
        s.begin();
        s.on_server_packet(srv(codes::server::CryptKey), &[]);
        s.on_server_packet(srv(codes::server::LoginGranted), &[]);
        s.on_server_packet(srv(codes::server::SessionID), &[0x01, 0x00]);
        let a = s.position_update(560871.0, 511685.0, 2344.0, 238.0, 100);
        let Action::Send(bytes) = &a[0] else {
            panic!("expected a send")
        };
        let (hdr, body) = ClientPacketHeader::decode(bytes).unwrap();
        assert_eq!(hdr.id, codes::client::PlayerPositionUpdate);
        assert_eq!(body.len(), 40, "v1127 position update is 40 bytes");
        assert_eq!(&body[..4], &560871.0f32.to_le_bytes(), "x is f32 LE");
        assert_eq!(&body[12..16], &238.0f32.to_le_bytes(), "speed is f32 LE");
        assert_eq!(&body[20..22], &[0x00, 0x01], "session id is BE");
        assert_eq!(body[32], 0x80, "steady-state action flag");
        assert_eq!(body[35], 100, "health percent");
        assert_eq!(&body[36..], &[0u8; 4], "zero tail");
    }

    /// A status update (0xAD) must decode into a `StatusUpdate` event during world ENTRY, not just
    /// once fully in-world: the server sends the first one before `CharacterInitFinished`, and
    /// gating it on `InWorld` would drop the packet that fills the HUD on spawn.
    #[test]
    fn status_update_decodes_while_entering_the_world() {
        let mut s = SessionState::new("a", "b");
        s.begin();
        s.on_server_packet(srv(codes::server::CryptKey), &[]);
        s.on_server_packet(srv(codes::server::LoginGranted), &[]);
        s.on_server_packet(srv(codes::server::SessionID), &[0x07, 0x00]);
        let mut empty = vec![0u8; 16];
        empty[0] = 1;
        s.on_server_packet(srv(codes::server::CharacterOverview), &empty);
        s.select_character(0);
        assert_eq!(s.phase(), SessionPhase::EnteringWorld);

        // Real capture bytes (cap_20260714_230416_conn38614): 1848/2004 HP = 92%.
        let payload: Vec<u8> = (0..22)
            .map(|i| {
                let hex = "5c640064640001ec0073017a07d40738007301ec017a";
                u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap()
            })
            .collect();
        let a = s.on_server_packet(srv(codes::server::CharacterStatusUpdate), &payload);
        match &a[0] {
            Action::Event(ServerEvent::StatusUpdate(st)) => {
                assert_eq!(st.health, 1848);
                assert_eq!(st.max_health, 2004);
                assert_eq!(st.health_pct, 92);
            }
            other => panic!("expected StatusUpdate, got {other:?}"),
        }
    }

    /// A truncated 0xAD must surface raw rather than being dropped or panicking — the wire is
    /// untrusted, and silently swallowing it would hide a protocol drift behind a frozen HUD.
    #[test]
    fn a_malformed_status_update_surfaces_raw() {
        let mut s = SessionState::new("a", "b");
        s.begin();
        s.on_server_packet(srv(codes::server::CryptKey), &[]);
        s.on_server_packet(srv(codes::server::LoginGranted), &[]);
        s.on_server_packet(srv(codes::server::SessionID), &[0x07, 0x00]);
        let mut empty = vec![0u8; 16];
        empty[0] = 1;
        s.on_server_packet(srv(codes::server::CharacterOverview), &empty);
        s.select_character(0);
        let a = s.on_server_packet(srv(codes::server::CharacterStatusUpdate), &[0x64, 0x64]);
        assert!(matches!(&a[0], Action::Event(ServerEvent::Raw { .. })));
    }

    /// The 1.127 position layout puts the zone id, our object id and the jump/strafe flags in
    /// slots that used to go out as zero. Asserted at the byte level against the field offsets the
    /// oracle's handler reads, because a silently-zero zone makes the server log "position in
    /// unknown zone" and mis-place the character.
    #[test]
    fn position_update_carries_zone_object_and_jump() {
        let mut s = SessionState::new("a", "b");
        s.begin();
        s.set_heading(1024);
        let motion = PlayerMotion {
            object_id: 0x4169,
            zone_id: 51,
            jumping: true,
            strafing: true,
        };
        let a = s.position_update_full(1.0, 2.0, 3.0, 150.0, 100, motion);
        let pkt = sends(&a).pop().expect("one packet");
        let (_, body) = ClientPacketHeader::decode(pkt).unwrap();

        assert_eq!(
            &body[22..24],
            &[0x41, 0x69],
            "objectID (1.127+) must be sent"
        );
        assert_eq!(
            &body[24..26],
            &[0x00, 51],
            "currentZoneID must be the real zone"
        );
        assert_ne!(
            u16::from_be_bytes([body[26], body[27]]) & 0xE000,
            0,
            "strafing bit"
        );
        assert_eq!(&body[30..32], &[0x04, 0x00], "heading");
        assert_ne!(body[32] & 0x40, 0, "jump bit in playerAction");
        assert_eq!(body[35], 100, "health");
        assert_eq!(
            body.len(),
            40,
            "the reference client sends exactly 40 bytes"
        );
    }

    /// Standing still: no jump bit, no strafe bits — otherwise the server would think we are
    /// permanently airborne.
    #[test]
    fn a_still_player_sets_no_motion_flags() {
        let mut s = SessionState::new("a", "b");
        s.begin();
        let a = s.position_update_full(0.0, 0.0, 0.0, 0.0, 100, PlayerMotion::default());
        let pkt = sends(&a).pop().unwrap();
        let (_, body) = ClientPacketHeader::decode(pkt).unwrap();
        assert_eq!(body[32] & 0x40, 0, "not jumping");
        assert_eq!(
            u16::from_be_bytes([body[26], body[27]]) & 0xE000,
            0,
            "not strafing"
        );
    }

    /// Group-invite Dialog (0x81 code 0x05) surfaces like every other dialog — never auto-clicked.
    /// Accept is a distinct typed [`SessionState::dialog_response`] path that sends one 0x82.
    #[test]
    fn group_invite_surfaces_until_typed_accept() {
        let mut s = SessionState::new("a", "b");
        s.begin();
        s.on_server_packet(srv(codes::server::CryptKey), &[]);
        s.on_server_packet(srv(codes::server::LoginGranted), &[]);
        s.on_server_packet(srv(codes::server::SessionID), &[0x07, 0x00]);
        // fast-forward to InWorld
        let mut empty = vec![0u8; 16];
        empty[0] = 1;
        s.on_server_packet(srv(codes::server::CharacterOverview), &empty);
        s.select_character(0);
        s.on_server_packet(srv(codes::server::CharacterInitFinished), &[0x00]);
        assert_eq!(s.phase(), SessionPhase::InWorld);

        // invite from session id 0x0003 (oracle SendGroupInviteCommand shape)
        let mut invite = vec![0x00, 0x05, 0x00, 0x03];
        invite.extend_from_slice(&[0u8; 6]); // data2..data4
        invite.extend_from_slice(&[0x01, 0x00]); // type, autowrap
        invite.extend_from_slice(b"Feile has invited you to join a group\0");
        let a = s.on_server_packet(srv(codes::server::Dialog), &invite);
        assert!(
            sends(&a).is_empty(),
            "inbound invite must not auto-send DialogResponse"
        );
        assert!(
            matches!(
                &a[..],
                [Action::Event(ServerEvent::Dialog {
                    code: 0x05,
                    data1: 3,
                    ..
                })]
            ),
            "invite must surface as Dialog, got {a:?}"
        );

        let accept = s.dialog_response(3, 0, 0, 0x05, 0x01);
        let reply = sends(&accept);
        assert_eq!(
            reply.len(),
            1,
            "accept must send exactly one DialogResponse"
        );
        let (hdr, body) = ClientPacketHeader::decode(reply[0]).unwrap();
        assert_eq!(hdr.id, codes::client::DialogResponse);
        assert_eq!(
            body,
            &[0x00, 0x03, 0, 0, 0, 0, 0x05, 0x01],
            "echo data1, type 0x05, accept"
        );
        assert!(
            accept.iter().all(|x| matches!(x, Action::Send(_))),
            "invite Yes is DialogResponse only (local intent); data1 is SessionID, not ObjectID"
        );

        let refuse = s.dialog_response(3, 0, 0, 0x05, 0x00);
        let refuse_sends = sends(&refuse);
        assert_eq!(refuse_sends.len(), 1);
        let (hdr, body) = ClientPacketHeader::decode(refuse_sends[0]).unwrap();
        assert_eq!(hdr.id, codes::client::DialogResponse);
        assert_eq!(body, &[0x00, 0x03, 0, 0, 0, 0, 0x05, 0x00]);
        assert!(
            refuse.iter().all(|x| matches!(x, Action::Send(_))),
            "invite refuse is DialogResponse only"
        );

        // a quest dialog (code 0x07) must NOT be auto-answered — structured Dialog event only
        let quest = [0x00, 0x07, 0x00, 0x09, 0, 0, 0, 0, 0, 0, 0x01, 0x00, 0x00];
        let a = s.on_server_packet(srv(codes::server::Dialog), &quest);
        assert!(
            sends(&a).is_empty(),
            "non-invite dialogs are surfaced, never clicked"
        );
        assert!(
            matches!(
                &a[..],
                [Action::Event(ServerEvent::Dialog {
                    code: 0x07,
                    data1: 9,
                    ..
                })]
            ),
            "quest dialog must surface as ServerEvent::Dialog, got {a:?}"
        );
    }

    /// SFRW1-01: GroupWindow is roster only. Dialog data1 (SessionID) is never equated with
    /// member object_id; no synthetic accept event exists to fire on numeric collision.
    #[test]
    fn group_window_is_roster_not_synthetic_accept() {
        let mut s = SessionState::new("a", "b");
        enter_world(&mut s);
        let yes = s.dialog_response(3, 0, 0, 0x05, 0x01);
        assert_eq!(sends(&yes).len(), 1);
        assert!(yes.iter().all(|x| matches!(x, Action::Send(_))));

        let roster = s.on_server_packet(
            srv(codes::server::VariousUpdate),
            &group_window_body(&[("A", 3), ("B", 4)]),
        );
        assert_eq!(
            roster.len(),
            1,
            "roster must not append a second event: {roster:?}"
        );
        assert!(
            matches!(
                &roster[0],
                Action::Event(ServerEvent::GroupWindow(gw)) if gw.members.len() == 2
            ),
            "authoritative membership is GroupWindow, got {roster:?}"
        );
    }

    /// A second SessionID (the server re-sends one per 0x10, as in the capture) must not
    /// trigger a duplicate overview request.
    #[test]
    fn duplicate_session_id_does_not_rerequest_overview() {
        let mut s = SessionState::new("a", "b");
        s.begin();
        s.on_server_packet(srv(codes::server::CryptKey), &[]);
        s.on_server_packet(srv(codes::server::LoginGranted), &[]);
        let first = s.on_server_packet(srv(codes::server::SessionID), &[0x01, 0x00]);
        assert_eq!(sends(&first).len(), 1);
        let second = s.on_server_packet(srv(codes::server::SessionID), &[0x01, 0x00]);
        assert!(
            sends(&second).is_empty(),
            "second SessionID must not re-request the overview"
        );
    }

    #[test]
    fn undecoded_packet_surfaces_raw_not_dropped() {
        let mut s = SessionState::new("a", "b");
        s.begin();
        let a = s.on_server_packet(srv(0xEE), &[1, 2, 3]);
        assert!(matches!(
            &a[0],
            Action::Event(ServerEvent::Raw { code: 0xEE, .. })
        ));
    }

    #[test]
    fn sequence_increments_per_packet() {
        let mut s = SessionState::new("a", "b");
        let a = s.begin();
        let first = if let Action::Send(b) = &a[1] {
            ClientPacketHeader::decode(b).unwrap().0.sequence
        } else {
            panic!()
        };
        let a = s.on_server_packet(srv(codes::server::CryptKey), &[]);
        let second = a
            .iter()
            .find_map(|x| {
                if let Action::Send(b) = x {
                    Some(ClientPacketHeader::decode(b).unwrap().0.sequence)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(second, first.wrapping_add(1));
    }

    /// Former CODED_RAW codecs must surface typed ServerEvent, not Raw, when payloads decode.
    #[test]
    fn coded_raw_s2c_surface_typed_not_raw() {
        let mut s = SessionState::new("a", "b");
        let _ = s.begin();
        let _ = s.on_server_packet(srv(codes::server::CryptKey), &[]);
        let _ = s.on_server_packet(srv(codes::server::LoginGranted), &[]);

        let a = s.on_server_packet(srv(codes::server::GameOpenReply), &[0x00]);
        assert!(matches!(
            &a[0],
            Action::Event(ServerEvent::GameOpenReply { flag: 0 })
        ));

        let a = s.on_server_packet(srv(codes::server::Realm), &[0x01]);
        assert!(matches!(
            &a[0],
            Action::Event(ServerEvent::Realm { realm: 1 })
        ));

        let money = crate::money::encode_consignment(&crate::money::ConsignmentMerchantMoney {
            copper: 1,
            silver: 2,
            gold: 3,
            mithril: 4,
            platinum: 5,
        });
        let a = s.on_server_packet(srv(codes::server::ConsignmentMerchantMoney), &money);
        assert!(matches!(
            &a[0],
            Action::Event(ServerEvent::ConsignmentMerchantMoney(_))
        ));

        let a = s.on_server_packet(
            srv(codes::server::MarketExplorerWindow),
            &crate::market::encode_empty_close(),
        );
        assert!(matches!(
            &a[0],
            Action::Event(ServerEvent::MarketExplorer(m)) if m.is_empty_close()
        ));

        let a = s.on_server_packet(
            srv(codes::server::TrainerWindow),
            &crate::trainer::encode_spec_window(
                3,
                &[crate::trainer::TrainerLine {
                    index: 0,
                    level: 1,
                    cost_or_next: 2,
                    name: "Slash".into(),
                }],
            ),
        );
        assert!(matches!(
            &a[0],
            Action::Event(ServerEvent::TrainerWindow(w)) if w.is_spec_list()
        ));

        let a = s.on_server_packet(srv(codes::server::FindGroupUpdate), &[0, 0]);
        assert!(matches!(
            &a[0],
            Action::Event(ServerEvent::FindGroupUpdate(f)) if f.empty_list
        ));

        let enc =
            crate::encumberance::encode(&crate::encumberance::Encumberance { max: 100, used: 40 });
        let a = s.on_server_packet(srv(codes::server::Encumberance), &enc);
        assert!(matches!(
            &a[0],
            Action::Event(ServerEvent::Encumberance(e)) if e.used == 40
        ));

        let guild = crate::social::encode_object_guild_id(0x0102, 0x00AB);
        let a = s.on_server_packet(srv(codes::server::ObjectGuildID), &guild);
        assert!(matches!(
            &a[0],
            Action::Event(ServerEvent::ObjectGuildId(_))
        ));

        let a = s.on_server_packet(srv(codes::server::EmblemDialogue), &crate::emblem::encode());
        assert!(matches!(
            &a[0],
            Action::Event(ServerEvent::EmblemDialogue(_))
        ));

        let anim = crate::siege::SiegeWeaponAnimation {
            object_id: 9,
            aim_x: 1,
            aim_y: 2,
            aim_z: 3,
            target_oid: 0,
            effect: 0,
            timer: 0,
            action: 1,
        };
        let a = s.on_server_packet(
            srv(codes::server::SiegeWeaponAnimation),
            &crate::siege::encode_animation_prefix(&anim),
        );
        assert!(matches!(
            &a[0],
            Action::Event(ServerEvent::SiegeWeaponAnimation(a)) if a.object_id == 9
        ));

        let a = s.on_server_packet(
            srv(codes::server::SiegeWeaponInterface),
            &crate::siege::encode_interface_close(),
        );
        assert!(matches!(
            &a[0],
            Action::Event(ServerEvent::SiegeWeaponInterface(i)) if i.is_close()
        ));

        let emote = crate::emote::encode(&crate::emote::EmoteAnimation {
            object_id: 12,
            emote: 7,
        });
        let a = s.on_server_packet(srv(codes::server::EmoteAnimation), &emote);
        assert!(matches!(
            &a[0],
            Action::Event(ServerEvent::EmoteAnimation(e)) if e.emote == 7
        ));
    }
}
