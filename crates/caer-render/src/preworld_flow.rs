//! Deterministic pre-world flow controller (Codex P0.1, 2026-08-14).
//!
//! Screens advance on **protocol / player events**, never merely because art decoded.
//! Visual `PreWorldScreen` is derived from this controller + live [`SessionPhase`].

use caer_protocol::session::SessionPhase;

use crate::preworld::PreWorldScreen;

/// Product-facing pre-world step (superset of rendered screens).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FlowStep {
    /// Intro EA splash (`pregame/splash.mpk`).
    StartupSplash,
    Login,
    Authenticating,
    RealmSelect,
    CharSelect,
    CharCreate,
    /// Appearance customization (`character_customize.xml`) — ledger E6/B3. A local sub-screen:
    /// the protocol stays in `CharacterSelect` the whole time.
    CharCustomize,
    /// Starting-stat allocation (`character_customize_stats.xml`) modal over customization.
    CharStats,
    /// Zone / enter-world loading plate.
    Loading,
    InWorld,
}

/// Why the flow advanced (tests assert these, not art presence).
/// Which flow event a local UI action means, if any.
///
/// Pure, and in the library rather than in `rustdaoc`, because this mapping is where **ledger A13**
/// lived: `CharSelectQuit` was mapped onto [`FlowEvent::Closed`], and `Closed` means "the session
/// ended, show the login screen". So pressing Quit navigated to the login screen — the login screen
/// was not a symptom, it was the destination the mapping asked for. While the mapping sat in a
/// binary, nothing could gate it.
///
/// Actions that end the process return `None`. There is no flow event for "there is no next
/// screen", and inventing one would put us straight back where we started.
#[must_use]
pub fn flow_event_for(action: crate::preworld::PreWorldAction) -> Option<FlowEvent> {
    use crate::preworld::PreWorldAction as A;
    match action {
        A::ChooseRealm(realm) => Some(FlowEvent::RealmChosen(realm)),
        A::OpenCharCreate => Some(FlowEvent::OpenCreate),
        // Continué on race/class opens customization. The customizer's own Continue is the
        // creation boundary; the stats form owns only local Reset/Optimize.
        A::CharCreateContinue => Some(FlowEvent::CustomizeOpened),
        A::CustomizeStats => Some(FlowEvent::StatsOpened),
        A::StatsDismiss => Some(FlowEvent::StatsDismissed),
        A::CustomizeBack => Some(FlowEvent::CreateDismissed),
        A::CustomizeCancel => Some(FlowEvent::CreateCancelled),
        // The customizer's Continue has put a real CharacterCreate request on the live-command
        // queue.  It does not navigate optimistically: the fresh overview is the authoritative
        // acceptance signal.  Keeping this distinct from `CreateAccepted` is what lets the form
        // show a coherent waiting state instead of looking like Continue was ignored.
        A::CustomizeAdvance => Some(FlowEvent::CreateSubmitted),
        A::StatsReset
        | A::StatsOptimize
        | A::StatsAdjust { .. }
        | A::CustomizeAdjust { .. }
        | A::CustomizeSlider { .. }
        | A::CustomizeToggleLock { .. }
        | A::CustomizeRandom
        | A::CustomizeCamera(_)
        | A::CustomizeReset => None,
        // Cancel is navigation, not chrome: it leaves the form for character select, the same
        // destination `CustomizeBack` reaches from one step further in. It sat in the group below
        // — the form-EDITING actions, which correctly navigate nowhere — so the button rendered,
        // hit-tested, dispatched, and did nothing at all.
        A::CharCreateCancel => Some(FlowEvent::CreateDismissed),
        A::CharCreateRandomName
        | A::CharCreateFocusName
        | A::CharCreateGender(_)
        | A::CharCreateRace(_)
        | A::CharCreateClass(_) => None,
        A::EnterWorld => Some(FlowEvent::EnterWorldRequested),
        A::BackToRealm => Some(FlowEvent::BackToRealm),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowEvent {
    /// Splash minimum dwell elapsed (or forced skip in tests).
    SplashFinished,
    /// Player activated Play / submitted credentials.
    CredentialsSubmitted,
    /// Server granted login.
    LoginGranted,
    /// Login denied / timeout / disconnect during auth.
    AuthFailed,
    /// Server reported unbound realm (Realm 0) — must show realm select.
    RealmUnbound,
    /// Server reported bound realm 1..3 (skip realm plate).
    RealmBound(u8),
    /// Player chose a realm on the plate.
    RealmChosen(u8),
    /// Player returned from character select to the realm plate.
    BackToRealm,
    /// Character overview arrived (char select).
    OverviewReady,
    /// Player opened create form.
    OpenCreate,
    /// Continué accepted on the create form — customize screen is next. Sends nothing.
    CustomizeOpened,
    /// The customizer's Adjust Attributes control opened the stats modal. Sends nothing.
    StatsOpened,
    /// The closable source stats modal was dismissed, revealing its customization parent.
    StatsDismissed,
    /// Create cancelled or Continué refused — stay / return to char select.
    CreateDismissed,
    /// The explicit Cancel button from a downstream creation sub-screen: return to character
    /// select instead of treating it as the one-step Race/Class back control.
    CreateCancelled,
    /// The customizer's Continue dispatched CharacterCreateRequest.  The client stays on the
    /// form until the server's refreshed overview confirms the exact new character.
    CreateSubmitted,
    /// Create accepted by server — back to overview.
    CreateAccepted,
    /// Player committed Enter World / Play on an occupied slot.
    EnterWorldRequested,
    /// Protocol entered EnteringWorld.
    WorldEntryStarted,
    /// Both protocol InWorld **and** required render assets ready.
    WorldReady,
    /// Linkdeath / reconnect path (separate from first enter).
    Linkdead,
    /// Clean quit / closed.
    Closed,
}

/// Authoritative loading-plate reference (OWN_CAPTURE retail `data/loading/`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadingPlate {
    pub archive_rel: &'static str,
    pub member: &'static str,
    /// Provenance note for evidence docs.
    pub provenance: &'static str,
}

/// Neutral stock fallback when realm/region mapping is unknown (Codex: not mid1).
pub const NEUTRAL_LOADING_PLATE: LoadingPlate = LoadingPlate {
    archive_rel: "data/loading/spirit1.mpk",
    member: "spirit1.dds",
    provenance: "neutral stock fallback — spirit1; not Midgard mid1",
};

/// Pick a loading plate from destination realm (+ optional region).
///
/// Exact Mythic region→filename table is still a provenance gap; realm capitals / starter
/// plates are OPEN_ORACLE-aligned guesses with documented provenance.
#[must_use]
pub fn loading_plate_for(realm: u8, region: u16) -> LoadingPlate {
    // Tiny known-region overrides (starter capitals / common hubs).
    match region {
        1 | 10 => {
            // Albion / Camelot Hills-ish — use Camelot city pack when present.
            if realm == 1 {
                return LoadingPlate {
                    archive_rel: "data/loading/camcity.mpk",
                    member: "camelot1.dds",
                    provenance: "region capital plate camcity/camelot1 (provisional)",
                };
            }
        }
        100 | 101 if realm == 2 => {
            return LoadingPlate {
                archive_rel: "data/loading/jordcity.mpk",
                member: "jordheim1.dds",
                provenance: "region capital plate jordcity/jordheim1 (provisional)",
            };
        }
        _ => {}
    }
    match realm {
        1 => LoadingPlate {
            archive_rel: "data/loading/alb1.mpk",
            member: "alb0.dds",
            provenance: "Albion realm fallback alb1/alb0",
        },
        2 => LoadingPlate {
            archive_rel: "data/loading/mid1.mpk",
            member: "mid1.dds",
            provenance: "Midgard realm fallback mid1/mid1",
        },
        3 => LoadingPlate {
            archive_rel: "data/loading/hib1.mpk",
            member: "hib0.dds",
            provenance: "Hibernia realm fallback hib1/hib0",
        },
        _ => NEUTRAL_LOADING_PLATE,
    }
}

/// Splash member `splash{1..8}.tga` — deterministic from seed (session/account hash).
#[must_use]
pub fn splash_member(seed: u64) -> &'static str {
    // Retail archive://pregame/splash.mpk:splash%d.tga with d in 1..=8.
    const MEMBERS: [&str; 8] = [
        "splash1.tga",
        "splash2.tga",
        "splash3.tga",
        "splash4.tga",
        "splash5.tga",
        "splash6.tga",
        "splash7.tga",
        "splash8.tga",
    ];
    MEMBERS[(seed % 8) as usize]
}

