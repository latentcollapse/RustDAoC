//! Hierarchy-aware comparison for authored avatar rigs.
//!
//! A skeleton's world-space bind positions are not enough to identify its rig. Two different
//! parent chains can place every joint in the same position, and duplicate bone names make a
//! name-to-index map lossy. This module compares the local transforms through the full labelled
//! hierarchy, matching duplicate sibling subtrees as an unordered set.

use std::collections::HashMap;
use std::fmt;

use caer_assets::nif::Skeleton;

/// Default comparison policy for the inspection tool. Translation is measured in authored model
/// units; rotation and scale use a tight component tolerance because they are dimensionless.
pub const DEFAULT_TOLERANCE: RigTolerance = RigTolerance {
    max_local_translation_delta: 1.5,
    max_rotation_component_delta: 0.0001,
    max_scale_delta: 0.0001,
};

/// The comparison surface printed by `rigcompare` beside every result.
pub const COMPARISON_BASIS: &str = "bone name + DAoC id + parent/child hierarchy; local translation distance, rotation components, and scale; duplicate siblings use a minimum-bottleneck match";

/// Limits applied to a pair of authored skeletons before they can share a tolerance-derived group.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RigTolerance {
    pub max_local_translation_delta: f32,
    pub max_rotation_component_delta: f32,
    pub max_scale_delta: f32,
}

impl RigTolerance {
    /// Reject a configuration that would turn an invalid value into an all-pass comparison.
    pub fn validate(self) -> Result<Self, RigShapeError> {
        for (name, value) in [
            (
                "max_local_translation_delta",
                self.max_local_translation_delta,
            ),
            (
                "max_rotation_component_delta",
                self.max_rotation_component_delta,
            ),
            ("max_scale_delta", self.max_scale_delta),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(RigShapeError::InvalidTolerance { name, value });
            }
        }
        Ok(self)
    }
}

/// The largest local-transform disagreement in one matched skeleton pair or group.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RigDivergence {
    pub max_local_translation_delta: f32,
    pub max_rotation_component_delta: f32,
    pub max_scale_delta: f32,
}

impl RigDivergence {
    fn combine(self, other: Self) -> Self {
        Self {
            max_local_translation_delta: self
                .max_local_translation_delta
                .max(other.max_local_translation_delta),
            max_rotation_component_delta: self
                .max_rotation_component_delta
                .max(other.max_rotation_component_delta),
            max_scale_delta: self.max_scale_delta.max(other.max_scale_delta),
        }
    }

    fn within(self, tolerance: RigTolerance) -> bool {
        self.max_local_translation_delta <= tolerance.max_local_translation_delta
            && self.max_rotation_component_delta <= tolerance.max_rotation_component_delta
            && self.max_scale_delta <= tolerance.max_scale_delta
    }

    fn non_translation_components_within(self, tolerance: RigTolerance) -> bool {
        self.max_rotation_component_delta <= tolerance.max_rotation_component_delta
            && self.max_scale_delta <= tolerance.max_scale_delta
    }
}

/// A deterministic complete-link cluster. Every pair of members is within the printed tolerance.
/// It is intentionally not called an "identical" rig class: tolerance-derived clustering is a
/// repair-scoping aid, not a claim that source files are byte-identical.
#[derive(Debug, Clone, PartialEq)]
pub struct RigFamily {
    pub members: Vec<usize>,
    pub max_divergence: RigDivergence,
}

/// A malformed skeleton cannot be compared honestly; treating it as a singleton would hide a
/// parser or source-data failure behind a plausible family tally.
#[derive(Debug, Clone, PartialEq)]
pub enum RigShapeError {
    EmptySkeleton,
    InvalidParent { bone: usize, parent: usize },
    CyclicHierarchy,
    NonFiniteTransform { bone: usize },
    InvalidTolerance { name: &'static str, value: f32 },
}

impl fmt::Display for RigShapeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySkeleton => write!(f, "skeleton contains no bones"),
            Self::InvalidParent { bone, parent } => {
                write!(f, "bone {bone} names out-of-range parent {parent}")
            }
            Self::CyclicHierarchy => write!(f, "skeleton hierarchy contains a cycle"),
            Self::NonFiniteTransform { bone } => {
                write!(f, "bone {bone} has a non-finite local bind transform")
            }
            Self::InvalidTolerance { name, value } => {
                write!(f, "{name} must be finite and non-negative, got {value}")
            }
        }
    }
}

