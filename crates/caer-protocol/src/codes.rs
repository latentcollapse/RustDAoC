//! Packet codes.
//!
//! DAoC obfuscates client packet codes with a constant XOR. The oracle's `eClientPackets`
//! enum documents it inline, e.g. `CryptKeyRequest = 0xF4, // 0x5C ^ 168`, where `168 = 0xA8`.
//!
//! **P0 FINDING (golden trace `rustdaoc_login_20260714`, 2026-07-14):** the XOR is NOT applied
//! on the client→server framing path — the wire header id byte is the *logical* code directly
//! (a CryptKeyRequest arrives as `0xF4` on the wire, not `0x5C`). So [`wire_code`] is a
//! derivation for the client's *internal* dispatch obfuscation, not a transform the framing
//! layer applies. The codes below are the on-wire values, verified against the capture.
//!
//! Curated subset sufficient for the login→in-world path plus the highest-frequency gameplay
//! packets; the full table is a mechanical port of the oracle enums.

/// The client packet-code obfuscation constant (`168` in the oracle comments).
pub const PACKET_CODE_XOR: u8 = 0xA8;

/// Map between a logical code (as named below / in the oracle enum) and the raw wire byte.
/// Involutive: applying it twice is the identity.
#[must_use]
pub const fn wire_code(logical: u8) -> u8 {
    logical ^ PACKET_CODE_XOR
}

/// Client → server packet codes. `[✓cap]` = verified on-wire in the golden trace.
/// These are the packets **we send**.
#[allow(non_upper_case_globals)]
pub mod client {
    pub const CryptKeyRequest: u8 = 0xF4; // [✓cap] seq0 (7B build info) + seq1 (256B RSA block)
    pub const LoginRequest: u8 = 0xA7; // [✓cap] account+password, see login::encode_login_request
    pub const PingRequest: u8 = 0xA3; // [✓cap] 12B keepalive w/ timestamp; server echoes it in 0x29

