//! GPU launch-path init: surface-usage negotiation, adapter selection, typed product errors.
//!
//! Wave1 LANE B ownership. Pure negotiators here are the capability-matrix falsifiers;
//! `Gpu::new` / `Gpu::new_headless` call them so launch never panics on missing GPU/surface.

use std::fmt;
use std::path::PathBuf;

/// Typed, actionable GPU launch failure. Replaces `expect`/`panic` on the product init path.
#[derive(Debug)]
pub enum GpuInitError {
    /// `Instance::create_surface` failed (bad window handle, backend disabled, etc.).
    SurfaceCreate { detail: String },
    /// No adapter matched the deliberate selection policy.
    AdapterUnavailable {
        policy: AdapterSelectPolicy,
        detail: String,
        remediation: &'static str,
    },
    /// `Adapter::request_device` failed after limits/features were accepted.
    DeviceRequest { detail: String },
    /// Advertised surface usages lack `RENDER_ATTACHMENT` — cannot draw.
    SurfaceNoRenderAttachment { advertised: String },
    /// Required wgpu limits exceed what the selected adapter reports.
    LimitsUnsupported { detail: String },
    /// Window creation failed before GPU init (shell path).
    WindowCreate { detail: String },
    /// Cross-process wgpu init lock could not be acquired (fail closed — never init unlocked).
    InitLock { detail: String },
}

impl fmt::Display for GpuInitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SurfaceCreate { detail } => write!(
                f,
                "GPU surface create failed: {detail}\n  \
                 Check that a display is available and Vulkan/DX12/Metal drivers work \
                 (`vulkaninfo` on Linux)."
            ),
            Self::AdapterUnavailable {
                policy,
                detail,
                remediation,
            } => write!(
                f,
                "No suitable GPU adapter ({policy}): {detail}\n  {remediation}"
            ),
            Self::DeviceRequest { detail } => write!(
                f,
                "GPU device request failed: {detail}\n  \
                 Adapter was found but could not create a device; try updating drivers \
                 or set CAER_GPU_FORCE_FALLBACK=1 only if you deliberately want software."
            ),
            Self::SurfaceNoRenderAttachment { advertised } => write!(
                f,
                "Surface does not advertise RENDER_ATTACHMENT (got {advertised}). \
                 Cannot present frames on this surface."
            ),
            Self::LimitsUnsupported { detail } => write!(
                f,
                "GPU adapter limits are below CAER requirements: {detail}\n  \
                 Choose a different GPU (see docs/INSTALL.md § GPU adapter policy) or lower resolution."
            ),
            Self::WindowCreate { detail } => {
                write!(f, "Window create failed before GPU init: {detail}")
            }
            Self::InitLock { detail } => write!(
                f,
                "GPU init lock failed: {detail}\n  \
                 Another CAER process may be initializing wgpu; retry after it exits. \
                 Init never proceeds without an exclusive portable init lock \
                 (Unix flock / Windows LockFileEx)."
            ),
        }
    }
}

impl std::error::Error for GpuInitError {}

/// Result of negotiating swapchain texture usages from advertised surface capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceUsagePlan {
    /// Usages to put in `SurfaceConfiguration` (always includes `RENDER_ATTACHMENT`).
    pub usage: wgpu::TextureUsages,
    /// True only when the surface advertised `COPY_SRC` and we opted into it.
    /// Frame capture is optional; rendering must proceed when this is false.
    pub capture_copy_src: bool,
}

/// Negotiate surface usages.
///
/// Rendering requires only `RENDER_ATTACHMENT`. `COPY_SRC` is optional and reported separately
/// so platforms that omit it still get a working present path.
pub fn negotiate_surface_usage(
    advertised: wgpu::TextureUsages,
) -> Result<SurfaceUsagePlan, GpuInitError> {
    if !advertised.contains(wgpu::TextureUsages::RENDER_ATTACHMENT) {
        return Err(GpuInitError::SurfaceNoRenderAttachment {
            advertised: format!("{advertised:?}"),
        });
    }
    let capture_copy_src = advertised.contains(wgpu::TextureUsages::COPY_SRC);
    let usage = if capture_copy_src {
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC
    } else {
        wgpu::TextureUsages::RENDER_ATTACHMENT
    };
    Ok(SurfaceUsagePlan {
        usage,
        capture_copy_src,
    })
}