impl std::error::Error for RigShapeError {}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BoneLabel {
    name: String,
    id: Option<i32>,
}

#[derive(Debug)]
struct RigNode {
    label: BoneLabel,
    rotation: [f32; 9],
    scale: f32,
    translation: [f32; 3],
    children: Vec<usize>,
}

#[derive(Debug)]
struct RigTree {
    nodes: Vec<RigNode>,
    roots: Vec<usize>,
}

impl RigTree {
    fn from_skeleton(skeleton: &Skeleton) -> Result<Self, RigShapeError> {
        if skeleton.bones.is_empty() {
            return Err(RigShapeError::EmptySkeleton);
        }

        let mut nodes = Vec::with_capacity(skeleton.bones.len());
        for (bone_index, bone) in skeleton.bones.iter().enumerate() {
            let (rotation, scale, translation) = &bone.local;
            if !rotation
                .iter()
                .chain([scale])
                .chain(translation)
                .all(|v| v.is_finite())
            {
                return Err(RigShapeError::NonFiniteTransform { bone: bone_index });
            }
            nodes.push(RigNode {
                label: BoneLabel {
                    name: bone.name.clone(),
                    id: bone.id,
                },
                rotation: *rotation,
                scale: *scale,
                translation: *translation,
                children: Vec::new(),
            });
        }

        let mut roots = Vec::new();
        for (bone_index, bone) in skeleton.bones.iter().enumerate() {
            match bone.parent {
                Some(parent) if parent < nodes.len() => nodes[parent].children.push(bone_index),
                Some(parent) => {
                    return Err(RigShapeError::InvalidParent {
                        bone: bone_index,
                        parent,
                    });
                }
                None => roots.push(bone_index),
            }
        }
        if roots.is_empty() {
            return Err(RigShapeError::CyclicHierarchy);
        }

        let mut seen = vec![false; nodes.len()];
        let mut stack = roots.clone();
        while let Some(index) = stack.pop() {
            if seen[index] {
                return Err(RigShapeError::CyclicHierarchy);
            }
            seen[index] = true;
            stack.extend(nodes[index].children.iter().copied());
        }
        if seen.iter().any(|visited| !visited) {
            return Err(RigShapeError::CyclicHierarchy);
        }

        Ok(Self { nodes, roots })
    }
}

/// Compare two skeletons on the documented local-hierarchy surface. `None` means their labelled
/// trees or non-translation transform components are incompatible; `Some` provides the exact
/// divergence used for tolerance grouping.
pub fn local_hierarchy_divergence(
    left: &Skeleton,
    right: &Skeleton,
    tolerance: RigTolerance,
) -> Result<Option<RigDivergence>, RigShapeError> {
    let tolerance = tolerance.validate()?;
    let left = RigTree::from_skeleton(left)?;
    let right = RigTree::from_skeleton(right)?;
    tree_divergence(&left, &right, tolerance)
}

