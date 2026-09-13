//! REQ-026 / MEAS-019 — within-run frame pacing (jitter), not run-to-run p99 spread.
//!
//! Percentiles are order-invariant; pacing is an ordering property. `d[i] = t[i] − t[i−1]`,
//! hitch = `t[i] > 2 × p50`. See `CAER_BUILD_SPEC.md` REQ-026.

/// Within-run pacing statistics over a measured (warmup-excluded) frame-time series.
#[derive(Debug, Clone, Copy)]
pub struct JitterStats {
    pub n: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    /// `max / p50` — makes a saturated hitch=0 visibly saturated when max < 2×p50.
    pub max_over_p50: f64,
    /// p99 of signed consecutive deltas `t[i] − t[i−1]` (n−1 values).
    pub delta_p99_ms: f64,
    /// max of signed consecutive deltas.
    pub delta_max_ms: f64,
    /// p99 of `|t[i] − t[i−1]|` — useful for stall-magnitude checks.
    pub abs_delta_p99_ms: f64,
    pub abs_delta_max_ms: f64,
    /// `|Δ|p99 / p50` — relative pacing (absolute ms alone is not an inter-path claim).
    pub abs_delta_p99_over_p50: f64,
    /// Frames where `t[i] > 2 × p50` (REQ-026 headline hitch).
    pub hitch_count: usize,
    /// Hitch count scaled to per-1000-frames.
    pub hitch_per_1000: f64,
    /// Secondary threshold with resolution in the current regime: `t[i] > 1.5 × p50`.
    pub hitch_1_5x_count: usize,
    pub hitch_1_5x_per_1000: f64,
}