/// Deliberate GPU adapter selection policy. See `docs/INSTALL.md` § GPU adapter policy.
///
/// **Never** silently select software or LowPower when the preferred adapter is missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterSelectPolicy {
    /// Default: HighPerformance, `force_fallback_adapter = false`. Fail closed if absent.
    PreferHighPerformance,
    /// Explicit software/fallback adapter (`CAER_GPU_FORCE_FALLBACK=1`).
    ForceFallback,
    /// Try HighPerformance first; if that fails, retry LowPower once and report the step-down
    /// (`CAER_GPU_ALLOW_LOW_POWER=1`). Still never force software unless ForceFallback.
    PreferHighPerformanceAllowLowPower,
}

impl fmt::Display for AdapterSelectPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PreferHighPerformance => write!(f, "PreferHighPerformance"),
            Self::ForceFallback => write!(f, "ForceFallback"),
            Self::PreferHighPerformanceAllowLowPower => {
                write!(f, "PreferHighPerformanceAllowLowPower")
            }
        }
    }
}

impl AdapterSelectPolicy {
    /// Resolve policy from environment. Defaults to PreferHighPerformance (fail closed).
    #[must_use]
    pub fn from_env() -> Self {
        if env_flag("CAER_GPU_FORCE_FALLBACK") {
            return Self::ForceFallback;
        }
        if env_flag("CAER_GPU_ALLOW_LOW_POWER") {
            return Self::PreferHighPerformanceAllowLowPower;
        }
        Self::PreferHighPerformance
    }

    #[must_use]
    pub fn remediation(self) -> &'static str {
        match self {
            Self::PreferHighPerformance => {
                "Install/update GPU drivers, or set CAER_GPU_ALLOW_LOW_POWER=1 to permit a \
                 deliberate LowPower retry, or CAER_GPU_FORCE_FALLBACK=1 for software (slow)."
            }
            Self::ForceFallback => {
                "CAER_GPU_FORCE_FALLBACK=1 is set but no fallback adapter exists. Unset the \
                 variable to use a real GPU, or fix the wgpu fallback backend install."
            }
            Self::PreferHighPerformanceAllowLowPower => {
                "Neither HighPerformance nor LowPower adapters were available. Check drivers \
                 or set CAER_GPU_FORCE_FALLBACK=1 only if you deliberately want software."
            }
        }
    }
}

/// One concrete `request_adapter` attempt derived from policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterRequestPlan {
    pub power_preference: wgpu::PowerPreference,
    pub force_fallback_adapter: bool,
    /// True when this attempt is the LowPower step-down (must be logged, never silent).
    pub is_low_power_fallback: bool,
}

/// Ordered adapter request attempts for a policy. First success wins; exhaustion → error.
#[must_use]
pub fn adapter_request_plans(policy: AdapterSelectPolicy) -> Vec<AdapterRequestPlan> {
    match policy {
        AdapterSelectPolicy::PreferHighPerformance => vec![AdapterRequestPlan {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            is_low_power_fallback: false,
        }],
        AdapterSelectPolicy::ForceFallback => vec![AdapterRequestPlan {
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: true,
            is_low_power_fallback: false,
        }],
        AdapterSelectPolicy::PreferHighPerformanceAllowLowPower => vec![
            AdapterRequestPlan {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                is_low_power_fallback: false,
            },
            AdapterRequestPlan {
                power_preference: wgpu::PowerPreference::LowPower,
                force_fallback_adapter: false,
                is_low_power_fallback: true,
            },
        ],
    }
}

