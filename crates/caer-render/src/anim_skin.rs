//! §20 strangler — animation-domain palette builds (MS-08).
//!
//! Per unique `(loco, 30 Hz phase)` key, palette evaluation is embarrassingly parallel: independent
//! reads of [`SkinnedRig`] + clip, writes into distinct slices. Parallel is the **shipped default**
//! (§20A retired 2026-08-08); set `CAER_PARALLEL_ANIM=0` to force the serial kill-switch.
//!
//! **Invariant:** for a fixed job list, parallel and serial MUST produce bit-identical matrix
//! bytes (REQ-020 render determinism), including under `RAYON_NUM_THREADS` ∈ {1,2,3,7,36}.

use std::cell::RefCell;

use caer_assets::nif::Xform;
use rayon::prelude::*;

use crate::entities::{Loco, SkinnedRig};

/// Parallel animation path (§20A). Default **on** (shipped). `CAER_PARALLEL_ANIM=0`/`false` forces serial.
#[must_use]
pub fn parallel_anim_enabled() -> bool {
    !matches!(
        std::env::var_os("CAER_PARALLEL_ANIM"),
        Some(v) if v == "0" || v == "false" || v.is_empty()
    )
}

/// Below this many unique jobs, rayon scheduling cost exceeds the gain on this host — stay serial.
const PARALLEL_JOB_FLOOR: usize = 8;

/// One unique palette to build. Encounter order of these jobs defines compact-buffer layout.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UniquePaletteJob {
    pub loco: Loco,
    /// Sample time on `loco`'s clip.
    pub t: f32,
    /// When set: cross-fade from `prev` at `prev_t` with weight `w` toward `(loco, t)`.
    pub blend: Option<BlendJob>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlendJob {
    pub prev: Loco,
    pub prev_t: f32,
    pub w: f32,
}

fn append_job(
    rig: &SkinnedRig,
    job: &UniquePaletteJob,
    world: &mut Vec<Xform>,
    out: &mut Vec<[[f32; 4]; 4]>,
) {
    match job.blend {
        None => {
            let (clip, _) = rig.clip_for_state(job.loco);
            rig.palette_of_extend(clip, job.t, world, out);
        }
        Some(b) => {
            let (pclip, _) = rig.clip_for_state(b.prev);
            let (clip, _) = rig.clip_for_state(job.loco);
            rig.palette_blend_extend(pclip, b.prev_t, clip, job.t, b.w, world, out);
        }
    }
}

fn eval_job_into(
    rig: &SkinnedRig,
    job: &UniquePaletteJob,
    world: &mut Vec<Xform>,
    tmp: &mut Vec<[[f32; 4]; 4]>,
) {
    tmp.clear();
    append_job(rig, job, world, tmp);
}

fn eval_bones(
    rig: &SkinnedRig,
    job: &UniquePaletteJob,
    world: &mut Vec<Xform>,
    tmp: &mut Vec<[[f32; 4]; 4]>,
) {
    tmp.clear();
    match job.blend {
        None => {
            let (clip, _) = rig.clip_for_state(job.loco);
            rig.world_bones_of_extend(clip, job.t, world, tmp);
        }
        Some(b) => {
            let (pclip, _) = rig.clip_for_state(b.prev);
            let (clip, _) = rig.clip_for_state(job.loco);
            rig.world_bones_blend_extend(pclip, b.prev_t, clip, job.t, b.w, world, tmp);
        }
    }
}

/// Serial full-palette build (CPU fold). Used by ATTR timed path and bit-identical tests.
#[must_use]
pub fn build_palettes_serial(rig: &SkinnedRig, jobs: &[UniquePaletteJob]) -> Vec<[[f32; 4]; 4]> {
    let stride = rig.palette_stride();
    let mut out = Vec::with_capacity(jobs.len().saturating_mul(stride));
    let mut world = Vec::new();
    let mut tmp = Vec::with_capacity(stride);
    for job in jobs {
        eval_job_into(rig, job, &mut world, &mut tmp);
        debug_assert_eq!(tmp.len(), stride, "palette stride mismatch");
        out.extend_from_slice(&tmp);
    }
    out
}