    pub const CharacterOverviewRequest: u8 = 0xFC; // [✓cap]
    pub const CharacterSelectRequest: u8 = 0x10; // [✓cap]
    pub const CharacterCreateRequest: u8 = 0xFF;
    pub const CharacterDeleteRequest: u8 = 0xC0;
    pub const RegionListRequest: u8 = 0x9D; // [✓cap] 1-byte slot (server reads ONLY byte 0 of the 29B the real client sends)
    pub const WorldInitRequest: u8 = 0xD4; // [✓cap] payload ignored by the oracle handler
    pub const GameOpenRequest: u8 = 0xBF; // [✓cap] 1-byte UDP-working flag (0 = TCP-only)
    pub const PlayerInitRequest: u8 = 0xE8; // [✓cap]
    pub const PlayerRegionChangeRequest: u8 = 0x90;
    pub const PlayerPositionUpdate: u8 = 0xA9; // high-frequency movement (verify vs enum)
    pub const PlayerHeadingUpdate: u8 = 0xBA;
    // [✓cap] 80 samples. The flag short is 0xE000 (examine + both LOS bits), NOT zero — see
    // `SessionState::target`.
    pub const PlayerTarget: u8 = 0xB0;
    pub const PlayerGroundTarget: u8 = 0xEC; // 0x44 ^ 168 — OPEN_ORACLE PlayerGroundTargetHandler
                                             // OPEN_ORACLE eClientPackets.DoorRequest — door open/close (System 5 Stream A).
    pub const DoorRequest: u8 = 0x99; // 0x31 ^ 168
    pub const Command: u8 = 0xAF; // [✓cap] slash commands: [skip:1][&cmd NUL-terminated] (oracle CommandHandler)
    pub const UseSpell: u8 = 0x7D;
    // [✓cap] 115 samples. Body verified against `UseSkillHandler`: position + heading + an INDEX
    // into the usable-skills list and its eSkillPage type — not an internal id.
    pub const UseSkill: u8 = 0xBB;
    pub const UseSlot: u8 = 0x71;
    // OPEN_ORACLE eClientPackets.PetWindow — aggro/walk/command (PetWindowHandler, no version gate).
    pub const PetWindow: u8 = 0x8A; // 0x22 ^ 168
                                    // OPEN_ORACLE eClientPackets.PlayerMoveItem — equip/bag move (System 4 verb 1).
    pub const PlayerMoveItem: u8 = 0xDD; // 0x75 ^ 168
                                         // OPEN_ORACLE eClientPackets.BuyRequest — merchant purchase (System 4 verb 2).
    pub const BuyRequest: u8 = 0x78; // 0xD0 ^ 168
                                     // OPEN_ORACLE eClientPackets.SellRequest — merchant sell (PlayerSellRequestHandler).
    pub const SellRequest: u8 = 0x79; // 0xD1 ^ 168
                                      // OPEN_ORACLE eClientPackets.CraftRequest — MakeProductHandler 0xED.
    pub const CraftRequest: u8 = 0xED; // 0x45 ^ 168
                                       // [✓cap] 19 samples. `[mode u8][userAction u8]` + 2 padding bytes the server never reads.
                                       // This was B.2's blocker; the capture settled it.
    pub const PlayerAttackRequest: u8 = 0x74;
    // [✓cap] 1406 samples — the SECOND most frequent thing the reference client sends. Asks the
    // server to (re)send the create packet for an object id we have seen an update for but have no
    // create for. Body is `[id u16 LITTLE-endian]` from 1.126 (`ReadShortLowEndian`), unlike almost
    // every other short in this protocol. Without it, entities we learn about mid-session never
    // resolve; the server answers a bad id with ObjectDelete, so it is self-cleaning.
    pub const CreateNPCRequest: u8 = 0xBE;
    pub const ObjectInteractRequest: u8 = 0x7A;
    // OPEN_ORACLE eClientPackets.PickUpRequest — ground loot; handler uses TargetObject.
    pub const PickUpRequest: u8 = 0xB5; // 0x1D ^ 168
    pub const DialogResponse: u8 = 0x82;
    pub const PlayerSitRequest: u8 = 0xC7;
    pub const ClientCrash: u8 = 0x37;
    // OPEN_ORACLE eClientPackets.InviteToGroup — empty body; uses TargetObject.
    pub const InviteToGroup: u8 = 0x87; // 0x2F ^ 168
                                        // OPEN_ORACLE eClientPackets.ModifyTrade — cancel/update/accept trade offers.
    pub const ModifyTrade: u8 = 0xEB; // 0x43 ^ 168
                                      // OPEN_ORACLE eClientPackets.DestroyItemRequest — Skip(4) then slot u16 BE.
    pub const DestroyItemRequest: u8 = 0x80; // 0x28 ^ 168
                                             // OPEN_ORACLE eClientPackets.TrainWindowHandler / TrainRequest.
    pub const TrainWindowHandler: u8 = 0x7B; // 0xD3 ^ 168
    pub const TrainRequest: u8 = 0x7C; // 0xD4 ^ 168
                                       // OPEN_ORACLE eClientPackets siege command (SiegeWeaponActionHandler 0xF5).
    pub const SiegeCommandRequest: u8 = 0xF5;

    // --- Shape-2 first-loop C2S (OPEN_ORACLE eClientPackets / Client/168) ---
    pub const UDPInitRequest: u8 = 0x14; // 0xBC ^ 168
    pub const QuestRewardChosen: u8 = 0x40; // 0xE8 ^ 168
    pub const RemoveQuestRequest: u8 = 0x4F; // 0xE7 ^ 168
    pub const RemoveConcentrationEffect: u8 = 0x76; // 0xDE ^ 168
    pub const DisbandFromGroup: u8 = 0x9F; // 0x37 ^ 168
    pub const ObjectUpdateRequest: u8 = 0xA5; // 0x0D ^ 168
    pub const BadNameCheck: u8 = 0xC2; // 0x6A ^ 168 (handler attribute)
    pub const Dismount: u8 = 0xC8; // PlayerDismountRequest 0x60 ^ 168
    pub const DupNameCheck: u8 = 0xCB; // DuplicateNameCheck 0x63 ^ 168
    pub const CheckLosResponse: u8 = 0xD0; // 0x78 ^ 168
    pub const CreatePlayerRequest: u8 = 0xD5; // 0x7D ^ 168
    pub const DetailRequest: u8 = 0xD8; // 0x70 ^ 168
    pub const AppraiseItem: u8 = 0xE0; // PlayerAppraiseItemRequest 0x48 ^ 168
    pub const UDPPing: u8 = 0xF2; // 0x5A ^ 168
    pub const CancelsEffect: u8 = 0xF8; // PlayerCancelsEffect 0x50 ^ 168
}

