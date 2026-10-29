//! Deterministic cluster partitioning and bounded parent generation.
use crate::lod_hierarchy::{InvalidHierarchy, LodHierarchy, LodNode};
use std::ops::Range;
#[derive(Clone, Copy)]
pub struct ClusterBudget {
    pub leaf_triangles: usize,
    pub max_triangles: usize,
    pub max_nodes: usize,
}
pub struct ClusterHierarchy {
    pub hierarchy: LodHierarchy,
    /// Each original triangle occurs once; node ranges partition this array.
    pub triangle_order: Vec<u32>,
    pub triangle_ranges: Vec<Range<usize>>,
}
impl ClusterHierarchy {
    pub fn build(
        vertices: &[[f32; 3]],
        triangles: &[[u32; 3]],
        budget: ClusterBudget,
    ) -> Result<Self, InvalidHierarchy> {
        if triangles.is_empty()
            || budget.leaf_triangles == 0
            || triangles.len() > budget.max_triangles
            || triangles.len() > u32::MAX as usize
            || budget.max_nodes == 0
            || budget.max_nodes > u32::MAX as usize
        {
            return Err(InvalidHierarchy);
        }
        // Validate before allocating metadata. Unreferenced vertices do not affect bounds.
        for tri in triangles {
            for &v in tri {
                if vertices
                    .get(v as usize)
                    .is_none_or(|v| v.iter().any(|x| !x.is_finite()))
                {
                    return Err(InvalidHierarchy);
                }
            }
        }
        let mut builder = Builder {
            vertices,
            triangles,
            budget,
            nodes: Vec::new(),
            ranges: Vec::new(),
        };
        let mut order: Vec<u32> = (0..triangles.len() as u32).collect();
        builder.branch(&mut order, 0)?;
        Ok(Self {
            hierarchy: LodHierarchy::new(builder.nodes, budget.max_nodes)?,
            triangle_order: order,
            triangle_ranges: builder.ranges,
        })
    }
}
struct Builder<'a> {
    vertices: &'a [[f32; 3]],
    triangles: &'a [[u32; 3]],
    budget: ClusterBudget,
    nodes: Vec<LodNode>,
    ranges: Vec<Range<usize>>,
}
impl Builder<'_> {
    fn branch(&mut self, order: &mut [u32], start: usize) -> Result<usize, InvalidHierarchy> {
        if self.nodes.len() >= self.budget.max_nodes {
            return Err(InvalidHierarchy);
        }
        let mut bounds = [f32::INFINITY; 6];
        bounds[3..].fill(f32::NEG_INFINITY);
        for &tri in order.iter() {
            for &vertex in &self.triangles[tri as usize] {
                for a in 0..3 {
                    let x = self.vertices[vertex as usize][a];
                    bounds[a] = bounds[a].min(x);
                    bounds[a + 3] = bounds[a + 3].max(x);
                }
            }
        }
        let id = self.nodes.len();
        self.nodes.push(LodNode {
            bounds,
            error: 0.,
            page: id as u32,
            children: vec![],
        });
        self.ranges.push(start..start + order.len());
        if order.len() > self.budget.leaf_triangles {
            let axis = (0..3)
                .max_by(|&a, &b| {
                    (f64::from(bounds[a + 3]) - f64::from(bounds[a]))
                        .total_cmp(&(f64::from(bounds[b + 3]) - f64::from(bounds[b])))
                })
                .unwrap();
            let center = |id: u32| {
                self.triangles[id as usize]
                    .iter()
                    .map(|&v| f64::from(self.vertices[v as usize][axis]))
                    .sum::<f64>()
                    / 3.
            };
            order.sort_unstable_by(|a, b| center(*a).total_cmp(&center(*b)).then(a.cmp(b)));
            let mid = order.len() / 2;
            let (a, b) = order.split_at_mut(mid);
            let a = self.branch(a, start)?;
            let b = self.branch(b, start + mid)?;
            self.nodes[id].children = vec![a, b];
        }
        Ok(id)
    }
}
/// Offline parent generation budget. Exact fallback storage is reserved up front.
#[derive(Clone, Copy)]
pub struct ParentGeometryBudget {
    pub simplify: crate::simplify::SimplifyBudget,
    pub max_source_vertices: usize,
    pub max_stored_triangles: usize,
    pub max_total_attempts: usize,
}
pub struct SimplifiedClusters {
    pub vertices: Vec<[f32; 3]>,
    pub hierarchy: LodHierarchy,
    pub meshes: Vec<crate::simplify::SimplifiedMesh>,
}
impl SimplifiedClusters {
    /// Simplifies each parent directly from its original triangle patch. Children
    /// are never used as input, so error does not accumulate across tree depth.
    pub fn build(
        vertices: &[[f32; 3]],
        triangles: &[[u32; 3]],
        locks: &[u32],
        cluster_budget: ClusterBudget,
        budget: ParentGeometryBudget,
    ) -> Result<Self, crate::simplify::SimplifyError> {
        use crate::simplify::{SimplifyError, simplify};
        if vertices.len() > budget.max_source_vertices
            || triangles.len() > budget.simplify.max_input_triangles
        {
            return Err(SimplifyError::Limit);
        }
        if budget.simplify.target_triangles == 0
            || !budget.simplify.max_error.is_finite()
            || budget.simplify.max_error < 0.
            || vertices.iter().flatten().any(|x| !x.is_finite())
        {
            return Err(SimplifyError::Input);
        }
        let exact = ClusterHierarchy::build(vertices, triangles, cluster_budget)
            .map_err(|_| SimplifyError::Input)?;
        exact
            .triangle_ranges
            .iter()
            .try_fold(0usize, |sum, range| {
                sum.checked_add(range.len())
                    .filter(|&n| n <= budget.max_stored_triangles)
                    .ok_or(SimplifyError::Limit)
            })?;
        let mut nodes = exact.hierarchy.nodes().to_vec();
        let mut meshes = Vec::with_capacity(nodes.len());
        let mut remaining = budget.max_total_attempts;
        for (node, range) in nodes.iter().zip(&exact.triangle_ranges) {
            let patch: Vec<_> = range
                .clone()
                .map(|i| triangles[exact.triangle_order[i] as usize])
                .collect();
            let mut options = budget.simplify;
            options.max_attempts = options.max_attempts.min(remaining);
            if node.children.is_empty() {
                options.target_triangles = patch.len();
                options.max_attempts = 0;
            }
            let mesh = simplify(vertices, &patch, locks, options)?;
            remaining -= mesh.attempts;
            meshes.push(mesh);
        }
        for i in (0..nodes.len()).rev() {
            nodes[i].error = nodes[i]
                .children
                .iter()
                .map(|&c| nodes[c].error)
                .fold(meshes[i].error, f32::max);
        }
        let hierarchy =
            LodHierarchy::new(nodes, cluster_budget.max_nodes).map_err(|_| SimplifyError::Input)?;
        Ok(Self {
            vertices: vertices.to_vec(),
            hierarchy,
            meshes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_partition_is_deterministic_and_preserves_boundary_vertices() {
        let vertices = [[0., 0., 0.], [1., 0., 0.], [0., 1., 0.], [1., 1., 0.]];
        let triangles = [[0, 1, 2], [1, 3, 2], [0, 1, 2], [1, 3, 2]];
        let budget = ClusterBudget {
            leaf_triangles: 1,
            max_triangles: 4,
            max_nodes: 7,
        };
        let a = ClusterHierarchy::build(&vertices, &triangles, budget).unwrap();
        let b = ClusterHierarchy::build(&vertices, &triangles, budget).unwrap();
        assert_eq!(a.triangle_order, b.triangle_order);
        assert_eq!(a.hierarchy.nodes().len(), 7);
        let mut ids = a.triangle_order.clone();
        ids.sort();
        assert_eq!(ids, vec![0, 1, 2, 3]);
        for (i, n) in a.hierarchy.nodes().iter().enumerate() {
            assert_eq!(n.error, 0.);
            if let [x, y] = n.children[..] {
                assert_eq!(a.triangle_ranges[x].start, a.triangle_ranges[i].start);
                assert_eq!(a.triangle_ranges[x].end, a.triangle_ranges[y].start);
                assert_eq!(a.triangle_ranges[y].end, a.triangle_ranges[i].end);
            } else {
                assert_eq!(a.triangle_ranges[i].len(), 1);
            }
        }
        // No resampling: every selected parent or leaf addresses original triangles.
        let cut = a.hierarchy.select(20, 20, |_, _| true, |_| true);
        assert_eq!(
            cut.nodes
                .iter()
                .map(|&n| a.triangle_ranges[n].len())
                .sum::<usize>(),
            4
        );
        assert!(
            ClusterHierarchy::build(
                &vertices,
                &triangles,
                ClusterBudget {
                    max_nodes: 6,
                    ..budget
                }
            )
            .is_err()
        );
        assert!(
            ClusterHierarchy::build(
                &vertices,
                &triangles,
                ClusterBudget {
                    max_triangles: 3,
                    ..budget
                }
            )
            .is_err()
        );
        assert!(ClusterHierarchy::build(&vertices, &[[0, 1, 9]], budget).is_err());
    }
}

#[cfg(test)]
mod parent_tests {
    use super::*;
    use crate::simplify::{SimplifyBudget, SimplifyError, tests::grid};
    fn budget() -> ParentGeometryBudget {
        ParentGeometryBudget {
            simplify: SimplifyBudget {
                target_triangles: 12,
                max_error: 2.,
                max_attempts: 200,
                max_input_triangles: 128,
            },
            max_source_vertices: 81,
            max_stored_triangles: 1024,
            max_total_attempts: 1000,
        }
    }
    fn clusters() -> ClusterBudget {
        ClusterBudget {
            leaf_triangles: 8,
            max_triangles: 128,
            max_nodes: 31,
        }
    }
    #[test]
    fn parents_reduce_preserve_seams_and_publish_monotone_error_with_atomic_cuts() {
        let (v, t) = grid(7, true);
        let generated = SimplifiedClusters::build(&v, &t, &[], clusters(), budget()).unwrap();
        assert_eq!(generated.vertices, v);
        assert!(generated.meshes[0].triangles.len() < t.len());
        let exact = ClusterHierarchy::build(&v, &t, clusters()).unwrap();
        for (i, node) in generated.hierarchy.nodes().iter().enumerate() {
            assert!(node.error >= generated.meshes[i].error);
            assert!(node.error <= budget().simplify.max_error);
            for &child in &node.children {
                assert!(node.error >= generated.hierarchy.nodes()[child].error);
            }
            let patch: Vec<_> = exact.triangle_ranges[i]
                .clone()
                .map(|k| t[exact.triangle_order[k] as usize])
                .collect();
            // Original patch boundary IDs cannot move, regardless of sibling LOD.
            let mut counts = BTreeMap::<(u32, u32), usize>::new();
            for tri in &patch {
                for (a, b) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
                    *counts.entry((a.min(b), a.max(b))).or_default() += 1;
                }
            }
            for ((a, b), count) in counts {
                if count == 1 {
                    assert_eq!(generated.meshes[i].vertex_remap[&a], a);
                    assert_eq!(generated.meshes[i].vertex_remap[&b], b);
                }
            }
            if node.children.is_empty() {
                assert_eq!(generated.meshes[i].triangles, patch);
                assert_eq!(generated.meshes[i].error, 0.);
            }
        }
        let h = &generated.hierarchy;
        assert_eq!(h.select(100, 100, |_, _| true, |p| p != 1).nodes, vec![0]);
        assert_eq!(h.select(1, 1, |_, _| true, |_| true).nodes, vec![0]);
        let cut = h.select(100, 100, |_, n| n.error > 0., |_| true);
        let covered = cut
            .nodes
            .iter()
            .map(|&i| exact.triangle_ranges[i].len())
            .sum::<usize>();
        assert_eq!(covered, t.len());
    }
    use std::collections::BTreeMap;
    #[test]
    fn generation_budgets_and_zero_error_keep_usable_exact_meshes() {
        let (v, t) = grid(5, false);
        let b = budget();
        for limited in [
            ParentGeometryBudget {
                max_source_vertices: 1,
                ..b
            },
            ParentGeometryBudget {
                max_stored_triangles: 1,
                ..b
            },
        ] {
            assert!(matches!(
                SimplifiedClusters::build(&v, &t, &[], clusters(), limited),
                Err(SimplifyError::Limit)
            ));
        }
        let exact = SimplifiedClusters::build(
            &v,
            &t,
            &[],
            clusters(),
            ParentGeometryBudget {
                max_total_attempts: 0,
                ..b
            },
        )
        .unwrap();
        assert_eq!(exact.meshes[0].triangles.len(), t.len());
        assert!(exact.hierarchy.nodes().iter().all(|n| n.error == 0.));
        let zero = SimplifiedClusters::build(
            &v,
            &t,
            &[],
            clusters(),
            ParentGeometryBudget {
                simplify: SimplifyBudget {
                    max_error: 0.,
                    ..b.simplify
                },
                ..b
            },
        )
        .unwrap();
        assert_eq!(zero.meshes[0].triangles.len(), t.len());
        let bounded = SimplifiedClusters::build(
            &v,
            &t,
            &[],
            clusters(),
            ParentGeometryBudget {
                max_total_attempts: 3,
                ..b
            },
        )
        .unwrap();
        assert!(bounded.meshes.iter().map(|m| m.attempts).sum::<usize>() <= 3);
    }
}