fn percentile_sorted(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((p / 100.0) * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Signed consecutive-frame deltas: `d[i] = t[i] − t[i−1]`.
#[must_use]
pub fn consecutive_deltas(times_ms: &[f64]) -> Vec<f64> {
    if times_ms.len() < 2 {
        return Vec::new();
    }
    times_ms.windows(2).map(|w| w[1] - w[0]).collect()
}

/// Hitch frames: `t[i] > mult × p50` (p50 of the same series).
#[must_use]
pub fn hitch_indices_at(times_ms: &[f64], p50_ms: f64, mult: f64) -> Vec<usize> {
    let thresh = mult * p50_ms;
    times_ms
        .iter()
        .enumerate()
        .filter_map(|(i, &t)| if t > thresh { Some(i) } else { None })
        .collect()
}

/// Hitch frames: `t[i] > 2 × p50` (REQ-026 headline).
#[must_use]
pub fn hitch_indices(times_ms: &[f64], p50_ms: f64) -> Vec<usize> {
    hitch_indices_at(times_ms, p50_ms, 2.0)
}

/// Compute REQ-026 pacing stats. `times_ms` must be the **warmup-excluded** ordered series.
#[must_use]
pub fn compute_jitter(times_ms: &[f64]) -> JitterStats {
    let n = times_ms.len();
    if n == 0 {
        return JitterStats {
            n: 0,
            p50_ms: 0.0,
            p95_ms: 0.0,
            p99_ms: 0.0,
            max_ms: 0.0,
            max_over_p50: 0.0,
            delta_p99_ms: 0.0,
            delta_max_ms: 0.0,
            abs_delta_p99_ms: 0.0,
            abs_delta_max_ms: 0.0,
            abs_delta_p99_over_p50: 0.0,
            hitch_count: 0,
            hitch_per_1000: 0.0,
            hitch_1_5x_count: 0,
            hitch_1_5x_per_1000: 0.0,
        };
    }
    let mut sorted = times_ms.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50 = percentile_sorted(&sorted, 50.0);
    let p95 = percentile_sorted(&sorted, 95.0);
    let p99 = percentile_sorted(&sorted, 99.0);
    let max = *sorted.last().unwrap();
    let max_over_p50 = if p50 > 0.0 { max / p50 } else { 0.0 };

    let deltas = consecutive_deltas(times_ms);
    let mut delta_sorted = deltas.clone();
    delta_sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let delta_p99 = percentile_sorted(&delta_sorted, 99.0);
    let delta_max = deltas.iter().copied().fold(0.0_f64, f64::max);
    // Also consider most-negative for "max magnitude" reporting of signed series.
    let delta_min = deltas.iter().copied().fold(0.0_f64, f64::min);
    let delta_max_ext = if delta_max.abs() >= delta_min.abs() {
        delta_max
    } else {
        delta_min
    };

    let mut abs_deltas: Vec<f64> = deltas.iter().map(|d| d.abs()).collect();
    abs_deltas.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let abs_delta_p99 = percentile_sorted(&abs_deltas, 99.0);
    let abs_delta_max = abs_deltas.last().copied().unwrap_or(0.0);
    let abs_delta_p99_over_p50 = if p50 > 0.0 { abs_delta_p99 / p50 } else { 0.0 };

    let hitch_count = hitch_indices(times_ms, p50).len();
    let hitch_1_5x_count = hitch_indices_at(times_ms, p50, 1.5).len();
    let hitch_per_1000 = (hitch_count as f64) * 1000.0 / (n as f64);
    let hitch_1_5x_per_1000 = (hitch_1_5x_count as f64) * 1000.0 / (n as f64);

    JitterStats {
        n,
        p50_ms: p50,
        p95_ms: p95,
        p99_ms: p99,
        max_ms: max,
        max_over_p50,
        delta_p99_ms: delta_p99,
        delta_max_ms: delta_max_ext,
        abs_delta_p99_ms: abs_delta_p99,
        abs_delta_max_ms: abs_delta_max,
        abs_delta_p99_over_p50,
        hitch_count,
        hitch_per_1000,
        hitch_1_5x_count,
        hitch_1_5x_per_1000,
    }
}

/// Format a REQ-026 `JITTER` line for the harness log.
#[must_use]
pub fn format_jitter_line(label: &str, warmup_excluded: usize, j: &JitterStats) -> String {
    format!(
        "JITTER\t{label}\tn={}\twarmup_excluded={warmup_excluded}\t\
p50_ms={:.3}\tp95_ms={:.3}\tp99_ms={:.3}\tmax_ms={:.3}\tmax_over_p50={:.3}\t\
delta_p99_ms={:.3}\tdelta_max_ms={:.3}\tabs_delta_p99_ms={:.3}\tabs_delta_max_ms={:.3}\t\
abs_delta_p99_over_p50={:.3}\t\
hitch_count={}\thitch_per_1000={:.2}\t\
hitch_1_5x_count={}\thitch_1_5x_per_1000={:.2}\t\
note=d[i]=t[i]-t[i-1];hitch=t[i]>2*p50;hitch_1_5x=t[i]>1.5*p50;run_to_run_spread_is_not_jitter",
        j.n,
        j.p50_ms,
        j.p95_ms,
        j.p99_ms,
        j.max_ms,
        j.max_over_p50,
        j.delta_p99_ms,
        j.delta_max_ms,
        j.abs_delta_p99_ms,
        j.abs_delta_max_ms,
        j.abs_delta_p99_over_p50,
        j.hitch_count,
        j.hitch_per_1000,
        j.hitch_1_5x_count,
        j.hitch_1_5x_per_1000,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// REQ-026 falsifier direction 2: flat series → delta≈0, hitches=0.
    #[test]
    fn flat_series_reports_zero_jitter() {
        let times = vec![16.0_f64; 1000];
        let j = compute_jitter(&times);
        assert_eq!(j.n, 1000);
        assert!((j.p50_ms - 16.0).abs() < 1e-9);
        assert!((j.p99_ms - 16.0).abs() < 1e-9);
        assert!(
            j.abs_delta_p99_ms < 1e-9,
            "abs_delta_p99={}",
            j.abs_delta_p99_ms
        );
        assert!(j.abs_delta_max_ms < 1e-9);
        assert_eq!(j.hitch_count, 0);
        assert!(j.hitch_per_1000 < 1e-9);
        assert_eq!(j.hitch_1_5x_count, 0);
        assert!((j.max_over_p50 - 1.0).abs() < 1e-9);
    }

    /// REQ-026 falsifier direction 1: periodic stall must move delta stats by ~injected amount.
    #[test]
    fn injected_periodic_stall_moves_delta_and_hitches() {
        const N: usize = 1000;
        const EVERY: usize = 10;
        const STALL: f64 = 25.0;
        const BASE: f64 = 10.0;
        let mut times = vec![BASE; N];
        let mut injected = 0usize;
        for i in (0..N).step_by(EVERY) {
            times[i] = BASE + STALL;
            injected += 1;
        }
        let j = compute_jitter(&times);
        // |d| must see the jump up and the jump back (~STALL).
        assert!(
            (j.abs_delta_max_ms - STALL).abs() < 1e-6,
            "abs_delta_max={} want {}",
            j.abs_delta_max_ms,
            STALL
        );
        assert!(
            j.abs_delta_p99_ms > STALL * 0.5,
            "abs_delta_p99={} should reflect injected stall {}",
            j.abs_delta_p99_ms,
            STALL
        );
        // Hitch: stalled frames are 35 > 2*p50. p50 of this series is still ~10 (90% base).
        assert!(
            j.hitch_count >= injected,
            "hitch_count={} injected_stalls={}",
            j.hitch_count,
            injected
        );
        assert!(j.hitch_per_1000 > 0.0);
        assert!(j.hitch_1_5x_count >= injected);
    }

    /// When max < 2×p50, headline hitch saturates at 0; 1.5× and max/p50 still resolve.
    #[test]
    fn saturated_2x_hitch_still_reports_1_5x_and_max_ratio() {
        // p50=10, max=16 → max/p50=1.6; 1.5× fires, 2× does not.
        let mut times = vec![10.0_f64; 100];
        times[50] = 16.0;
        let j = compute_jitter(&times);
        assert_eq!(j.hitch_count, 0, "2× threshold must not fire");
        assert_eq!(j.hitch_1_5x_count, 1);
        assert!((j.max_over_p50 - 1.6).abs() < 1e-9);
    }

    /// Alternating 8/20 vs clustered slow-then-fast share percentiles but not deltas.
    #[test]
    fn alternating_and_clustered_share_percentiles_not_deltas() {
        let mut alt = Vec::with_capacity(1000);
        for i in 0..1000 {
            alt.push(if i % 2 == 0 { 8.0 } else { 20.0 });
        }
        let mut clustered = vec![20.0; 500];
        clustered.extend(std::iter::repeat_n(8.0, 500));
        let ja = compute_jitter(&alt);
        let jc = compute_jitter(&clustered);
        // Same multiset → same order-invariant percentiles.
        assert!((ja.p50_ms - jc.p50_ms).abs() < 1e-9);
        assert!((ja.p99_ms - jc.p99_ms).abs() < 1e-9);
        // Pacing differs: alternating has |d|=12 every frame; clustered mostly 0 with one jump.
        assert!(
            ja.abs_delta_p99_ms > 10.0,
            "alternating abs_delta_p99={}",
            ja.abs_delta_p99_ms
        );
        assert!(
            jc.abs_delta_p99_ms < 1.0,
            "clustered abs_delta_p99={} (almost flat within halves)",
            jc.abs_delta_p99_ms
        );
    }
}