/// Form deterministic complete-link groups from labelled skeletons.
///
/// Labels establish a stable processing order; they do not participate in the rig comparison.
/// A member joins only if it is compatible with every existing member, so no A~B~C chain can be
/// printed as one group when A and C exceed the tolerance.
pub fn complete_link_groups(
    rigs: &[(&str, &Skeleton)],
    tolerance: RigTolerance,
) -> Result<Vec<RigFamily>, RigShapeError> {
    let tolerance = tolerance.validate()?;
    let trees: Vec<RigTree> = rigs
        .iter()
        .map(|(_, skeleton)| RigTree::from_skeleton(skeleton))
        .collect::<Result<_, _>>()?;

    let mut pairwise = vec![vec![None; rigs.len()]; rigs.len()];
    for left in 0..rigs.len() {
        pairwise[left][left] = Some(RigDivergence::default());
        for right in (left + 1)..rigs.len() {
            let divergence = tree_divergence(&trees[left], &trees[right], tolerance)?;
            pairwise[left][right] = divergence;
            pairwise[right][left] = divergence;
        }
    }

    let mut order: Vec<usize> = (0..rigs.len()).collect();
    order.sort_by(|left, right| rigs[*left].0.cmp(rigs[*right].0).then(left.cmp(right)));

    let mut groups: Vec<Vec<usize>> = Vec::new();
    for candidate in order {
        if let Some(group) = groups.iter_mut().find(|group| {
            group.iter().all(|&member| {
                pairwise[candidate][member].is_some_and(|divergence| divergence.within(tolerance))
            })
        }) {
            group.push(candidate);
        } else {
            groups.push(vec![candidate]);
        }
    }

    let mut families: Vec<RigFamily> = groups
        .into_iter()
        .map(|mut members| {
            members.sort_by(|left, right| rigs[*left].0.cmp(rigs[*right].0).then(left.cmp(right)));
            let mut max_divergence = RigDivergence::default();
            for (offset, &left) in members.iter().enumerate() {
                for &right in members.iter().skip(offset + 1) {
                    max_divergence =
                        max_divergence.combine(pairwise[left][right].expect("complete-link pair"));
                }
            }
            RigFamily {
                members,
                max_divergence,
            }
        })
        .collect();
    families.sort_by(|left, right| {
        right
            .members
            .len()
            .cmp(&left.members.len())
            .then_with(|| rigs[left.members[0]].0.cmp(rigs[right.members[0]].0))
    });
    Ok(families)
}

fn tree_divergence(
    left: &RigTree,
    right: &RigTree,
    tolerance: RigTolerance,
) -> Result<Option<RigDivergence>, RigShapeError> {
    if left.roots.len() != right.roots.len() {
        return Ok(None);
    }
    let mut memo = HashMap::new();
    let costs = pair_costs(left, right, &left.roots, &right.roots, tolerance, &mut memo);
    Ok(minimum_bottleneck_match(&costs).map(|(_, divergence)| divergence))
}

fn node_divergence(
    left: &RigTree,
    right: &RigTree,
    left_index: usize,
    right_index: usize,
    tolerance: RigTolerance,
    memo: &mut HashMap<(usize, usize), Option<RigDivergence>>,
) -> Option<RigDivergence> {
    if let Some(cached) = memo.get(&(left_index, right_index)) {
        return *cached;
    }

    let left_node = &left.nodes[left_index];
    let right_node = &right.nodes[right_index];
    let result = if left_node.label != right_node.label {
        None
    } else {
        let local = local_divergence(left_node, right_node);
        if !local.non_translation_components_within(tolerance)
            || left_node.children.len() != right_node.children.len()
        {
            None
        } else {
            let costs = pair_costs(
                left,
                right,
                &left_node.children,
                &right_node.children,
                tolerance,
                memo,
            );
            minimum_bottleneck_match(&costs).map(|(_, children)| local.combine(children))
        }
    };
    memo.insert((left_index, right_index), result);
    result
}

fn pair_costs(
    left: &RigTree,
    right: &RigTree,
    left_children: &[usize],
    right_children: &[usize],
    tolerance: RigTolerance,
    memo: &mut HashMap<(usize, usize), Option<RigDivergence>>,
) -> Vec<Vec<Option<RigDivergence>>> {
    left_children
        .iter()
        .map(|&left_child| {
            right_children
                .iter()
                .map(|&right_child| {
                    node_divergence(left, right, left_child, right_child, tolerance, memo)
                })
                .collect()
        })
        .collect()
}

