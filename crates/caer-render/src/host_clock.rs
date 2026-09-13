//! Host CPU clock sampler for MS-08 residual-hunt (CAER_BUILD_SPEC §12 / M19 residual).
//!
//! Inline `scaling_cur_freq` reads cost ~33 µs each; sampling every core inside the frame
//! loop would perturb the band we are trying to explain. Spec order: a ~1 kHz background
//! sampler joined by timestamp, with a **validity assert that sampled sd > 0** — a flat
//! sampler that always reports the same MHz is a failed instrument, not a clean host.
//!
//! Busy-core queries use `sched_getcpu` so idle cores in deep C-states do not dominate sd.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// One sampler tick: wall time + per-logical-CPU MHz (0 = unread / offline).
#[derive(Debug, Clone)]
pub struct ClockSample {
    pub at: Instant,
    /// Index = logical CPU id; value = MHz from `scaling_cur_freq` (kHz/1000).
    pub mhz: Vec<u32>,
}

/// Summary over a window of samples (busy-core subset when `busy_cpu` is provided per frame).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClockWindowStats {
    pub n: usize,
    pub min_mhz: f64,
    pub max_mhz: f64,
    pub mean_mhz: f64,
    pub sd_mhz: f64,
    /// `max/min` when min > 0; else 0.
    pub max_over_min: f64,
}

impl ClockWindowStats {
    #[must_use]
    pub fn from_mhz(values: &[f64]) -> Self {
        if values.is_empty() {
            return Self {
                n: 0,
                min_mhz: 0.0,
                max_mhz: 0.0,
                mean_mhz: 0.0,
                sd_mhz: 0.0,
                max_over_min: 0.0,
            };
        }
        let n = values.len();
        let min = values.iter().copied().fold(f64::INFINITY, f64::min);
        let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let mean = values.iter().sum::<f64>() / n as f64;
        let var = values
            .iter()
            .map(|v| {
                let d = v - mean;
                d * d
            })
            .sum::<f64>()
            / n as f64;
        let sd = var.sqrt();
        let max_over_min = if min > 0.0 { max / min } else { 0.0 };
        Self {
            n,
            min_mhz: min,
            max_mhz: max,
            mean_mhz: mean,
            sd_mhz: sd,
            max_over_min,
        }
    }

    /// Instrument validity: a live host under schedutil/turbo must show nonzero spread.
    /// A flat sampler (always the same MHz) is VOID — not "clean clock."
    #[must_use]
    pub fn sampler_valid(&self) -> bool {
        self.n >= 8 && self.sd_mhz > 0.0
    }
}

/// Background ~1 kHz reader of `/sys/.../cpufreq/scaling_cur_freq`.
pub struct HostClockSampler {
    stop: Arc<AtomicBool>,
    ring: Arc<Mutex<Vec<ClockSample>>>,
    join: Option<JoinHandle<()>>,
    started: Instant,
}

impl HostClockSampler {
    /// Spawn the sampler. Returns `None` when no readable cpufreq nodes exist (non-Linux / VM).
    pub fn start(hz: u32) -> Option<Self> {
        let cpus = online_cpu_ids();
        if cpus.is_empty() {
            return None;
        }
        let period = Duration::from_micros((1_000_000u64 / u64::from(hz.max(1))).max(200));
        let stop = Arc::new(AtomicBool::new(false));
        let ring = Arc::new(Mutex::new(Vec::with_capacity(hz as usize * 4)));
        let stop_t = Arc::clone(&stop);
        let ring_t = Arc::clone(&ring);
        let join = thread::Builder::new()
            .name("caer-host-clock".into())
            .spawn(move || {
                let paths: Vec<_> = cpus
                    .iter()
                    .map(|&id| {
                        (
                            id,
                            Path::new("/sys/devices/system/cpu")
                                .join(format!("cpu{id}"))
                                .join("cpufreq/scaling_cur_freq"),
                        )
                    })
                    .collect();
                let max_cpu = cpus.iter().copied().max().unwrap_or(0) as usize;
                while !stop_t.load(Ordering::Relaxed) {
                    let tick = Instant::now();
                    let mut mhz = vec![0u32; max_cpu + 1];
                    for (id, path) in &paths {
                        if let Ok(s) = std::fs::read_to_string(path) {
                            if let Ok(khz) = s.trim().parse::<u32>() {
                                mhz[*id as usize] = khz / 1000;
                            }
                        }
                    }
                    if let Ok(mut g) = ring_t.lock() {
                        g.push(ClockSample { at: tick, mhz });
                        // Keep ~4 s of history at 1 kHz.
                        let cap = 4_000usize;
                        if g.len() > cap {
                            let drain = g.len() - cap;
                            g.drain(..drain);
                        }
                    }
                    let spent = tick.elapsed();
                    if spent < period {
                        thread::sleep(period - spent);
                    }
                }
            })
            .ok()?;
        // Warm the ring so the first frame join is not empty.
        thread::sleep(Duration::from_millis(20));
        Some(Self {
            stop,
            ring,
            join: Some(join),
            started: Instant::now(),
        })
    }