/// Whether the client should be telling the flow its world assets are ready this tick.
///
/// In the library rather than the binary for the same reason [`flow_event_for`] is: while this
/// decision sat inline in `rustdaoc` it read `phase == InWorld && !world_assets_loaded`, and
/// nothing could gate it. That latch is set once per PROCESS, so on a reconnect it was already
/// true, the readiness signal never fired a second time, and `reconcile_phase(InWorld)` held the
/// flow on the loading plate with no way out. The load itself is idempotent; the signal must not
/// be conditioned on the load having been the first one.
#[must_use]
pub fn should_signal_assets_ready(phase: SessionPhase, world_assets_already_loaded: bool) -> bool {
    let _ = world_assets_already_loaded;
    matches!(phase, SessionPhase::InWorld)
}

/// Truthful CAER version chrome — never counterfeit retail `1.130a`.
#[must_use]
pub fn caer_version_label() -> String {
    format!(
        "CAER {} (rustdaoc)",
        option_env!("CARGO_PKG_VERSION").unwrap_or("0.0.1")
    )
}

/// A request the client has sent and is waiting on the server to answer.
///
/// B5: the pre-world path had two navigators. The click product moved the HUD screen locally while
/// `PreWorldFlow` deliberately waited for the matching server reply, and `reconcile_phase` — which
/// runs every frame off the live `SessionPhase` — then snapped the screen back. From the player's
/// side the click did nothing, even though the packet went out and the phase later advanced.
///
/// The fix is not to let the HUD navigate. It is to give the flow an explicit "asked, waiting"
/// state so it can hold a stable screen across the old phase ticks that arrive before the reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pending {
    /// `RequestCharacterOverview` sent for this realm; waiting for `CharacterOverview`.
    Overview { realm: u8 },
    /// `CharacterCreate` sent; waiting for the refreshed overview.
    Create,
    /// `SelectCharacter` / enter-world sent; waiting for `EnteringWorld`.
    EnterWorld,
}

/// Flow controller — single owner of which product step is active.
#[derive(Debug, Clone)]
pub struct PreWorldFlow {
    step: FlowStep,
    /// Splash plate index seed (fixed for the session).
    pub splash_seed: u64,
    /// Destination realm for loading-plate pick (0 = unknown).
    pub dest_realm: u8,
    /// Destination region for loading-plate pick (0 = unknown).
    pub dest_region: u16,
    /// Protocol says world entry is underway.
    pub protocol_entering: bool,
    /// Render/asset side ready (driver sets when meshes/textures ready).
    pub assets_ready: bool,
    /// Selected char-select slot (UI index into overview), if any.
    pub selected_slot: Option<u8>,
    /// Outstanding server request, if any. While set, `reconcile_phase` will not rewind the
    /// screen on a stale pre-reply phase tick.
    pending: Option<Pending>,
    /// The player pressed **Realm** on the character plate to go back.
    ///
    /// Going back is client-side: the session stays in `SessionPhase::CharacterSelect` until a
    /// realm is picked again, so without this latch the very next `reconcile_phase` tick sees
    /// `CharacterSelect` and snaps the screen straight back to the character plate. Observed as
    /// fifteen consecutive `preworld action BackToRealm` lines in a live playtest with the screen
    /// never changing — the same split-brain B5 fixed for realm *clicks*, in the other direction.
    ///
    /// Cleared as soon as the player picks a realm, an overview lands, or the session leaves
    /// `CharacterSelect`.
    returned_to_realm: bool,
}

impl Default for PreWorldFlow {
    fn default() -> Self {
        Self::new(0)
    }
}

impl PreWorldFlow {
    #[must_use]
    pub fn new(splash_seed: u64) -> Self {
        Self {
            step: FlowStep::StartupSplash,
            splash_seed,
            dest_realm: 0,
            dest_region: 0,
            protocol_entering: false,
            assets_ready: false,
            selected_slot: None,
            pending: None,
            returned_to_realm: false,
        }
    }

    /// A flow that starts on `screen` and answers to local navigation alone.
    ///
    /// For an **offline pre-world session** — a window opened with `--preworld` and no server.
    /// There is no socket, so `SessionPhase` sits at `Disconnected` forever, and
    /// [`Self::reconcile_phase`] rewinds that to the login plate on every single tick. Letting it
    /// run in a session that has nothing to reconcile against is what made Realm, Cancel and
    /// Options look dead: the click landed, the step changed, and the next frame put it back.
    ///
    /// Realm choice resolves locally here too, because the reply it would otherwise wait for is
    /// never coming — see [`Self::choose_realm_offline`].
    #[must_use]
    pub fn offline_at(splash_seed: u64, screen: PreWorldScreen) -> Self {
        let mut f = Self::new(splash_seed);
        f.step = match screen {
            PreWorldScreen::Splash => FlowStep::StartupSplash,
            PreWorldScreen::Login => FlowStep::Login,
            PreWorldScreen::RealmSelect => FlowStep::RealmSelect,
            PreWorldScreen::CharSelect => FlowStep::CharSelect,
            PreWorldScreen::CharCreate => FlowStep::CharCreate,
            PreWorldScreen::CharCustomize => FlowStep::CharCustomize,
            PreWorldScreen::CharStats => FlowStep::CharStats,
            PreWorldScreen::Loading => FlowStep::Authenticating,
        };
        f
    }