/// Serial posed-bone mat4s only (`jobs × bone_stride`) — hot path when the GPU folds inverse-bind.
#[must_use]
pub fn build_bone_worlds_serial(rig: &SkinnedRig, jobs: &[UniquePaletteJob]) -> Vec<[[f32; 4]; 4]> {
    let stride = rig.bone_stride();
    let mut out = Vec::with_capacity(jobs.len().saturating_mul(stride));
    let mut world = Vec::new();
    let mut tmp = Vec::with_capacity(stride);
    for job in jobs {
        eval_bones(rig, job, &mut world, &mut tmp);
        debug_assert_eq!(tmp.len(), stride, "bone stride mismatch");
        out.extend_from_slice(&tmp);
    }
    out
}

/// Parallel posed-bone mat4s — bit-identical to [`build_bone_worlds_serial`].
#[must_use]
pub fn build_bone_worlds_parallel(
    rig: &SkinnedRig,
    jobs: &[UniquePaletteJob],
) -> Vec<[[f32; 4]; 4]> {
    let stride = rig.bone_stride();
    if jobs.is_empty() || stride == 0 {
        return Vec::new();
    }
    let mut out = vec![[[0.0f32; 4]; 4]; jobs.len() * stride];
    out.par_chunks_mut(stride)
        .zip(jobs.par_iter())
        .for_each(|(slot, job)| {
            thread_local! {
                static WORLD: RefCell<Vec<Xform>> = const { RefCell::new(Vec::new()) };
                static TMP: RefCell<Vec<[[f32; 4]; 4]>> = const { RefCell::new(Vec::new()) };
            }
            WORLD.with(|w| {
                TMP.with(|t| {
                    let mut world = w.borrow_mut();
                    let mut tmp = t.borrow_mut();
                    eval_bones(rig, job, &mut world, &mut tmp);
                    debug_assert_eq!(tmp.len(), stride);
                    slot.copy_from_slice(&tmp);
                });
            });
        });
    out
}

/// Build bone worlds using the active strangler path.
#[must_use]
pub fn build_bone_worlds(rig: &SkinnedRig, jobs: &[UniquePaletteJob]) -> Vec<[[f32; 4]; 4]> {
    if parallel_anim_enabled() && jobs.len() >= PARALLEL_JOB_FLOOR {
        build_bone_worlds_parallel(rig, jobs)
    } else {
        build_bone_worlds_serial(rig, jobs)
    }
}

/// When unset/true: GPU compute folds inverse-bind (default). `CAER_CPU_PALETTE_FOLD=1` forces the
/// old expanded CPU palette upload (tests / A/B).
#[must_use]
pub fn gpu_palette_fold_enabled() -> bool {
    !matches!(
        std::env::var_os("CAER_CPU_PALETTE_FOLD"),
        Some(v) if v == "1" || v == "true"
    )
}

/// Parallel build: same job order and bit-identical contents as [`build_palettes_serial`].
#[must_use]
pub fn build_palettes_parallel(rig: &SkinnedRig, jobs: &[UniquePaletteJob]) -> Vec<[[f32; 4]; 4]> {
    let stride = rig.palette_stride();
    if jobs.is_empty() || stride == 0 {
        return Vec::new();
    }
    // Pre-size so each job owns a disjoint slice — no shared mutable palette state.
    let mut out = vec![[[0.0f32; 4]; 4]; jobs.len() * stride];
    out.par_chunks_mut(stride)
        .zip(jobs.par_iter())
        .for_each(|(slot, job)| {
            thread_local! {
                static WORLD: RefCell<Vec<Xform>> = const { RefCell::new(Vec::new()) };
                static TMP: RefCell<Vec<[[f32; 4]; 4]>> = const { RefCell::new(Vec::new()) };
            }
            WORLD.with(|w| {
                TMP.with(|t| {
                    let mut world = w.borrow_mut();
                    let mut tmp = t.borrow_mut();
                    eval_job_into(rig, job, &mut world, &mut tmp);
                    debug_assert_eq!(tmp.len(), stride);
                    slot.copy_from_slice(&tmp);
                });
            });
        });
    out
}

