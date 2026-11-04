//! Coverage-first allowance for all registered asset draws in one view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewDrawBudget {
    pub clusters: u64,
    pub triangles: u64,
}
impl Default for ViewDrawBudget {
    fn default() -> Self {
        Self {
            clusters: 65536,
            triangles: 8_000_000,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrawCost {
    pub clusters: u64,
    pub triangles: u64,
}
impl DrawCost {
    pub(crate) fn add(self, other: Self) -> Self {
        Self {
            clusters: self.clusters.saturating_add(other.clusters),
            triangles: self.triangles.saturating_add(other.triangles),
        }
    }
    pub(crate) fn scaled(self, count: u64) -> Self {
        Self {
            clusters: self.clusters.saturating_mul(count),
            triangles: self.triangles.saturating_mul(count),
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ViewDrawStats {
    pub requested: DrawCost,
    pub minimum: DrawCost,
    pub effective: DrawCost,
    pub drawn: DrawCost,
    pub fallback_instances: u64,
}
pub(crate) struct DrawAllowance {
    stats: ViewDrawStats,
    reserved: DrawCost,
}
impl DrawAllowance {
    pub fn new(budget: ViewDrawBudget, minimum: DrawCost) -> Self {
        Self {
            stats: ViewDrawStats {
                requested: DrawCost {
                    clusters: budget.clusters,
                    triangles: budget.triangles,
                },
                minimum,
                effective: DrawCost {
                    clusters: budget.clusters.max(minimum.clusters),
                    triangles: budget.triangles.max(minimum.triangles),
                },
                ..Default::default()
            },
            reserved: minimum,
        }
    }
    /// Release this instance's reserved baseline; admit only a complete cut.
    pub fn admit(&mut self, baseline: DrawCost, candidate: DrawCost, instances: u64) -> bool {
        self.reserved.clusters -= baseline.clusters;
        self.reserved.triangles -= baseline.triangles;
        let projected = self.stats.drawn.add(self.reserved).add(candidate);
        let accept = projected.clusters <= self.stats.effective.clusters
            && projected.triangles <= self.stats.effective.triangles;
        self.stats.drawn = self
            .stats
            .drawn
            .add(if accept { candidate } else { baseline });
        if !accept {
            self.stats.fallback_instances += instances;
        }
        accept
    }
    pub fn stats(&self) -> ViewDrawStats {
        self.stats
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reserves_future_coverage_and_checks_both_limits() {
        let root = DrawCost {
            clusters: 1,
            triangles: 4,
        };
        for clusters in 0..12 {
            for triangles in 0..40 {
                let mut a = DrawAllowance::new(
                    ViewDrawBudget {
                        clusters,
                        triangles,
                    },
                    root.scaled(3),
                );
                for _ in 0..3 {
                    a.admit(
                        root,
                        DrawCost {
                            clusters: 4,
                            triangles: 12,
                        },
                        1,
                    );
                }
                let s = a.stats();
                assert!(s.drawn.clusters <= s.effective.clusters);
                assert!(s.drawn.triangles <= s.effective.triangles);
                assert!(s.drawn.clusters >= 3 && s.drawn.triangles >= 12);
            }
        }
        let mut a = DrawAllowance::new(
            ViewDrawBudget {
                clusters: 4,
                triangles: 20,
            },
            root.scaled(3),
        );
        assert!(!a.admit(
            root,
            DrawCost {
                clusters: 4,
                triangles: 8
            },
            1
        ));
        assert!(!a.admit(
            root,
            DrawCost {
                clusters: 1,
                triangles: 20
            },
            1
        ));
        assert!(a.admit(root, root, 1));
        assert_eq!(a.stats().drawn, root.scaled(3));
    }
    #[test]
    fn primitive_triangles_and_instanced_legacy_ranges_are_reserved() {
        let primitives = DrawCost {
            clusters: 0,
            triangles: 240,
        };
        let roots = DrawCost {
            clusters: 1,
            triangles: 4,
        }
        .scaled(10);
        let mut allowance = DrawAllowance::new(
            ViewDrawBudget {
                clusters: 10,
                triangles: 280,
            },
            primitives.add(roots),
        );
        assert!(allowance.admit(primitives, primitives, 0));
        // A legacy instanced range is accepted or reverted as one complete range.
        assert!(!allowance.admit(
            roots,
            DrawCost {
                clusters: 10,
                triangles: 120
            },
            10
        ));
        assert_eq!(allowance.stats().drawn, primitives.add(roots));
        assert_eq!(allowance.stats().fallback_instances, 10);
    }
    #[test]
    fn cached_cuts_cannot_bypass_allowance_and_rejection_preserves_history_and_gather() {
        use crate::{
            asset_pages::{
                HierarchyCutBudget, Lod, PageAdmission, PagePool, hierarchy_tests::fixture,
                tests::document,
            },
            batching::GeometryBatches,
            cpu_lod::CpuLodPolicy,
            cut_cache::{CutCache, CutView},
            gather::{PlanCache, Prepared},
        };
        let (m, bytes) = fixture();
        let d = document(m.reference());
        let mut pool = PagePool::new(1 << 20, 16);
        for (n, bytes) in bytes.into_iter().enumerate() {
            let PageAdmission::Load(r) = pool.begin(&d, 1, &m, n as u32).unwrap() else {
                panic!()
            };
            pool.complete(&d, r, Ok(m.decode_page(n as u32, bytes).unwrap()), |p| p)
                .unwrap();
        }
        let cost = |p: &crate::asset_pages::PageSelection| DrawCost {
            clusters: 1,
            triangles: p
                .handle
                .and_then(|h| pool.get(h))
                .map_or(12, |p| u64::from(p.indices() / 3)),
        };
        let root = pool.select(&m, false);
        assert_eq!(root.lod, Lod::Root);
        let baseline = cost(&root);
        let mut data = vec![d.scene().instances()[0]; 1200];
        for (i, v) in data.iter_mut().enumerate() {
            v.color[3] = (1 + i % 3) as f32;
        }
        let mut batches = GeometryBatches::default();
        batches.sync(&data, std::iter::once(0..1200));
        let revision = batches.revision(0);
        let limits = wgpu::Limits {
            max_storage_buffer_binding_size: 2560,
            ..Default::default()
        };
        let mut plans = PlanCache::default();
        assert!(matches!(
            plans.prepare(&limits, [100000, 100000], &batches, 0, true),
            Prepared::Gather { upload: true, .. }
        ));
        let mut cache = CutCache::default();
        let view = || CutView {
            vp: [0.; 16],
            size: [640, 480],
            model: [[0.; 4]; 4],
            policy: CpuLodPolicy::default(),
            root: true,
            budget: HierarchyCutBudget {
                max_visits: 64,
                max_draws: 32,
            },
        };
        for frame in 0..3 {
            // Request planning is independent of residency, draw/cache history and bank state.
            let mut requests = crate::streaming::RequestBudget {
                requests: 2,
                visits: 3,
            };
            let planned = crate::streaming::plan(
                &m,
                &threex_core::identity(),
                crate::streaming::StreamingView {
                    camera: [0; 16],
                    size: [640, 480],
                    error_pixels: 2f32.to_bits(),
                },
                &mut requests,
            );
            assert_eq!(planned.pages.len(), 2);
            assert!(planned.visited <= 3);
            cache.begin_visible(d.scene().asset_layout_stamp(), (0..3).map(|s| (s, 100)));
            let mut allowance = DrawAllowance::new(
                ViewDrawBudget {
                    clusters: 0,
                    triangles: 0,
                },
                baseline.scaled(3),
            );
            let mut visits = 0;
            for source in 0..3 {
                let (selected, used) = cache.select(source, &m, &pool, view(), |_| 3.);
                visits += used;
                assert_eq!(selected.len(), 4);
                let candidate = selected
                    .iter()
                    .fold(DrawCost::default(), |sum, p| sum.add(cost(p)));
                assert!(!allowance.admit(baseline, candidate, 1));
            }
            assert_eq!(allowance.stats().drawn, baseline.scaled(3));
            assert_eq!(allowance.stats().fallback_instances, 3);
            if frame > 0 {
                assert_eq!(visits, 0);
                assert_eq!(cache.stats().hits, 3);
            }
        }
        // Raising the draw allowance can reuse the unmodified fine cut immediately.
        let (cut, used) = cache.select(0, &m, &pool, view(), |_| panic!());
        assert_eq!(used, 0);
        let candidate = cut
            .iter()
            .fold(DrawCost::default(), |sum, p| sum.add(cost(p)));
        let mut generous = DrawAllowance::new(
            ViewDrawBudget {
                clusters: 100,
                triangles: 1000,
            },
            baseline,
        );
        assert!(generous.admit(baseline, candidate, 1));
        // Camera changes retain fine-node history in the dead band after budget rejection.
        let mut changed = view();
        changed.vp[0] = 1.;
        assert_eq!(cache.select(0, &m, &pool, changed, |_| 1.8).0.len(), 4);
        batches.sync(&data, std::iter::empty());
        assert_eq!(batches.revision(0), revision);
        assert_eq!(batches.transfer_count(0, false), 0);
        assert!(matches!(
            plans.prepare(&limits, [100000, 100000], &batches, 0, true),
            Prepared::Gather { upload: false, .. }
        ));
        // No resident root: the same accounting reserves a 12-triangle bounds proxy.
        pool.clear();
        let proxy = pool.select(&m, false);
        assert_eq!(proxy.lod, Lod::BoundsProxy);
        let mut empty = DrawAllowance::new(
            ViewDrawBudget {
                clusters: 0,
                triangles: 0,
            },
            DrawCost {
                clusters: 1,
                triangles: 12,
            },
        );
        assert!(empty.admit(
            DrawCost {
                clusters: 1,
                triangles: 12
            },
            DrawCost {
                clusters: 1,
                triangles: 12
            },
            1
        ));
        assert_eq!(empty.stats().drawn.triangles, 12);
    }
}
