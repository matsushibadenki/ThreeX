use super::*;
use crate::asset_loader::{DiskSource, Loaded, PageSource};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use threex_core::{
    AssetId, AssetRef, Document, asset_command,
    asset_pages::{
        AssetManifest, DESCRIPTOR_BYTES, HEADER_BYTES, PageDescriptor, ValidatedPage, page_hash,
    },
};
use threex_renderer::{
    asset_pages::{Lod, PageAdmission, PageFailure, PagePool},
    native::{
        AcquireFailure, AssetDelivery, NativeSurfaces, SurfaceBackend, SurfaceFailure,
        SurfaceHandle,
    },
};
static SERIAL: AtomicU64 = AtomicU64::new(1);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "threex-loader-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn fixture() -> (AssetRef, Vec<u8>) {
    let mut page = Vec::new();
    for v in [[0f32, 0., 0.], [1., 0., 0.], [0., 1., 0.]] {
        for x in v {
            page.extend(x.to_le_bytes());
        }
    }
    for i in [0u32, 1, 2] {
        page.extend(i.to_le_bytes());
    }
    let offset = (HEADER_BYTES + 2 * DESCRIPTOR_BYTES) as u64;
    let root = PageDescriptor {
        offset,
        length: page.len() as u64,
        vertices: 3,
        indices: 3,
        error: 1.,
        hash: page_hash(&page),
    };
    let detail = PageDescriptor {
        offset: offset + root.length,
        error: 0.,
        ..root
    };
    let (reference, mut bytes) = AssetManifest::encode(
        AssetId::from_bytes([6; 16]).unwrap(),
        1,
        [0., 0., 0., 1., 1., 0.],
        detail.offset + detail.length,
        &[root, detail],
    )
    .unwrap();
    bytes.extend(&page);
    bytes.extend(page);
    (reference, bytes)
}
#[derive(Default)]
struct GateState {
    open: bool,
    started: usize,
    active: usize,
    peak: usize,
}
#[derive(Default)]
struct Gate {
    state: Mutex<GateState>,
    changed: Condvar,
}
impl Gate {
    fn wait_started(&self, count: usize) {
        let mut s = self.state.lock().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while s.started < count {
            let (next, timeout) = self
                .changed
                .wait_timeout(s, deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            s = next;
            assert!(!timeout.timed_out(), "loader did not start");
        }
    }
    fn open(&self) {
        self.state.lock().unwrap().open = true;
        self.changed.notify_all();
    }
}
struct GatedSource(Arc<Gate>);
impl PageSource for GatedSource {
    fn read(&self, path: &Path, reference: AssetRef, level: u32) -> Loaded {
        {
            let mut s = self.0.state.lock().unwrap();
            s.started += 1;
            s.active += 1;
            s.peak = s.peak.max(s.active);
            self.0.changed.notify_all();
            while !s.open {
                s = self.0.changed.wait(s).unwrap();
            }
        }
        assert!(
            std::thread::current()
                .name()
                .unwrap()
                .starts_with("threex-asset-")
        );
        let result = DiskSource.read(path, reference, level);
        self.0.state.lock().unwrap().active -= 1;
        result
    }
}
type CutLog = Arc<Mutex<Vec<(u32, Vec<u32>)>>>;
struct Backend {
    pool: PagePool<ValidatedPage>,
    manifests: BTreeMap<AssetRef, AssetManifest>,
    log: Arc<Mutex<Vec<Lod>>>,
    cuts: Option<CutLog>,
}
impl SurfaceBackend for Backend {
    fn configure(&mut self, _w: u32, _h: u32) -> Result<(), SurfaceFailure> {
        Ok(())
    }
    fn draw(
        &mut self,
        _scene: &threex_core::Scene,
        _camera: &[f32; 16],
    ) -> Result<(), AcquireFailure> {
        Ok(())
    }
    fn upload_asset(
        &mut self,
        d: &Document,
        entity: u32,
        m: AssetManifest,
        level: u32,
        page: Result<ValidatedPage, PageFailure>,
    ) -> Result<AssetDelivery, SurfaceFailure> {
        assert_eq!(std::thread::current().name(), Some("threex-document"));
        let reference = m.reference();
        let registered = self.manifests.insert(reference, m.clone()).is_none();
        let mut changed = false;
        let result = match self.pool.begin(d, entity, &m, level) {
            Ok(PageAdmission::Resident(_)) => Ok(()),
            Ok(PageAdmission::Load(r)) => self.pool.complete(d, r, page, |p| p).map(|_| {
                changed = true;
            }),
            Err(e) => Err(e),
        };
        if let Some(cuts) = &self.cuts {
            let selected = self.pool.select_cut(&m, Default::default(), |_, _| true);
            let ids = selected
                .iter()
                .filter_map(|s| s.handle.and_then(|h| self.pool.get(h)).map(|p| p.level()))
                .collect();
            cuts.lock().unwrap().push((level, ids));
        }
        self.log
            .lock()
            .unwrap()
            .push(self.pool.select(&m, true).lod);
        Ok(AssetDelivery {
            changed: changed || registered,
            ready: result.is_ok(),
            failure: result.err(),
        })
    }
}
fn backend(bytes: u64, log: Arc<Mutex<Vec<Lod>>>) -> Box<dyn SurfaceBackend> {
    Box::new(Backend {
        pool: PagePool::new(bytes, 16),
        manifests: BTreeMap::new(),
        cuts: None,
        log,
    })
}
fn call(c: &WorkerClient, next: &mut u64, action: WorkerAction) -> WorkerOutcome {
    *next += 1;
    c.try_submit(WorkerRequest { id: *next, action })
        .unwrap()
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .result
        .unwrap()
}
fn bind(c: &WorkerClient, next: &mut u64, reference: AssetRef, count: u32) {
    let mut commands = Vec::new();
    for id in 1..=count {
        let mut b = [0; 64];
        b[..4].copy_from_slice(&1u32.to_le_bytes());
        b[4..8].copy_from_slice(&id.to_le_bytes());
        commands.extend(b);
        commands.extend(asset_command(id, Some(reference)));
    }
    call(
        c,
        next,
        WorkerAction::Commit {
            expected_revision: 0,
            commands,
        },
    );
}
fn load(
    c: &WorkerClient,
    next: &mut u64,
    target: SurfaceHandle,
    entity: u32,
    path: PathBuf,
    detail: bool,
) -> (
    AssetLoadStatus,
    Option<threex_renderer::resource_handles::ResourceHandle>,
) {
    let WorkerOutcome::AssetLoad { status, ticket, .. } = call(
        c,
        next,
        WorkerAction::LoadAsset {
            target,
            entity,
            path,
            detail,
        },
    ) else {
        panic!()
    };
    (status, ticket)
}
fn idle(c: &WorkerClient, next: &mut u64) -> AssetLoaderStats {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let WorkerOutcome::AssetLoads { stats, .. } =
            call(c, next, WorkerAction::InspectAssetLoads)
        else {
            panic!()
        };
        if stats.pending == 0 {
            return stats;
        }
        assert!(Instant::now() < deadline, "loader did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
}
#[test]
fn io_is_off_owner_edit_undo_and_surface_replacement_reject_late_results() {
    let t = Temp::new();
    let (reference, bytes) = fixture();
    let path = t.0.join("asset.bin");
    fs::write(&path, bytes).unwrap();
    let gate = Arc::new(Gate::default());
    let surfaces = NativeSurfaces::default();
    let log = Arc::new(Mutex::new(Vec::new()));
    let target = surfaces
        .attach(1, backend(96, log.clone()), 64, 64)
        .unwrap();
    let wakes = Arc::new(AtomicUsize::new(0));
    let wake = wakes.clone();
    surfaces.set_asset_waker(Arc::new(move |_| {
        wake.fetch_add(1, Ordering::Relaxed);
    }));
    let worker = JournalWorker::start_native(
        DocumentStore::new(t.0.join("doc")),
        WorkerConfig::default(),
        surfaces.clone(),
        Arc::new(GatedSource(gate.clone())),
    )
    .unwrap();
    let c = worker.client();
    let mut next = 0;
    bind(&c, &mut next, reference, 1);
    assert_eq!(
        load(&c, &mut next, target, 1, path.clone(), false).0,
        AssetLoadStatus::Queued
    );
    gate.wait_started(1);
    // These acknowledged edits complete while the only read is deliberately blocked.
    call(
        &c,
        &mut next,
        WorkerAction::Commit {
            expected_revision: 1,
            commands: asset_command(1, None).to_vec(),
        },
    );
    call(
        &c,
        &mut next,
        WorkerAction::Undo {
            expected_revision: 2,
        },
    );
    gate.open();
    let stats = idle(&c, &mut next);
    assert_eq!(stats.stale, 1);
    assert!(log.lock().unwrap().is_empty());
    assert_eq!(wakes.load(Ordering::Relaxed), 0);
    // A stale native target is rejected before starting I/O.
    let replacement = surfaces
        .attach(1, backend(96, log.clone()), 64, 64)
        .unwrap();
    assert_eq!(
        load(&c, &mut next, target, 1, path.clone(), false).0,
        AssetLoadStatus::StaleTarget
    );
    assert_eq!(
        load(&c, &mut next, replacement, 1, path, false).0,
        AssetLoadStatus::Queued
    );
    let stats = idle(&c, &mut next);
    assert_eq!(stats.completed, 1);
    assert_eq!(wakes.load(Ordering::Relaxed), 1);
    worker.shutdown().unwrap();
}
#[test]
fn eight_leases_bound_staging_coalesce_and_shutdown_drains_blocked_work() {
    let t = Temp::new();
    let (reference, bytes) = fixture();
    let path = t.0.join("asset.bin");
    fs::write(&path, bytes).unwrap();
    let gate = Arc::new(Gate::default());
    let surfaces = NativeSurfaces::default();
    let log = Arc::new(Mutex::new(Vec::new()));
    let target = surfaces
        .attach(1, backend(96, log.clone()), 64, 64)
        .unwrap();
    let worker = JournalWorker::start_native(
        DocumentStore::new(t.0.join("doc")),
        WorkerConfig::default(),
        surfaces,
        Arc::new(GatedSource(gate.clone())),
    )
    .unwrap();
    let c = worker.client();
    let mut next = 0;
    bind(&c, &mut next, reference, 9);
    let first = load(&c, &mut next, target, 1, path.clone(), false);
    for entity in 2..=8 {
        assert_eq!(
            load(&c, &mut next, target, entity, path.clone(), false).0,
            AssetLoadStatus::Queued
        );
    }
    gate.wait_started(2);
    assert_eq!(load(&c, &mut next, target, 1, path.clone(), false), first);
    assert_eq!(
        load(&c, &mut next, target, 9, path, false).0,
        AssetLoadStatus::Busy
    );
    let WorkerOutcome::AssetLoads { stats, .. } =
        call(&c, &mut next, WorkerAction::InspectAssetLoads)
    else {
        panic!()
    };
    assert_eq!(stats.pending, 8);
    assert_eq!(stats.reserved_payload_bytes, 8 * 1024 * 1024);
    assert_eq!(stats.queued, 8);
    assert_eq!(stats.capacity_rejections, 1);
    assert_eq!(gate.state.lock().unwrap().peak, 2);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        worker.shutdown().unwrap();
        tx.send(()).unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        next += 1;
        if c.try_submit(WorkerRequest {
            id: next,
            action: WorkerAction::Inspect,
        })
        .is_err_and(|e| e.code == WorkerCode::Stopped)
        {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(rx.try_recv().is_err());
    gate.open();
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(gate.state.lock().unwrap().started, 8);
    assert_eq!(log.lock().unwrap().len(), 8);
}
#[test]
fn detail_failure_and_page_capacity_keep_coarse_or_proxy_and_do_not_edit_document() {
    let t = Temp::new();
    let (reference, mut bytes) = fixture();
    let path = t.0.join("asset.bin");
    let n = bytes.len();
    bytes[n - 1] ^= 1;
    fs::write(&path, bytes).unwrap();
    let surfaces = NativeSurfaces::default();
    let root_log = Arc::new(Mutex::new(Vec::new()));
    let proxy_log = Arc::new(Mutex::new(Vec::new()));
    let target = surfaces
        .attach(1, backend(96, root_log.clone()), 64, 64)
        .unwrap();
    let proxy = surfaces
        .attach(2, backend(0, proxy_log.clone()), 64, 64)
        .unwrap();
    let wakes = Arc::new(Mutex::new(Vec::new()));
    let w = wakes.clone();
    surfaces.set_asset_waker(Arc::new(move |handle| w.lock().unwrap().push(handle)));
    let worker = JournalWorker::start_with_surfaces(
        DocumentStore::new(t.0.join("doc")),
        WorkerConfig::default(),
        surfaces,
    )
    .unwrap();
    let c = worker.client();
    let mut next = 0;
    bind(&c, &mut next, reference, 1);
    assert_eq!(
        load(&c, &mut next, target, 1, path.clone(), true).0,
        AssetLoadStatus::Queued
    );
    let stats = idle(&c, &mut next);
    assert_eq!((stats.completed, stats.failed), (1, 1));
    assert_eq!(*root_log.lock().unwrap(), vec![Lod::Root, Lod::Root]);
    assert_eq!(*wakes.lock().unwrap(), vec![target]);
    load(&c, &mut next, proxy, 1, path, false);
    let stats = idle(&c, &mut next);
    assert_eq!(stats.failed, 2);
    assert_eq!(*proxy_log.lock().unwrap(), vec![Lod::BoundsProxy]);
    load(&c, &mut next, target, 1, t.0.join("missing"), false);
    let stats = idle(&c, &mut next);
    assert_eq!(stats.failed, 3);
    assert_eq!(stats.pending, 0);
    let WorkerOutcome::State(state) = call(&c, &mut next, WorkerAction::Inspect) else {
        panic!()
    };
    assert_eq!(state.revision, 1);
    assert_eq!(state.successful_sync_groups, 1);
    worker.shutdown().unwrap();
}
#[test]
fn root_then_detail_automatically_continue_under_one_lease_and_skip_closed_surface() {
    let t = Temp::new();
    let (reference, bytes) = fixture();
    let path = t.0.join("asset.bin");
    fs::write(&path, bytes).unwrap();
    let gate = Arc::new(Gate::default());
    let surfaces = NativeSurfaces::default();
    let log = Arc::new(Mutex::new(Vec::new()));
    let target = surfaces
        .attach(1, backend(96, log.clone()), 64, 64)
        .unwrap();
    let worker = JournalWorker::start_native(
        DocumentStore::new(t.0.join("doc")),
        WorkerConfig::default(),
        surfaces.clone(),
        Arc::new(GatedSource(gate.clone())),
    )
    .unwrap();
    let c = worker.client();
    let mut next = 0;
    bind(&c, &mut next, reference, 1);
    load(&c, &mut next, target, 1, path.clone(), true);
    gate.wait_started(1);
    surfaces.detach(target).unwrap();
    let replacement = surfaces
        .attach(1, backend(96, log.clone()), 64, 64)
        .unwrap();
    gate.open();
    assert_eq!(idle(&c, &mut next).stale, 1);
    assert!(log.lock().unwrap().is_empty());
    load(&c, &mut next, replacement, 1, path, true);
    let stats = idle(&c, &mut next);
    assert_eq!(stats.queued, 2);
    assert_eq!(stats.completed, 2);
    assert_eq!(stats.reserved_payload_bytes, 0);
    assert_eq!(*log.lock().unwrap(), vec![Lod::Root, Lod::Detail]);
    worker.shutdown().unwrap();
}

fn hierarchy_fixture(path: &Path) -> AssetRef {
    use threex_core::{
        cluster_builder::{ClusterBudget, ParentGeometryBudget, SimplifiedClusters},
        simplify::SimplifyBudget,
    };
    let v = [
        [0., 0., 1.],
        [0., 0., -1.],
        [1., 0., 0.],
        [0., 1., 0.],
        [-1., 0., 0.],
        [0., -1., 0.],
    ];
    let t = [
        [0, 2, 3],
        [0, 3, 4],
        [0, 4, 5],
        [0, 5, 2],
        [1, 3, 2],
        [1, 4, 3],
        [1, 5, 4],
        [1, 2, 5],
    ];
    let source = SimplifiedClusters::build(
        &v,
        &t,
        &[],
        ClusterBudget {
            leaf_triangles: 2,
            max_triangles: 8,
            max_nodes: 7,
        },
        ParentGeometryBudget {
            simplify: SimplifyBudget {
                target_triangles: 4,
                max_error: 2.,
                max_attempts: 64,
                max_input_triangles: 8,
            },
            max_source_vertices: 6,
            max_stored_triangles: 24,
            max_total_attempts: 64,
        },
    )
    .unwrap();
    crate::hierarchy_asset::write_hierarchy_asset(
        path,
        AssetId::from_bytes([82; 16]).unwrap(),
        1,
        &source,
        1024 * 1024,
    )
    .unwrap()
}
struct RecordingSource(Arc<Mutex<Vec<(u32, bool)>>>);
impl PageSource for RecordingSource {
    fn read(&self, p: &Path, r: AssetRef, n: u32) -> Loaded {
        self.read_known(p, r, n, None)
    }
    fn read_known(&self, p: &Path, r: AssetRef, n: u32, known: Option<&AssetManifest>) -> Loaded {
        self.0.lock().unwrap().push((n, known.is_some()));
        DiskSource.read_known(p, r, n, known)
    }
}
#[test]
fn hierarchy_breadth_first_delivery_reuses_metadata_and_keeps_partial_coverage() {
    let t = Temp::new();
    let path = t.0.join("hierarchy");
    let reference = hierarchy_fixture(&path);
    let surfaces = NativeSurfaces::default();
    let cuts = Arc::new(Mutex::new(Vec::new()));
    let reads = Arc::new(Mutex::new(Vec::new()));
    let target = surfaces
        .attach(
            1,
            Box::new(Backend {
                pool: PagePool::new(1024 * 1024, 16),
                manifests: BTreeMap::new(),
                log: Arc::new(Mutex::new(Vec::new())),
                cuts: Some(cuts.clone()),
            }),
            64,
            64,
        )
        .unwrap();
    let worker = JournalWorker::start_native(
        DocumentStore::new(t.0.join("doc")),
        WorkerConfig::default(),
        surfaces,
        Arc::new(RecordingSource(reads.clone())),
    )
    .unwrap();
    let c = worker.client();
    let mut next = 0;
    bind(&c, &mut next, reference, 1);
    assert_eq!(
        load(&c, &mut next, target, 1, path, true).0,
        AssetLoadStatus::Queued
    );
    let stats = idle(&c, &mut next);
    assert_eq!(stats.completed, 7);
    assert_eq!(stats.failed, 0);
    assert_eq!(stats.queued, 1);
    assert_eq!(stats.reserved_manifest_bytes, 0);
    assert_eq!(
        *reads.lock().unwrap(),
        vec![
            (0, false),
            (1, true),
            (4, true),
            (2, true),
            (3, true),
            (5, true),
            (6, true)
        ]
    );
    assert_eq!(
        *cuts.lock().unwrap(),
        vec![
            (0, vec![0]),
            (1, vec![0]),
            (4, vec![1, 4]),
            (2, vec![1, 4]),
            (3, vec![2, 3, 4]),
            (5, vec![2, 3, 4]),
            (6, vec![2, 3, 5, 6])
        ]
    );
    worker.shutdown().unwrap();
}
#[test]
fn hierarchy_undo_aba_rejects_old_root_then_restart_loads_current_reference() {
    let t = Temp::new();
    let path = t.0.join("hierarchy");
    let reference = hierarchy_fixture(&path);
    let gate = Arc::new(Gate::default());
    let surfaces = NativeSurfaces::default();
    let log = Arc::new(Mutex::new(Vec::new()));
    let target = surfaces
        .attach(1, backend(1024 * 1024, log.clone()), 64, 64)
        .unwrap();
    let worker = JournalWorker::start_native(
        DocumentStore::new(t.0.join("doc")),
        WorkerConfig::default(),
        surfaces.clone(),
        Arc::new(GatedSource(gate.clone())),
    )
    .unwrap();
    let c = worker.client();
    let mut next = 0;
    bind(&c, &mut next, reference, 1);
    load(&c, &mut next, target, 1, path.clone(), true);
    gate.wait_started(1);
    call(
        &c,
        &mut next,
        WorkerAction::Commit {
            expected_revision: 1,
            commands: asset_command(1, None).to_vec(),
        },
    );
    call(
        &c,
        &mut next,
        WorkerAction::Undo {
            expected_revision: 2,
        },
    );
    gate.open();
    assert_eq!(idle(&c, &mut next).stale, 1);
    assert!(log.lock().unwrap().is_empty());
    call(&c, &mut next, WorkerAction::Save);
    worker.shutdown().unwrap();
    let worker = JournalWorker::start_native(
        DocumentStore::new(t.0.join("doc")),
        WorkerConfig::default(),
        surfaces,
        Arc::new(DiskSource),
    )
    .unwrap();
    let c = worker.client();
    let mut next = 0;
    load(&c, &mut next, target, 1, path, true);
    assert_eq!(idle(&c, &mut next).completed, 7);
    worker.shutdown().unwrap();
}

#[test]
fn hierarchy_page_failure_and_capacity_stop_without_publishing_partial_children() {
    use std::io::{Seek, SeekFrom, Write};
    for mode in 0..3 {
        let t = Temp::new();
        let path = t.0.join("hierarchy");
        let reference = hierarchy_fixture(&path);
        let reader = crate::asset_pages::AssetReader::open(&path, reference).unwrap();
        let root_bytes = reader.manifest().page(0).unwrap().length;
        if mode == 2 {
            let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            file.seek(SeekFrom::Start(reader.manifest().page(1).unwrap().offset))
                .unwrap();
            file.write_all(&[0xff; 4]).unwrap();
        }
        let log = Arc::new(Mutex::new(Vec::new()));
        let surfaces = NativeSurfaces::default();
        let bytes = match mode {
            0 => 0,
            1 => root_bytes,
            _ => 1024 * 1024,
        };
        let target = surfaces
            .attach(1, backend(bytes, log.clone()), 64, 64)
            .unwrap();
        let worker = JournalWorker::start_native(
            DocumentStore::new(t.0.join("doc")),
            WorkerConfig::default(),
            surfaces,
            Arc::new(DiskSource),
        )
        .unwrap();
        let c = worker.client();
        let mut next = 0;
        bind(&c, &mut next, reference, 1);
        load(&c, &mut next, target, 1, path, true);
        let stats = idle(&c, &mut next);
        assert_eq!(stats.failed, 1);
        assert_eq!(stats.completed, if mode == 0 { 0 } else { 1 });
        assert_eq!(stats.reserved_payload_bytes, 0);
        assert_eq!(stats.reserved_manifest_bytes, 0);
        assert!(log.lock().unwrap().iter().all(|&lod| lod
            == if mode == 0 {
                Lod::BoundsProxy
            } else {
                Lod::Root
            }));
        worker.shutdown().unwrap();
    }
}