    /// Offline realm choice: land on the creation form for that realm instead of waiting for a
    /// character overview that no server will send.
    ///
    /// Returns false when the flow is not on the realm plate, so the caller can fall through to
    /// the normal online path.
    pub fn choose_realm_offline(&mut self, realm: u8) -> bool {
        if self.step != FlowStep::RealmSelect {
            return false;
        }
        self.dest_realm = realm;
        self.returned_to_realm = false;
        self.pending = None;
        self.selected_slot = None;
        self.step = FlowStep::CharCreate;
        true
    }

    /// Construct the controller after the synchronous login warm-up has already observed a
    /// protocol phase.  The product currently opens its window *after* that warm-up, so starting
    /// every live run at `StartupSplash` would make the visual state lag behind the real session.
    #[must_use]
    pub fn from_observed_phase(splash_seed: u64, phase: SessionPhase) -> Self {
        let mut flow = Self::new(splash_seed);
        flow.reconcile_phase(phase);
        flow
    }

    #[must_use]
    pub fn step(&self) -> FlowStep {
        self.step
    }

    #[must_use]
    pub fn splash_member(&self) -> &'static str {
        splash_member(self.splash_seed)
    }

    #[must_use]
    pub fn loading_plate(&self) -> LoadingPlate {
        loading_plate_for(self.dest_realm, self.dest_region)
    }

    /// Map flow step → render screen. `None` once in-world.
    #[must_use]
    pub fn screen(&self) -> Option<PreWorldScreen> {
        match self.step {
            FlowStep::StartupSplash => Some(PreWorldScreen::Splash),
            // Authenticating shares its plate with the linkdead drop, which is what retail shows
            // between the login dialog and realm select — Matt, 2026-08-19. `splash.mpk` is the
            // boot logo and belongs to `StartupSplash` alone; drawing it here is why the login
            // handoff looked like it went straight to the realm plate.
            FlowStep::Authenticating => Some(PreWorldScreen::Loading),
            FlowStep::Login => Some(PreWorldScreen::Login),
            FlowStep::RealmSelect => Some(PreWorldScreen::RealmSelect),
            FlowStep::CharSelect => Some(PreWorldScreen::CharSelect),
            FlowStep::CharCreate => Some(PreWorldScreen::CharCreate),
            FlowStep::CharCustomize => Some(PreWorldScreen::CharCustomize),
            FlowStep::CharStats => Some(PreWorldScreen::CharStats),
            FlowStep::Loading => Some(PreWorldScreen::Loading),
            FlowStep::InWorld => None,
        }
    }

    /// Apply a discriminating event. Returns whether the step changed.
    pub fn on_event(&mut self, ev: FlowEvent) -> bool {
        let before = self.step;
        match (self.step, ev) {
            (FlowStep::StartupSplash, FlowEvent::SplashFinished) => {
                self.step = FlowStep::Login;
            }
            (FlowStep::Login, FlowEvent::CredentialsSubmitted) => {
                self.step = FlowStep::Authenticating;
            }
            (FlowStep::Authenticating, FlowEvent::LoginGranted) => {
                // Wait for realm bind/unbound — do not invent RealmSelect yet.
            }
            (FlowStep::Authenticating, FlowEvent::RealmUnbound) => {
                self.step = FlowStep::RealmSelect;
            }
            (FlowStep::Authenticating, FlowEvent::RealmBound(r)) => {
                self.dest_realm = r;
                self.step = FlowStep::CharSelect;
            }
            (FlowStep::Authenticating | FlowStep::Login, FlowEvent::AuthFailed) => {
                self.step = FlowStep::Login;
            }
            (FlowStep::RealmSelect, FlowEvent::RealmChosen(r)) => {
                self.dest_realm = r;
                self.returned_to_realm = false;
                // Stay on the realm plate until the overview arrives, but latch that we asked so a
                // stale `SessionPhase::RealmSelect` tick cannot be mistaken for "nothing happened".
                self.pending = Some(Pending::Overview { realm: r });
            }
            // The creation form authors its own Realm button (ControlId 1053), so Back must work
            // from there too, not only from the character plate. E6: from any creation
            // sub-screen — customize and stats included.
            (
                FlowStep::CharSelect
                | FlowStep::CharCreate
                | FlowStep::CharCustomize
                | FlowStep::CharStats,
                FlowEvent::BackToRealm,
            ) => {
                self.dest_realm = 0;
                self.selected_slot = None;
                self.pending = None;
                self.returned_to_realm = true;
                self.step = FlowStep::RealmSelect;
            }
            (FlowStep::RealmSelect, FlowEvent::OverviewReady)
            | (FlowStep::Authenticating, FlowEvent::OverviewReady) => {
                self.pending = None;
                self.returned_to_realm = false;
                self.step = FlowStep::CharSelect;
            }
            (FlowStep::CharSelect, FlowEvent::OpenCreate) => {
                self.step = FlowStep::CharCreate;
            }
            // E6: the creation chain is three local sub-screens; the server still reports
            // CharacterSelect across all of them. Each forward step is client-side.
            (FlowStep::CharCreate, FlowEvent::CustomizeOpened) => {
                self.step = FlowStep::CharCustomize;
            }
            (FlowStep::CharCustomize, FlowEvent::StatsOpened) => {
                self.step = FlowStep::CharStats;
            }
            // The source stats window is a closable modal over customization.
            (FlowStep::CharStats, FlowEvent::StatsDismissed) => {
                self.step = FlowStep::CharCustomize;
            }
            // Continue has crossed the packet boundary, but a create is not accepted merely
            // because it was queued.  Hold the authored form while the server processes it;
            // `CreateAccepted` arrives only when the refreshed overview contains the requested
            // name.  The old mapping returned `None`, so a successful create left the player
            // staring at the same customizer until they pressed Cancel and happened to discover
            // the new character on the select screen.
            (FlowStep::CharCustomize, FlowEvent::CreateSubmitted) => {
                self.pending = Some(Pending::Create);
            }
            // Back navigation down the same chain. CreateDismissed from Customize returns to
            // the create form — NOT to char select, which is what it means from CharCreate.
            (FlowStep::CharCustomize, FlowEvent::CreateDismissed) => {
                self.step = FlowStep::CharCreate;
            }
            // Cancel is not the local one-step back action. Every creation sub-screen's Cancel
            // returns to the character rows, matching the separate round `quit` button the
            // client authors on each form.
            (
                FlowStep::CharCreate | FlowStep::CharCustomize | FlowStep::CharStats,
                FlowEvent::CreateCancelled,
            ) => {
                self.pending = None;
                self.step = FlowStep::CharSelect;
            }
            (FlowStep::CharCreate, FlowEvent::CreateDismissed | FlowEvent::CreateAccepted) => {
                self.pending = None;
                self.step = FlowStep::CharSelect;
            }
            // The customizer's Continue put the packet out; the overview refresh returns to the
            // character rows. The stats window can never own this event because it is modal.
            (FlowStep::CharCustomize, FlowEvent::CreateAccepted) => {
                self.pending = None;
                self.step = FlowStep::CharSelect;
            }
            (FlowStep::CharSelect, FlowEvent::EnterWorldRequested) => {
                self.pending = Some(Pending::EnterWorld);
                self.step = FlowStep::Loading;
                self.protocol_entering = false;
                self.assets_ready = false;
            }
            (_, FlowEvent::WorldEntryStarted) => {
                self.pending = None;
                self.step = FlowStep::Loading;
                self.protocol_entering = true;
            }
            (FlowStep::Loading, FlowEvent::WorldReady)
                if self.protocol_entering && self.assets_ready =>
            {
                self.step = FlowStep::InWorld;
            }
            (_, FlowEvent::Linkdead) => {
                self.pending = None;
                self.step = FlowStep::Loading;
                self.protocol_entering = false;
                self.assets_ready = false;
            }
            (_, FlowEvent::Closed) => {
                self.pending = None;
                self.step = FlowStep::Login;
                self.protocol_entering = false;
                self.assets_ready = false;
            }
            // Art-decoded / spurious: no transition.
            _ => {}
        }
        self.step != before
    }

    /// Mark render assets ready; may complete Loading → InWorld if protocol already entered.
    pub fn set_assets_ready(&mut self, ready: bool) -> bool {
        self.assets_ready = ready;
        if ready && self.protocol_entering && self.step == FlowStep::Loading {
            return self.on_event(FlowEvent::WorldReady);
        }
        false
    }

    /// Align with an authoritative [`SessionPhase`] from *any* current visual step.
    ///
    /// This is deliberately a reconciliation, not a chain of synthetic UI events.  A live client
    /// may consume login/realm packets before its first winit frame; requiring the intermediate
    /// `SplashFinished` event made a genuine `CharacterSelect` observation remain stuck on the
    /// splash forever.  Character creation is the one local sub-screen retained while the
    /// protocol remains at character select.
    pub fn reconcile_phase(&mut self, phase: SessionPhase) -> bool {
        let before = self.step;
        match phase {
            SessionPhase::Disconnected | SessionPhase::Closed => {
                self.step = FlowStep::Login;
                self.protocol_entering = false;
                self.assets_ready = false;
            }
            SessionPhase::CryptHandshake | SessionPhase::Authenticating => {
                self.step = FlowStep::Authenticating;
                self.protocol_entering = false;
                self.assets_ready = false;
            }
            SessionPhase::RealmSelect => {
                // B5: an outstanding overview request means this tick is the *old* phase arriving
                // before the reply, not a fresh instruction to show the realm plate. Rewinding here
                // is what made a realm click look dead. Character creation is likewise a local
                // sub-screen that a pre-reply tick must not close.
                if !matches!(self.pending, Some(Pending::Overview { .. }))
                    && !matches!(
                        self.step,
                        FlowStep::CharCreate | FlowStep::CharCustomize | FlowStep::CharStats
                    )
                {
                    self.step = FlowStep::RealmSelect;
                }
                self.protocol_entering = false;
                self.assets_ready = false;
            }
            SessionPhase::CharacterSelect => {
                // A just-issued EnterWorld request remains on the loading plate while the socket
                // still reports CharacterSelect. The next authoritative phase either confirms
                // EnteringWorld or a session failure returns us to login.
                //
                // `returned_to_realm` is the same idea for a *local* back-navigation: the session
                // legitimately stays in CharacterSelect while the player is on the realm plate, so
                // without it this arm undoes the Realm button on the next tick.
                if !matches!(
                    self.step,
                    FlowStep::CharCreate | FlowStep::CharCustomize | FlowStep::CharStats
                ) && self.step != FlowStep::Loading
                    && self.pending.is_none()
                    && !self.returned_to_realm
                {
                    self.step = FlowStep::CharSelect;
                }
                self.protocol_entering = false;
                self.assets_ready = false;
            }
            SessionPhase::EnteringWorld => {
                self.step = FlowStep::Loading;
                self.protocol_entering = true;
                self.assets_ready = false;
            }
            SessionPhase::InWorld => {
                self.protocol_entering = true;
                self.step = if self.assets_ready {
                    FlowStep::InWorld
                } else {
                    FlowStep::Loading
                };
            }
        }
        self.step != before
    }

    /// Backward-compatible name used by protocol drains.
    pub fn observe_phase(&mut self, phase: SessionPhase) -> bool {
        self.reconcile_phase(phase)
    }

    /// The outstanding server request, if any. The HUD renders a busy affordance from this rather
    /// than navigating on its own.
    #[must_use]
    pub fn pending(&self) -> Option<Pending> {
        self.pending
    }

    /// Whether the player is waiting on a reply — drives the "asked, waiting" presentation so a
    /// click is visibly acknowledged without the HUD becoming a second navigator.
    #[must_use]
    pub fn is_awaiting_server(&self) -> bool {
        self.pending.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn art_decode_alone_does_not_advance() {
        let mut f = PreWorldFlow::new(0);
        assert_eq!(f.step(), FlowStep::StartupSplash);
        // No FlowEvent for "art decoded" exists — splash stays until SplashFinished.
        assert!(!f.on_event(FlowEvent::OverviewReady));
        assert_eq!(f.step(), FlowStep::StartupSplash);
    }

    #[test]
    fn unbound_account_matrix() {
        let mut f = PreWorldFlow::new(3);
        assert!(f.on_event(FlowEvent::SplashFinished));
        assert_eq!(f.step(), FlowStep::Login);
        assert!(f.on_event(FlowEvent::CredentialsSubmitted));
        assert_eq!(f.step(), FlowStep::Authenticating);
        // LoginGranted alone must NOT invent realm select.
        assert!(!f.on_event(FlowEvent::LoginGranted));
        assert_eq!(f.step(), FlowStep::Authenticating);
        assert!(f.on_event(FlowEvent::RealmUnbound));
        assert_eq!(f.step(), FlowStep::RealmSelect);
        assert_eq!(f.screen(), Some(PreWorldScreen::RealmSelect));
        f.on_event(FlowEvent::RealmChosen(1));
        assert!(f.on_event(FlowEvent::OverviewReady));
        assert_eq!(f.step(), FlowStep::CharSelect);
    }

    #[test]
    fn bound_realm_skips_realm_plate() {
        let mut f = PreWorldFlow::new(0);
        f.on_event(FlowEvent::SplashFinished);
        f.on_event(FlowEvent::CredentialsSubmitted);
        assert!(f.on_event(FlowEvent::RealmBound(2)));
        assert_eq!(f.step(), FlowStep::CharSelect);
        assert_eq!(f.dest_realm, 2);
    }

    #[test]
    fn loading_holds_until_protocol_and_assets() {
        let mut f = PreWorldFlow::new(0);
        f.step = FlowStep::CharSelect;
        f.dest_realm = 1;
        assert!(f.on_event(FlowEvent::EnterWorldRequested));
        assert_eq!(f.step(), FlowStep::Loading);
        // WorldReady ignored until both flags set.
        assert!(!f.on_event(FlowEvent::WorldReady));
        f.protocol_entering = true;
        assert!(!f.on_event(FlowEvent::WorldReady)); // assets still false
        assert!(f.set_assets_ready(true));
        assert_eq!(f.step(), FlowStep::InWorld);
    }

    /// A successful create is an asynchronous server transaction, not a local screen jump.
    /// Continue must visibly latch that request, survive the stale CharacterSelect phase while
    /// DOLSharp processes it, then leave the form only when the refreshed overview confirms it.
    #[test]
    fn customizer_continue_waits_for_server_confirmation_then_returns_to_character_select() {
        use crate::preworld::PreWorldAction as A;

        let mut f = PreWorldFlow::from_observed_phase(0, SessionPhase::CharacterSelect);
        assert!(f.on_event(FlowEvent::OpenCreate));
        assert!(f.on_event(FlowEvent::CustomizeOpened));
        assert_eq!(f.step(), FlowStep::CharCustomize);
        assert_eq!(
            flow_event_for(A::CustomizeAdvance),
            Some(FlowEvent::CreateSubmitted),
            "the packet boundary must not be invisible to the flow"
        );

        assert!(
            !f.on_event(FlowEvent::CreateSubmitted),
            "submission stays on the form while the server owns acceptance"
        );
        assert_eq!(f.pending(), Some(Pending::Create));
        f.reconcile_phase(SessionPhase::CharacterSelect);
        assert_eq!(
            f.step(),
            FlowStep::CharCustomize,
            "the stale pre-reply phase must not make Continue appear to do nothing"
        );

        assert!(f.on_event(FlowEvent::CreateAccepted));
        assert_eq!(f.pending(), None);
        assert_eq!(f.step(), FlowStep::CharSelect);
    }

    #[test]
    fn auth_failure_returns_to_login() {
        let mut f = PreWorldFlow::new(0);
        f.step = FlowStep::Authenticating;
        assert!(f.on_event(FlowEvent::AuthFailed));
        assert_eq!(f.step(), FlowStep::Login);
    }

    /// The readiness signal must not depend on the client's once-per-process load latch. This is
    /// the binary's own decision, lifted here so it can be gated at all — with the latch already
    /// true, as it is on every reconnect, the answer must still be yes.
    #[test]
    fn readiness_is_signalled_on_a_reconnect_not_only_the_first_load() {
        for latched in [false, true] {
            assert!(
                should_signal_assets_ready(SessionPhase::InWorld, latched),
                "in-world must signal readiness with the load latch at {latched}"
            );
        }
        // The loop above IS the control. The condition this replaces was
        // `phase == InWorld && !world_assets_loaded`, which differs from the current answer on
        // exactly one input — `latched == true` — and that input is every reconnect. Drop the
        // `latched: true` case and this test passes against the stalling implementation.

        for phase in [
            SessionPhase::Disconnected,
            SessionPhase::CharacterSelect,
            SessionPhase::EnteringWorld,
            SessionPhase::Closed,
        ] {
            assert!(
                !should_signal_assets_ready(phase, false),
                "{phase:?} is not in the world and must not claim readiness"
            );
        }
    }

    /// Link-death, retry, and back in. The return leg is the half that was broken: the client
    /// latches "world assets loaded" for the whole process, so after a reconnect nothing was left
    /// to re-arm `assets_ready` and `reconcile_phase(InWorld)` held the flow on the plate forever.
    /// The client now signals readiness unconditionally while in-world; this pins the round trip.
    #[test]
    fn a_linkdead_round_trip_returns_to_the_world() {
        let mut f = PreWorldFlow::new(0);
        f.reconcile_phase(SessionPhase::InWorld);
        f.set_assets_ready(true);
        assert_eq!(f.step(), FlowStep::InWorld, "must start in the world");

        assert!(f.on_event(FlowEvent::Linkdead));
        assert_eq!(f.step(), FlowStep::Loading, "link death shows the plate");
        assert_eq!(f.screen(), Some(PreWorldScreen::Loading));

        // The socket comes back. The plate must stay up until the world is actually ready —
        // otherwise the player is dropped into a half-loaded world.
        f.reconcile_phase(SessionPhase::InWorld);
        assert_eq!(
            f.step(),
            FlowStep::Loading,
            "phase alone must not clear the plate"
        );

        // And it must actually clear once readiness is signalled. This is the assertion that
        // fails if the client forgets to re-arm it.
        f.set_assets_ready(true);
        assert_eq!(f.step(), FlowStep::InWorld, "stuck on the loading plate");
        assert_eq!(f.screen(), None);
    }

    #[test]
    fn linkdead_uses_loading_not_charselect() {
        let mut f = PreWorldFlow::new(0);
        f.step = FlowStep::InWorld;
        assert!(f.on_event(FlowEvent::Linkdead));
        assert_eq!(f.step(), FlowStep::Loading);
    }

    #[test]
    fn splash_rotates_all_eight() {
        let mut seen = [false; 8];
        for seed in 0..8u64 {
            let m = splash_member(seed);
            let idx = m
                .trim_start_matches("splash")
                .trim_end_matches(".tga")
                .parse::<usize>()
                .unwrap();
            seen[idx - 1] = true;
        }
        assert!(seen.iter().all(|&x| x));
    }

    #[test]
    fn loading_unknown_realm_is_neutral_not_mid1() {
        let p = loading_plate_for(0, 0);
        assert_eq!(p, NEUTRAL_LOADING_PLATE);
        assert!(!p.member.eq_ignore_ascii_case("mid1.dds"));
    }

    #[test]
    fn caer_version_label_is_not_retail_1130() {
        let s = caer_version_label();
        assert!(s.contains("CAER"));
        assert!(!s.contains("1.130"));
        assert!(!s.contains("b1421"));
    }

    /// An offline pre-world window navigates, and the dead phase must not undo it.
    ///
    /// With no server the phase sits at `Disconnected` forever. Reconciling that rewinds the flow
    /// to the login plate every tick, so Realm / Cancel / Options each landed, moved the screen,
    /// and were put straight back — which reads as "the buttons do nothing". An offline flow is
    /// driven by local navigation alone.
    #[test]
    fn an_offline_flow_navigates_and_resolves_its_own_realm_choice() {
        let mut f = PreWorldFlow::offline_at(0, PreWorldScreen::CharCreate);
        assert_eq!(
            f.screen(),
            Some(PreWorldScreen::CharCreate),
            "starts where asked"
        );

        // Realm, from the creation form.
        assert!(f.on_event(FlowEvent::BackToRealm));
        assert_eq!(f.screen(), Some(PreWorldScreen::RealmSelect));

        // Picking a realm offline lands on that realm's creation form rather than waiting for a
        // character overview no server will send.
        assert!(f.choose_realm_offline(2));
        assert_eq!(f.screen(), Some(PreWorldScreen::CharCreate));
        assert_eq!(
            f.dest_realm, 2,
            "the stage and the form follow the chosen realm"
        );
        assert!(f.pending().is_none(), "nothing is outstanding offline");

        // Off the realm plate it declines, so the caller falls through to the online path.
        assert!(
            !f.choose_realm_offline(3),
            "only the realm plate resolves a realm"
        );
        assert_eq!(f.dest_realm, 2, "a declined choice changes nothing");
    }

    /// Every screen `--preworld` accepts must be a legal starting point, or opening the window
    /// there silently lands somewhere else.
    #[test]
    fn offline_start_round_trips_every_screen() {
        for screen in [
            PreWorldScreen::Splash,
            PreWorldScreen::Login,
            PreWorldScreen::RealmSelect,
            PreWorldScreen::CharSelect,
            PreWorldScreen::CharCreate,
            PreWorldScreen::CharCustomize,
            PreWorldScreen::CharStats,
        ] {
            assert_eq!(
                PreWorldFlow::offline_at(0, screen).screen(),
                Some(screen),
                "{screen:?} must open where it was asked for"
            );
        }
    }

    #[test]
    fn observe_phase_realm_select_requires_phase() {
        let mut f = PreWorldFlow::new(0);
        f.step = FlowStep::Authenticating;
        assert!(f.observe_phase(SessionPhase::RealmSelect));
        assert_eq!(f.step(), FlowStep::RealmSelect);
    }

    #[test]
    fn warm_live_character_select_does_not_stick_on_splash() {
        let f = PreWorldFlow::from_observed_phase(7, SessionPhase::CharacterSelect);
        assert_eq!(f.step(), FlowStep::CharSelect);
        assert_eq!(f.screen(), Some(PreWorldScreen::CharSelect));
    }

    #[test]
    fn observed_character_select_preserves_open_create_form() {
        let mut f = PreWorldFlow::from_observed_phase(0, SessionPhase::CharacterSelect);
        assert!(f.on_event(FlowEvent::OpenCreate));
        assert!(!f.observe_phase(SessionPhase::CharacterSelect));
        assert_eq!(f.step(), FlowStep::CharCreate);
    }

    #[test]
    fn observed_in_world_holds_loading_until_assets_are_ready() {
        let mut f = PreWorldFlow::from_observed_phase(0, SessionPhase::InWorld);
        assert_eq!(f.step(), FlowStep::Loading);
        assert_eq!(f.screen(), Some(PreWorldScreen::Loading));
        assert!(f.set_assets_ready(true));
        assert_eq!(f.step(), FlowStep::InWorld);
        assert_eq!(f.screen(), None);
    }

    /// **B5 falsifier.** The missing twin of
    /// `stale_character_select_phase_does_not_undo_enter_world_click`. Between a realm click and
    /// the server's overview, the socket keeps reporting `RealmSelect`, and `reconcile_phase` runs
    /// every frame. Those ticks must not rewind the flow, or the click reads as dead.
    #[test]
    fn realm_chosen_survives_repeated_realm_select_phase_observation() {
        let mut f = PreWorldFlow::from_observed_phase(0, SessionPhase::RealmSelect);
        assert_eq!(f.step(), FlowStep::RealmSelect);
        assert!(!f.is_awaiting_server());

        f.on_event(FlowEvent::RealmChosen(3));
        assert_eq!(f.pending(), Some(Pending::Overview { realm: 3 }));
        assert!(
            f.is_awaiting_server(),
            "the click must be visibly acknowledged"
        );

        // Sixty frames of the pre-reply phase — one second at 60 Hz.
        for _ in 0..60 {
            assert!(
                !f.observe_phase(SessionPhase::RealmSelect),
                "a stale pre-reply tick must not change the step"
            );
            assert!(
                f.is_awaiting_server(),
                "the pending request must survive the tick"
            );
            assert_eq!(f.dest_realm, 3, "the chosen realm must not be forgotten");
        }

        // The matching reply advances exactly once and clears the wait.
        assert!(f.on_event(FlowEvent::OverviewReady));
        assert_eq!(f.step(), FlowStep::CharSelect);
        assert!(!f.is_awaiting_server());
        assert!(
            !f.on_event(FlowEvent::OverviewReady),
            "a duplicate reply must not advance a second time"
        );
    }

    /// Back navigation must not rebound: leaving char select clears the outstanding request so the
    /// realm plate is interactive again rather than stuck showing a wait for a reply we abandoned.
    #[test]
    fn back_to_realm_clears_the_pending_request() {
        let mut f = PreWorldFlow::from_observed_phase(0, SessionPhase::RealmSelect);
        f.on_event(FlowEvent::RealmChosen(1));
        f.on_event(FlowEvent::OverviewReady);
        assert_eq!(f.step(), FlowStep::CharSelect);
        f.on_event(FlowEvent::BackToRealm);
        assert_eq!(f.step(), FlowStep::RealmSelect);
        assert!(
            !f.is_awaiting_server(),
            "back navigation left a stale pending request"
        );
        assert_eq!(f.dest_realm, 0);
        // And the realm plate stays put under the phase ticks that follow.
        for _ in 0..10 {
            assert!(!f.observe_phase(SessionPhase::RealmSelect));
        }
        assert_eq!(f.step(), FlowStep::RealmSelect);
    }

    /// The tick that actually arrives after Back — and the one that used to undo it.
    ///
    /// Choosing a realm moves the **session** to `CharacterSelect` and it stays there while the
    /// player is on the realm plate, because going back is client-side. The test above ticks
    /// `RealmSelect`, which cannot undo anything; live, the phase is `CharacterSelect` and every
    /// frame reconciled the screen straight back to the character plate. A playtest logged fifteen
    /// consecutive `BackToRealm` actions with the screen never changing.
    #[test]
    fn back_to_realm_survives_the_character_select_phase_ticks_that_follow() {
        let mut f = PreWorldFlow::from_observed_phase(0, SessionPhase::RealmSelect);
        f.on_event(FlowEvent::RealmChosen(3));
        f.on_event(FlowEvent::OverviewReady);
        assert_eq!(f.step(), FlowStep::CharSelect);

        assert!(f.on_event(FlowEvent::BackToRealm));
        assert_eq!(f.step(), FlowStep::RealmSelect);
        for i in 0..30 {
            assert!(
                !f.observe_phase(SessionPhase::CharacterSelect),
                "tick {i} moved the step; the session sits in CharacterSelect the whole time the \
                 player is on the realm plate"
            );
            assert_eq!(f.step(), FlowStep::RealmSelect, "tick {i} rebounded");
        }

        // Picking a realm again resumes the normal wait-for-overview path. `RealmChosen` holds the
        // plate and latches the request, so it does not move the step and returns false.
        f.on_event(FlowEvent::RealmChosen(1));
        assert!(f.is_awaiting_server(), "realm pick must latch a request");
        assert_eq!(f.step(), FlowStep::RealmSelect);
        assert!(f.on_event(FlowEvent::OverviewReady));
        assert_eq!(f.step(), FlowStep::CharSelect);
    }

    /// Proves that latch is not a blanket "ignore CharacterSelect": with no back-navigation the
    /// same tick still reconciles.
    #[test]
    fn character_select_tick_still_reconciles_without_a_back_navigation() {
        let mut f = PreWorldFlow::from_observed_phase(0, SessionPhase::RealmSelect);
        assert_eq!(f.step(), FlowStep::RealmSelect);
        assert!(
            f.observe_phase(SessionPhase::CharacterSelect),
            "with no back-navigation the flow must follow the authoritative phase"
        );
        assert_eq!(f.step(), FlowStep::CharSelect);
    }

    /// Proves the guard is not vacuous: without a pending request the same tick *does* reconcile.
    #[test]
    fn realm_select_tick_still_reconciles_when_nothing_is_pending() {
        let mut f = PreWorldFlow::from_observed_phase(0, SessionPhase::CharacterSelect);
        assert_eq!(f.step(), FlowStep::CharSelect);
        assert!(!f.is_awaiting_server());
        assert!(
            f.observe_phase(SessionPhase::RealmSelect),
            "with no pending request the flow must follow the authoritative phase"
        );
        assert_eq!(f.step(), FlowStep::RealmSelect);
    }

    /// An open creation form is a local sub-screen; a pre-reply realm tick must not close it.
    #[test]
    fn realm_select_tick_does_not_close_an_open_create_form() {
        let mut f = PreWorldFlow::from_observed_phase(0, SessionPhase::CharacterSelect);
        assert!(f.on_event(FlowEvent::OpenCreate));
        assert_eq!(f.step(), FlowStep::CharCreate);
        assert!(!f.observe_phase(SessionPhase::RealmSelect));
        assert_eq!(f.step(), FlowStep::CharCreate);
    }

    /// **E6, flow half of acceptance criterion 1.** The whole creation chain is three local
    /// sub-screens under one authoritative `CharacterSelect` phase, and every seam navigates
    /// exactly one step: Continué opens customize (it mints nothing — there is no packet here,
    /// only steps), customize advances to stats, back walks down the same stairs, and Back to
    /// Realm works from anywhere on the chain.
    #[test]
    fn the_creation_chain_is_three_local_steps_under_one_phase() {
        let mut f = PreWorldFlow::from_observed_phase(0, SessionPhase::CharacterSelect);
        f.on_event(FlowEvent::OpenCreate);
        assert_eq!(f.step(), FlowStep::CharCreate);

        // Forward: create -> customize -> stats.
        assert!(f.on_event(FlowEvent::CustomizeOpened));
        assert_eq!(f.step(), FlowStep::CharCustomize);
        assert!(f.on_event(FlowEvent::StatsOpened));
        assert_eq!(f.step(), FlowStep::CharStats);
        assert_eq!(f.screen(), Some(crate::preworld::PreWorldScreen::CharStats));

        // Every frame of the chain, the protocol still says CharacterSelect and must not
        // yank the player back to the plate.
        for _ in 0..30 {
            assert!(!f.observe_phase(SessionPhase::CharacterSelect));
            assert_eq!(f.step(), FlowStep::CharStats);
        }

        // Closing the stats modal reveals its customization parent; Back then descends to the
        // race/class form.
        assert!(f.on_event(FlowEvent::StatsDismissed));
        assert_eq!(f.step(), FlowStep::CharCustomize);
        assert!(f.on_event(FlowEvent::CreateDismissed));
        assert_eq!(f.step(), FlowStep::CharCreate);

        // From CharCreate, CreateDismissed keeps its historical meaning: out to char select.
        assert!(f.on_event(FlowEvent::CreateDismissed));
        assert_eq!(f.step(), FlowStep::CharSelect);

        // The downstream Cancel button is deliberately NOT another CreateDismissed: on the
        // customize form that event is the one-step Race/Class back control. Cancel returns to
        // the rows from wherever it was pressed.
        f.on_event(FlowEvent::OpenCreate);
        f.on_event(FlowEvent::CustomizeOpened);
        f.on_event(FlowEvent::StatsOpened);
        assert_eq!(
            flow_event_for(crate::preworld::PreWorldAction::CustomizeCancel),
            Some(FlowEvent::CreateCancelled)
        );
        assert!(f.on_event(FlowEvent::CreateCancelled));
        assert_eq!(f.step(), FlowStep::CharSelect);

        // And Back to Realm works from the deepest sub-screen.
        f.on_event(FlowEvent::OpenCreate);
        f.on_event(FlowEvent::CustomizeOpened);
        f.on_event(FlowEvent::StatsOpened);
        assert!(f.on_event(FlowEvent::BackToRealm));
        assert_eq!(f.step(), FlowStep::RealmSelect);
        assert_eq!(f.dest_realm, 0);
    }

    /// The accepted create arrives while the flow is still on the customizer — the stats form is
    /// a local modal and its Optimize control never sends a create packet.
    #[test]
    fn create_accepted_from_the_customizer_lands_on_char_select() {
        let mut f = PreWorldFlow::from_observed_phase(0, SessionPhase::CharacterSelect);
        f.on_event(FlowEvent::OpenCreate);
        f.on_event(FlowEvent::CustomizeOpened);
        assert!(f.on_event(FlowEvent::CreateAccepted));
        assert_eq!(f.step(), FlowStep::CharSelect);
        assert!(!f.is_awaiting_server());
    }

    #[test]
    fn stale_character_select_phase_does_not_undo_enter_world_click() {
        let mut f = PreWorldFlow::from_observed_phase(0, SessionPhase::CharacterSelect);
        assert!(f.on_event(FlowEvent::EnterWorldRequested));
        assert!(!f.observe_phase(SessionPhase::CharacterSelect));
        assert_eq!(f.step(), FlowStep::Loading);
    }

    /// **Ledger A13.** Quit must not be spelled as "the session ended".
    ///
    /// `FlowEvent::Closed` sets `step = FlowStep::Login`, so any Quit routed through it navigates to
    /// the login screen instead of quitting. That is the whole defect, and this is its regression
    /// gate.
    ///
    /// The known-bad mapping is written out below and run through the same `FlowStep` machine, so
    /// the control is mechanical: the test proves the historical arm really did land on the login
    /// screen, and that the shipping mapping does not. A test that only asserted
    /// `flow_event_for(CharSelectQuit) != Some(Closed)` would also pass if the function were gutted
    /// to return `None` for everything, which is why the positive mappings are asserted too.
    #[test]
    fn quit_never_maps_onto_the_event_that_means_the_session_ended() {
        use crate::preworld::PreWorldAction as A;

        // Known-bad: the arm this file shipped before A13 was fixed.
        let historical = |action: A| match action {
            A::LoginExit | A::CharSelectQuit => Some(FlowEvent::Closed),
            other => flow_event_for(other),
        };
        for quit in [A::CharSelectQuit, A::LoginExit] {
            let mut bad = PreWorldFlow::default();
            bad.observe_phase(SessionPhase::CharacterSelect);
            bad.on_event(FlowEvent::OverviewReady);
            let before = bad.step();
            if let Some(e) = historical(quit) {
                bad.on_event(e);
            }
            assert_eq!(
                bad.step(),
                FlowStep::Login,
                "{quit:?}: the known-bad mapping must reproduce the defect (was {before:?})"
            );

            // Shipping behaviour: no event at all, so the step cannot move.
            let mut good = PreWorldFlow::default();
            good.observe_phase(SessionPhase::CharacterSelect);
            good.on_event(FlowEvent::OverviewReady);
            let before = good.step();
            assert_eq!(
                flow_event_for(quit),
                None,
                "{quit:?} must not map to any flow event — quitting is not a navigation"
            );
            if let Some(e) = flow_event_for(quit) {
                good.on_event(e);
            }
            assert_eq!(
                good.step(),
                before,
                "{quit:?} must leave the flow where it was"
            );
        }

        // Not vacuous: the mappings that *are* navigations still are.
        assert_eq!(
            flow_event_for(A::ChooseRealm(2)),
            Some(FlowEvent::RealmChosen(2))
        );
        assert_eq!(
            flow_event_for(A::OpenCharCreate),
            Some(FlowEvent::OpenCreate)
        );
        assert_eq!(flow_event_for(A::BackToRealm), Some(FlowEvent::BackToRealm));
        assert_eq!(
            flow_event_for(A::EnterWorld),
            Some(FlowEvent::EnterWorldRequested)
        );
    }

    /// Matt, 2026-08-19: "I don't even think we have loading screens ... it dumps me straight to
    /// realm select." The step between the login dialog and the realm plate drew `splash.mpk`,
    /// the boot logo, so the transition retail covers with a loading plate showed the wrong art
    /// and read as no transition at all.
    ///
    /// The boot splash is asserted alongside it, because moving both onto one screen would satisfy
    /// a test that only checked authenticating.
    #[test]
    fn authenticating_shows_the_loading_plate_and_boot_keeps_the_splash() {
        let mut f = PreWorldFlow::default();
        assert_eq!(f.step(), FlowStep::StartupSplash);
        assert_eq!(f.screen(), Some(PreWorldScreen::Splash));

        f.on_event(FlowEvent::SplashFinished);
        f.on_event(FlowEvent::CredentialsSubmitted);
        assert_eq!(f.step(), FlowStep::Authenticating);
        assert_eq!(
            f.screen(),
            Some(PreWorldScreen::Loading),
            "the login handoff is a loading plate, not the boot splash"
        );

        // Same plate as a linkdead drop, which is how Matt named it.
        let mut dropped = PreWorldFlow::default();
        dropped.on_event(FlowEvent::Linkdead);
        assert_eq!(dropped.screen(), f.screen());

        // No realm chosen yet, so it resolves the neutral stock plate rather than a realm's.
        assert_eq!(f.dest_realm, 0);
        assert_eq!(f.loading_plate(), NEUTRAL_LOADING_PLATE);

        f.on_event(FlowEvent::RealmUnbound);
        assert_eq!(f.screen(), Some(PreWorldScreen::RealmSelect));
    }

    /// Cancel on the create form must leave the form.
    ///
    /// Seen red against the shipped mapping: `CharCreateCancel` returned `None`, so the button
    /// dispatched and the step never changed — Matt's own session log shows
    /// "preworld step unchanged (CharCreate) after CharCreateCancel" twice in a row. The control
    /// drew, hit-tested and resolved correctly; only the flow mapping was missing, which is why
    /// no hitbox or art gate ever caught it.
    #[test]
    fn cancel_on_the_create_form_returns_to_character_select() {
        use crate::preworld::PreWorldAction as A;

        // The mapping itself, which is where the defect lived.
        assert_eq!(
            flow_event_for(A::CharCreateCancel),
            Some(FlowEvent::CreateDismissed),
            "Cancel must raise a navigation event, not None"
        );

        // And the round trip a player actually makes.
        let mut f = PreWorldFlow::from_observed_phase(0, SessionPhase::CharacterSelect);
        assert!(f.on_event(FlowEvent::OpenCreate));
        assert_eq!(f.step(), FlowStep::CharCreate);
        let ev = flow_event_for(A::CharCreateCancel).expect("Cancel maps to an event");
        assert!(f.on_event(ev));
        assert_eq!(
            f.step(),
            FlowStep::CharSelect,
            "Cancel leaves the create form for character select"
        );

        // The form-editing actions stay put: they are the group Cancel was wrongly filed under.
        for a in [
            A::CharCreateRandomName,
            A::CharCreateFocusName,
            A::CharCreateGender(1),
            A::CharCreateRace(2),
            A::CharCreateClass(3),
        ] {
            assert_eq!(flow_event_for(a), None, "{a:?} must not navigate");
        }
    }
}