/// Build unique palettes using the active strangler path (parallel default).
///
/// Parallel only kicks in at [`PARALLEL_JOB_FLOOR`] jobs so tiny per-model batches don't pay rayon.
/// Set `CAER_PARALLEL_ANIM=0` to force serial.
#[must_use]
pub fn build_palettes(rig: &SkinnedRig, jobs: &[UniquePaletteJob]) -> Vec<[[f32; 4]; 4]> {
    if parallel_anim_enabled() && jobs.len() >= PARALLEL_JOB_FLOOR {
        build_palettes_parallel(rig, jobs)
    } else {
        build_palettes_serial(rig, jobs)
    }
}

/// ATTR path: serial only — shared [`BoneMatrixTiming`] is not Sync and Instant probes dominate.
#[must_use]
pub fn build_palettes_timed(
    rig: &SkinnedRig,
    jobs: &[UniquePaletteJob],
    timing: &mut caer_assets::nif::BoneMatrixTiming,
) -> Vec<[[f32; 4]; 4]> {
    let stride = rig.palette_stride();
    let mut out = Vec::with_capacity(jobs.len().saturating_mul(stride));
    let mut world = Vec::new();
    for job in jobs {
        let start = out.len();
        match job.blend {
            None => {
                let (clip, _) = rig.clip_for_state(job.loco);
                rig.palette_of_timed_extend(clip, job.t, timing, &mut world, &mut out);
            }
            Some(b) => {
                let (pclip, _) = rig.clip_for_state(b.prev);
                let (clip, _) = rig.clip_for_state(job.loco);
                rig.palette_blend_timed_extend(
                    pclip, b.prev_t, clip, job.t, b.w, timing, &mut world, &mut out,
                );
            }
        }
        debug_assert_eq!(out.len() - start, stride);
    }
    out
}

/// One model's discovered skin work, ready for serial or parallel palette build.
pub struct ModelPaletteWork {
    pub model: u16,
    pub insts: Vec<crate::gpu::SkinnedInstance>,
    pub jobs: Vec<UniquePaletteJob>,
}

/// GPU submit tuple: `(model_id, instances, palette-or-bone matrices)`.
pub type ModelGpuUpload = (u16, Vec<crate::gpu::SkinnedInstance>, Vec<[[f32; 4]; 4]>);

/// Build palettes for every prepared model. When parallel is on, models run concurrently (each
/// model uses [`build_palettes`] so large job sets still fan out internally). Result order matches
/// `work` order (stable for GPU submit / REQ-020).
#[must_use]
pub fn build_all_model_palettes(
    em: &crate::entities::EntityModels,
    work: Vec<ModelPaletteWork>,
) -> Vec<ModelGpuUpload> {
    if parallel_anim_enabled() && work.len() > 1 {
        let palettes: Vec<Option<Vec<[[f32; 4]; 4]>>> = work
            .par_iter()
            .map(|w| {
                let rig = em.skinned_rig(w.model)?;
                Some(build_palettes(rig, &w.jobs))
            })
            .collect();
        work.into_iter()
            .zip(palettes)
            .filter_map(|(w, palettes)| Some((w.model, w.insts, palettes?)))
            .collect()
    } else {
        work.into_iter()
            .filter_map(|w| {
                let rig = em.skinned_rig(w.model)?;
                let palettes = build_palettes(rig, &w.jobs);
                Some((w.model, w.insts, palettes))
            })
            .collect()
    }
}