/// Return a deterministic perfect matching with the smallest possible maximum local translation
/// delta. Rotation and scale compatibility were already checked while constructing each edge.
fn minimum_bottleneck_match(
    costs: &[Vec<Option<RigDivergence>>],
) -> Option<(Vec<usize>, RigDivergence)> {
    if costs.is_empty() {
        return Some((Vec::new(), RigDivergence::default()));
    }
    if costs.iter().any(|row| row.len() != costs.len()) {
        return None;
    }

    let mut thresholds: Vec<f32> = costs
        .iter()
        .flatten()
        .filter_map(|edge| edge.map(|divergence| divergence.max_local_translation_delta))
        .collect();
    thresholds.sort_by(f32::total_cmp);
    thresholds.dedup_by(|left, right| left.total_cmp(right).is_eq());

    for threshold in thresholds {
        if let Some(matching) = perfect_matching(costs, threshold) {
            let divergence = matching
                .iter()
                .enumerate()
                .map(|(left, &right)| costs[left][right].expect("matching edge"))
                .fold(RigDivergence::default(), RigDivergence::combine);
            return Some((matching, divergence));
        }
    }
    None
}

fn perfect_matching(costs: &[Vec<Option<RigDivergence>>], threshold: f32) -> Option<Vec<usize>> {
    let mut matched_right = vec![None; costs.len()];
    for left in 0..costs.len() {
        let mut visited = vec![false; costs.len()];
        if !augment(left, costs, threshold, &mut visited, &mut matched_right) {
            return None;
        }
    }

    let mut matching = vec![usize::MAX; costs.len()];
    for (right, left) in matched_right.into_iter().enumerate() {
        matching[left.expect("perfect matching left")] = right;
    }
    Some(matching)
}

fn augment(
    left: usize,
    costs: &[Vec<Option<RigDivergence>>],
    threshold: f32,
    visited: &mut [bool],
    matched_right: &mut [Option<usize>],
) -> bool {
    for right in 0..costs.len() {
        let Some(divergence) = costs[left][right] else {
            continue;
        };
        if visited[right] || divergence.max_local_translation_delta > threshold {
            continue;
        }
        visited[right] = true;
        let can_assign = match matched_right[right] {
            None => true,
            Some(other_left) => augment(other_left, costs, threshold, visited, matched_right),
        };
        if can_assign {
            matched_right[right] = Some(left);
            return true;
        }
    }
    false
}