/// Server → client packet codes. `[✓cap]` = verified on-wire in the golden trace (with the
/// corrected values — several earlier guesses from the oracle enum were wrong and are now
/// pinned to real bytes). These are the packets **we receive/decode**.
///
/// NOTE: server→client string fields use a **1-byte** pascal length prefix (e.g. LoginGranted
/// carries `08 "rustdaoc" 08 "SOLODAOC"`), whereas the client→server LoginRequest uses **4-byte
/// little-endian** length prefixes. The asymmetry is real; don't assume one convention.
#[allow(non_upper_case_globals)]
pub mod server {
    pub const CryptKey: u8 = 0x22; // [✓cap] crypt reply, carries version "1.127"
    pub const LoginGranted: u8 = 0x2A; // [✓cap] 08"account" 08"servername" + version
    /// OPEN_ORACLE `eServerPackets.LoginDenied = 0x2C` — PacketLib168: error + version bytes.
    pub const LoginDenied: u8 = 0x2C;
    // 0x28/0x29 were mislabeled on 2026-07-13 and re-pinned 2026-07-14 from oracle + capture:
    // SessionID (oracle `eServerPackets.SessionID = 0x28`) carries the session id as a 2-byte
    // LE payload, sent in reply to a c2s CharacterSelectRequest (0x10). PingReply (0x29, 16B)
    // echoes the 4-byte timestamp from the c2s PingRequest (0xA3) — it is NOT a session packet.
    pub const SessionID: u8 = 0x28; // [✓cap+oracle] 2B LE session id, reply to c2s 0x10
    pub const PingReply: u8 = 0x29; // [✓cap+oracle] echoes ping timestamp + counter
    pub const CharacterOverview: u8 = 0xFC; // [✓cap] (corrected: was 0xFD) — the char list
    pub const Realm: u8 = 0xFE; // [✓cap] account realm reply (13B; realm byte 0 = show realm select)
    pub const Dialog: u8 = 0x81; // [00][dialog code][data1..4 u16][type][wrap][msg] — code 0x05 = group invite
    pub const RegionServer: u8 = 0xB1; // [✓cap] world handoff (oracle `ClientRegion`): pstrIntLE ip + 2× u32 LE port
    pub const PositionAndObjectID: u8 = 0x20; // [✓cap] our spawn position + object id (world entry)
    pub const CharacterInitFinished: u8 = 0x2B; // [✓cap] 1B — the server's "you are in the world" marker
    pub const GameOpenReply: u8 = 0x2D; // [✓cap] 1B reply to GameOpenRequest
    /// OPEN_ORACLE `eServerPackets.UDPInitReply = 0x2F` — PacketLib168: IP pstr22 + UDP port u16.
    pub const UDPInitReply: u8 = 0x2F;
    /// OPEN_ORACLE `eServerPackets.AttackMode = 0x74` — PacketLib168: [state u8][3 pad].
    pub const AttackMode: u8 = 0x74;
    /// OPEN_ORACLE `eServerPackets.MaxSpeed = 0xB6` — PacketLib168: [pct u16 BE][turningDisabled u8][waterPct u8].
    pub const MaxSpeed: u8 = 0xB6;
    pub const Message: u8 = 0xAF; // [✓cap] system/chat text (readable ASCII)
    pub const NPCCreate: u8 = 0xDA; // [✓cap] living NPC came into view (111 in trace)
    pub const ObjectCreate: u8 = 0xD9; // [✓cap] static object came into view
    pub const EquipmentUpdate: u8 = 0x15; // [✓cap] paired with each NPCCreate
    pub const ObjectUpdate: u8 = 0xA1; // [✓cap] highest-frequency in-world packet (1143 in trace)
    pub const PlayerPositionUpdate: u8 = 0xA9; // [✓cap]
                                               // [✓cap+oracle] Our own vitals; 22B, present in every capture (29–349 each). The oracle is
                                               // authoritative here in a way it is NOT for the client→server combat codes: this is a packet
                                               // the DoL server WRITES (`PacketLib190.SendStatusUpdate`), so its source is the encoder.
    pub const CharacterStatusUpdate: u8 = 0xAD;
    // [✓cap+oracle] An entity left our view: [oid u16 BE][unknown u16 BE, always 1]. 4 bytes in
    // every one of the 207 captured samples, matching `PacketLib168.SendObjectDelete` exactly.
    pub const ObjectDelete: u8 = 0xE1;
    /// OPEN_ORACLE `eServerPackets.RemoveObject = 0xA2` — PacketLib168: [oid u16][oType u16].
    pub const RemoveObject: u8 = 0xA2;
    /// OPEN_ORACLE `eServerPackets.ChangeTarget = 0xF6` — PacketLib168: [oid u16][pad u16].
    pub const ChangeTarget: u8 = 0xF6;
    // [✓cap+oracle] Multiplexed update, keyed by a leading subcode byte. Subcode 0x01 is the
    // player's usable-skills list (54 pages / 2196 entries across the captures); 0x03 is the
    // character header, 0x05 a small periodic update, 0x08 the crafting list.
    pub const VariousUpdate: u8 = 0x16;
    // [✓cap] Another player came into view. **NOT the enum's `PlayerCreate = 0xD4`** — this server
    // sends the 172-era form, `eServerPackets.PlayerCreate172 = 0x4B`, which is what appears in the
    // captures (with readable character names in the payload). Another case of the oracle's enum
    // naming a code the live server never uses; trust the wire.
    pub const PlayerCreate: u8 = 0x4B;
    pub const Quit: u8 = 0xA4; // [oracle eServerPackets.Quit] logout confirmed: [totalOut u8][level u8] — server has saved+removed us, safe to close
                               // [oracle eServerPackets.CombatAnimation = 0xBC] PacketLib186 layout; 55 samples in
                               // rustdaoc_combat_20260716.bin — structured swing feedback (NOT damage amounts).
    pub const CombatAnimation: u8 = 0xBC;
    // [oracle eServerPackets.InventoryUpdate = 0x02] PacketLib189 hdr + PacketLib1124 WriteItemData;
    // present on every world-entry capture (bags + worn gear).
    pub const InventoryUpdate: u8 = 0x02;
    // [oracle eServerPackets.MoneyUpdate = 0xFA] PacketLib168 SendUpdateMoney — 8B purse.
    pub const MoneyUpdate: u8 = 0xFA;
    // [oracle eServerPackets.PlayerDeath = 0xAE] PacketLib168 SendPlayerDied — oid + killer + 4 pad.
    pub const PlayerDeath: u8 = 0xAE;
    // [oracle eServerPackets.CharacterJump = 0x04] PacketLib168 SendPlayerJump — teleports.
    pub const CharacterJump: u8 = 0x04;
    // [oracle eServerPackets.PlayerRevive = 0x89] PacketLib168 SendPlayerRevive — oid + 0.
    pub const PlayerRevive: u8 = 0x89;
    // [oracle eServerPackets.MerchantWindow = 0x17] PacketLib1125 SendMerchantWindow (1.127);
    // OWN_CAPTURE in cap_20260714_230416_conn41042.bin (12 pages).
    pub const MerchantWindow: u8 = 0x17;
    // [oracle eServerPackets.ConsignmentMerchantMoney = 0x1E] PacketLib168 SendConsignmentMerchantMoney.
    // Same 8-byte purse layout as MoneyUpdate; NOT the player 0xFA purse.
    pub const ConsignmentMerchantMoney: u8 = 0x1E;
    // [oracle eServerPackets.ObjectGuildID = 0xDE] PacketLib168 SendObjectGuildID.
    pub const ObjectGuildID: u8 = 0xDE;
    // [oracle eServerPackets.SpellCastAnimation = 0x72] PacketLib168 SendSpellCastAnimation.
    pub const SpellCastAnimation: u8 = 0x72;
    // [oracle eServerPackets.ConcentrationList = 0x75] PacketLib168 SendConcentrationList.
    pub const ConcentrationList: u8 = 0x75;
    // [oracle eServerPackets.UpdateIcons = 0x7F] PacketLib1110 SendUpdateIcons (1.127 inherit).
    pub const UpdateIcons: u8 = 0x7F;
    // [oracle eServerPackets.CharacterPointsUpdate = 0x91] PacketLib190 SendUpdatePoints.
    pub const CharacterPointsUpdate: u8 = 0x91;
    // [oracle eServerPackets.SpellEffectAnimation = 0x1B] PacketLib174 (1.127; no 0xFFBF trailer).
    pub const SpellEffectAnimation: u8 = 0x1B;
    // [oracle eServerPackets.InterruptSpellCast = 0x73] PacketLib168 SendInterruptAnimation.
    pub const InterruptSpellCast: u8 = 0x73;
    // [oracle eServerPackets.RegionChanged = 0xB7] PacketLib174 SendRegionChanged (1.127 inherit).
    pub const RegionChanged: u8 = 0xB7;
    // [oracle eServerPackets.DoorState = 0x99] PacketLib168 SendDoorState — open/close confirm.
    // Same numeric code as c2s DoorRequest; direction disambiguates.
    pub const DoorState: u8 = 0x99;
    // [oracle eServerPackets.ChangeGroundTarget = 0xDF] PacketLib168 SendChangeGroundTarget.
    pub const ChangeGroundTarget: u8 = 0xDF;
    // [oracle eServerPackets.GroupMemberUpdate = 0x70] PacketLib1125 WriteGroupMemberUpdate.
    pub const GroupMemberUpdate: u8 = 0x70;
    // [oracle eServerPackets.TradeWindow = 0xEA] PacketLib172 SendTradeWindow / close = 40 zeros.
    pub const TradeWindow: u8 = 0xEA;
    // [oracle eServerPackets.QuestEntry = 0x83] PacketLib1124 SendQuestPacket — quest log slot.
    pub const QuestEntry: u8 = 0x83;
    // [oracle eServerPackets.PetWindow = 0x88] PacketLib181 SendPetWindow (1.127 inherit).
    pub const PetWindow: u8 = 0x88;
    // [oracle eServerPackets.Encumberance = 0xBD] PacketLib168 SendEncumberance — max then used.
    pub const Encumberance: u8 = 0xBD;
    // [oracle eServerPackets.MarketExplorerWindow = 0x1F] PacketLib1125 header / 168 empty=255.
    pub const MarketExplorerWindow: u8 = 0x1F;
    // [oracle eServerPackets.TrainerWindow = 0x7B] PacketLib1105 multiplexed types.
    pub const TrainerWindow: u8 = 0x7B;
    // [oracle eServerPackets.FindGroupUpdate = 0x86] LFG, not GroupWindow.
    pub const FindGroupUpdate: u8 = 0x86;
    // [oracle eServerPackets.EmblemDialogue = 0xE2] PacketLib168 Fill(0,4).
    pub const EmblemDialogue: u8 = 0xE2;
    // [oracle eServerPackets.SiegeWeaponAnimation = 0xE3] PacketLib1124 prefix.
    pub const SiegeWeaponAnimation: u8 = 0xE3;
    // [oracle eServerPackets.SiegeWeaponInterface = 0xF5] close = WriteShort(0)+WriteShort(1).
    pub const SiegeWeaponInterface: u8 = 0xF5;
    // [oracle eServerPackets.EmoteAnimation = 0xF9] PacketLib168 oid+emote+pad.
    pub const EmoteAnimation: u8 = 0xF9;
    // [oracle eServerPackets.StatsUpdate = 0xFB] PacketLib175 attributes/resists multiplex.
    pub const StatsUpdate: u8 = 0xFB;