/// Build posed bone worlds for every prepared model (GPU palette-fold hot path).
#[must_use]
pub fn build_all_model_bones(
    em: &crate::entities::EntityModels,
    work: Vec<ModelPaletteWork>,
) -> Vec<ModelGpuUpload> {
    if parallel_anim_enabled() && work.len() > 1 {
        let bones: Vec<Option<Vec<[[f32; 4]; 4]>>> = work
            .par_iter()
            .map(|w| {
                let rig = em.skinned_rig(w.model)?;
                Some(build_bone_worlds(rig, &w.jobs))
            })
            .collect();
        work.into_iter()
            .zip(bones)
            .filter_map(|(w, bones)| Some((w.model, w.insts, bones?)))
            .collect()
    } else {
        work.into_iter()
            .filter_map(|w| {
                let rig = em.skinned_rig(w.model)?;
                let bones = build_bone_worlds(rig, &w.jobs);
                Some((w.model, w.insts, bones))
            })
            .collect()
    }
}

/// Absolute element tolerance for CPU mat4 fold vs `xform_mul`→mat4 (GPU fold reference).
///
/// **Justification (not tuned to pass):** each output element is a 4-term MAC of f32 values that
/// already carry uniform scale. With DAoC bone translations typically O(1)–O(1e3) and rotations
/// O(1), a single f32 mul rounds at ~1e-7 relative; four products summed give ~4 ulps of noise
/// plus associativity differences vs the CPU's separate-scale path. Bound: `abs_tol = 1e-3`
/// (covers O(1e3) translations at ~1e-6 relative) and `rel_tol = 1e-4` of `|expected|`.
pub const FOLD_ABS_TOL: f32 = 1e-3;
/// Relative tolerance companion to [`FOLD_ABS_TOL`].
pub const FOLD_REL_TOL: f32 = 1e-4;

/// Column-major mat4×mat4 matching WGSL `a * b` (apply `b` then `a` to column vectors).
#[must_use]
pub fn mat4_mul_col(a: [[f32; 4]; 4], b: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut out = [[0.0f32; 4]; 4];
    for col in 0..4 {
        for row in 0..4 {
            out[col][row] = a[0][row] * b[col][0]
                + a[1][row] * b[col][1]
                + a[2][row] * b[col][2]
                + a[3][row] * b[col][3];
        }
    }
    out
}

/// CPU reference for [`palette_fold.wgsl`]: `bones * inverse_bind` + `z_offset` on translation.z.
#[must_use]
pub fn fold_palette_mat4_cpu(
    bones: &[[[f32; 4]; 4]],
    inverse_bind: &[[[f32; 4]; 4]],
    bone_stride: usize,
    part_count: usize,
    z_offset: f32,
) -> Vec<[[f32; 4]; 4]> {
    if bone_stride == 0 || part_count == 0 || bones.is_empty() {
        return Vec::new();
    }
    debug_assert_eq!(bones.len() % bone_stride, 0);
    debug_assert_eq!(inverse_bind.len(), part_count * bone_stride);
    let unique = bones.len() / bone_stride;
    let stride = part_count * bone_stride;
    let mut out = vec![[[0.0f32; 4]; 4]; unique * stride];
    for slot in 0..unique {
        for part in 0..part_count {
            for bone in 0..bone_stride {
                let mut m = mat4_mul_col(
                    bones[slot * bone_stride + bone],
                    inverse_bind[part * bone_stride + bone],
                );
                m[3][2] += z_offset;
                out[slot * stride + part * bone_stride + bone] = m;
            }
        }
    }
    out
}

