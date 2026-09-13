//! Golden-image tests for terrain rendering (Foundation Audit step 3, the small half).
//!
//! The structural floor in `render_floor.rs` asserts invariants — indices in range, geometry
//! finite, poses applied. It deliberately cannot catch "the terrain still draws, but wrong":
//! a broken texture bind, an inverted normal, a shader regression, a seam that reopens. For
//! terrain specifically, *looking right* genuinely is the property, so a couple of goldens earn
//! their keep here even though they'd be the wrong tool for the whole crate.
//!
//! Kept deliberately cheap and few: small frames, fixed camera, no entities (an empty mob dump),
//! compared by **mean absolute pixel difference** rather than exact equality so GPU/driver dither
//! doesn't cause false failures. A real regression moves the mean far past the threshold; a driver
//! difference does not.
//!
//! Refresh after an intentional visual change:
//!
//! ```bash
//! CAER_UPDATE_GOLDENS=1 cargo test -p caer-render --test terrain_golden
//! ```
//!
//! …then **look at the new PNGs before committing them**. A golden refreshed without being viewed
//! is worse than no golden: it launders a regression into the baseline.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

const DEFAULT_RENDER_TIMEOUT_SECS: u64 = 120;
const TERMINATION_GRACE: Duration = Duration::from_secs(2);
static RENDER_PROCESS_LOCK: Mutex<()> = Mutex::new(());

/// Where the baselines live (committed; a few KB each at this size).
fn goldens_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goldens")
}

/// The client install root, or `None` when the game isn't present (→ skip).
fn client_present() -> bool {
    caer_render::terrain::client_root().join("zones").is_dir()
        || caer_render::terrain::client_root().join("figures").is_dir()
}

/// Decode a PNG to raw RGBA plus its dimensions.
fn decode(bytes: &[u8]) -> (Vec<u8>, u32, u32) {
    let dec = png::Decoder::new(bytes);
    let mut reader = dec.read_info().expect("png header");
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("png data");
    buf.truncate(info.buffer_size());
    (buf, info.width, info.height)
}

/// Comparison metrics between two same-sized RGBA buffers: `(changed_pixel_fraction, mean_diff)`.
///
/// **`changed` is the metric that decides**, and that is a measured choice, not a guess. Mean
/// absolute difference over the whole frame turned out to be far too blunt: disabling seam
/// blending — a genuine render change — moved the mean only 0.229, which any tolerance loose
/// enough to survive driver dither would have swallowed. It dilutes localized regressions
/// (seams, one wrong texture, a single broken model) across every unaffected pixel.
///
/// Counting pixels that changed *appreciably* discriminates properly. Measured on this box:
///
/// | case | mean | changed |
/// |---|---|---|
/// | identical rerun | 0.000 | **0.00 %** |
/// | `CAER_SEAM_BLEND=0` | 0.229 | **0.93 %** |
///
/// The render is bit-deterministic run to run, so the noise floor is zero and the threshold below
/// carries a wide margin on both sides.
fn compare(a: &[u8], b: &[u8]) -> (f64, f64) {
    assert_eq!(a.len(), b.len(), "buffers differ in size");
    let mean = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| x.abs_diff(*y) as u64)
        .sum::<u64>() as f64
        / a.len() as f64;
    let px = a.len() / 4;
    let changed = (0..px)
        .filter(|i| (0..4).any(|c| a[i * 4 + c].abs_diff(b[i * 4 + c]) > 8))
        .count();
    (changed as f64 / px as f64, mean)
}

/// Fraction of appreciably-changed pixels tolerated. Observed noise floor is 0.00 % (deterministic
/// render); the smallest real change measured is 0.93 %. 0.10 % sits ~9× below that while leaving
/// headroom for a different GPU/driver, since goldens are committed and may run elsewhere.
const MAX_CHANGED_FRACTION: f64 = 0.001;

