//! CPU request planning. No I/O, residency mutation or draw/cache ownership.
use threex_core::{
    asset_pages::AssetManifest,
    lod_hierarchy::LodHierarchy,
    visibility::{BoundsView, BoundsVisibility},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamingView {
    pub camera: [u32; 16],
    pub size: [u32; 2],
    pub error_pixels: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestBudget {
    pub requests: usize,
    pub visits: usize,
}
impl Default for RequestBudget {
    fn default() -> Self {
        Self {
            requests: 32,
            visits: 256,
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub struct RequestPlan {
    pub pages: Vec<u32>,
    pub visited: usize,
}
/// Root was already requested. Emit complete child groups, highest parent SSE first.
/// The supplied mutable allowance is shared by all assets in a view batch.
pub fn plan(
    manifest: &AssetManifest,
    model: &[[f32; 4]; 4],
    view: StreamingView,
    budget: &mut RequestBudget,
) -> RequestPlan {
    if let Some(hierarchy) = manifest.hierarchy() {
        return plan_hierarchy(hierarchy, model, view, budget);
    }
    plan_hierarchy(&LodHierarchy::from_manifest(manifest), model, view, budget)
}
fn plan_hierarchy(
    hierarchy: &LodHierarchy,
    model: &[[f32; 4]; 4],
    view: StreamingView,
    budget: &mut RequestBudget,
) -> RequestPlan {
    let camera = BoundsView::new(&view.camera.map(f32::from_bits), view.size[0], view.size[1]);
    let threshold = f32::from_bits(view.error_pixels);
    let threshold = if threshold.is_finite() && threshold > 0. {
        f64::from(threshold)
    } else {
        2.
    };
    let mut heap = std::collections::BinaryHeap::new();
    let mut visited = 0;
    let mut pages = Vec::new();
    let mut assess =
        |id: usize, heap: &mut std::collections::BinaryHeap<(u64, std::cmp::Reverse<usize>)>| {
            if budget.visits == 0 {
                return;
            }
            budget.visits -= 1;
            visited += 1;
            let n = &hierarchy.nodes()[id];
            let a = camera
                .as_ref()
                .map_or_else(threex_core::visibility::BoundsAssessment::uncertain, |v| {
                    v.assess(n.bounds, model, n.error)
                });
            if a.visibility != BoundsVisibility::Outside
                && a.error_pixels > threshold
                && !n.children.is_empty()
            {
                heap.push((a.error_pixels.to_bits(), std::cmp::Reverse(id)));
            }
        };
    assess(0, &mut heap);
    while let Some((_, std::cmp::Reverse(id))) = heap.pop() {
        let children = &hierarchy.nodes()[id].children;
        if children.len() > budget.requests {
            continue;
        }
        budget.requests -= children.len();
        pages.extend(children.iter().map(|&c| hierarchy.nodes()[c].page));
        for &child in children {
            assess(child, &mut heap);
        }
    }
    RequestPlan { pages, visited }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset_pages::{hierarchy_tests::fixture, tests::fixture as legacy};
    fn view() -> StreamingView {
        StreamingView {
            camera: bytemuck::cast::<_, [f32; 16]>(threex_core::identity()).map(f32::to_bits),
            size: [800, 600],
            error_pixels: 2f32.to_bits(),
        }
    }
    #[test]
    fn request_and_visit_limits_preserve_complete_child_groups_and_skip_invisible() {
        let (m, _) = fixture();
        for requests in 0..10 {
            for visits in 0..10 {
                let mut b = RequestBudget { requests, visits };
                let p = plan(&m, &threex_core::identity(), view(), &mut b);
                assert!(p.pages.len() <= requests && p.visited <= visits);
                let h = m.hierarchy().unwrap();
                for node in h.nodes() {
                    let children: Vec<_> =
                        node.children.iter().map(|&n| h.nodes()[n].page).collect();
                    let count = children.iter().filter(|id| p.pages.contains(id)).count();
                    assert!(count == 0 || count == children.len());
                }
                assert!(!p.pages.contains(&0));
            }
        }
        let mut b = RequestBudget {
            requests: 6,
            visits: 7,
        };
        let first = plan(&m, &threex_core::identity(), view(), &mut b);
        assert_eq!(first.pages.len(), 6);
        assert!(
            plan(&m, &threex_core::identity(), view(), &mut b)
                .pages
                .is_empty()
        );
        let mut outside = threex_core::identity();
        outside[3][0] = 100.;
        let mut b = RequestBudget::default();
        assert!(plan(&m, &outside, view(), &mut b).pages.is_empty());
        let (m, _) = legacy();
        let mut b = RequestBudget::default();
        assert_eq!(
            plan(&m, &threex_core::identity(), view(), &mut b).pages,
            vec![1]
        );
        let mut invalid = view();
        invalid.camera = [0; 16];
        let mut b = RequestBudget {
            requests: 1,
            visits: 1,
        };
        assert_eq!(
            plan(&m, &threex_core::identity(), invalid, &mut b).pages,
            vec![1]
        );
    }
    #[test]
    fn higher_screen_error_parent_gets_complete_children_first() {
        use threex_core::lod_hierarchy::LodNode;
        let node = |page, error, children| LodNode {
            bounds: [0., 0., 0.2, 0.5, 0.5, 0.8],
            page,
            error,
            children,
        };
        let h = LodHierarchy::new(
            vec![
                node(0, 10., vec![1, 4]),
                node(1, 1., vec![2, 3]),
                node(2, 0., vec![]),
                node(3, 0., vec![]),
                node(4, 8., vec![5, 6]),
                node(5, 0., vec![]),
                node(6, 0., vec![]),
            ],
            7,
        )
        .unwrap();
        let mut budget = RequestBudget {
            requests: 4,
            visits: 7,
        };
        assert_eq!(
            plan_hierarchy(&h, &threex_core::identity(), view(), &mut budget).pages,
            vec![1, 4, 5, 6]
        );
    }
}