    // --- Shape-2 first-loop S2C (OPEN_ORACLE eServerPackets) ---
    pub const MovingObjectCreate: u8 = 0x12;
    pub const ControlledHorse: u8 = 0x4E;
    pub const PlayerModelTypeChange: u8 = 0x8D;
    pub const BadNameCheckReply: u8 = 0xC3;
    pub const Riding: u8 = 0xC8;
    pub const SoundEffect: u8 = 0xC9;
    pub const DupNameCheckReply: u8 = 0xCC;
    pub const CheckLosRequest: u8 = 0xD0;
    pub const PlaySound: u8 = 0xD3;
    pub const DisableSkills: u8 = 0xD6;
    pub const DelveInfo: u8 = 0xD8;
    pub const ModelChange: u8 = 0xDB;
    pub const ObjectDataUpdate: u8 = 0xEE;
    pub const CharacterCreateReply: u8 = 0xF0;
    pub const TimerWindow: u8 = 0xF3;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xor_is_involutive() {
        for logical in 0u8..=255 {
            assert_eq!(wire_code(wire_code(logical)), logical);
        }
    }

    #[test]
    fn matches_oracle_documented_pair() {
        // Oracle: `CryptKeyRequest = 0xF4, // 0x5C ^ 168`.
        assert_eq!(wire_code(client::CryptKeyRequest), 0x5C);
    }
}