/// Render one deterministic frame via the real `caer-render` binary and compare against a golden.
///
/// Shelling out (rather than calling into the library) is intentional: it exercises the *actual*
/// headless path the agent debug loop uses, so if `--screenshot` breaks, these fail.
fn assert_golden(name: &str, cam: &str, region: &str) {
    if !client_present() {
        return; // no proprietary assets on this machine — skip
    }
    let out = std::env::temp_dir().join(format!("caer_golden_{name}.png"));
    // An empty mob dump keeps the frame pure terrain: entity spawns would make it depend on
    // creature-mesh work that the structural tests already cover.
    let empty = std::env::temp_dir().join("caer_golden_empty.tsv");
    std::fs::write(&empty, "").expect("write empty mob dump");

    let timeout = std::env::var("CAER_GOLDEN_RENDER_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|seconds| *seconds > 0)
        .unwrap_or(DEFAULT_RENDER_TIMEOUT_SECS);
    let mut command = Command::new(env!("CARGO_BIN_EXE_caer-render"));
    command
        .arg(&empty)
        .args(["--region", region])
        .args(["--cam", cam])
        .args(["--size", "320x200"])
        .arg("--screenshot")
        .arg(&out)
        .env("CAER_LOG", "warn");
    let status = run_bounded(&mut command, Duration::from_secs(timeout)).unwrap_or_else(|error| {
        panic!(
            "{name}: {error}; camera={cam}, region={region}, output={}, timeout={timeout}s",
            out.display()
        )
    });
    assert!(status.success(), "{name}: renderer exited with {status}");

    let actual = std::fs::read(&out).expect("read rendered frame");
    let golden_path = goldens_dir().join(format!("{name}.png"));

    if std::env::var_os("CAER_UPDATE_GOLDENS").is_some() || !golden_path.exists() {
        std::fs::create_dir_all(goldens_dir()).ok();
        std::fs::write(&golden_path, &actual).expect("write golden");
        eprintln!(
            "golden {name} written to {} — REVIEW IT before committing",
            golden_path.display()
        );
        return;
    }

    let (a, aw, ah) = decode(&actual);
    let (g, gw, gh) = decode(&std::fs::read(&golden_path).expect("read golden"));
    assert_eq!((aw, ah), (gw, gh), "{name}: frame size changed");

    let (changed, mean) = compare(&a, &g);
    assert!(
        changed < MAX_CHANGED_FRACTION,
        "{name}: {:.2}% of pixels changed (mean diff {mean:.3}) — terrain render regressed.\n\
         Rendered frame kept at {} for inspection.\n\
         If intentional: CAER_UPDATE_GOLDENS=1 cargo test -p caer-render --test terrain_golden, then LOOK at the PNG.",
        changed * 100.0,
        out.display(),
    );
}

/// Run the real renderer without allowing a mounted-filesystem or driver stall to hold Cargo
/// forever. The process owns a fresh group so a helper it starts cannot outlive the failed gate.
fn run_bounded(command: &mut Command, timeout: Duration) -> Result<ExitStatus, String> {
    let _exclusive = RENDER_PROCESS_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    configure_descendant_adoption()?;
    #[cfg(target_os = "linux")]
    let adopted_baseline = direct_child_identities(std::process::id() as i32);
    configure_process_group(command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("renderer spawn failed: {error}"))?;
    let group = child.id() as i32;
    let mut descendants = DescendantTracker::new(
        group,
        #[cfg(target_os = "linux")]
        adopted_baseline,
    );
    let started = Instant::now();
    let mut next_progress = Instant::now() + Duration::from_secs(10);
    loop {
        descendants.refresh();
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("renderer poll failed: {error}"))?
        {
            if process_group_alive(group) || descendants.any_alive() {
                terminate_group(&mut child, group, &mut descendants);
                return Err(format!(
                    "renderer exited with {status}, but a descendant survived"
                ));
            }
            return Ok(status);
        }
        if started.elapsed() >= timeout {
            eprintln!(
                "terrain_golden: renderer pid={} made no terminal screenshot after {:.1}s; terminating process group",
                child.id(),
                started.elapsed().as_secs_f64()
            );
            terminate_group(&mut child, group, &mut descendants);
            descendants.refresh();
            descendants.reap_adopted();
            descendants.refresh();
            if process_group_alive(group) || descendants.any_alive() {
                return Err(format!(
                    "TIMEOUT: renderer process group {group} survived TERM and KILL after {:.1}s",
                    started.elapsed().as_secs_f64()
                ));
            }
            return Err(format!(
                "TIMEOUT: renderer was terminated after {:.1}s without producing a terminal result",
                started.elapsed().as_secs_f64()
            ));
        }
        if Instant::now() >= next_progress {
            eprintln!(
                "terrain_golden: waiting for renderer pid={} ({:.1}s elapsed, {:.1}s deadline)",
                child.id(),
                started.elapsed().as_secs_f64(),
                timeout.as_secs_f64()
            );
            next_progress += Duration::from_secs(10);
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

#[cfg(target_os = "linux")]
fn configure_descendant_adoption() -> Result<(), String> {
    let result = unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) };
    if result == 0 {
        Ok(())
    } else {
        Err(format!(
            "enable renderer descendant adoption: {}",
            std::io::Error::last_os_error()
        ))
    }
}

#[cfg(not(target_os = "linux"))]
fn configure_descendant_adoption() -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn process_group_alive(group: i32) -> bool {
    let result = unsafe { libc::kill(-group, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_group_alive(_group: i32) -> bool {
    false
}

#[derive(Default)]
struct DescendantTracker {
    #[cfg(target_os = "linux")]
    known: BTreeMap<i32, u64>,
    #[cfg(target_os = "linux")]
    root: i32,
    #[cfg(target_os = "linux")]
    adopter: i32,
    #[cfg(target_os = "linux")]
    adopted_baseline: BTreeMap<i32, u64>,
}

impl DescendantTracker {
    fn new(root: i32, #[cfg(target_os = "linux")] adopted_baseline: BTreeMap<i32, u64>) -> Self {
        let mut tracker = Self {
            #[cfg(target_os = "linux")]
            root,
            #[cfg(target_os = "linux")]
            adopter: std::process::id() as i32,
            #[cfg(target_os = "linux")]
            adopted_baseline,
            ..Self::default()
        };
        #[cfg(target_os = "linux")]
        if let Some(process) = linux_process(root) {
            tracker.known.insert(root, process.start_time);
        }
        tracker.refresh();
        tracker
    }

    fn refresh(&mut self) {
        #[cfg(target_os = "linux")]
        {
            let processes = linux_processes();
            loop {
                let additions = processes
                    .values()
                    .filter(|process| {
                        (self.known.contains_key(&process.parent)
                            || (process.parent == self.adopter
                                && process.pid != self.root
                                && self.adopted_baseline.get(&process.pid)
                                    != Some(&process.start_time)))
                            && !self.known.contains_key(&process.pid)
                    })
                    .map(|process| (process.pid, process.start_time))
                    .collect::<Vec<_>>();
                if additions.is_empty() {
                    break;
                }
                self.known.extend(additions);
            }
        }
    }

    fn signal(&self, signal: i32) {
        #[cfg(target_os = "linux")]
        for (&pid, &start_time) in self.known.iter().rev() {
            if linux_process(pid)
                .is_some_and(|process| process.start_time == start_time && process.state != 'Z')
            {
                unsafe {
                    libc::kill(pid, signal);
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = signal;
    }

    fn any_alive(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            return self.known.iter().any(|(&pid, &start_time)| {
                linux_process(pid)
                    .is_some_and(|process| process.start_time == start_time && process.state != 'Z')
            });
        }
        #[cfg(not(target_os = "linux"))]
        false
    }

    fn reap_adopted(&self) {
        #[cfg(target_os = "linux")]
        for &pid in self.known.keys() {
            if pid != self.root {
                unsafe {
                    libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG);
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct LinuxProcess {
    pid: i32,
    parent: i32,
    state: char,
    start_time: u64,
}

#[cfg(target_os = "linux")]
fn linux_process(pid: i32) -> Option<LinuxProcess> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let suffix = stat.get(stat.rfind(')')? + 1..)?.trim();
    let fields = suffix.split_whitespace().collect::<Vec<_>>();
    Some(LinuxProcess {
        pid,
        state: fields.first()?.chars().next()?,
        parent: fields.get(1)?.parse().ok()?,
        start_time: fields.get(19)?.parse().ok()?,
    })
}

#[cfg(target_os = "linux")]
fn linux_processes() -> BTreeMap<i32, LinuxProcess> {
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok())
        .filter_map(linux_process)
        .map(|process| (process.pid, process))
        .collect()
}

#[cfg(target_os = "linux")]
fn direct_child_identities(parent: i32) -> BTreeMap<i32, u64> {
    linux_processes()
        .into_values()
        .filter(|process| process.parent == parent)
        .map(|process| (process.pid, process.start_time))
        .collect()
}

fn terminate_group(child: &mut Child, group: i32, descendants: &mut DescendantTracker) {
    descendants.refresh();
    descendants.signal(libc::SIGTERM);
    #[cfg(unix)]
    unsafe {
        libc::kill(-group, libc::SIGTERM);
    }
    #[cfg(not(unix))]
    let _ = child.kill();
    let term_deadline = Instant::now() + TERMINATION_GRACE;
    while Instant::now() < term_deadline {
        let _ = child.try_wait();
        descendants.refresh();
        descendants.reap_adopted();
        if !process_group_alive(group) && !descendants.any_alive() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    #[cfg(unix)]
    unsafe {
        libc::kill(-group, libc::SIGKILL);
    }
    descendants.signal(libc::SIGKILL);
    #[cfg(not(unix))]
    let _ = child.kill();
    let kill_deadline = Instant::now() + TERMINATION_GRACE;
    while Instant::now() < kill_deadline {
        let _ = child.try_wait();
        descendants.refresh();
        descendants.reap_adopted();
        if !process_group_alive(group) && !descendants.any_alive() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn golden_camelot_hills_terrain() {
    // Ground-level view across the Camelot Hills test area — the vantage used throughout the
    // terrain and animation work, so a regression here is immediately recognisable.
    assert_golden("camelot_hills", "592250,537600,1810,90,-8", "1");
}

#[test]
fn golden_camelot_hills_topdown() {
    // A high plan view exercises a different path: wide culling, many zones, seam blending
    // between them. Catches seam/offset regressions the ground view can hide.
    assert_golden("camelot_hills_topdown", "592250,538000,40000,90,-88", "1");
}

#[cfg(unix)]
#[test]
fn bounded_renderer_control_reaches_real_success() {
    let mut command = Command::new("bash");
    command.args(["-c", "exit 0"]);
    let status = run_bounded(&mut command, Duration::from_secs(1)).unwrap();
    assert!(status.success());
}

#[cfg(unix)]
#[test]
fn bounded_renderer_control_times_out_and_reaps_ignored_term() {
    let mut command = Command::new("bash");
    command.args(["-c", "trap '' TERM; sleep 30"]);
    let error = run_bounded(&mut command, Duration::from_millis(100)).unwrap_err();
    assert!(error.contains("TIMEOUT"), "{error}");
}

#[cfg(target_os = "linux")]
#[test]
fn bounded_renderer_control_reaps_new_session_escape() {
    let path = std::env::temp_dir().join(format!(
        "caer_renderer_escape_{}_{}.pid",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut command = Command::new("bash");
    command.args([
        "-c",
        &format!(
            "setsid sh -c 'trap \"\" TERM; sleep 30' >/dev/null 2>&1 & echo $! > '{}'; exit 0",
            path.display()
        ),
    ]);
    let error = run_bounded(&mut command, Duration::from_secs(1)).unwrap_err();
    assert!(error.contains("descendant survived"), "{error}");
    let pid: i32 = std::fs::read_to_string(&path)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let alive = linux_process(pid).is_some_and(|process| process.state != 'Z');
    assert!(!alive, "new-session renderer descendant {pid} survived");
    std::fs::remove_file(path).unwrap();
}