fn env_flag(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Subset of limits CAER requires for product rendering. Compared via `Limits::check_limits`.
#[must_use]
pub fn caer_required_limits() -> wgpu::Limits {
    // Floor below wgpu::Limits::default() for a few fields so downlevel GPUs can still run,
    // but still fail closed on absurdly low caps (capability-matrix "unsupported limits").
    wgpu::Limits {
        max_texture_dimension_2d: 2048,
        max_buffer_size: 1 << 26, // 64 MiB
        max_bind_groups: 4,
        ..wgpu::Limits::downlevel_defaults()
    }
}

/// Return Err when `available` cannot satisfy `required` (discriminating limits check).
pub fn check_adapter_limits(
    required: &wgpu::Limits,
    available: &wgpu::Limits,
) -> Result<(), GpuInitError> {
    if required.check_limits(available) {
        Ok(())
    } else {
        let mut fails = Vec::new();
        required.check_limits_with_fail_fn(available, false, |name, required_v, allowed_v| {
            fails.push(format!(
                "{name}: need {required_v}, adapter has {allowed_v}"
            ));
        });
        Err(GpuInitError::LimitsUnsupported {
            detail: fails.join("; "),
        })
    }
}

/// Outcome of an adapter selection pass (for tests and logging).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterSelectionOutcome {
    Selected {
        plan: AdapterRequestPlan,
    },
    Unavailable {
        policy: AdapterSelectPolicy,
        attempts: usize,
    },
}

/// Pure selection over a sequence of attempt success/failure flags (capability-matrix tests).
///
/// `attempt_ok[i]` mirrors whether `request_adapter(plans[i])` succeeded. Never invents a
/// success that was not offered.
#[must_use]
pub fn select_adapter_from_attempts(
    policy: AdapterSelectPolicy,
    attempt_ok: &[bool],
) -> AdapterSelectionOutcome {
    let plans = adapter_request_plans(policy);
    for (i, plan) in plans.iter().enumerate() {
        if attempt_ok.get(i).copied().unwrap_or(false) {
            return AdapterSelectionOutcome::Selected { plan: *plan };
        }
    }
    AdapterSelectionOutcome::Unavailable {
        policy,
        attempts: plans.len(),
    }
}

/// Serialize wgpu adapter+device init across threads **and** processes.
///
/// Concurrent `request_adapter`/`request_device` on this host SIGSEGVs `hud_golden` when
/// `cargo test --workspace -j N` runs multiple GPU binaries at once. The OS lock lives in
/// [`crate::init_lock`] (Unix `flock` / Windows `LockFileEx`): the kernel releases it if the
/// holder dies. There is no mkdir steal path — failing to acquire is [`GpuInitError::InitLock`].
pub struct WgpuInitGuard {
    lock: Option<crate::init_lock::InitLockGuard>,
}

