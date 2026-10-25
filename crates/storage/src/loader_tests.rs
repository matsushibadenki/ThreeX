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
    fn read(&self, path: &Path, reference: AssetRef, level: u8) -> Loaded {
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
struct Backend {
    pool: PagePool<ValidatedPage>,
    manifests: BTreeMap<AssetRef, AssetManifest>,
    log: Arc<Mutex<Vec<Lod>>>,
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
        level: u8,
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