fn local_divergence(left: &RigNode, right: &RigNode) -> RigDivergence {
    let translation = left
        .translation
        .iter()
        .zip(right.translation)
        .map(|(&left, right)| (left - right).powi(2))
        .sum::<f32>()
        .sqrt();
    let rotation = left
        .rotation
        .iter()
        .zip(right.rotation)
        .map(|(&left, right)| (left - right).abs())
        .fold(0.0, f32::max);
    RigDivergence {
        max_local_translation_delta: translation,
        max_rotation_component_delta: rotation,
        max_scale_delta: (left.scale - right.scale).abs(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use caer_assets::nif::Bone;

    const IDENTITY: [f32; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];

    fn bone(name: &str, parent: Option<usize>, local_z: f32, world_z: f32) -> Bone {
        Bone {
            name: name.into(),
            parent,
            local: (IDENTITY, 1.0, [0.0, 0.0, local_z]),
            world_bind: (IDENTITY, 1.0, [0.0, 0.0, world_z]),
            id: None,
        }
    }

    fn skeleton(bones: Vec<Bone>) -> Skeleton {
        let name_to_index = bones
            .iter()
            .enumerate()
            .map(|(index, bone)| (bone.name.clone(), index))
            .collect();
        Skeleton {
            bones,
            name_to_index,
        }
    }

    fn tolerance(translation: f32) -> RigTolerance {
        RigTolerance {
            max_local_translation_delta: translation,
            ..DEFAULT_TOLERANCE
        }
    }

    #[test]
    fn duplicate_named_siblings_match_as_an_unordered_set() {
        let left = skeleton(vec![
            bone("root", None, 0.0, 0.0),
            bone("hand", Some(0), 1.0, 1.0),
            bone("hand", Some(0), 4.0, 4.0),
        ]);
        let right = skeleton(vec![
            bone("root", None, 0.0, 0.0),
            bone("hand", Some(0), 4.0, 4.0),
            bone("hand", Some(0), 1.0, 1.0),
        ]);

        assert_eq!(
            local_hierarchy_divergence(&left, &right, tolerance(0.0)).unwrap(),
            Some(RigDivergence::default()),
            "source order must not pair one duplicate hand with the other"
        );
        let groups =
            complete_link_groups(&[("left", &left), ("right", &right)], tolerance(0.0)).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].members, vec![0, 1]);
    }

    #[test]
    fn local_chain_divergence_is_not_hidden_by_equal_world_positions() {
        let left = skeleton(vec![
            bone("root", None, 0.0, 0.0),
            bone("tip", Some(0), 10.0, 10.0),
        ]);
        let right = skeleton(vec![
            bone("root", None, 4.0, 4.0),
            bone("tip", Some(0), 6.0, 10.0),
        ]);

        let divergence = local_hierarchy_divergence(&left, &right, tolerance(0.0))
            .unwrap()
            .expect("same labelled hierarchy");
        assert_eq!(divergence.max_local_translation_delta, 4.0);
        assert!(
            complete_link_groups(&[("left", &left), ("right", &right)], tolerance(3.99))
                .unwrap()
                .iter()
                .all(|group| group.members.len() == 1),
            "the old world-bind surface would report zero divergence here"
        );
    }

    #[test]
    fn same_name_multiset_with_a_different_chain_is_incompatible() {
        let chain = skeleton(vec![
            bone("root", None, 0.0, 0.0),
            bone("joint", Some(0), 1.0, 1.0),
            bone("joint", Some(1), 1.0, 2.0),
        ]);
        let fork = skeleton(vec![
            bone("root", None, 0.0, 0.0),
            bone("joint", Some(0), 1.0, 1.0),
            bone("joint", Some(0), 2.0, 2.0),
        ]);

        assert_eq!(
            local_hierarchy_divergence(&chain, &fork, tolerance(10.0)).unwrap(),
            None,
            "matching only sorted bone names would erase this topology change"
        );
    }

    #[test]
    fn complete_link_does_not_chain_merge_a_and_c() {
        let a = skeleton(vec![bone("root", None, 0.0, 0.0)]);
        let b = skeleton(vec![bone("root", None, 0.9, 0.9)]);
        let c = skeleton(vec![bone("root", None, 1.8, 1.8)]);
        let groups =
            complete_link_groups(&[("a", &a), ("b", &b), ("c", &c)], tolerance(1.0)).unwrap();

        assert_eq!(groups.len(), 2);
        assert!(
            groups
                .iter()
                .all(|group| group.max_divergence.max_local_translation_delta <= 1.0),
            "every member pair in a complete-link group must fit the reported tolerance"
        );
        assert!(
            groups
                .iter()
                .all(|group| !(group.members.contains(&0) && group.members.contains(&2))),
            "A~B and B~C must not turn A and C into one group"
        );
    }

    #[test]
    fn rotation_delta_cannot_hide_inside_a_translation_tolerance() {
        let left = skeleton(vec![bone("root", None, 0.0, 0.0)]);
        let mut rotated_bone = bone("root", None, 0.0, 0.0);
        rotated_bone.local.0[0] = 0.9;
        let right = skeleton(vec![rotated_bone]);

        assert_eq!(
            local_hierarchy_divergence(&left, &right, tolerance(100.0)).unwrap(),
            None,
            "translation tolerance is not permission to group different local rotations"
        );
    }

    #[test]
    fn malformed_parent_fails_closed() {
        let invalid = skeleton(vec![bone("root", Some(9), 0.0, 0.0)]);
        assert_eq!(
            local_hierarchy_divergence(&invalid, &invalid, DEFAULT_TOLERANCE),
            Err(RigShapeError::InvalidParent { bone: 0, parent: 9 })
        );
    }

    #[test]
    fn invalid_tolerance_is_rejected() {
        let error = RigTolerance {
            max_local_translation_delta: f32::NAN,
            ..DEFAULT_TOLERANCE
        }
        .validate()
        .expect_err("NaN tolerance must not become an all-pass comparison");
        assert!(
            matches!(
                error,
                RigShapeError::InvalidTolerance {
                    name: "max_local_translation_delta",
                    value,
                } if value.is_nan()
            ),
            "unexpected tolerance error: {error:?}"
        );
    }
}