pub fn lock_wgpu_init() -> Result<WgpuInitGuard, GpuInitError> {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    let deadline = Instant::now() + Duration::from_secs(60);
    while wgpu_init_inproc().swap(true, Ordering::AcqRel) {
        if Instant::now() >= deadline {
            return Err(GpuInitError::InitLock {
                detail: "timed out after 60s waiting for in-process wgpu init flag \
                         (possible leaked WgpuInitGuard)"
                    .into(),
            });
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let path = std::env::temp_dir().join("caer-wgpu-init.lock");
    match crate::init_lock::lock_exclusive(&path, Duration::from_secs(60)) {
        Ok(lock) => Ok(WgpuInitGuard { lock: Some(lock) }),
        Err(e) => {
            lock_wgpu_init_release_inproc();
            Err(GpuInitError::InitLock {
                detail: e.to_string(),
            })
        }
    }
}

impl Drop for WgpuInitGuard {
    fn drop(&mut self) {
        // OS unlock first (kernel-owned), then in-process flag — same order as the old flock path.
        drop(self.lock.take());
        lock_wgpu_init_release_inproc();
    }
}

fn lock_wgpu_init_release_inproc() {
    use std::sync::atomic::Ordering;
    wgpu_init_inproc().store(false, Ordering::Release);
}

fn wgpu_init_inproc() -> &'static std::sync::atomic::AtomicBool {
    use std::sync::atomic::AtomicBool;
    static INPROC: AtomicBool = AtomicBool::new(false);
    &INPROC
}

/// Host-wide GPU device lock path. Independent of repo and `$CARGO_TARGET_DIR`.
/// Override with `$CAER_GPU_TEST_LOCK` when tests need an isolated file.
#[must_use]
pub fn gpu_device_lock_path() -> PathBuf {
    if let Ok(p) = std::env::var("CAER_GPU_TEST_LOCK") {
        let p = p.trim();
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    std::env::temp_dir().join("caer-gpu-device.lock")
}

/// Test/gate contract: when set, [`crate::gpu::Gpu`] constructors hold the host-wide device
/// lock for the Gpu lifetime. Production (unset) never takes this lock — only short
/// [`lock_wgpu_init`] during adapter/device creation — so dual-client / PLAYER_SCENARIO stays possible.
///
/// Pure parser — tests must call this with injected values instead of mutating process env
/// under parallel `--test-threads`.
#[must_use]
pub fn parse_gpu_test_serialize(raw: Option<&str>) -> bool {
    match raw {
        Some(v) => {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("on")
        }
        None => false,
    }
}

/// Live env read for product/gates. Prefer [`parse_gpu_test_serialize`] in unit tests.
#[must_use]
pub fn gpu_test_serialize_enabled() -> bool {
    parse_gpu_test_serialize(std::env::var("CAER_GPU_TEST_SERIALIZE").ok().as_deref())
}

/// What a [`GpuDeviceLock`] guard actually holds, and therefore what its `Drop` must release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeviceLockKind {
    /// Holds the in-process flag and the OS lock; releases both and pops this thread's depth.
    Owner,
    /// A nested acquisition on a thread that already owns the lock. Holds nothing; pops depth.
    Nested,
    /// A non-blocking OS-level acquisition. Never touched the in-process flag or the depth, so it
    /// must not clear them — doing so used to hand another thread's ownership away.
    OsOnly,
}

/// Exclusive lock for **test-side** GPU device lifetime serialization across processes/threads.
/// Distinct from [`lock_wgpu_init`], which only covers adapter/device *creation*.
pub struct GpuDeviceLock {
    lock: Option<crate::init_lock::InitLockGuard>,
    kind: DeviceLockKind,
}

thread_local! {
    /// How many `GpuDeviceLock`s this thread currently holds.
    ///
    /// The lock serializes GPU device lifetime **across threads and processes**; a single thread
    /// holding two devices at once is not what it guards against. Without this counter the second
    /// `Gpu::new_headless` on one thread spins on a flag that same thread already set, and the
    /// wait is unwinnable — it ends in the 120s timeout reporting "possible leaked
    /// GpuDeviceLock", which is exactly wrong: nothing leaked, the caller deadlocked itself.
    ///
    /// Not hypothetical. `other_player_avatars`'
    /// `swap_remote_race_gender_or_equipment_changes_remote_pixels_control_unchanged` builds a
    /// second device while the first is still in scope, and failed **deterministically** at 120s
    /// under `CAER_GPU_TEST_SERIALIZE=1` — which `scripts/gates.sh` always sets. It passed
    /// locally only because a bare `cargo test` leaves that variable unset and takes no lock.
    static DEVICE_LOCK_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

fn device_lock_depth() -> u32 {
    DEVICE_LOCK_DEPTH.with(std::cell::Cell::get)
}

fn set_device_lock_depth(n: u32) {
    DEVICE_LOCK_DEPTH.with(|d| d.set(n));
}

/// Acquire the host-wide GPU device lock (bounded in-process wait + portable OS lock).
///
/// Re-entrant per thread: a nested acquisition returns immediately and releases nothing until the
/// outermost guard drops.
pub fn lock_gpu_device() -> Result<GpuDeviceLock, GpuInitError> {
    lock_gpu_device_with_timeout(std::time::Duration::from_secs(120))
}

/// Same as [`lock_gpu_device`] with an explicit in-process + OS wait bound (tests use a short value).
pub fn lock_gpu_device_with_timeout(
    timeout: std::time::Duration,
) -> Result<GpuDeviceLock, GpuInitError> {
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    // Already ours: hand back a guard that releases nothing. See `DEVICE_LOCK_DEPTH`.
    if device_lock_depth() > 0 {
        set_device_lock_depth(device_lock_depth() + 1);
        return Ok(GpuDeviceLock {
            lock: None,
            kind: DeviceLockKind::Nested,
        });
    }

    let deadline = Instant::now() + timeout;
    while gpu_device_inproc().swap(true, Ordering::AcqRel) {
        if Instant::now() >= deadline {
            return Err(GpuInitError::InitLock {
                detail: format!(
                    "timed out after {}s waiting for in-process GPU device lock flag \
                     (possible leaked GpuDeviceLock)",
                    timeout.as_secs().max(1)
                ),
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    match crate::init_lock::lock_exclusive(&gpu_device_lock_path(), timeout) {
        Ok(lock) => {
            set_device_lock_depth(1);
            Ok(GpuDeviceLock {
                lock: Some(lock),
                kind: DeviceLockKind::Owner,
            })
        }
        Err(e) => {
            gpu_device_inproc().store(false, Ordering::Release);
            Err(GpuInitError::InitLock {
                detail: e.to_string(),
            })
        }
    }
}

/// Non-blocking OS attempt at the host-wide device lock.
pub fn try_lock_gpu_device() -> Result<Option<GpuDeviceLock>, GpuInitError> {
    match crate::init_lock::try_exclusive(&gpu_device_lock_path()) {
        Ok(Some(lock)) => Ok(Some(GpuDeviceLock {
            lock: Some(lock),
            kind: DeviceLockKind::OsOnly,
        })),
        Ok(None) => Ok(None),
        Err(e) => Err(GpuInitError::InitLock {
            detail: e.to_string(),
        }),
    }
}

/// Decision helper: acquire the test device lock when `serialize` is true. Injected for tests.
pub fn maybe_lock_gpu_device_if(serialize: bool) -> Result<Option<GpuDeviceLock>, GpuInitError> {
    if serialize {
        Ok(Some(lock_gpu_device()?))
    } else {
        Ok(None)
    }
}

/// Acquire the test device lock only when [`gpu_test_serialize_enabled`]. Product paths pass `None`.
pub fn maybe_lock_gpu_device_for_test() -> Result<Option<GpuDeviceLock>, GpuInitError> {
    maybe_lock_gpu_device_if(gpu_test_serialize_enabled())
}

impl Drop for GpuDeviceLock {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;
        match self.kind {
            DeviceLockKind::Nested => {
                set_device_lock_depth(device_lock_depth().saturating_sub(1));
            }
            DeviceLockKind::Owner => {
                set_device_lock_depth(device_lock_depth().saturating_sub(1));
                drop(self.lock.take());
                gpu_device_inproc().store(false, Ordering::Release);
            }
            // Never took the in-process flag, so it must not release it.
            DeviceLockKind::OsOnly => drop(self.lock.take()),
        }
    }
}

fn gpu_device_inproc() -> &'static std::sync::atomic::AtomicBool {
    use std::sync::atomic::AtomicBool;
    static INPROC: AtomicBool = AtomicBool::new(false);
    &INPROC
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Falsifier: surface without COPY_SRC still yields a renderable usage plan.
    #[test]
    fn negotiate_no_copy_src_still_renders() {
        let plan = negotiate_surface_usage(wgpu::TextureUsages::RENDER_ATTACHMENT)
            .expect("RENDER_ATTACHMENT alone must succeed");
        assert_eq!(plan.usage, wgpu::TextureUsages::RENDER_ATTACHMENT);
        assert!(
            !plan.capture_copy_src,
            "capture must be reported false without COPY_SRC"
        );
        assert!(
            !plan.usage.contains(wgpu::TextureUsages::COPY_SRC),
            "must not request COPY_SRC when unadvertised"
        );
    }

    /// Falsifier: COPY_SRC present → capture capability reported true and usage includes it.
    #[test]
    fn negotiate_with_copy_src_enables_capture() {
        let advertised = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC;
        let plan = negotiate_surface_usage(advertised).unwrap();
        assert!(plan.capture_copy_src);
        assert!(plan.usage.contains(wgpu::TextureUsages::COPY_SRC));
        assert!(plan.usage.contains(wgpu::TextureUsages::RENDER_ATTACHMENT));
    }

    /// Falsifier: no RENDER_ATTACHMENT → typed error (not a silent empty config).
    #[test]
    fn negotiate_without_render_attachment_errors() {
        let err = negotiate_surface_usage(wgpu::TextureUsages::COPY_SRC).unwrap_err();
        assert!(
            matches!(err, GpuInitError::SurfaceNoRenderAttachment { .. }),
            "got {err}"
        );
    }

    /// Capture-disabled rendering: plan is render-only; callers must not treat capture as required.
    #[test]
    fn capture_disabled_plan_is_render_only() {
        let plan = negotiate_surface_usage(wgpu::TextureUsages::RENDER_ATTACHMENT).unwrap();
        assert!(!plan.capture_copy_src);
        // Product path: present uses RENDER_ATTACHMENT only — capture is optional side channel.
        assert_eq!(
            plan.usage & wgpu::TextureUsages::COPY_SRC,
            wgpu::TextureUsages::empty()
        );
    }

    /// Falsifier: lock_wgpu_init fails closed (Result) and never returns an unlocked guard.
    #[test]
    fn wgpu_init_lock_is_fail_closed_result() {
        let g = lock_wgpu_init().expect("uncontended init lock must succeed");
        drop(g);
        let g2 = lock_wgpu_init().expect("lock must be reusable after Drop unlocks");
        drop(g2);
    }

    /// Falsifier: PreferHighPerformance does not invent a LowPower/software success.
    #[test]
    fn prefer_high_performance_fails_closed_on_absence() {
        let outcome =
            select_adapter_from_attempts(AdapterSelectPolicy::PreferHighPerformance, &[false]);
        assert_eq!(
            outcome,
            AdapterSelectionOutcome::Unavailable {
                policy: AdapterSelectPolicy::PreferHighPerformance,
                attempts: 1,
            }
        );
        // Empty attempts (adapter absence) likewise unavailable.
        let absent = select_adapter_from_attempts(AdapterSelectPolicy::PreferHighPerformance, &[]);
        assert!(matches!(
            absent,
            AdapterSelectionOutcome::Unavailable { attempts: 1, .. }
        ));
    }

    /// Falsifier: LowPower step-down only when policy explicitly allows it; marked as fallback.
    #[test]
    fn allow_low_power_selects_second_attempt_deliberately() {
        let outcome = select_adapter_from_attempts(
            AdapterSelectPolicy::PreferHighPerformanceAllowLowPower,
            &[false, true],
        );
        match outcome {
            AdapterSelectionOutcome::Selected { plan } => {
                assert!(plan.is_low_power_fallback);
                assert_eq!(plan.power_preference, wgpu::PowerPreference::LowPower);
                assert!(!plan.force_fallback_adapter);
            }
            other => panic!("expected Selected low-power fallback, got {other:?}"),
        }
    }

    /// Falsifier: default policy plans never set force_fallback_adapter.
    #[test]
    fn default_policy_never_force_fallback() {
        for plan in adapter_request_plans(AdapterSelectPolicy::PreferHighPerformance) {
            assert!(!plan.force_fallback_adapter);
            assert!(!plan.is_low_power_fallback);
        }
    }

    /// ForceFallback is the only policy that requests wgpu software fallback.
    #[test]
    fn force_fallback_policy_is_explicit() {
        let plans = adapter_request_plans(AdapterSelectPolicy::ForceFallback);
        assert_eq!(plans.len(), 1);
        assert!(plans[0].force_fallback_adapter);
    }

    /// Low/unsupported limits → LimitsUnsupported (capability matrix).
    #[test]
    fn unsupported_limits_are_rejected() {
        let required = caer_required_limits();
        let mut low = required.clone();
        low.max_texture_dimension_2d = 256; // below CAER floor
        let err = check_adapter_limits(&required, &low).unwrap_err();
        assert!(
            matches!(err, GpuInitError::LimitsUnsupported { .. }),
            "got {err}"
        );
        assert!(
            err.to_string().contains("max_texture_dimension_2d"),
            "detail must name the failing limit: {err}"
        );
    }

    /// Adequate limits pass.
    #[test]
    fn adequate_limits_ok() {
        let required = caer_required_limits();
        let available = wgpu::Limits::default();
        check_adapter_limits(&required, &available)
            .expect("default limits must satisfy CAER floor");
    }

    /// Display text is actionable (product error contract).
    #[test]
    fn adapter_unavailable_display_names_remediation() {
        let err = GpuInitError::AdapterUnavailable {
            policy: AdapterSelectPolicy::PreferHighPerformance,
            detail: "RequestAdapterError::NotFound".into(),
            remediation: AdapterSelectPolicy::PreferHighPerformance.remediation(),
        };
        let s = err.to_string();
        assert!(s.contains("CAER_GPU_ALLOW_LOW_POWER") || s.contains("CAER_GPU_FORCE_FALLBACK"));
        assert!(s.contains("PreferHighPerformance"));
    }

    /// Falsifier B2: lock path is host-wide, not `$CARGO_TARGET_DIR`.
    #[test]
    fn gpu_device_lock_path_ignores_cargo_target_dir() {
        let prev_lock = std::env::var_os("CAER_GPU_TEST_LOCK");
        let prev_td = std::env::var_os("CARGO_TARGET_DIR");
        std::env::remove_var("CAER_GPU_TEST_LOCK");
        std::env::set_var("CARGO_TARGET_DIR", "/tmp/caer-isolated-agent-target");
        let path = gpu_device_lock_path();
        match prev_lock {
            Some(v) => std::env::set_var("CAER_GPU_TEST_LOCK", v),
            None => std::env::remove_var("CAER_GPU_TEST_LOCK"),
        }
        match prev_td {
            Some(v) => std::env::set_var("CARGO_TARGET_DIR", v),
            None => std::env::remove_var("CARGO_TARGET_DIR"),
        }
        let s = path.to_string_lossy();
        assert!(
            !s.contains("caer-isolated-agent-target"),
            "device lock must not live under CARGO_TARGET_DIR, got {s}"
        );
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("caer-gpu-device.lock")
        );
    }

    /// Falsifier B2: process B with a different `CARGO_TARGET_DIR` cannot enter allocation
    /// while process A holds the host-wide device lock.
    #[test]
    fn gpu_device_lock_excludes_other_target_dir_process() {
        if std::env::var("CAER_GPU_LOCK_CHILD").ok().as_deref() == Some("1") {
            match try_lock_gpu_device() {
                Ok(None) => std::process::exit(0),
                Ok(Some(_)) => std::process::exit(2),
                Err(_) => std::process::exit(3),
            }
        }
        let _held = lock_gpu_device().expect("parent must acquire host-wide device lock");
        let exe = std::env::current_exe().expect("test exe");
        let status = std::process::Command::new(exe)
            .arg("--exact")
            .arg("gpu_init::tests::gpu_device_lock_excludes_other_target_dir_process")
            .env("CAER_GPU_LOCK_CHILD", "1")
            .env(
                "CARGO_TARGET_DIR",
                "/tmp/caer-other-target-dir-does-not-matter",
            )
            .env("RUST_TEST_THREADS", "1")
            .status()
            .expect("spawn child");
        assert!(
            status.success(),
            "child with a different CARGO_TARGET_DIR must not acquire while parent holds \
             (exit 2 = acquired, 3 = lock error): {status}"
        );
    }

    /// Falsifier B2: product default serialize decision is false without mutating process env.
    #[test]
    fn product_gpu_does_not_hold_device_lock() {
        assert!(
            !parse_gpu_test_serialize(None),
            "unset env must mean product mode"
        );
        assert!(!parse_gpu_test_serialize(Some("")));
        assert!(!parse_gpu_test_serialize(Some("0")));
        assert!(!parse_gpu_test_serialize(Some("false")));
        assert!(parse_gpu_test_serialize(Some("1")));
        assert!(parse_gpu_test_serialize(Some("true")));
        assert!(parse_gpu_test_serialize(Some("ON")));
        assert!(
            maybe_lock_gpu_device_if(false).expect("decision").is_none(),
            "product decision must not acquire the host-wide device lock"
        );
        let held = maybe_lock_gpu_device_if(true).expect("test decision must lock");
        assert!(held.is_some());
        drop(held);
        // Live env may be set by gates.sh — must not clear it. The live reader must agree
        // with the pure parser on whatever value is currently present.
        let live = std::env::var("CAER_GPU_TEST_SERIALIZE").ok();
        assert_eq!(
            gpu_test_serialize_enabled(),
            parse_gpu_test_serialize(live.as_deref()),
            "live reader must match pure parser without env mutation"
        );
    }

    /// Falsifier B2: two concurrently scheduled constructors cannot bypass serialization.
    #[test]
    fn concurrent_gpu_device_locks_serialize() {
        use std::sync::{Arc, Barrier};
        use std::time::Duration;

        let barrier = Arc::new(Barrier::new(2));
        let held = lock_gpu_device().expect("first constructor acquires");
        let b = barrier.clone();
        let child = std::thread::spawn(move || {
            b.wait();
            // Second concurrent acquire must not succeed while the first still holds.
            lock_gpu_device_with_timeout(Duration::from_millis(100))
        });
        barrier.wait();
        let second = child.join().expect("join");
        assert!(
            second.is_err(),
            "second concurrent GPU device lock must fail closed while first is held"
        );
        drop(held);
        let after = lock_gpu_device_with_timeout(Duration::from_secs(2)).expect("reacquire");
        drop(after);
    }

    /// Falsifier B2 residual: a *contended* device lock wait is bounded (no infinite AtomicBool
    /// spin).
    ///
    /// The contender has to be another thread. This used to acquire twice on the calling thread
    /// and assert the second attempt failed — asserting the self-deadlock as the requirement. That
    /// is the deadlock `swap_remote_race_gender_or_equipment_changes_remote_pixels_control_unchanged`
    /// hit for real, one `Gpu` per line, and this test stood behind it saying the behaviour was
    /// intended.
    #[test]
    fn gpu_device_contended_wait_is_bounded() {
        use std::sync::{Arc, Barrier};

        let held = lock_gpu_device().expect("hold device lock");
        let barrier = Arc::new(Barrier::new(2));
        let b = barrier.clone();
        let child = std::thread::spawn(move || {
            b.wait();
            let start = std::time::Instant::now();
            let r = lock_gpu_device_with_timeout(std::time::Duration::from_millis(80));
            (r, start.elapsed())
        });
        barrier.wait();
        let (result, waited) = child.join().expect("join");

        let err = match result {
            Ok(_) => panic!("another thread must not acquire while the lock is held"),
            Err(e) => e,
        };
        assert!(
            matches!(err, GpuInitError::InitLock { .. }),
            "must be InitLock, got {err}"
        );
        assert!(
            waited < std::time::Duration::from_secs(2),
            "short timeout must return promptly, waited {waited:?}"
        );
        let s = err.to_string();
        assert!(s.contains("timed out"), "actionable timeout, got {s}");
        drop(held);
    }

    /// One thread building two GPU devices must not wait on itself.
    ///
    /// The lock serializes device lifetime across threads and processes; nesting on a single thread
    /// is not what it guards against. A short timeout is the discriminator — before the depth
    /// counter this call burned the whole timeout and returned `InitLock`, so a passing run here
    /// cannot be the old behaviour.
    #[test]
    fn nested_gpu_device_lock_on_one_thread_does_not_self_deadlock() {
        let outer = lock_gpu_device().expect("first acquire");
        let start = std::time::Instant::now();
        let inner = lock_gpu_device_with_timeout(std::time::Duration::from_millis(50))
            .expect("nested acquire on the same thread must succeed immediately");
        assert!(
            start.elapsed() < std::time::Duration::from_millis(50),
            "nested acquire waited {:?} — it should not wait at all",
            start.elapsed()
        );

        // Dropping the inner guard must not release the lock the outer one still owns.
        drop(inner);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ =
                tx.send(lock_gpu_device_with_timeout(std::time::Duration::from_millis(80)).is_ok());
        });
        assert!(
            !rx.recv().expect("child reported"),
            "inner guard's Drop released a lock the outer guard still holds"
        );

        drop(outer);
        let after = lock_gpu_device_with_timeout(std::time::Duration::from_secs(2))
            .expect("outermost Drop must release");
        drop(after);
    }
}