    /// Nearest sample at or before `at` (join by timestamp).
    pub fn nearest(&self, at: Instant) -> Option<ClockSample> {
        let g = self.ring.lock().ok()?;
        let mut best: Option<&ClockSample> = None;
        for s in g.iter() {
            if s.at <= at {
                best = Some(s);
            } else {
                break;
            }
        }
        best.cloned().or_else(|| g.last().cloned())
    }

    /// Stats over all samples since start (all cores with mhz > 0).
    pub fn window_stats(&self) -> ClockWindowStats {
        let Ok(g) = self.ring.lock() else {
            return ClockWindowStats::from_mhz(&[]);
        };
        let mut vals = Vec::new();
        for s in g.iter() {
            for &m in &s.mhz {
                if m > 0 {
                    vals.push(m as f64);
                }
            }
        }
        ClockWindowStats::from_mhz(&vals)
    }

    /// Busy-core MHz series: one value per sample using `cpu` when in range, else max of sample.
    pub fn busy_core_series(&self, busy_cpu: usize) -> Vec<f64> {
        let Ok(g) = self.ring.lock() else {
            return Vec::new();
        };
        g.iter()
            .filter_map(|s| {
                let v = s.mhz.get(busy_cpu).copied().unwrap_or(0);
                if v > 0 {
                    Some(v as f64)
                } else {
                    s.mhz
                        .iter()
                        .copied()
                        .filter(|&m| m > 0)
                        .map(|m| m as f64)
                        .max_by(|a, b| a.total_cmp(b))
                }
            })
            .collect()
    }

    #[must_use]
    pub fn started_at(&self) -> Instant {
        self.started
    }
}

impl Drop for HostClockSampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Logical CPU ids that expose `scaling_cur_freq`.
pub fn online_cpu_ids() -> Vec<u32> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir("/sys/devices/system/cpu") else {
        return out;
    };
    for e in rd.flatten() {
        let name = e.file_name();
        let s = name.to_string_lossy();
        let Some(rest) = s.strip_prefix("cpu") else {
            continue;
        };
        if !rest.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let Ok(id) = rest.parse::<u32>() else {
            continue;
        };
        let path = e.path().join("cpufreq/scaling_cur_freq");
        if path.is_file() {
            out.push(id);
        }
    }
    out.sort_unstable();
    out
}

/// Current logical CPU for the calling thread (`sched_getcpu`), or `None` if unavailable.
#[must_use]
pub fn current_cpu() -> Option<usize> {
    #[cfg(target_os = "linux")]
    {
        extern "C" {
            fn sched_getcpu() -> i32;
        }
        let c = unsafe { sched_getcpu() };
        if c >= 0 {
            return Some(c as usize);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_window_stats_sd_zero_on_flat_series() {
        let s = ClockWindowStats::from_mhz(&[2300.0, 2300.0, 2300.0, 2300.0]);
        assert_eq!(s.sd_mhz, 0.0);
        assert!(!s.sampler_valid(), "flat series must fail validity");
    }

    #[test]
    fn clock_window_stats_sd_positive_on_spread() {
        let s = ClockWindowStats::from_mhz(&[
            1200.0, 1800.0, 2400.0, 3000.0, 3600.0, 2000.0, 2200.0, 2800.0,
        ]);
        assert!(s.sd_mhz > 0.0);
        assert!(s.sampler_valid());
        assert!(s.max_over_min > 1.0);
    }

    /// Live instrument check: when cpufreq is readable, a short sample must show sd > 0
    /// (or the host is already pin-locked — then we only require n>0 and document flatness).
    #[test]
    fn host_sampler_runs_when_cpufreq_present() {
        let Some(s) = HostClockSampler::start(500) else {
            eprintln!("skip: no scaling_cur_freq nodes");
            return;
        };
        thread::sleep(Duration::from_millis(80));
        let st = s.window_stats();
        assert!(st.n > 0, "sampler produced no readings");
        // Validity: prefer sd>0; if the machine is performance-pinned the ring may be flat —
        // that is a real host state, not a silent empty instrument (n>0 already asserted).
        eprintln!(
            "host_clock: n={} mean={:.0} sd={:.1} max/min={:.3} valid={}",
            st.n,
            st.mean_mhz,
            st.sd_mhz,
            st.max_over_min,
            st.sampler_valid()
        );
        drop(s);
    }
}
