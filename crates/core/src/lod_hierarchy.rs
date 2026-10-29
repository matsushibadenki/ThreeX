//! Asset-local coverage tree. Independent of instance placement and GPU residency.
use crate::asset_pages::AssetManifest;
#[derive(Clone, Debug)]
pub struct LodNode {
    pub bounds: [f32; 6],
    pub error: f32,
    pub page: u32,
    pub children: Vec<usize>,
}
#[derive(Clone, Debug)]
pub struct LodHierarchy {
    nodes: Vec<LodNode>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidHierarchy;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LodCut {
    pub nodes: Vec<usize>,
    pub visited: usize,
}
impl LodHierarchy {
    /// Parent-first rooted tree; each child's bounds and error must fit its parent.
    /// Children are declared to cover the parent's geometry, not merely its AABB.
    pub fn new(nodes: Vec<LodNode>, max_nodes: usize) -> Result<Self, InvalidHierarchy> {
        if nodes.is_empty() || nodes.len() > max_nodes {
            return Err(InvalidHierarchy);
        }
        let mut owners = vec![0u8; nodes.len()];
        for (i, n) in nodes.iter().enumerate() {
            if !n.error.is_finite()
                || n.error < 0.
                || n.bounds.iter().any(|v| !v.is_finite())
                || (0..3).any(|a| n.bounds[a] > n.bounds[a + 3])
            {
                return Err(InvalidHierarchy);
            }
            for &c in &n.children {
                if c <= i || c >= nodes.len() || owners[c] != 0 {
                    return Err(InvalidHierarchy);
                }
                let child = &nodes[c];
                if child.error > n.error
                    || (0..3).any(|a| {
                        child.bounds[a] < n.bounds[a] || child.bounds[a + 3] > n.bounds[a + 3]
                    })
                {
                    return Err(InvalidHierarchy);
                }
                owners[c] = 1;
            }
        }
        if owners[1..].contains(&0) {
            return Err(InvalidHierarchy);
        }
        Ok(Self { nodes })
    }
    pub fn from_manifest(m: &AssetManifest) -> Self {
        let mut nodes = vec![LodNode {
            bounds: m.bounds(),
            error: m.page(0).unwrap().error,
            page: 0,
            children: vec![],
        }];
        if let Some(p) = m.page(1) {
            nodes[0].children.push(1);
            nodes.push(LodNode {
                bounds: m.bounds(),
                error: p.error,
                page: 1,
                children: vec![],
            });
        }
        Self { nodes }
    }
    pub fn nodes(&self) -> &[LodNode] {
        &self.nodes
    }
    /// Allocation-free cut for a single draw. Branching keeps its parent.
    pub fn select_one(
        &self,
        max_visits: usize,
        mut refine: impl FnMut(usize, &LodNode) -> bool,
        mut resident: impl FnMut(u32) -> bool,
    ) -> Option<usize> {
        if max_visits == 0 || !resident(self.nodes[0].page) {
            return None;
        }
        let mut id = 0;
        for _ in 1..max_visits {
            let n = &self.nodes[id];
            if let [child] = n.children[..]
                && refine(id, n)
                && resident(self.nodes[child].page)
            {
                id = child;
            } else {
                break;
            }
        }
        Some(id)
    }
    /// Replaces a parent only when every child page is ready and the complete
    /// replacement fits both budgets. Budget exhaustion retains coverage.
    /// An unavailable root yields an empty cut (caller draws its bounds proxy).
    pub fn select(
        &self,
        max_visits: usize,
        max_draws: usize,
        mut refine: impl FnMut(usize, &LodNode) -> bool,
        mut resident: impl FnMut(u32) -> bool,
    ) -> LodCut {
        if max_draws == 0 || max_visits == 0 || !resident(self.nodes[0].page) {
            return LodCut {
                nodes: vec![],
                visited: 0,
            };
        }
        let mut cut = vec![0];
        let mut visited = 0;
        let mut at = 0;
        while at < cut.len() && visited < max_visits {
            let id = cut[at];
            let n = &self.nodes[id];
            visited += 1;
            if !n.children.is_empty()
                && refine(id, n)
                && cut.len() - 1 + n.children.len() <= max_draws
                && visited + n.children.len() <= max_visits
                && n.children.iter().all(|&c| resident(self.nodes[c].page))
            {
                cut.splice(at..at + 1, n.children.iter().copied());
            } else {
                at += 1;
            }
        }
        LodCut {
            nodes: cut,
            visited,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn tree() -> LodHierarchy {
        let n = |page, error, children| LodNode {
            bounds: [0., 0., 0., 1., 1., 1.],
            error,
            page,
            children,
        };
        LodHierarchy::new(
            vec![
                n(0, 4., vec![1, 2]),
                n(1, 2., vec![3, 4]),
                n(2, 0., vec![]),
                n(3, 0., vec![]),
                n(4, 0., vec![]),
            ],
            5,
        )
        .unwrap()
    }
    #[test]
    fn atomic_coverage_and_budgets() {
        let t = tree();
        assert_eq!(t.select(10, 10, |_, _| true, |_| true).nodes, vec![3, 4, 2]);
        assert_eq!(t.select(10, 10, |_, _| true, |p| p != 4).nodes, vec![1, 2]);
        assert_eq!(t.select(10, 10, |_, _| true, |p| p != 2).nodes, vec![0]);
        assert_eq!(t.select(2, 10, |_, _| true, |_| true).nodes, vec![0]);
        assert_eq!(t.select(10, 2, |_, _| true, |_| true).nodes, vec![1, 2]);
        assert!(t.select(10, 10, |_, _| true, |p| p != 0).nodes.is_empty());
        assert!(t.select(10, 0, |_, _| true, |_| true).nodes.is_empty());
        for visits in 0..8 {
            for missing in 0..6 {
                assert_eq!(
                    t.select_one(visits, |_, _| true, |p| p != missing),
                    t.select(visits, 1, |_, _| true, |p| p != missing)
                        .nodes
                        .first()
                        .copied()
                );
            }
        }
    }
    #[test]
    fn rejects_cycles_orphans_duplicate_ownership_and_nonmonotone_error() {
        for mode in 0..5 {
            let mut n = tree().nodes;
            match mode {
                0 => n[1].children.push(0),
                1 => n[0].children.clear(),
                2 => n[0].children.push(1),
                3 => n[1].error = 5.,
                _ => n[1].bounds[0] = -1.,
            }
            assert!(LodHierarchy::new(n, 10).is_err());
        }
        assert!(LodHierarchy::new(tree().nodes, 4).is_err());
    }
}
