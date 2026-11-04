//! World-space instance broad phase; asset-local LOD nodes never live here.
use std::collections::{BTreeMap, BTreeSet};
use threex_core::{
    Instance, identity,
    visibility::{BoundsView, BoundsVisibility},
};
type Bounds = Option<[f32; 6]>;
struct Node {
    bounds: Bounds,
    parent: Option<usize>,
    children: Option<[usize; 2]>,
    source: Option<u32>,
    span: std::ops::Range<usize>,
}
#[derive(Default)]
pub(crate) struct SpatialIndex {
    nodes: Vec<Node>,
    leaves: BTreeMap<u32, usize>,
    order: Vec<u32>,
    moved: BTreeSet<u32>,
}
pub(crate) struct SpatialQuery {
    pub visible: BTreeSet<u32>,
    pub visited: usize,
    pub deferred: usize,
}
fn union(a: Bounds, b: Bounds) -> Bounds {
    let (a, b) = (a?, b?);
    Some(std::array::from_fn(|i| {
        if i < 3 {
            a[i].min(b[i])
        } else {
            a[i].max(b[i])
        }
    }))
}
pub(crate) fn world_bounds(bounds: [f32; 6], instance: &Instance) -> Bounds {
    let mut out = [0.; 6];
    for r in 0..3 {
        let mut min = f64::from(instance.model[3][r]);
        let mut max = min;
        let mut magnitude = min.abs();
        for c in 0..3 {
            let a = f64::from(instance.model[c][r]) * f64::from(bounds[c]);
            let b = f64::from(instance.model[c][r]) * f64::from(bounds[c + 3]);
            min += a.min(b);
            max += a.max(b);
            magnitude += a.abs().max(b.abs());
        }
        // Outward rounding: never narrow the represented world bounds.
        let margin = 64. * f64::from(f32::EPSILON) * magnitude.max(1.);
        out[r] = ((min - margin) as f32).next_down();
        out[r + 3] = ((max + margin) as f32).next_up();
    }
    if out.iter().any(|v| !v.is_finite())
        || instance.model.iter().flatten().any(|v| !v.is_finite())
        || instance.model[0][3] != 0.
        || instance.model[1][3] != 0.
        || instance.model[2][3] != 0.
        || instance.model[3][3] != 1.
    {
        None
    } else {
        Some(out)
    }
}
impl SpatialIndex {
    pub fn rebuild(&mut self, mut items: Vec<(u32, Bounds)>) {
        self.nodes.clear();
        self.leaves.clear();
        self.order.clear();
        self.moved.clear();
        if !items.is_empty() {
            self.build(&mut items, None);
        }
    }
    fn build(&mut self, items: &mut [(u32, Bounds)], parent: Option<usize>) -> usize {
        let start = self.order.len();
        let index = self.nodes.len();
        let bounds = items.iter().map(|i| i.1).reduce(union).flatten();
        self.nodes.push(Node {
            bounds,
            parent,
            children: None,
            source: None,
            span: start..start + items.len(),
        });
        if items.len() == 1 {
            self.nodes[index].source = Some(items[0].0);
            self.order.push(items[0].0);
            self.leaves.insert(items[0].0, index);
        } else {
            let axis = bounds.map_or(0, |b| {
                (0..3)
                    .max_by(|&a, &c| {
                        (f64::from(b[a + 3]) - f64::from(b[a]))
                            .total_cmp(&(f64::from(b[c + 3]) - f64::from(b[c])))
                    })
                    .unwrap()
            });
            let center =
                |b: Bounds| b.map_or(0., |b| (f64::from(b[axis]) + f64::from(b[axis + 3])) * 0.5);
            items.sort_unstable_by(|a, b| center(a.1).total_cmp(&center(b.1)).then(a.0.cmp(&b.0)));
            let mid = items.len() / 2;
            let (left, right) = items.split_at_mut(mid);
            let a = self.build(left, Some(index));
            let b = self.build(right, Some(index));
            self.nodes[index].children = Some([a, b]);
        }
        index
    }
    pub fn refit(&mut self, source: u32, bounds: Bounds) {
        let Some(&leaf) = self.leaves.get(&source) else {
            return;
        };
        if self.nodes[leaf].bounds == bounds {
            return;
        }
        self.moved.insert(source);
        self.nodes[leaf].bounds = bounds;
        let mut parent = self.nodes[leaf].parent;
        while let Some(i) = parent {
            let [a, b] = self.nodes[i].children.unwrap();
            self.nodes[i].bounds = union(self.nodes[a].bounds, self.nodes[b].bounds);
            parent = self.nodes[i].parent;
        }
    }
    /// Repartition after substantial distinct movement. Never touches source slots or GPU layout.
    pub fn rebalance(&mut self) -> bool {
        let threshold = (self.leaves.len() / 4).max(32);
        if self.moved.len() < threshold {
            return false;
        }
        let items = self
            .leaves
            .iter()
            .map(|(&source, &i)| (source, self.nodes[i].bounds))
            .collect();
        self.rebuild(items);
        true
    }
    #[cfg(test)]
    pub fn query(&self, view: Option<&BoundsView>) -> SpatialQuery {
        self.query_budget(view, usize::MAX)
    }
    pub fn query_budget(&self, view: Option<&BoundsView>, max_visits: usize) -> SpatialQuery {
        let mut q = SpatialQuery {
            visible: BTreeSet::new(),
            visited: 0,
            deferred: 0,
        };
        let mut stack = Vec::new();
        if !self.nodes.is_empty() {
            stack.push(0);
        }
        while let Some(i) = stack.pop() {
            let n = &self.nodes[i];
            if q.visited == max_visits {
                // An untested subtree remains a conservative candidate set.
                let sources = &self.order[n.span.clone()];
                q.deferred += sources.len();
                q.visible.extend(sources.iter().copied());
                continue;
            }
            q.visited += 1;
            if let (Some(view), Some(b)) = (view, n.bounds)
                && view.assess(b, &identity(), 0.).visibility == BoundsVisibility::Outside
            {
                continue;
            }
            if let Some(source) = n.source {
                q.visible.insert(source);
            }
            if let Some([a, b]) = n.children {
                stack.extend([b, a]);
            }
        }
        q
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn branch_rejection_refit_unknown_and_membership() {
        let vp = bytemuck::cast(identity());
        let v = BoundsView::new(&vp, 100, 100).unwrap();
        let mut index = SpatialIndex::default();
        index.rebuild(
            (0..1024)
                .map(|i| (i, Some([10. + i as f32, 0., 0.2, 11. + i as f32, 1., 0.8])))
                .collect(),
        );
        let q = index.query(Some(&v));
        assert!(q.visible.is_empty());
        assert_eq!(q.visited, 1);
        index.refit(100, Some([0., 0., 0.2, 0.5, 0.5, 0.8]));
        assert_eq!(index.query(Some(&v)).visible, BTreeSet::from([100]));
        index.refit(101, None);
        assert_eq!(index.query(Some(&v)).visible, BTreeSet::from([100, 101]));
        assert_eq!(index.query(None).visible.len(), 1024);
        index.rebuild(vec![(4, Some([0., 0., 0.2, 0.5, 0.5, 0.8]))]);
        assert_eq!(index.query(Some(&v)).visible, BTreeSet::from([4]));
        index.rebuild(vec![]);
        assert!(index.query(Some(&v)).visible.is_empty());
    }
    #[test]
    fn budget_frontier_preserves_coverage_and_rebalance_restores_partitioning() {
        let vp = bytemuck::cast(identity());
        let view = BoundsView::new(&vp, 800, 600).unwrap();
        let mut tree = SpatialIndex::default();
        tree.rebuild(
            (0..1024)
                .map(|i| (i, Some([i as f32, 0., 0.2, i as f32 + 0.1, 0.1, 0.8])))
                .collect(),
        );
        for i in 0..1024 {
            let x = if i % 2 == 0 { -1000. } else { 1000. } + i as f32 * 0.001;
            tree.refit(i, Some([x, 0., 0.2, x + 0.1, 0.1, 0.8]));
        }
        tree.refit(500, Some([0., 0., 0.2, 0.1, 0.1, 0.8]));
        tree.refit(501, None);
        let before = tree.query(Some(&view));
        assert_eq!(before.visible, BTreeSet::from([500, 501]));
        for budget in 0..32 {
            let q = tree.query_budget(Some(&view), budget);
            assert!(q.visited <= budget);
            assert!(q.visible.is_superset(&before.visible));
            if budget == 0 {
                assert_eq!(q.deferred, 1024);
                assert_eq!(q.visible.len(), 1024);
            }
        }
        assert!(tree.rebalance());
        let after = tree.query(Some(&view));
        assert_eq!(before.visible, after.visible);
        assert!(after.visited < before.visited);
        assert!(!tree.rebalance());
        for budget in [0, 1, 2, 7, 32, 2048] {
            let q = tree.query_budget(Some(&view), budget);
            assert!(q.visited <= budget);
            assert!(q.visible.is_superset(&after.visible));
        }
        let node = tree.leaves[&500];
        tree.refit(500, tree.nodes[node].bounds);
        assert!(
            tree.moved.is_empty(),
            "identical bounds/color edits do not trigger rebuilds"
        );
        tree.rebuild(vec![]);
        assert_eq!(tree.query_budget(Some(&view), 0).deferred, 0);
    }
    #[test]
    fn reflected_sheared_bounds_contain_corners_and_overflow_is_unknown() {
        let mut m = identity();
        m[0][0] = -2.;
        m[1][0] = 0.7;
        m[3][0] = 3.;
        let mut instance = Instance {
            model: m,
            color: [1.; 4],
        };
        let b = [-1., -2., -3., 1., 2., 3.];
        let w = world_bounds(b, &instance).unwrap();
        for bits in 0..8 {
            for r in 0..3 {
                let x = (0..3)
                    .map(|c| m[c][r] * b[c + if bits & (1 << c) == 0 { 0 } else { 3 }])
                    .sum::<f32>()
                    + m[3][r];
                assert!(w[r] <= x && x <= w[r + 3]);
            }
        }
        instance.model[0][0] = f32::MAX;
        assert!(world_bounds([-2.; 6], &instance).is_none());
    }
    #[test]
    fn spatial_rejection_never_removes_a_flat_visible_instance_after_refit() {
        let vp = bytemuck::cast(identity());
        let view = BoundsView::new(&vp, 800, 600).unwrap();
        let bounds = [-0.2, -0.3, -0.1, 0.2, 0.3, 0.1];
        let mut instances: Vec<_> = (0..256)
            .map(|i| {
                let t = i as f32 * 0.17;
                let (sn, cs) = t.sin_cos();
                let mut m = identity();
                m[0][0] = -cs;
                m[0][1] = -sn;
                m[1][0] = -sn + 0.2;
                m[1][1] = cs;
                m[3] = [t.sin() * 5., (t * 1.3).cos() * 5., 0.5, 1.];
                Instance {
                    model: m,
                    color: [1.; 4],
                }
            })
            .collect();
        let mut tree = SpatialIndex::default();
        tree.rebuild(
            instances
                .iter()
                .enumerate()
                .map(|(i, v)| (i as u32, world_bounds(bounds, v)))
                .collect(),
        );
        for step in 0..20 {
            let source = step * 11;
            instances[source].model[3][0] = 0.;
            instances[source].model[3][1] = 0.;
            tree.refit(source as u32, world_bounds(bounds, &instances[source]));
            let q = tree.query(Some(&view));
            for (i, v) in instances.iter().enumerate() {
                if view.assess(bounds, &v.model, 0.).visibility != BoundsVisibility::Outside {
                    assert!(q.visible.contains(&(i as u32)));
                }
            }
        }
    }
}
