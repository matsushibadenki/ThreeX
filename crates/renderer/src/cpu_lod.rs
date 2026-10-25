//! CPU asset bounds/LOD reference. No Instance snapshots, residency mutation or GPU index writes.
use crate::asset_pages::AssetDrawLayout;
use std::{collections::BTreeMap, ops::Range};
use threex_core::{
    AssetRef, Instance,
    asset_pages::AssetManifest,
    visibility::{BoundsAssessment, BoundsView, BoundsVisibility},
};
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CpuLodPolicy {
    pub max_error_pixels: f32,
    pub hysteresis_ratio: f32,
    pub frustum_culling: bool,
}
impl Default for CpuLodPolicy {
    fn default() -> Self {
        Self {
            max_error_pixels: 2.,
            hysteresis_ratio: 0.2,
            frustum_culling: true,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidLodPolicy;
impl CpuLodPolicy {
    pub fn validate(self) -> Result<Self, InvalidLodPolicy> {
        if !self.max_error_pixels.is_finite()
            || self.max_error_pixels <= 0.
            || !self.hysteresis_ratio.is_finite()
            || !(0. ..1.).contains(&self.hysteresis_ratio)
        {
            return Err(InvalidLodPolicy);
        }
        Ok(self)
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpuLodStats {
    pub candidates: usize,
    pub visible: usize,
    pub culled: usize,
    pub detail_wanted: usize,
    pub bounds_tests: usize,
    pub draw_ranges: usize,
}
#[derive(Clone, Copy)]
pub(crate) struct LodView {
    pub vp: [f32; 16],
    pub width: u32,
    pub height: u32,
    pub policy: CpuLodPolicy,
}
#[derive(PartialEq, Eq)]
struct ViewKey {
    vp: [u32; 16],
    size: [u32; 2],
    threshold: u32,
    hysteresis: u32,
    cull: bool,
}
struct Entry {
    reference: AssetRef,
    assessment: BoundsAssessment,
    want_detail: bool,
    culled: bool,
}
pub(crate) struct AssetLodDraw {
    pub reference: AssetRef,
    pub want_detail: bool,
    pub range: Range<u32>,
}
#[derive(Default)]
pub(crate) struct CpuLodPlanner {
    entries: BTreeMap<u32, Entry>,
    key: Option<ViewKey>,
    pub draws: Vec<AssetLodDraw>,
    stats: CpuLodStats,
}
impl CpuLodPlanner {
    pub fn status(&self) -> CpuLodStats {
        self.stats
    }
    pub fn sync<'a>(
        &mut self,
        data: &[Instance],
        layout: &AssetDrawLayout,
        layout_changed: bool,
        changes: &[Range<usize>],
        view: LodView,
        manifest: impl Fn(AssetRef) -> &'a AssetManifest,
    ) {
        let policy = view.policy;
        let key = ViewKey {
            vp: view.vp.map(f32::to_bits),
            size: [view.width, view.height],
            threshold: policy.max_error_pixels.to_bits(),
            hysteresis: policy.hysteresis_ratio.to_bits(),
            cull: policy.frustum_culling,
        };
        let view_changed = self.key.as_ref() != Some(&key);
        let mut plan_changed = layout_changed;
        if layout_changed {
            self.entries.clear();
            for (&reference, ranges) in &layout.draws {
                for source in ranges.iter().flat_map(|r| r.clone()) {
                    self.entries.insert(
                        source,
                        Entry {
                            reference,
                            assessment: BoundsAssessment::uncertain(),
                            want_detail: false,
                            culled: false,
                        },
                    );
                }
            }
        }
        let dirty: Vec<u32> = if view_changed || layout_changed {
            self.entries.keys().copied().collect()
        } else {
            let mut keys: Vec<_> = changes
                .iter()
                .flat_map(|r| {
                    self.entries
                        .range(r.start as u32..r.end as u32)
                        .map(|(&key, _)| key)
                })
                .collect();
            keys.sort_unstable();
            keys.dedup();
            keys
        };
        let camera = BoundsView::new(&view.vp, view.width, view.height);
        self.stats.bounds_tests = dirty.len();
        for source in dirty {
            let e = self.entries.get_mut(&source).unwrap();
            let m = manifest(e.reference);
            let assessment = camera
                .as_ref()
                .map_or_else(BoundsAssessment::uncertain, |v| {
                    v.assess(
                        m.bounds(),
                        &data[source as usize].model,
                        m.page(0).unwrap().error,
                    )
                });
            let culled =
                policy.frustum_culling && assessment.visibility == BoundsVisibility::Outside;
            let mut detail = e.want_detail;
            if m.page_count() < 2 {
                detail = false;
            } else if assessment.error_pixels > f64::from(policy.max_error_pixels) {
                detail = true;
            } else if assessment.error_pixels
                <= f64::from(policy.max_error_pixels * (1. - policy.hysteresis_ratio))
            {
                detail = false;
            }
            plan_changed |= culled != e.culled || detail != e.want_detail;
            e.assessment = assessment;
            e.culled = culled;
            e.want_detail = detail;
        }
        self.key = Some(key);
        if !plan_changed {
            return;
        }
        let tests = self.stats.bounds_tests;
        self.stats = CpuLodStats {
            candidates: self.entries.len(),
            bounds_tests: tests,
            ..Default::default()
        };
        self.draws.clear();
        for (&source, e) in &self.entries {
            if e.culled {
                self.stats.culled += 1;
                continue;
            }
            self.stats.visible += 1;
            self.stats.detail_wanted += usize::from(e.want_detail);
            if let Some(last) = self.draws.last_mut()
                && last.reference == e.reference
                && last.want_detail == e.want_detail
                && last.range.end == source
            {
                last.range.end += 1;
            } else {
                self.draws.push(AssetLodDraw {
                    reference: e.reference,
                    want_detail: e.want_detail,
                    range: source..source + 1,
                });
            }
        }
        self.stats.draw_ranges = self.draws.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        asset_pages::tests::{document, fixture},
        batching::GeometryBatches,
        gather::{PlanCache, Prepared},
    };
    fn view(width: u32) -> LodView {
        LodView {
            vp: [
                1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0.5, 1.,
            ],
            width,
            height: width,
            policy: CpuLodPolicy {
                max_error_pixels: 100.,
                ..Default::default()
            },
        }
    }
    #[test]
    fn camera_selection_hysteresis_sparse_updates_and_gather_residency() {
        let (m, _) = fixture();
        let d = document(m.reference());
        let mut data = vec![d.scene().instances()[0]; 1200];
        for (i, item) in data.iter_mut().enumerate() {
            item.color[3] = (i % 3) as f32;
        }
        let mut batches = GeometryBatches::default();
        batches.sync(&data, std::iter::once(0..data.len()));
        let revision = batches.revision(0);
        let mut layout = AssetDrawLayout::default();
        layout.sync(d.scene(), &batches, true, false, |_| true);
        let mut planner = CpuLodPlanner::default();
        let run =
            |p: &mut CpuLodPlanner, data: &[Instance], width, changed, dirty: &[Range<usize>]| {
                p.sync(data, &layout, changed, dirty, view(width), |_| &m)
            };
        run(&mut planner, &data, 100, true, &[]);
        assert!(!planner.draws[0].want_detail);
        run(&mut planner, &data, 160, false, &[]);
        assert!(planner.draws[0].want_detail);
        run(&mut planner, &data, 130, false, &[]);
        assert!(planner.draws[0].want_detail);
        run(&mut planner, &data, 100, false, &[]);
        assert!(!planner.draws[0].want_detail);
        run(&mut planner, &data, 100, false, &[]);
        assert_eq!(planner.status().bounds_tests, 0);
        data[0].model[3][0] = 10.;
        run(
            &mut planner,
            &data,
            100,
            false,
            std::slice::from_ref(&(0..1)),
        );
        assert_eq!(planner.status().bounds_tests, 1);
        assert_eq!(planner.status().culled, 1);
        assert!(planner.draws.is_empty());
        assert!(!layout.excluded.iter().all(|e| e.is_empty()));
        batches.sync(&data, std::iter::once(0..1));
        assert_eq!(batches.revision(0), revision);
        let mut plans = PlanCache::default();
        let limits = wgpu::Limits {
            max_storage_buffer_binding_size: 2560,
            ..Default::default()
        };
        assert!(matches!(
            plans.prepare(&limits, [100000, 100000], &batches, 0, true),
            Prepared::Gather { upload: true, .. }
        ));
        batches.sync(&data, std::iter::empty());
        run(&mut planner, &data, 200, false, &[]);
        assert_eq!(batches.transfer_count(0, false), 0);
        assert!(matches!(
            plans.prepare(&limits, [100000, 100000], &batches, 0, true),
            Prepared::Gather { upload: false, .. }
        ));
        let mut invalid = view(100);
        invalid.vp = [0.; 16];
        planner.sync(&data, &layout, false, &[], invalid, |_| &m);
        assert_eq!(planner.status().visible, 1);
        assert!(planner.draws[0].want_detail);
        let mut disabled = view(100);
        disabled.policy.frustum_culling = false;
        planner.sync(&data, &layout, false, &[], disabled, |_| &m);
        assert_eq!(planner.status().visible, 1);
    }
    #[test]
    fn policy_rejects_nonfinite_and_unstable_thresholds() {
        for threshold in [0., -1., f32::NAN, f32::INFINITY] {
            assert!(
                CpuLodPolicy {
                    max_error_pixels: threshold,
                    ..Default::default()
                }
                .validate()
                .is_err()
            );
        }
        for ratio in [-0.1, 1., f32::NAN] {
            assert!(
                CpuLodPolicy {
                    hysteresis_ratio: ratio,
                    ..Default::default()
                }
                .validate()
                .is_err()
            );
        }
    }
    #[test]
    fn mixed_instance_lods_keep_root_proxy_fallback_and_layout_rebuild() {
        use crate::asset_pages::{Lod, PageAdmission, PageFailure, PagePool};
        let (m, payload) = fixture();
        let d = document(m.reference());
        let mut data = vec![d.scene().instances()[0]; 3];
        data[0].model[0][0] = 0.01;
        data[0].model[1][1] = 0.01;
        data[2].model[3][0] = 10.;
        let mut layout = AssetDrawLayout::default();
        layout
            .draws
            .insert(m.reference(), std::iter::once(0..3).collect());
        let mut planner = CpuLodPlanner::default();
        planner.sync(&data, &layout, true, &[], view(400), |_| &m);
        assert_eq!(planner.status().culled, 1);
        assert_eq!(planner.draws.len(), 2);
        assert!(!planner.draws[0].want_detail);
        assert!(planner.draws[1].want_detail);
        let mut pool = PagePool::new(168, 2);
        assert_eq!(pool.select(&m, true).lod, Lod::BoundsProxy);
        let PageAdmission::Load(root) = pool.begin(&d, 1, &m, 0).unwrap() else {
            panic!()
        };
        pool.complete(
            &d,
            root,
            Ok(m.decode_page(0, payload[0].clone()).unwrap()),
            |p| p,
        )
        .unwrap();
        let PageAdmission::Load(detail) = pool.begin(&d, 1, &m, 1).unwrap() else {
            panic!()
        };
        assert_eq!(pool.select(&m, true).lod, Lod::Root);
        pool.complete(&d, detail, Err(PageFailure::ReadFailed), |p| p)
            .unwrap_err();
        assert_eq!(pool.select(&m, true).lod, Lod::Root);
        let PageAdmission::Load(detail) = pool.begin(&d, 1, &m, 1).unwrap() else {
            panic!()
        };
        pool.complete(
            &d,
            detail,
            Ok(m.decode_page(1, payload[1].clone()).unwrap()),
            |p| p,
        )
        .unwrap();
        for draw in &planner.draws {
            assert_eq!(
                pool.select(&m, draw.want_detail).lod,
                if draw.want_detail {
                    Lod::Detail
                } else {
                    Lod::Root
                }
            );
        }
        planner.sync(&data, &layout, false, &[], view(400), |_| &m);
        assert_eq!(planner.status().bounds_tests, 0);
        // Simulate a packed membership swap: old visible source 1 becomes source 0.
        data.swap(0, 1);
        data.truncate(1);
        layout
            .draws
            .insert(m.reference(), std::iter::once(0..1).collect());
        planner.sync(&data, &layout, true, &[], view(400), |_| &m);
        assert_eq!(planner.status().candidates, 1);
        assert_eq!(planner.draws[0].range, 0..1);
        assert!(planner.draws[0].want_detail);
    }
}