/// Approx equality for fold parity — see [`FOLD_ABS_TOL`] / [`FOLD_REL_TOL`].
#[must_use]
pub fn palettes_within_tol(
    a: &[[[f32; 4]; 4]],
    b: &[[[f32; 4]; 4]],
    abs_tol: f32,
    rel_tol: f32,
) -> bool {
    if a.len() != b.len() {
        return false;
    }
    for (ma, mb) in a.iter().zip(b.iter()) {
        for c in 0..4 {
            for r in 0..4 {
                let x = ma[c][r];
                let y = mb[c][r];
                let diff = (x - y).abs();
                let lim = abs_tol.max(rel_tol * y.abs());
                if diff > lim {
                    return false;
                }
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CPU full-palette strangler (used when `CAER_CPU_PALETTE_FOLD=1`). Name states the claim:
    /// parallel vs serial **CPU palettes**, not the GPU fold.
    #[test]
    fn parallel_cpu_palettes_bit_identical_to_serial_when_client_present() {
        let Some(root) = caer_assets::client_dep::require_caer_client(
            "parallel_cpu_palettes_bit_identical_to_serial_when_client_present",
        ) else {
            return;
        };
        let nif_path = root.join("figures/Skel01.NIF");
        if !nif_path.is_file() {
            caer_assets::client_dep::skip_or_fail(
                "parallel_cpu_palettes_bit_identical_to_serial_when_client_present",
                "no figures/Skel01.NIF under CAER_CLIENT",
            );
            return;
        }
        let bytes = std::fs::read(&nif_path).expect("read Skel01");
        let Some(rigged) = caer_assets::nif::read_rigged(&bytes).ok().flatten() else {
            caer_assets::client_dep::skip_or_fail(
                "parallel_cpu_palettes_bit_identical_to_serial_when_client_present",
                "Skel01 not rigged",
            );
            return;
        };
        let clip = {
            let kfa = root.join("anims/skel_CIDLE.kfa");
            if kfa.is_file() {
                let kb = std::fs::read(&kfa).expect("read skel_CIDLE");
                caer_assets::nif::read_clip(&kb).unwrap_or_else(|_| caer_assets::nif::Clip {
                    tracks: Default::default(),
                    duration: 1.0,
                    rate: 1.0,
                })
            } else {
                caer_assets::nif::Clip {
                    tracks: Default::default(),
                    duration: 1.0,
                    rate: 1.0,
                }
            }
        };
        let skinned = SkinnedRig {
            rigs: vec![rigged],
            clip,
            walk: None,
            run: None,
            back: None,
            slide_left: None,
            slide_right: None,
            stride_walk: 0.0,
            stride_run: 0.0,
            stride_back: 0.0,
            stride_strafe: 0.0,
            z_offset: 0.0,
        };
        let jobs: Vec<UniquePaletteJob> = (0..64)
            .map(|i| UniquePaletteJob {
                loco: Loco::Idle,
                t: (i as f32) * (1.0 / 30.0),
                blend: None,
            })
            .collect();
        let serial = build_palettes_serial(&skinned, &jobs);
        let parallel = build_palettes_parallel(&skinned, &jobs);
        assert_eq!(serial.len(), parallel.len(), "palette length must match");
        assert_eq!(
            serial, parallel,
            "REQ-020 / §20: parallel CPU palettes must be bit-identical to serial"
        );
    }

    /// Posed-bone builders stay bit-identical (the bytes streamed under GPU fold).
    #[test]
    fn parallel_bone_worlds_bit_identical_to_serial_when_client_present() {
        let Some(root) = caer_assets::client_dep::require_caer_client(
            "parallel_bone_worlds_bit_identical_to_serial_when_client_present",
        ) else {
            return;
        };
        let nif_path = root.join("figures/Skel01.NIF");
        if !nif_path.is_file() {
            caer_assets::client_dep::skip_or_fail(
                "parallel_bone_worlds_bit_identical_to_serial_when_client_present",
                "no figures/Skel01.NIF under CAER_CLIENT",
            );
            return;
        }
        let bytes = std::fs::read(&nif_path).expect("read Skel01");
        let Some(rigged) = caer_assets::nif::read_rigged(&bytes).ok().flatten() else {
            caer_assets::client_dep::skip_or_fail(
                "parallel_bone_worlds_bit_identical_to_serial_when_client_present",
                "Skel01 not rigged",
            );
            return;
        };
        let clip = {
            let kfa = root.join("anims/skel_CIDLE.kfa");
            if kfa.is_file() {
                let kb = std::fs::read(&kfa).expect("read skel_CIDLE");
                caer_assets::nif::read_clip(&kb).unwrap_or_else(|_| caer_assets::nif::Clip {
                    tracks: Default::default(),
                    duration: 1.0,
                    rate: 1.0,
                })
            } else {
                caer_assets::nif::Clip {
                    tracks: Default::default(),
                    duration: 1.0,
                    rate: 1.0,
                }
            }
        };
        let skinned = SkinnedRig {
            rigs: vec![rigged],
            clip,
            walk: None,
            run: None,
            back: None,
            slide_left: None,
            slide_right: None,
            stride_walk: 0.0,
            stride_run: 0.0,
            stride_back: 0.0,
            stride_strafe: 0.0,
            z_offset: 0.0,
        };
        let jobs: Vec<UniquePaletteJob> = (0..64)
            .map(|i| UniquePaletteJob {
                loco: Loco::Idle,
                t: (i as f32) * (1.0 / 30.0),
                blend: None,
            })
            .collect();
        let serial = build_bone_worlds_serial(&skinned, &jobs);
        let parallel = build_bone_worlds_parallel(&skinned, &jobs);
        assert_eq!(serial.len(), parallel.len());
        assert_eq!(
            serial, parallel,
            "REQ-020: parallel bone worlds must be bit-identical to serial"
        );
        assert_eq!(serial.len(), jobs.len() * skinned.bone_stride());
        let full = build_palettes_serial(&skinned, &jobs);
        assert_eq!(full.len(), jobs.len() * skinned.palette_stride());
        assert!(
            skinned.palette_stride() >= skinned.bone_stride(),
            "parts×bones expansion: palette_stride >= bone_stride"
        );
    }

    /// §20A gate 2 — byte-identical under every listed rayon width.
    /// Uses `ThreadPoolBuilder` (not the global pool) so each width is isolated even if the
    /// process already initialized rayon.
    #[test]
    fn parallel_bit_identical_across_rayon_thread_counts_when_client_present() {
        let Some(skinned) = load_skel01_rig(
            "parallel_bit_identical_across_rayon_thread_counts_when_client_present",
        ) else {
            return;
        };
        let jobs: Vec<UniquePaletteJob> = (0..64)
            .map(|i| UniquePaletteJob {
                loco: Loco::Idle,
                t: (i as f32) * (1.0 / 30.0),
                blend: None,
            })
            .collect();
        let serial_pal = build_palettes_serial(&skinned, &jobs);
        let serial_bones = build_bone_worlds_serial(&skinned, &jobs);
        // §20A: {1, 2, 3, 7, 36} — 1, a prime, host thread count.
        const WIDTHS: &[usize] = &[1, 2, 3, 7, 36];
        for &n in WIDTHS {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(n)
                .build()
                .unwrap_or_else(|e| panic!("rayon pool n={n}: {e}"));
            let (par_pal, par_bones) = pool.install(|| {
                (
                    build_palettes_parallel(&skinned, &jobs),
                    build_bone_worlds_parallel(&skinned, &jobs),
                )
            });
            assert_eq!(
                par_pal, serial_pal,
                "§20A gate 2: CPU palettes nondeterministic at RAYON_NUM_THREADS={n}"
            );
            assert_eq!(
                par_bones, serial_bones,
                "§20A gate 2: bone worlds nondeterministic at RAYON_NUM_THREADS={n}"
            );
        }
    }

    fn load_skel01_rig(test: &str) -> Option<SkinnedRig> {
        let root = caer_assets::client_dep::require_caer_client(test)?;
        let nif_path = root.join("figures/Skel01.NIF");
        if !nif_path.is_file() {
            caer_assets::client_dep::skip_or_fail(test, "no Skel01.NIF");
            return None;
        }
        let bytes = std::fs::read(&nif_path).ok()?;
        let rigged = caer_assets::nif::read_rigged(&bytes).ok().flatten()?;
        let clip = {
            let kfa = root.join("anims/skel_CIDLE.kfa");
            if kfa.is_file() {
                let kb = std::fs::read(&kfa).ok()?;
                caer_assets::nif::read_clip(&kb).unwrap_or_else(|_| caer_assets::nif::Clip {
                    tracks: Default::default(),
                    duration: 1.0,
                    rate: 1.0,
                })
            } else {
                caer_assets::nif::Clip {
                    tracks: Default::default(),
                    duration: 1.0,
                    rate: 1.0,
                }
            }
        };
        Some(SkinnedRig {
            rigs: vec![rigged],
            clip,
            walk: None,
            run: None,
            back: None,
            slide_left: None,
            slide_right: None,
            stride_walk: 0.0,
            stride_run: 0.0,
            stride_back: 0.0,
            stride_strafe: 0.0,
            z_offset: 12.5, // non-zero so z_offset path is covered
        })
    }

    /// Mat4 fold (GPU math) stays within the justified tolerance of the CPU xform fold.
    #[test]
    fn cpu_mat4_fold_matches_xform_fold_within_justified_tol() {
        let Some(skinned) =
            load_skel01_rig("cpu_mat4_fold_matches_xform_fold_within_justified_tol")
        else {
            return;
        };
        let jobs = [UniquePaletteJob {
            loco: Loco::Idle,
            t: 0.25,
            blend: None,
        }];
        let bones = build_bone_worlds_serial(&skinned, &jobs);
        let ib: Vec<[[f32; 4]; 4]> = skinned
            .rigs
            .iter()
            .flat_map(|r| r.inverse_bind_mat4s())
            .collect();
        let part_count = skinned.rigs.iter().map(|r| r.parts.len()).sum::<usize>();
        let folded = fold_palette_mat4_cpu(
            &bones,
            &ib,
            skinned.bone_stride(),
            part_count,
            skinned.z_offset,
        );
        let expected = build_palettes_serial(&skinned, &jobs);
        assert!(
            palettes_within_tol(&folded, &expected, FOLD_ABS_TOL, FOLD_REL_TOL),
            "mat4 fold vs xform fold exceeded FOLD_ABS_TOL={FOLD_ABS_TOL} / FOLD_REL_TOL={FOLD_REL_TOL}"
        );
    }

    /// Dual-path trap falsifier: a skipped fold leaves the previous frame's palette. Comparing
    /// that stale buffer to this frame's expected palette MUST go red — if it passes, the poses
    /// were identical and the falsifier is vacuous (also red).
    #[test]
    fn stale_palette_parity_goes_red_when_fold_skipped() {
        let Some(skinned) = load_skel01_rig("stale_palette_parity_goes_red_when_fold_skipped")
        else {
            return;
        };
        let job0 = [UniquePaletteJob {
            loco: Loco::Idle,
            t: 0.0,
            blend: None,
        }];
        let job1 = [UniquePaletteJob {
            loco: Loco::Idle,
            t: 0.4,
            blend: None,
        }];
        let bones0 = build_bone_worlds_serial(&skinned, &job0);
        let bones1 = build_bone_worlds_serial(&skinned, &job1);
        let ib: Vec<[[f32; 4]; 4]> = skinned
            .rigs
            .iter()
            .flat_map(|r| r.inverse_bind_mat4s())
            .collect();
        let part_count = skinned.rigs.iter().map(|r| r.parts.len()).sum::<usize>();
        let bs = skinned.bone_stride();
        let z = skinned.z_offset;
        let frame0 = fold_palette_mat4_cpu(&bones0, &ib, bs, part_count, z);
        let frame1 = fold_palette_mat4_cpu(&bones1, &ib, bs, part_count, z);
        // Vacuous-guard: poses must differ or a skipped fold would look correct by accident.
        assert!(
            !palettes_within_tol(&frame0, &frame1, FOLD_ABS_TOL, FOLD_REL_TOL),
            "frame0 and frame1 palettes are within tol — falsifier would be vacuous"
        );
        // Skip dispatch: GPU buffer still holds frame0; expected is frame1 → must go RED.
        let stale_buffer = &frame0;
        let expected_this_frame = &frame1;
        assert!(
            !palettes_within_tol(
                stale_buffer,
                expected_this_frame,
                FOLD_ABS_TOL,
                FOLD_REL_TOL
            ),
            "stale palette must fail parity (skipped-fold trap)"
        );
    }
}
