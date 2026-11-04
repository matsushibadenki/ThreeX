//! Bounded per-source cut cache and deep-node Schmitt-trigger history.
use crate::{
    asset_pages::{HierarchyCutBudget, PagePool, PageSelection},
    cpu_lod::CpuLodPolicy,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use threex_core::{AssetRef, SceneStamp, asset_pages::AssetManifest};
const MAX_ENTRIES: usize = 256;
const FAIR_SLOTS: usize = MAX_ENTRIES / 2;
const LEASE_FRAMES: u64 = 8;
#[derive(Clone, PartialEq, Eq)]
struct Key {
    reference: AssetRef,
    view: [u32; 16],
    model: [[u32; 4]; 4],
    size: [u32; 2],
    policy: [u32; 2],
    residency: u64,
    budget: [usize; 2],
    root: bool,
}
struct Entry {
    key: Key,
    history: BTreeMap<usize, bool>,
    cut: Vec<PageSelection>,
}
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct CutCacheStats {
    pub hits: usize,
    pub misses: usize,
    pub entries: usize,
    pub evictions: usize,
    pub bypasses: usize,
    pub history_restores: usize,
    pub history_entries: usize,
}
#[derive(Default)]
pub(crate) struct CutCache {
    layout: Option<SceneStamp>,
    entries: BTreeMap<u32, Entry>,
    stats: CutCacheStats,
    admitted: Option<BTreeSet<u32>>,
    ghosts: BTreeMap<u32, Entry>,
    ghost_order: VecDeque<u32>,
    frame: u64,
    cursor: u32,
}
pub(crate) struct CutView {
    pub vp: [f32; 16],
    pub size: [u32; 2],
    pub model: [[f32; 4]; 4],
    pub policy: CpuLodPolicy,
    pub root: bool,
    pub budget: HierarchyCutBudget,
}
impl CutCache {
    pub fn begin(&mut self, layout: SceneStamp) {
        if self.layout.as_ref() != Some(&layout) {
            self.entries.clear();
            self.ghosts.clear();
            self.ghost_order.clear();
            self.cursor = 0;
            self.frame = 0;
            self.layout = Some(layout);
        }
        self.admitted = None;
        self.frame = self.frame.wrapping_add(1);
        self.stats = CutCacheStats {
            entries: self.entries.len(),
            history_entries: self.ghosts.len(),
            ..Default::default()
        };
    }
    /// Bounded streaming admission: half cyclic fair slots, remaining slots by cost.
    /// The cyclic cursor is held for eight render frames to allow reuse.
    pub fn begin_visible(
        &mut self,
        layout: SceneStamp,
        candidates: impl IntoIterator<Item = (u32, u32)>,
    ) {
        self.begin(layout);
        let mut fair = BTreeSet::new();
        let mut priority = BTreeSet::new();
        for (source, cost) in candidates {
            let rank = source.wrapping_sub(self.cursor);
            fair.insert((rank, source));
            if fair.len() > FAIR_SLOTS {
                fair.pop_last();
            }
            priority.insert((std::cmp::Reverse(cost), rank, source));
            if priority.len() > MAX_ENTRIES {
                priority.pop_last();
            }
        }
        let mut admitted: BTreeSet<_> = fair.iter().map(|&(_, s)| s).collect();
        for (_, _, source) in priority {
            if admitted.len() == MAX_ENTRIES {
                break;
            }
            admitted.insert(source);
        }
        if self.frame.is_multiple_of(LEASE_FRAMES)
            && let Some(&(_, last)) = fair.last()
        {
            self.cursor = last.wrapping_add(1);
        }
        let evicted: Vec<_> = self
            .entries
            .keys()
            .filter(|s| !admitted.contains(s))
            .copied()
            .collect();
        for source in evicted {
            let entry = self.entries.remove(&source).unwrap();
            self.stash(source, entry);
            self.stats.evictions += 1;
        }
        self.admitted = Some(admitted);
        self.update_counts();
    }
    fn stash(&mut self, source: u32, mut entry: Entry) {
        entry.cut = Vec::new();
        self.ghost_order.retain(|&s| s != source);
        self.ghost_order.push_back(source);
        self.ghosts.insert(source, entry);
        if self.ghosts.len() > MAX_ENTRIES {
            let oldest = self.ghost_order.pop_front().unwrap();
            self.ghosts.remove(&oldest);
        }
    }
    fn update_counts(&mut self) {
        self.stats.entries = self.entries.len();
        self.stats.history_entries = self.ghosts.len();
    }
    pub fn stats(&self) -> CutCacheStats {
        self.stats
    }
    pub fn select<T>(
        &mut self,
        source: u32,
        manifest: &AssetManifest,
        pool: &PagePool<T>,
        view: CutView,
        mut error: impl FnMut(&threex_core::lod_hierarchy::LodNode) -> f64,
    ) -> (Vec<PageSelection>, usize) {
        let key = Key {
            reference: manifest.reference(),
            view: view.vp.map(f32::to_bits),
            model: view.model.map(|c| c.map(f32::to_bits)),
            size: view.size,
            policy: [
                view.policy.max_error_pixels.to_bits(),
                view.policy.hysteresis_ratio.to_bits(),
            ],
            residency: pool.residency_revision(),
            budget: [view.budget.max_visits, view.budget.max_draws],
            root: view.root,
        };
        if let Some(entry) = self.entries.get(&source)
            && entry.key == key
        {
            self.stats.hits += 1;
            return (entry.cut.clone(), 0);
        }
        self.stats.misses += 1;
        let old = self.entries.remove(&source).or_else(|| {
            self.ghost_order.retain(|&s| s != source);
            self.ghosts.remove(&source)
        });
        let mut history = old
            .filter(|e| e.key.reference == key.reference && e.key.policy == key.policy)
            .map(|e| {
                if e.cut.is_empty() {
                    self.stats.history_restores += 1;
                }
                e.history
            })
            .unwrap_or_default();
        let (cut, visited) = pool.select_cut_counted(manifest, view.budget, |id, node| {
            if id == 0 {
                return view.root;
            }
            let previous = history.get(&id).copied().unwrap_or(false);
            let threshold = f64::from(view.policy.max_error_pixels)
                * if previous {
                    1. - f64::from(view.policy.hysteresis_ratio)
                } else {
                    1.
                };
            let refine = error(node) > threshold;
            history.insert(id, refine);
            refine
        });
        let entry = Entry {
            key,
            history,
            cut: cut.clone(),
        };
        if self
            .admitted
            .as_ref()
            .is_some_and(|set| !set.contains(&source))
        {
            self.stats.bypasses += 1;
            self.stash(source, entry);
        } else {
            if self.entries.len() == MAX_ENTRIES {
                let first = *self.entries.keys().next().unwrap();
                let old = self.entries.remove(&first).unwrap();
                self.stash(first, old);
                self.stats.evictions += 1;
            }
            self.entries.insert(source, entry);
        }
        self.update_counts();
        (cut, visited)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset_pages::{Lod, PageAdmission, hierarchy_tests::fixture, tests::document};
    fn view() -> CutView {
        CutView {
            vp: [0.; 16],
            size: [640, 480],
            model: [[0.; 4]; 4],
            policy: CpuLodPolicy::default(),
            root: true,
            budget: HierarchyCutBudget {
                max_visits: 64,
                max_draws: 32,
            },
        }
    }
    #[test]
    fn history_cache_invalidation_and_bounded_coverage() {
        let (manifest, bytes) = fixture();
        let d = document(manifest.reference());
        let mut pool = PagePool::new(1 << 20, 16);
        let mut cache = CutCache::default();
        cache.begin(d.scene().asset_layout_stamp());
        let (proxy, _) = cache.select(0, &manifest, &pool, view(), |_| 3.);
        assert_eq!(proxy[0].lod, Lod::BoundsProxy);
        for (n, bytes) in bytes.into_iter().enumerate() {
            let PageAdmission::Load(r) = pool.begin(&d, 1, &manifest, n as u32).unwrap() else {
                panic!()
            };
            pool.complete(
                &d,
                r,
                Ok(manifest.decode_page(n as u32, bytes).unwrap()),
                |p| p,
            )
            .unwrap();
        }
        let (fine, visits) = cache.select(0, &manifest, &pool, view(), |_| 3.);
        assert_eq!(fine.len(), 4);
        assert!(visits > 0);
        // Spatial repartitioning alone changes neither source identity nor model/page keys.
        let mut spatial = crate::spatial::SpatialIndex::default();
        spatial.rebuild(
            (0..64)
                .map(|source| (source, Some([0., 0., 0., 1., 1., 1.])))
                .collect(),
        );
        for source in 1..64 {
            spatial.refit(source, Some([10., 0., 0., 11., 1., 1.]));
        }
        assert!(spatial.rebalance());
        cache.begin(d.scene().asset_layout_stamp());
        let (hit, visits) = cache.select(0, &manifest, &pool, view(), |_| {
            panic!("cache hit must not assess bounds")
        });
        assert_eq!(hit, fine);
        assert_eq!(visits, 0);
        // Camera changes invalidate cut but preserve deep refinement in the dead band.
        let mut moved = view();
        moved.vp[0] = 1.;
        let (retained, _) = cache.select(0, &manifest, &pool, moved, |_| 1.8);
        assert_eq!(retained.len(), 4);
        let mut moved = view();
        moved.vp[0] = 2.;
        let (coarse, _) = cache.select(0, &manifest, &pool, moved, |_| 1.5);
        assert_eq!(coarse.len(), 2);
        let mut moved = view();
        moved.vp[0] = 3.;
        let (coarse, _) = cache.select(0, &manifest, &pool, moved, |_| 1.8);
        assert_eq!(coarse.len(), 2);
        // Transform, viewport and policy changes are cache misses.
        for mode in 0..3 {
            let mut changed = view();
            match mode {
                0 => changed.model[0][0] = 2.,
                1 => changed.size[0] = 800,
                _ => changed.policy.hysteresis_ratio = 0.1,
            }
            let (_, visits) = cache.select(0, &manifest, &pool, changed, |_| 3.);
            assert!(visits > 0);
        }
        let mut zero = view();
        zero.budget.max_visits = 0;
        let (root, visits) = cache.select(0, &manifest, &pool, zero, |_| panic!());
        assert_eq!(visits, 0);
        assert_eq!(root.len(), 1);
        assert_eq!(root[0].lod, Lod::Root);
        let mut root_only = view();
        root_only.root = false;
        let (root, _) = cache.select(0, &manifest, &pool, root_only, |_| 3.);
        assert_eq!(root[0].lod, Lod::Root);
        let mut one_draw = view();
        one_draw.budget.max_draws = 1;
        let (root, _) = cache.select(0, &manifest, &pool, one_draw, |_| 3.);
        assert_eq!(root.len(), 1);
        assert_eq!(root[0].lod, Lod::Root);
        pool.evict_detail(manifest.reference());
        let (root, _) = cache.select(0, &manifest, &pool, view(), |_| 3.);
        assert_eq!(root.len(), 1);
        assert_eq!(root[0].lod, Lod::Root);
        pool.release_asset(manifest.reference());
        let (proxy, _) = cache.select(0, &manifest, &pool, view(), |_| 3.);
        assert_eq!(proxy[0].lod, Lod::BoundsProxy);
        for source in 0..1000 {
            cache.select(source, &manifest, &pool, view(), |_| 3.);
        }
        assert_eq!(cache.stats().entries, MAX_ENTRIES);
        // Scene identity prevents packed source reuse from inheriting another scene's history.
        let other = document(manifest.reference());
        cache.begin(other.scene().asset_layout_stamp());
        assert_eq!(cache.stats().entries, 0);
        assert_eq!(cache.stats().hits, 0);
        pool.clear();
        assert!(pool.residency_revision() > 0);
    }
    #[test]
    fn cached_and_uncached_instances_share_actual_visit_budget() {
        let (manifest, bytes) = fixture();
        let d = document(manifest.reference());
        let mut pool = PagePool::new(1 << 20, 16);
        for (n, bytes) in bytes.into_iter().enumerate() {
            let PageAdmission::Load(r) = pool.begin(&d, 1, &manifest, n as u32).unwrap() else {
                panic!()
            };
            pool.complete(
                &d,
                r,
                Ok(manifest.decode_page(n as u32, bytes).unwrap()),
                |p| p,
            )
            .unwrap();
        }
        let mut cache = CutCache::default();
        for _ in 0..4 {
            cache.begin_visible(d.scene().asset_layout_stamp(), (0..100).map(|s| (s, 100)));
            let mut remaining = 13;
            for source in 0..100 {
                let mut v = view();
                v.budget.max_visits = remaining.min(8);
                let (cut, used) = cache.select(source, &manifest, &pool, v, |_| 3.);
                assert!(used <= remaining);
                remaining -= used;
                assert!(!cut.is_empty());
                assert!(cut.iter().all(|p| p.handle.is_some()));
            }
        }
        assert!(cache.stats().hits > 0);
    }
    #[test]
    fn priority_and_round_robin_admission_are_bounded_deterministic_and_fair() {
        let (m, _) = fixture();
        let d = document(m.reference());
        let mut a = CutCache::default();
        let mut b = CutCache::default();
        let mut seen = BTreeSet::new();
        for _ in 0..64 {
            let candidates = || (0..1000).map(|s| (s, if s < 200 { 100 } else { 1 }));
            a.begin_visible(d.scene().asset_layout_stamp(), candidates());
            b.begin_visible(d.scene().asset_layout_stamp(), candidates());
            assert_eq!(a.admitted, b.admitted);
            let admitted = a.admitted.as_ref().unwrap();
            assert_eq!(admitted.len(), MAX_ENTRIES);
            assert!(admitted.iter().filter(|&&s| s < 200).count() >= 128);
            seen.extend(admitted.iter().copied());
        }
        assert_eq!(
            seen.len(),
            1000,
            "even low-priority visible sources receive slots"
        );
        a.begin_visible(d.scene().asset_layout_stamp(), [(999, 1)]);
        assert_eq!(a.admitted.as_ref().unwrap(), &BTreeSet::from([999]));
        a.begin_visible(d.scene().asset_layout_stamp(), []);
        assert!(a.admitted.as_ref().unwrap().is_empty());
        // Cyclic ordering handles sparse source IDs and u32 wraparound.
        a.cursor = u32::MAX - 1;
        a.begin_visible(
            d.scene().asset_layout_stamp(),
            [(0, 1), (u32::MAX, 1), (17, 2)],
        );
        assert_eq!(a.admitted.as_ref().unwrap().len(), 3);
    }
    #[test]
    fn pressure_eviction_retains_recent_hysteresis_without_unbounded_history() {
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
        let mut cache = CutCache::default();
        let candidates = || (0..300).map(|s| (s, if s == 0 { 0 } else { 100 }));
        cache.begin_visible(d.scene().asset_layout_stamp(), candidates());
        assert_eq!(cache.select(0, &m, &pool, view(), |_| 3.).0.len(), 4);
        for _ in 0..7 {
            cache.begin_visible(d.scene().asset_layout_stamp(), candidates());
        }
        cache.begin_visible(d.scene().asset_layout_stamp(), candidates());
        assert!(!cache.admitted.as_ref().unwrap().contains(&0));
        assert!(cache.ghosts.contains_key(&0));
        let mut changed = view();
        changed.vp[0] = 1.;
        assert_eq!(cache.select(0, &m, &pool, changed, |_| 1.8).0.len(), 4);
        assert_eq!(cache.stats().history_restores, 1);
        assert_eq!(cache.stats().bypasses, 1);
        cache.begin_visible(d.scene().asset_layout_stamp(), [(0, 0)]);
        let mut changed = view();
        changed.vp[0] = 2.;
        assert_eq!(cache.select(0, &m, &pool, changed, |_| 1.8).0.len(), 4);
        assert!(cache.entries.contains_key(&0));
        // Bypass-only traffic cannot consume protected cut slots or grow ghost storage.
        for source in 1..1000 {
            cache.select(source, &m, &pool, view(), |_| 3.);
        }
        assert!(cache.entries.contains_key(&0));
        assert_eq!(cache.stats().entries, 1);
        assert_eq!(cache.stats().history_entries, MAX_ENTRIES);
        cache.begin_visible(d.scene().asset_layout_stamp(), [(0, 0)]);
        let mut changed = view();
        changed.vp[0] = 2.;
        assert_eq!(cache.select(0, &m, &pool, changed, |_| panic!()).1, 0);
        let other = document(m.reference());
        cache.begin_visible(other.scene().asset_layout_stamp(), [(0, 0)]);
        assert!(cache.entries.is_empty() && cache.ghosts.is_empty());
    }
    #[test]
    fn sequential_pressure_reuses_protected_slots_with_zero_visit_and_draw_allowances() {
        use crate::draw_budget::{DrawAllowance, DrawCost, ViewDrawBudget};
        let (m, bytes) = fixture();
        let d = document(m.reference());
        let mut pool = PagePool::new(1 << 20, 16);
        let PageAdmission::Load(r) = pool.begin(&d, 1, &m, 0).unwrap() else {
            panic!()
        };
        pool.complete(
            &d,
            r,
            Ok(m.decode_page(0, bytes[0].clone()).unwrap()),
            |p| p,
        )
        .unwrap();
        let baseline = DrawCost {
            clusters: 1,
            triangles: u64::from(m.page(0).unwrap().indices / 3),
        };
        let mut cache = CutCache::default();
        let mut retained = BTreeSet::new();
        for frame in 0..24 {
            cache.begin_visible(
                d.scene().asset_layout_stamp(),
                (0..300).map(|s| (s, if s < 128 { 100 } else { 0 })),
            );
            let mut allowance = DrawAllowance::new(
                ViewDrawBudget {
                    clusters: 0,
                    triangles: 0,
                },
                baseline.scaled(300),
            );
            for source in 0..300 {
                let mut v = view();
                v.budget.max_visits = 0;
                let (cut, visits) = cache.select(source, &m, &pool, v, |_| panic!());
                assert_eq!(visits, 0);
                assert_eq!(cut.len(), 1);
                assert_eq!(cut[0].lod, Lod::Root);
                assert!(allowance.admit(baseline, baseline, 1));
            }
            assert_eq!(allowance.stats().drawn, baseline.scaled(300));
            assert_eq!(cache.stats().entries, MAX_ENTRIES);
            assert!(cache.stats().history_entries <= MAX_ENTRIES);
            if frame > 0 {
                assert!(
                    cache.stats().hits >= 128,
                    "priority slots survive sequential bypass traffic"
                );
            }
            retained.extend(cache.entries.keys().copied());
        }
        assert_eq!(retained.len(), 300);
    }
}
