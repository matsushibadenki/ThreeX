//! Fixed-capacity page residency and two-level LOD selection, independent of GPU availability.
use crate::resource_handles::{ResourceHandle, ResourcePool};
use std::collections::BTreeMap;
use threex_core::{
    AssetJobGuard, AssetRef, Document,
    asset_pages::{AssetManifest, ValidatedPage},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageFailure {
    Capacity,
    RootMissing,
    MissingReference,
    ReadFailed,
    InvalidPage,
    Stale,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageRequest {
    handle: ResourceHandle,
    reference: AssetRef,
    level: u8,
}
impl PageRequest {
    pub fn reference(self) -> AssetRef {
        self.reference
    }
    pub fn level(self) -> u8 {
        self.level
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageAdmission {
    Resident(ResourceHandle),
    Load(PageRequest),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lod {
    Detail,
    Root,
    BoundsProxy,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageSelection {
    pub lod: Lod,
    pub handle: Option<ResourceHandle>,
    pub bounds: [f32; 6],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PagePoolStatus {
    pub reserved_bytes: u64,
    pub resident_bytes: u64,
    pub pending_pages: usize,
    pub resident_pages: usize,
}
struct Pending {
    guard: AssetJobGuard,
    request: PageRequest,
    bytes: u64,
}
struct Resident {
    handle: ResourceHandle,
    bytes: u64,
}
/// Payload bytes (ready + pending) and slot count are bounded for this pool's lifetime.
/// Factory output must not exceed the validated page byte size (e.g. raw GPU vertex/u32 index buffers).
pub struct PagePool<T> {
    resources: ResourcePool<T>,
    pending: BTreeMap<ResourceHandle, Pending>,
    ready: BTreeMap<(AssetRef, u8), Resident>,
    document: Option<threex_core::DocumentId>,
    max_bytes: u64,
    used: u64,
}
impl<T> PagePool<T> {
    pub fn new(max_bytes: u64, max_pages: u32) -> Self {
        Self {
            resources: ResourcePool::new(max_pages),
            pending: BTreeMap::new(),
            ready: BTreeMap::new(),
            document: None,
            max_bytes,
            used: 0,
        }
    }
    pub fn get(&self, handle: ResourceHandle) -> Option<&T> {
        self.resources.get(handle)
    }
    pub fn status(&self) -> PagePoolStatus {
        PagePoolStatus {
            reserved_bytes: self.used,
            resident_bytes: self.ready.values().map(|r| r.bytes).sum(),
            pending_pages: self.pending.len(),
            resident_pages: self.ready.len(),
        }
    }
    fn discard(&mut self, handle: ResourceHandle) {
        if let Some(p) = self.pending.remove(&handle) {
            self.used -= p.bytes;
            self.resources.release(handle);
        }
    }
    pub fn begin(
        &mut self,
        document: &Document,
        entity: u32,
        manifest: &AssetManifest,
        level: u8,
    ) -> Result<PageAdmission, PageFailure> {
        let id = document
            .document_id()
            .ok_or(PageFailure::MissingReference)?;
        if self.document.is_some_and(|old| old != id) {
            return Err(PageFailure::Stale);
        }
        // A changed Document never leaves old reservations consuming capacity indefinitely.
        let stale: Vec<_> = self
            .pending
            .iter()
            .filter_map(|(&h, p)| {
                (!document.accepts_asset_result(&p.guard, p.request.reference)).then_some(h)
            })
            .collect();
        for h in stale {
            self.discard(h);
        }
        let guard = document
            .asset_job_guard(entity)
            .ok_or(PageFailure::MissingReference)?;
        if guard.reference() != manifest.reference() {
            return Err(PageFailure::Stale);
        }
        let descriptor = manifest.page(level).ok_or(PageFailure::InvalidPage)?;
        self.document = Some(id);
        let key = (manifest.reference(), level);
        if let Some(r) = self.ready.get(&key) {
            return Ok(PageAdmission::Resident(r.handle));
        }
        if level != 0 && !self.ready.contains_key(&(key.0, 0)) {
            return Err(PageFailure::RootMissing);
        }
        if let Some(p) = self
            .pending
            .values()
            .find(|p| (p.request.reference, p.request.level) == key)
        {
            return Ok(PageAdmission::Load(p.request));
        }
        if descriptor.length > self.max_bytes.saturating_sub(self.used) {
            return Err(PageFailure::Capacity);
        }
        let handle = self.resources.reserve().ok_or(PageFailure::Capacity)?;
        let request = PageRequest {
            handle,
            reference: key.0,
            level,
        };
        self.used += descriptor.length;
        self.pending.insert(
            handle,
            Pending {
                guard,
                request,
                bytes: descriptor.length,
            },
        );
        Ok(PageAdmission::Load(request))
    }
    /// Completion is single-use. Failed/stale results release reservations before any factory call.
    pub fn complete(
        &mut self,
        document: &Document,
        request: PageRequest,
        result: Result<ValidatedPage, PageFailure>,
        create: impl FnOnce(ValidatedPage) -> T,
    ) -> Result<ResourceHandle, PageFailure> {
        if document.document_id() != self.document {
            return Err(PageFailure::Stale);
        }
        let Some(p) = self.pending.remove(&request.handle) else {
            return Err(PageFailure::Stale);
        };
        let valid =
            p.request == request && document.accepts_asset_result(&p.guard, request.reference);
        let page = result.and_then(|page| {
            if !valid {
                return Err(PageFailure::Stale);
            }
            if page.reference() != request.reference
                || page.level() != request.level
                || page.byte_len() != p.bytes
            {
                return Err(PageFailure::InvalidPage);
            }
            Ok(page)
        });
        match page {
            Ok(page) => {
                let value = create(page);
                self.resources
                    .publish(request.handle, value)
                    .unwrap_or_else(|_| unreachable!("private pending reservation"));
                self.ready.insert(
                    (request.reference, request.level),
                    Resident {
                        handle: request.handle,
                        bytes: p.bytes,
                    },
                );
                Ok(request.handle)
            }
            Err(error) => {
                self.used -= p.bytes;
                self.resources.release(request.handle);
                Err(if valid { error } else { PageFailure::Stale })
            }
        }
    }
    pub fn cancel(&mut self, request: PageRequest) -> bool {
        if !self
            .pending
            .get(&request.handle)
            .is_some_and(|p| p.request == request)
        {
            return false;
        }
        self.discard(request.handle);
        true
    }
    /// Detail is a whole-asset replacement. Root stays pinned until the asset is explicitly released.
    pub(crate) fn select_hierarchy(
        &self,
        manifest: &AssetManifest,
        hierarchy: &threex_core::lod_hierarchy::LodHierarchy,
        want_detail: bool,
    ) -> PageSelection {
        let node = hierarchy.select_one(
            3,
            |_, _| want_detail,
            |page| {
                u8::try_from(page)
                    .ok()
                    .is_some_and(|level| self.ready.contains_key(&(manifest.reference(), level)))
            },
        );
        let level = node.map(|n| hierarchy.nodes()[n].page as u8);
        PageSelection {
            lod: match level {
                Some(1) => Lod::Detail,
                Some(0) => Lod::Root,
                _ => Lod::BoundsProxy,
            },
            handle: level.and_then(|level| {
                self.ready
                    .get(&(manifest.reference(), level))
                    .map(|r| r.handle)
            }),
            bounds: manifest.bounds(),
        }
    }
    pub fn select(&self, manifest: &AssetManifest, want_detail: bool) -> PageSelection {
        for (level, lod) in [(1, Lod::Detail), (0, Lod::Root)] {
            if level == 1 && !want_detail {
                continue;
            }
            if let Some(r) = self.ready.get(&(manifest.reference(), level)) {
                return PageSelection {
                    lod,
                    handle: Some(r.handle),
                    bounds: manifest.bounds(),
                };
            }
        }
        PageSelection {
            lod: Lod::BoundsProxy,
            handle: None,
            bounds: manifest.bounds(),
        }
    }
    pub fn evict_detail(&mut self, reference: AssetRef) {
        let handles: Vec<_> = self
            .pending
            .iter()
            .filter_map(|(&h, p)| {
                (p.request.reference == reference && p.request.level == 1).then_some(h)
            })
            .collect();
        for h in handles {
            self.discard(h);
        }
        if let Some(r) = self.ready.remove(&(reference, 1)) {
            self.used -= r.bytes;
            self.resources.release(r.handle);
        }
    }
    pub fn release_asset(&mut self, reference: AssetRef) {
        self.evict_detail(reference);
        let handles: Vec<_> = self
            .pending
            .iter()
            .filter_map(|(&h, p)| (p.request.reference == reference).then_some(h))
            .collect();
        for h in handles {
            self.discard(h);
        }
        if let Some(r) = self.ready.remove(&(reference, 0)) {
            self.used -= r.bytes;
            self.resources.release(r.handle);
        }
    }
    pub fn clear(&mut self) {
        self.resources.clear();
        self.pending.clear();
        self.ready.clear();
        self.used = 0;
        self.document = None;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use threex_core::{
        AssetId, DocumentId, asset_command,
        asset_pages::{DESCRIPTOR_BYTES, HEADER_BYTES, PageDescriptor, page_hash},
    };
    pub(crate) fn fixture() -> (AssetManifest, [Vec<u8>; 2]) {
        let vertices = [
            [[0f32, 0., 0.], [1., 0., 0.], [0., 1., 0.]].to_vec(),
            vec![
                [0., 0., 0.],
                [1., 0., 0.],
                [0., 1., 0.],
                [0.5, 0., 0.],
                [0.5, 0.5, 0.],
                [0., 0.5, 0.],
            ],
        ];
        let indices = [vec![0u32, 1, 2], vec![0, 3, 5, 3, 1, 4, 5, 4, 2, 3, 4, 5]];
        let payload: [Vec<u8>; 2] = std::array::from_fn(|i| {
            let mut b = Vec::new();
            for v in &vertices[i] {
                for x in v {
                    b.extend(x.to_le_bytes());
                }
            }
            for x in &indices[i] {
                b.extend(x.to_le_bytes());
            }
            b
        });
        let mut offset = (HEADER_BYTES + 2 * DESCRIPTOR_BYTES) as u64;
        let pages: Vec<_> = (0..2)
            .map(|i| {
                let p = PageDescriptor {
                    offset,
                    length: payload[i].len() as u64,
                    vertices: vertices[i].len() as u32,
                    indices: indices[i].len() as u32,
                    error: if i == 0 { 1. } else { 0. },
                    hash: page_hash(&payload[i]),
                };
                offset += p.length;
                p
            })
            .collect();
        let (reference, bytes) = AssetManifest::encode(
            AssetId::from_bytes([1; 16]).unwrap(),
            1,
            [0., 0., 0., 1., 1., 0.],
            offset,
            &pages,
        )
        .unwrap();
        (AssetManifest::decode(&bytes, reference).unwrap(), payload)
    }
    pub(crate) fn document(reference: AssetRef) -> Document {
        let mut d = Document::default();
        d.bind_document_id(DocumentId::from_bytes([2; 16]).unwrap())
            .unwrap();
        let mut create = [0u8; 64];
        create[..4].copy_from_slice(&1u32.to_le_bytes());
        create[4..8].copy_from_slice(&1u32.to_le_bytes());
        let mut update = create;
        update[..4].copy_from_slice(&2u32.to_le_bytes());
        for at in [32, 36, 40, 44, 48, 52] {
            update[at..at + 4].copy_from_slice(&1f32.to_le_bytes());
        }
        update[56..60].copy_from_slice(&7u32.to_le_bytes());
        d.commit(
            0,
            &[create, update, asset_command(1, Some(reference))].concat(),
        )
        .unwrap();
        d
    }
    fn load<T>(pool: &mut PagePool<T>, d: &Document, m: &AssetManifest, level: u8) -> PageRequest {
        let PageAdmission::Load(r) = pool.begin(d, 1, m, level).unwrap() else {
            panic!()
        };
        r
    }
    #[test]
    fn reservations_count_root_capacity_failures_and_proxy_fallback() {
        let (m, payload) = fixture();
        let d = document(m.reference());
        for (bytes, pages) in [(0, 2), (1000, 0), (47, 2)] {
            let mut pool = PagePool::<ValidatedPage>::new(bytes, pages);
            assert_eq!(pool.begin(&d, 1, &m, 0), Err(PageFailure::Capacity));
            assert_eq!(pool.select(&m, true).lod, Lod::BoundsProxy);
            assert_eq!(pool.select(&m, true).bounds, m.bounds());
            assert_eq!(pool.status().reserved_bytes, 0);
        }
        let mut pool = PagePool::new(48, 2);
        assert_eq!(pool.begin(&d, 1, &m, 1), Err(PageFailure::RootMissing));
        let request = load(&mut pool, &d, &m, 0);
        assert_eq!(pool.begin(&d, 1, &m, 0), Ok(PageAdmission::Load(request)));
        assert_eq!(pool.status().pending_pages, 1);
        assert_eq!(pool.status().reserved_bytes, 48);
        assert_eq!(
            pool.complete(&d, request, Err(PageFailure::ReadFailed), |p| p),
            Err(PageFailure::ReadFailed)
        );
        assert_eq!(pool.status().reserved_bytes, 0);
        let retry = load(&mut pool, &d, &m, 0);
        let handle = pool
            .complete(
                &d,
                retry,
                Ok(m.decode_page(0, payload[0].clone()).unwrap()),
                |p| p,
            )
            .unwrap();
        assert_eq!(pool.select(&m, true).handle, Some(handle));
        assert_eq!(pool.select(&m, true).lod, Lod::Root);
        assert_eq!(pool.begin(&d, 1, &m, 1), Err(PageFailure::Capacity));
        assert_eq!(pool.status().resident_bytes, 48);
        let mut count_pool = PagePool::new(1000, 1);
        let r = load(&mut count_pool, &d, &m, 0);
        count_pool
            .complete(
                &d,
                r,
                Ok(m.decode_page(0, payload[0].clone()).unwrap()),
                |p| p,
            )
            .unwrap();
        assert_eq!(count_pool.begin(&d, 1, &m, 1), Err(PageFailure::Capacity));
    }
    #[test]
    fn root_detail_release_reuse_and_stale_async_results() {
        let (m, payload) = fixture();
        let mut d = document(m.reference());
        let mut pool = PagePool::new(168, 2);
        let cancelled = load(&mut pool, &d, &m, 0);
        assert!(pool.cancel(cancelled));
        let root_request = load(&mut pool, &d, &m, 0);
        assert_eq!(
            pool.complete(
                &d,
                cancelled,
                Ok(m.decode_page(0, payload[0].clone()).unwrap()),
                |_| panic!("stale factory")
            ),
            Err(PageFailure::Stale)
        );
        let root = pool
            .complete(
                &d,
                root_request,
                Ok(m.decode_page(0, payload[0].clone()).unwrap()),
                |p| p,
            )
            .unwrap();
        assert_eq!(pool.begin(&d, 1, &m, 0), Ok(PageAdmission::Resident(root)));
        let request = load(&mut pool, &d, &m, 1);
        assert_eq!(pool.select(&m, true).lod, Lod::Root);
        assert_eq!(pool.status().reserved_bytes, 168);
        assert_eq!(
            pool.complete(&d, request, Err(PageFailure::ReadFailed), |_| panic!(
                "failure factory"
            )),
            Err(PageFailure::ReadFailed)
        );
        assert_eq!(pool.select(&m, true).lod, Lod::Root);
        let request = load(&mut pool, &d, &m, 1);
        let detail = pool
            .complete(
                &d,
                request,
                Ok(m.decode_page(1, payload[1].clone()).unwrap()),
                |p| p,
            )
            .unwrap();
        assert_eq!(pool.select(&m, true).handle, Some(detail));
        assert_eq!(pool.select(&m, false).handle, Some(root));
        assert_eq!(
            pool.complete(
                &d,
                request,
                Ok(m.decode_page(1, payload[1].clone()).unwrap()),
                |_| panic!("duplicate factory")
            ),
            Err(PageFailure::Stale)
        );
        pool.evict_detail(m.reference());
        assert!(pool.get(detail).is_none());
        assert!(pool.get(root).is_some());
        let stale = load(&mut pool, &d, &m, 1);
        d.commit(1, &asset_command(1, None)).unwrap();
        d.undo(2).unwrap();
        assert_eq!(
            pool.complete(
                &d,
                stale,
                Ok(m.decode_page(1, payload[1].clone()).unwrap()),
                |_| panic!("ABA factory")
            ),
            Err(PageFailure::Stale)
        );
        let fresh = load(&mut pool, &d, &m, 1);
        let restored = Document::from_checkpoint(&d.checkpoint().unwrap()).unwrap();
        assert_eq!(
            pool.complete(
                &restored,
                fresh,
                Ok(m.decode_page(1, payload[1].clone()).unwrap()),
                |_| panic!("restart factory")
            ),
            Err(PageFailure::Stale)
        );
        let pending = load(&mut pool, &restored, &m, 1);
        pool.clear();
        assert!(pool.get(root).is_none());
        assert_eq!(
            pool.complete(
                &restored,
                pending,
                Err(PageFailure::ReadFailed),
                |_| panic!()
            ),
            Err(PageFailure::Stale)
        );
        let next = load(&mut pool, &restored, &m, 0);
        let root2 = pool
            .complete(
                &restored,
                next,
                Ok(m.decode_page(0, payload[0].clone()).unwrap()),
                |p| p,
            )
            .unwrap();
        assert_ne!(root, root2);
        pool.release_asset(m.reference());
        assert!(pool.get(root2).is_none());
        assert_eq!(pool.status().reserved_bytes, 0);
    }
    #[test]
    fn page_lifecycle_preserves_sparse_instances_and_cached_gather_indices() {
        use crate::{
            batching::GeometryBatches,
            gather::{PlanCache, Prepared},
        };
        let (m, payload) = fixture();
        let mut d = document(m.reference());
        let mut commands = Vec::new();
        for id in 2..=1200u32 {
            let mut create = [0; 64];
            create[..4].copy_from_slice(&1u32.to_le_bytes());
            create[4..8].copy_from_slice(&id.to_le_bytes());
            commands.extend(create);
        }
        d.commit(1, &commands).unwrap();
        commands.clear();
        for id in 1..=1200u32 {
            let mut update = [0; 64];
            update[..4].copy_from_slice(&2u32.to_le_bytes());
            update[4..8].copy_from_slice(&id.to_le_bytes());
            for at in [32, 36, 40, 44, 48, 52] {
                update[at..at + 4].copy_from_slice(&1f32.to_le_bytes());
            }
            update[56..60].copy_from_slice(&(3 | ((id % 3) << 2)).to_le_bytes());
            commands.extend(update);
        }
        d.commit(2, &commands).unwrap();
        let stamp = d.scene().stamp();
        let mut batches = GeometryBatches::default();
        batches.sync(d.scene().instances(), std::iter::once(0..1200));
        let revision = batches.revision(1);
        let mut views = AssetDrawLayout::default();
        assert!(views.sync(d.scene(), &batches, true, false, |r| r == m.reference()));
        assert_eq!(
            views.draws[&m.reference()]
                .iter()
                .flat_map(|r| r.clone())
                .collect::<Vec<_>>(),
            vec![0]
        );
        let (kind, offset) = batches.bank_slot(0);
        assert!(views.excluded[kind].contains(&offset));
        let mut plans = PlanCache::default();
        let limits = wgpu::Limits {
            max_storage_buffer_binding_size: 2560,
            ..Default::default()
        };
        assert!(matches!(
            plans.prepare(&limits, [100000, 100000], &batches, 1, true),
            Prepared::Gather { upload: true, .. }
        ));
        let mut pool = PagePool::new(168, 2);
        let root = load(&mut pool, &d, &m, 0);
        pool.complete(
            &d,
            root,
            Ok(m.decode_page(0, payload[0].clone()).unwrap()),
            |p| p,
        )
        .unwrap();
        let detail = load(&mut pool, &d, &m, 1);
        pool.complete(&d, detail, Err(PageFailure::ReadFailed), |p| p)
            .unwrap_err();
        batches.sync(
            d.scene().instances(),
            d.scene().instance_updates_since(Some(&stamp), false).ranges,
        );
        assert_eq!(batches.revision(1), revision);
        assert_eq!(batches.transfer_count(1, false), 0);
        assert!(!views.sync(d.scene(), &batches, true, false, |_| true));
        assert!(matches!(
            plans.prepare(&limits, [100000, 100000], &batches, 1, true),
            Prepared::Gather { upload: false, .. }
        ));
        let mut rgb = commands[..64].to_vec();
        rgb[44..48].copy_from_slice(&0.5f32.to_le_bytes());
        d.commit(3, &rgb).unwrap();
        let ranges = d.scene().instance_updates_since(Some(&stamp), false).ranges;
        assert_eq!(ranges.len(), 1);
        batches.sync(d.scene().instances(), ranges);
        assert_eq!(batches.transfer_count(1, false), 1);
        assert!(!views.sync(d.scene(), &batches, true, false, |_| true));
        assert_eq!(batches.revision(1), revision);
        assert!(matches!(
            plans.prepare(&limits, [100000, 100000], &batches, 1, false),
            Prepared::Copies(_)
        ));
        assert!(matches!(
            plans.prepare(&limits, [100000, 100000], &batches, 1, true),
            Prepared::Gather { upload: false, .. }
        ));
        rgb[56..60].copy_from_slice(&11u32.to_le_bytes());
        let prior = d.scene().stamp();
        d.commit(4, &rgb).unwrap();
        batches.sync(
            d.scene().instances(),
            d.scene().instance_updates_since(Some(&prior), false).ranges,
        );
        assert!(views.sync(d.scene(), &batches, true, false, |_| true));
        let (kind, offset) = batches.bank_slot(0);
        assert_eq!(kind, 2);
        assert!(views.excluded[2].contains(&offset));
        rgb[56..60].copy_from_slice(&10u32.to_le_bytes());
        let prior = d.scene().stamp();
        d.commit(5, &rgb).unwrap();
        batches.sync(
            d.scene().instances(),
            d.scene().instance_updates_since(Some(&prior), false).ranges,
        );
        assert!(views.sync(d.scene(), &batches, true, false, |_| true));
        assert!(views.draws.is_empty());
        rgb[56..60].copy_from_slice(&11u32.to_le_bytes());
        let prior = d.scene().stamp();
        d.commit(6, &rgb).unwrap();
        batches.sync(
            d.scene().instances(),
            d.scene().instance_updates_since(Some(&prior), false).ranges,
        );
        assert!(views.sync(d.scene(), &batches, true, false, |_| true));
        let source = d.scene().instance_index(1).unwrap();
        assert_eq!(
            views.draws[&m.reference()]
                .iter()
                .flat_map(|r| r.clone())
                .collect::<Vec<_>>(),
            vec![source as u32]
        );
    }
    #[test]
    fn exclusion_ranges_and_foreign_document_do_not_duplicate_or_steal_slots() {
        use std::collections::BTreeSet;
        for excluded in [
            BTreeSet::new(),
            BTreeSet::from([0, 2, 4, 9]),
            (0..10).collect(),
        ] {
            for range in [0..10, 1..4, 4..4, 9..10] {
                let expected: Vec<_> = range.clone().filter(|i| !excluded.contains(i)).collect();
                let actual: Vec<_> = primitive_ranges(range, &excluded).flatten().collect();
                assert_eq!(actual, expected);
            }
        }
        let (m, payload) = fixture();
        let d = document(m.reference());
        let mut other = Document::default();
        other
            .bind_document_id(DocumentId::from_bytes([7; 16]).unwrap())
            .unwrap();
        let mut pool = PagePool::new(48, 1);
        let request = load(&mut pool, &d, &m, 0);
        assert_eq!(pool.begin(&other, 1, &m, 0), Err(PageFailure::Stale));
        assert_eq!(pool.status().pending_pages, 1);
        assert_eq!(
            pool.complete(
                &other,
                request,
                Err(PageFailure::ReadFailed),
                |_| -> ValidatedPage { panic!() }
            ),
            Err(PageFailure::Stale)
        );
        assert_eq!(pool.status().pending_pages, 1);
        assert_eq!(
            pool.complete(
                &d,
                request,
                Ok(m.decode_page(1, payload[1].clone()).unwrap()),
                |_| -> ValidatedPage { panic!("wrong level factory") }
            ),
            Err(PageFailure::InvalidPage)
        );
        assert_eq!(pool.status().reserved_bytes, 0);
        let request = load(&mut pool, &d, &m, 0);
        pool.complete(
            &d,
            request,
            Ok(m.decode_page(0, payload[0].clone()).unwrap()),
            |p| p,
        )
        .unwrap();
    }
}

/// Split primitive draw ranges around asset overrides, without scanning Instance payloads.
pub(crate) fn primitive_ranges(
    range: std::ops::Range<u32>,
    excluded: &std::collections::BTreeSet<u32>,
) -> impl Iterator<Item = std::ops::Range<u32>> + '_ {
    let mut at = range.start;
    let mut skip = excluded.range(range.clone());
    std::iter::from_fn(move || {
        for &offset in skip.by_ref() {
            let start = at;
            at = offset + 1;
            if start < offset {
                return Some(start..offset);
            }
        }
        let start = at;
        at = range.end;
        (start < range.end).then_some(start..range.end)
    })
}

#[derive(Default)]
pub(crate) struct AssetDrawLayout {
    pub draws: BTreeMap<AssetRef, Vec<std::ops::Range<u32>>>,
    pub excluded: [std::collections::BTreeSet<u32>; 3],
    key: Option<(threex_core::SceneStamp, [u64; 3], bool)>,
}
impl AssetDrawLayout {
    pub fn sync(
        &mut self,
        scene: &threex_core::Scene,
        batches: &crate::batching::GeometryBatches,
        banked: bool,
        dirty: bool,
        registered: impl Fn(AssetRef) -> bool,
    ) -> bool {
        let key = (
            scene.asset_layout_stamp(),
            std::array::from_fn(|k| batches.revision(k)),
            banked,
        );
        if !dirty && self.key.as_ref() == Some(&key) {
            return false;
        }
        self.draws.clear();
        self.excluded
            .iter_mut()
            .for_each(std::collections::BTreeSet::clear);
        let mut groups = BTreeMap::<AssetRef, std::collections::BTreeSet<u32>>::new();
        for (reference, index) in scene.asset_instances() {
            if !registered(reference) {
                continue;
            }
            let (kind, offset) = batches.bank_slot(index);
            self.excluded[kind].insert(if banked { offset } else { index as u32 });
            groups.entry(reference).or_default().insert(index as u32);
        }
        for (reference, indices) in groups {
            let mut ranges: Vec<std::ops::Range<u32>> = Vec::new();
            for index in indices {
                if let Some(last) = ranges.last_mut()
                    && last.end == index
                {
                    last.end += 1;
                } else {
                    ranges.push(index..index + 1);
                }
            }
            self.draws.insert(reference, ranges);
        }
        self.key = Some(key);
        true
    }
}

#[cfg(test)]
mod generated_parent_tests {
    use super::*;
    use crate::{
        batching::GeometryBatches,
        gather::{PlanCache, Prepared},
    };
    use threex_core::{
        AssetId,
        asset_pages::{DESCRIPTOR_BYTES, HEADER_BYTES, PageDescriptor, page_hash},
        cluster_builder::{ClusterBudget, ParentGeometryBudget, SimplifiedClusters},
        lod_hierarchy::LodHierarchy,
        simplify::SimplifyBudget,
    };
    #[test]
    fn generated_parent_roundtrip_keeps_fallback_sparse_uploads_and_resident_gather_indices() {
        let vertices = [
            [-0.5, -0.5, 0.],
            [0.5, -0.5, 0.],
            [0.5, 0.5, 0.],
            [-0.5, 0.5, 0.],
            [0., 0., 0.1],
        ];
        let triangles = [[0, 1, 4], [1, 2, 4], [2, 3, 4], [3, 0, 4]];
        let parents = SimplifiedClusters::build(
            &vertices,
            &triangles,
            &[],
            ClusterBudget {
                leaf_triangles: 2,
                max_triangles: 4,
                max_nodes: 3,
            },
            ParentGeometryBudget {
                simplify: SimplifyBudget {
                    target_triangles: 2,
                    max_error: 1.,
                    max_attempts: 32,
                    max_input_triangles: 4,
                },
                max_source_vertices: 5,
                max_stored_triangles: 8,
                max_total_attempts: 32,
            },
        )
        .unwrap();
        assert_eq!(parents.meshes[0].triangles.len(), 2);
        let payload: Vec<Vec<u8>> = [parents.meshes[0].triangles.as_slice(), triangles.as_slice()]
            .into_iter()
            .map(|triangles| {
                let mut bytes = Vec::new();
                for v in &vertices {
                    for x in v {
                        bytes.extend(x.to_le_bytes());
                    }
                }
                for t in triangles {
                    for i in t {
                        bytes.extend(i.to_le_bytes());
                    }
                }
                bytes
            })
            .collect();
        let mut offset = (HEADER_BYTES + 2 * DESCRIPTOR_BYTES) as u64;
        let pages: Vec<_> = (0..2)
            .map(|i| {
                let p = PageDescriptor {
                    offset,
                    length: payload[i].len() as u64,
                    vertices: 5,
                    indices: if i == 0 { 6 } else { 12 },
                    error: if i == 0 {
                        parents.hierarchy.nodes()[0].error
                    } else {
                        0.
                    },
                    hash: page_hash(&payload[i]),
                };
                offset += p.length;
                p
            })
            .collect();
        let (reference, encoded) = AssetManifest::encode(
            AssetId::from_bytes([25; 16]).unwrap(),
            1,
            parents.hierarchy.nodes()[0].bounds,
            offset,
            &pages,
        )
        .unwrap();
        let manifest = AssetManifest::decode(&encoded, reference).unwrap();
        let hierarchy = LodHierarchy::from_manifest(&manifest);
        let document = tests::document(reference);
        let mut data = vec![document.scene().instances()[0]; 1200];
        for (i, item) in data.iter_mut().enumerate() {
            item.color[3] = (1 + i % 3) as f32;
        }
        let mut batches = GeometryBatches::default();
        batches.sync(&data, std::iter::once(0..data.len()));
        let revision = batches.revision(0);
        let mut cache = PlanCache::default();
        let limits = wgpu::Limits {
            max_storage_buffer_binding_size: 2560,
            ..Default::default()
        };
        assert!(matches!(
            cache.prepare(&limits, [100000, 100000], &batches, 0, true),
            Prepared::Gather { upload: true, .. }
        ));
        let mut pool = PagePool::new(offset, 2);
        assert_eq!(
            pool.select_hierarchy(&manifest, &hierarchy, true).lod,
            Lod::BoundsProxy
        );
        let PageAdmission::Load(root) = pool.begin(&document, 1, &manifest, 0).unwrap() else {
            panic!()
        };
        pool.complete(
            &document,
            root,
            Ok(manifest.decode_page(0, payload[0].clone()).unwrap()),
            |p| p,
        )
        .unwrap();
        assert_eq!(
            pool.select_hierarchy(&manifest, &hierarchy, true).lod,
            Lod::Root
        );
        let PageAdmission::Load(detail) = pool.begin(&document, 1, &manifest, 1).unwrap() else {
            panic!()
        };
        pool.complete(&document, detail, Err(PageFailure::ReadFailed), |p| p)
            .unwrap_err();
        assert_eq!(
            pool.select_hierarchy(&manifest, &hierarchy, true).lod,
            Lod::Root
        );
        let PageAdmission::Load(detail) = pool.begin(&document, 1, &manifest, 1).unwrap() else {
            panic!()
        };
        pool.complete(
            &document,
            detail,
            Ok(manifest.decode_page(1, payload[1].clone()).unwrap()),
            |p| p,
        )
        .unwrap();
        assert_eq!(
            pool.select_hierarchy(&manifest, &hierarchy, true).lod,
            Lod::Detail
        );
        assert_eq!(
            pool.select_hierarchy(&manifest, &hierarchy, false).lod,
            Lod::Root
        );
        pool.evict_detail(reference);
        assert_eq!(
            pool.select_hierarchy(&manifest, &hierarchy, true).lod,
            Lod::Root
        );
        batches.sync(&data, std::iter::empty());
        assert_eq!(batches.transfer_count(0, false), 0);
        assert!(matches!(
            cache.prepare(&limits, [100000, 100000], &batches, 0, true),
            Prepared::Gather { upload: false, .. }
        ));
        data[0].color[0] = 0.25;
        batches.sync(&data, std::iter::once(0..1));
        assert_eq!(batches.revision(0), revision);
        assert_eq!(batches.transfer_count(0, false), 1);
        assert!(matches!(
            cache.prepare(&limits, [100000, 100000], &batches, 0, true),
            Prepared::Gather { upload: false, .. }
        ));
        pool.release_asset(reference);
        assert_eq!(
            pool.select_hierarchy(&manifest, &hierarchy, true).lod,
            Lod::BoundsProxy
        );
        let mut empty = PagePool::<ValidatedPage>::new(0, 0);
        assert_eq!(
            empty.begin(&document, 1, &manifest, 0),
            Err(PageFailure::Capacity)
        );
    }
}
