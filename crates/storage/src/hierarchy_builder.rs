//! Single bounded CPU lane with session-local disk LRU and durable output copies.
use crate::hierarchy_asset::{HierarchyAssetReader, write_hierarchy_asset};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
use threex_core::{
    AssetJobGuard, AssetRef, Document, asset_command,
    cluster_builder::{ClusterBudget, ParentGeometryBudget, SimplifiedClusters, generation_key},
    simplify::SimplifyError,
};
#[derive(Clone, Copy)]
pub struct BuildSettings {
    pub cluster: ClusterBudget,
    pub parents: ParentGeometryBudget,
}
pub struct BuildInput {
    pub reference: AssetRef,
    pub vertices: Box<[[f32; 3]]>,
    pub triangles: Box<[[u32; 3]]>,
    pub locks: Box<[u32]>,
}
impl BuildInput {
    fn bytes(&self) -> Option<u64> {
        (self.vertices.len() as u64)
            .checked_mul(12)?
            .checked_add((self.triangles.len() as u64).checked_mul(12)?)?
            .checked_add((self.locks.len() as u64).checked_mul(4)?)
    }
}
#[derive(Clone, Copy)]
pub struct BuilderConfig {
    pub max_jobs: usize,
    pub max_input_bytes: u64,
    pub max_cache_bytes: u64,
    pub max_cache_entries: usize,
    pub max_vertices: usize,
    pub max_triangles: usize,
    pub max_nodes: usize,
    pub max_stored_triangles: usize,
    pub max_attempts: usize,
}
impl Default for BuilderConfig {
    fn default() -> Self {
        Self {
            max_jobs: 4,
            max_input_bytes: 8 * 1024 * 1024,
            max_cache_bytes: 64 * 1024 * 1024,
            max_cache_entries: 8,
            max_vertices: 16384,
            max_triangles: 16384,
            max_nodes: 4096,
            max_stored_triangles: 131072,
            max_attempts: 512,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildFailure {
    Busy,
    Timeout,
    Limit,
    InvalidInput,
    Io,
    OutputConflict,
    Cancelled,
    Stale,
    Closed,
    Panic,
}
#[derive(Clone, Debug)]
pub struct BuildArtifact {
    reference: AssetRef,
    source: AssetRef,
    path: PathBuf,
    key: [u8; 32],
    cache_hit: bool,
    guard: Option<AssetJobGuard>,
}
impl BuildArtifact {
    pub fn reference(&self) -> AssetRef {
        self.reference
    }
    pub fn source_reference(&self) -> AssetRef {
        self.source
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn key(&self) -> [u8; 32] {
        self.key
    }
    pub fn cache_hit(&self) -> bool {
        self.cache_hit
    }
    /// No implicit edit: submit this command through the owner's durable protocol
    /// with its current expected revision. Stale guards never produce a command.
    pub fn bind_command(&self, document: &Document) -> Result<[u8; 64], BuildFailure> {
        let guard = self.guard.as_ref().ok_or(BuildFailure::Stale)?;
        if !document.accepts_asset_result(guard, self.source) {
            return Err(BuildFailure::Stale);
        }
        Ok(asset_command(guard.entity(), Some(self.reference)))
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct BuilderStats {
    pub jobs: usize,
    pub reserved_input_bytes: u64,
    pub builds: u64,
    pub cache_hits: u64,
    pub corrupt_rebuilds: u64,
    pub failures: u64,
    pub cache_entries: usize,
    pub cache_bytes: u64,
}
#[derive(Default)]
struct Admission {
    jobs: usize,
    bytes: u64,
}
#[derive(Default)]
struct Metrics {
    builds: AtomicU64,
    hits: AtomicU64,
    corrupt: AtomicU64,
    failures: AtomicU64,
    entries: AtomicU64,
    bytes: AtomicU64,
}
struct Lease {
    admission: Arc<Mutex<Admission>>,
    bytes: u64,
    cancelled: AtomicBool,
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut a = self.admission.lock().unwrap_or_else(|e| e.into_inner());
        a.jobs -= 1;
        a.bytes -= self.bytes;
    }
}
pub struct BuildTicket {
    receiver: mpsc::Receiver<Result<BuildArtifact, BuildFailure>>,
    lease: Option<Arc<Lease>>,
}
impl BuildTicket {
    pub fn cancel(&self) {
        if let Some(lease) = &self.lease {
            lease.cancelled.store(true, Ordering::Release);
        }
    }
    fn is_cancelled(&self) -> bool {
        self.lease
            .as_ref()
            .is_some_and(|l| l.cancelled.load(Ordering::Acquire))
    }
    pub fn try_recv(&mut self) -> Result<Option<BuildArtifact>, BuildFailure> {
        if self.lease.is_none() {
            return Err(BuildFailure::Closed);
        }
        if self.is_cancelled() {
            self.lease.take();
            return Err(BuildFailure::Cancelled);
        }
        match self.receiver.try_recv() {
            Ok(result) => {
                self.lease.take();
                result.map(Some)
            }
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(_) => {
                self.lease.take();
                Err(BuildFailure::Closed)
            }
        }
    }
    pub fn wait(mut self, timeout: Duration) -> Result<BuildArtifact, BuildFailure> {
        if self.is_cancelled() {
            return Err(BuildFailure::Cancelled);
        }
        match self.receiver.recv_timeout(timeout) {
            Ok(result) => {
                self.lease.take();
                result
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Err(BuildFailure::Timeout),
            Err(_) => Err(BuildFailure::Closed),
        }
    }
}
impl Drop for BuildTicket {
    fn drop(&mut self) {
        self.cancel();
    }
}
struct Job {
    input: Arc<BuildInput>,
    settings: BuildSettings,
    destination: PathBuf,
    reply: mpsc::SyncSender<Result<BuildArtifact, BuildFailure>>,
    lease: Arc<Lease>,
    guard: Option<AssetJobGuard>,
}
static SERIAL: AtomicU64 = AtomicU64::new(1);
struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
type BuildFn =
    dyn Fn(&BuildInput, BuildSettings) -> Result<SimplifiedClusters, SimplifyError> + Send + Sync;
pub struct HierarchyBuilder {
    sender: Option<mpsc::SyncSender<Job>>,
    thread: Option<thread::JoinHandle<()>>,
    admission: Arc<Mutex<Admission>>,
    metrics: Arc<Metrics>,
    config: BuilderConfig,
}
impl HierarchyBuilder {
    pub fn start(
        cache_root: impl AsRef<Path>,
        config: BuilderConfig,
    ) -> Result<Self, BuildFailure> {
        Self::start_with(
            cache_root.as_ref(),
            config,
            Arc::new(|input, settings| {
                SimplifiedClusters::build(
                    &input.vertices,
                    &input.triangles,
                    &input.locks,
                    settings.cluster,
                    settings.parents,
                )
            }),
        )
    }
    fn start_with(
        root: &Path,
        config: BuilderConfig,
        build: Arc<BuildFn>,
    ) -> Result<Self, BuildFailure> {
        if config.max_jobs == 0
            || config.max_jobs > 64
            || config.max_input_bytes == 0
            || config.max_cache_bytes == 0
            || config.max_cache_entries == 0
            || config.max_nodes == 0
            || config.max_nodes > threex_core::hierarchy_asset::MAX_NODES
        {
            return Err(BuildFailure::Limit);
        }
        fs::create_dir_all(root).map_err(|_| BuildFailure::Io)?;
        let directory = (0..32)
            .find_map(|_| {
                let path = root.join(format!(
                    "threex-build-{}-{}",
                    std::process::id(),
                    SERIAL.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::create_dir(&path) {
                    Ok(()) => Some(Ok(Directory(path))),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => None,
                    Err(_) => Some(Err(BuildFailure::Io)),
                }
            })
            .unwrap_or(Err(BuildFailure::Io))?;
        let admission = Arc::new(Mutex::new(Admission::default()));
        let metrics = Arc::new(Metrics::default());
        let m = metrics.clone();
        let (sender, receiver) = mpsc::sync_channel::<Job>(config.max_jobs);
        let thread = thread::Builder::new()
            .name("threex-lod-builder".into())
            .spawn(move || {
                let mut cache = Cache {
                    directory,
                    entries: BTreeMap::new(),
                    bytes: 0,
                    clock: 0,
                    unhealthy: false,
                    config,
                    metrics: m,
                };
                while let Ok(job) = receiver.recv() {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        cache.process(&job, &*build)
                    }))
                    .unwrap_or(Err(BuildFailure::Panic));
                    if result.is_err() {
                        cache.metrics.failures.fetch_add(1, Ordering::Relaxed);
                    }
                    let Job {
                        reply,
                        lease,
                        input,
                        ..
                    } = job;
                    drop(input);
                    drop(lease);
                    let _ = reply.send(result);
                }
            })
            .map_err(|_| BuildFailure::Io)?;
        Ok(Self {
            sender: Some(sender),
            thread: Some(thread),
            admission,
            metrics,
            config,
        })
    }
    /// O(1) admission: no geometry hashing/building/copying on the calling thread.
    pub fn try_submit(
        &self,
        input: Arc<BuildInput>,
        settings: BuildSettings,
        destination: PathBuf,
    ) -> Result<BuildTicket, BuildFailure> {
        self.submit(input, settings, destination, None)
    }
    /// Capture the owner stamp at admission; cache hits receive this job's guard.
    pub fn try_submit_for(
        &self,
        document: &Document,
        entity: u32,
        input: Arc<BuildInput>,
        settings: BuildSettings,
        destination: PathBuf,
    ) -> Result<BuildTicket, BuildFailure> {
        let guard = document
            .asset_job_guard(entity)
            .filter(|g| g.reference() == input.reference)
            .ok_or(BuildFailure::Stale)?;
        self.submit(input, settings, destination, Some(guard))
    }
    fn submit(
        &self,
        input: Arc<BuildInput>,
        settings: BuildSettings,
        destination: PathBuf,
        guard: Option<AssetJobGuard>,
    ) -> Result<BuildTicket, BuildFailure> {
        let c = self.config;
        let p = settings.parents;
        let s = p.simplify;
        if input.vertices.len() > c.max_vertices
            || input.triangles.len() > c.max_triangles
            || input.locks.len() > c.max_vertices
            || settings.cluster.max_triangles > c.max_triangles
            || settings.cluster.max_nodes > c.max_nodes
            || p.max_source_vertices > c.max_vertices
            || p.max_stored_triangles > c.max_stored_triangles
            || s.max_input_triangles > c.max_triangles
            || s.max_attempts > c.max_attempts
            || p.max_total_attempts > c.max_attempts
            || destination.as_os_str().as_encoded_bytes().len() > 8192
        {
            return Err(BuildFailure::Limit);
        }
        let bytes = input.bytes().ok_or(BuildFailure::Limit)?;
        if bytes > c.max_input_bytes {
            return Err(BuildFailure::Limit);
        }
        let mut a = self.admission.lock().map_err(|_| BuildFailure::Closed)?;
        if a.jobs >= c.max_jobs || bytes > c.max_input_bytes.saturating_sub(a.bytes) {
            return Err(BuildFailure::Busy);
        }
        a.jobs += 1;
        a.bytes += bytes;
        drop(a);
        let lease = Arc::new(Lease {
            admission: self.admission.clone(),
            bytes,
            cancelled: AtomicBool::new(false),
        });
        let (tx, receiver) = mpsc::sync_channel(1);
        let job = Job {
            input,
            settings,
            destination,
            reply: tx,
            lease: lease.clone(),
            guard,
        };
        self.sender
            .as_ref()
            .ok_or(BuildFailure::Closed)?
            .try_send(job)
            .map_err(|_| BuildFailure::Busy)?;
        Ok(BuildTicket {
            receiver,
            lease: Some(lease),
        })
    }
    pub fn stats(&self) -> BuilderStats {
        let a = self.admission.lock().unwrap_or_else(|e| e.into_inner());
        let m = &self.metrics;
        BuilderStats {
            jobs: a.jobs,
            reserved_input_bytes: a.bytes,
            builds: m.builds.load(Ordering::Relaxed),
            cache_hits: m.hits.load(Ordering::Relaxed),
            corrupt_rebuilds: m.corrupt.load(Ordering::Relaxed),
            failures: m.failures.load(Ordering::Relaxed),
            cache_entries: m.entries.load(Ordering::Relaxed) as usize,
            cache_bytes: m.bytes.load(Ordering::Relaxed),
        }
    }
    pub fn shutdown(mut self) -> Result<(), BuildFailure> {
        self.finish()
    }
    fn finish(&mut self) -> Result<(), BuildFailure> {
        self.sender.take();
        if let Some(t) = self.thread.take() {
            t.join().map_err(|_| BuildFailure::Panic)?;
        }
        Ok(())
    }
}
impl Drop for HierarchyBuilder {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}
struct Entry {
    path: PathBuf,
    reference: AssetRef,
    bytes: u64,
    last: u64,
}
struct Cache {
    directory: Directory,
    entries: BTreeMap<[u8; 32], Entry>,
    bytes: u64,
    clock: u64,
    unhealthy: bool,
    config: BuilderConfig,
    metrics: Arc<Metrics>,
}
fn verify(path: &Path, reference: AssetRef) -> bool {
    let Ok(mut reader) = HierarchyAssetReader::open(path, reference) else {
        return false;
    };
    (0..reader.manifest().hierarchy().nodes().len() as u32)
        .all(|n| reader.read_page(reference, n).is_ok())
}
fn cancelled(job: &Job) -> Result<(), BuildFailure> {
    if job.lease.cancelled.load(Ordering::Acquire) {
        Err(BuildFailure::Cancelled)
    } else {
        Ok(())
    }
}
impl Cache {
    fn remove(&mut self, key: [u8; 32]) -> Result<(), BuildFailure> {
        let entry = &self.entries[&key];
        match fs::remove_file(&entry.path) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => {
                self.unhealthy = true;
                return Err(BuildFailure::Io);
            }
        }
        self.bytes -= entry.bytes;
        self.entries.remove(&key);
        self.measure();
        Ok(())
    }
    fn measure(&self) {
        self.metrics
            .entries
            .store(self.entries.len() as u64, Ordering::Relaxed);
        self.metrics.bytes.store(self.bytes, Ordering::Relaxed);
    }
    fn process(&mut self, job: &Job, build: &BuildFn) -> Result<BuildArtifact, BuildFailure> {
        cancelled(job)?;
        if self.unhealthy {
            return Err(BuildFailure::Io);
        }
        let parent = job
            .destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let parent = fs::canonicalize(parent).map_err(|_| BuildFailure::Io)?;
        if parent.starts_with(fs::canonicalize(&self.directory.0).map_err(|_| BuildFailure::Io)?) {
            return Err(BuildFailure::InvalidInput);
        }
        let i = &job.input;
        let s = job.settings;
        let key = generation_key(
            i.reference,
            &i.vertices,
            &i.triangles,
            &i.locks,
            s.cluster,
            s.parents,
        );
        self.clock = self.clock.saturating_add(1);
        let hit = self
            .entries
            .get(&key)
            .is_some_and(|e| verify(&e.path, e.reference));
        if self.entries.contains_key(&key) && !hit {
            self.metrics.corrupt.fetch_add(1, Ordering::Relaxed);
            self.remove(key)?;
        }
        if hit {
            self.metrics.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.metrics.builds.fetch_add(1, Ordering::Relaxed);
            let source = build(i, s).map_err(|e| match e {
                SimplifyError::Limit => BuildFailure::Limit,
                SimplifyError::Input => BuildFailure::InvalidInput,
            })?;
            cancelled(job)?;
            let (manifest, _) = threex_core::hierarchy_asset::HierarchyManifest::prepare(
                i.reference.id(),
                i.reference.revision(),
                &source,
            )
            .map_err(|_| BuildFailure::Limit)?;
            let bytes = manifest.file_len();
            if bytes > self.config.max_cache_bytes {
                return Err(BuildFailure::Limit);
            }
            while self.entries.len() >= self.config.max_cache_entries
                || bytes > self.config.max_cache_bytes.saturating_sub(self.bytes)
            {
                let victim = *self
                    .entries
                    .iter()
                    .min_by_key(|(_, e)| e.last)
                    .ok_or(BuildFailure::Limit)?
                    .0;
                self.remove(victim)?;
            }
            let name = key.iter().map(|b| format!("{b:02x}")).collect::<String>();
            let path = self.directory.0.join(format!("{name}.a2"));
            if write_hierarchy_asset(
                &path,
                i.reference.id(),
                i.reference.revision(),
                &source,
                self.config.max_cache_bytes,
            )
            .is_err()
            {
                let _ = fs::remove_file(&path);
                // Cleanup/fsync errors can leave an owned temporary behind. Keep
                // its reservation and stop writes rather than undercounting disk.
                self.bytes += bytes;
                self.unhealthy = true;
                self.measure();
                return Err(BuildFailure::Io);
            }
            self.entries.insert(
                key,
                Entry {
                    path,
                    reference: manifest.reference(),
                    bytes,
                    last: self.clock,
                },
            );
            self.bytes += bytes;
            self.measure();
        }
        cancelled(job)?;
        let entry = self.entries.get_mut(&key).unwrap();
        entry.last = self.clock;
        publish(&entry.path, &job.destination, entry.reference, job)?;
        Ok(BuildArtifact {
            reference: entry.reference,
            source: i.reference,
            path: job.destination.clone(),
            key,
            cache_hit: hit,
            guard: job.guard.clone(),
        })
    }
}
struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn publish(
    source: &Path,
    destination: &Path,
    reference: AssetRef,
    job: &Job,
) -> Result<(), BuildFailure> {
    cancelled(job)?;
    if destination.exists() {
        return if verify(destination, reference) {
            Ok(())
        } else {
            Err(BuildFailure::OutputConflict)
        };
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let (temp, mut output) = (0..32)
        .find_map(|_| {
            let path = parent.join(format!(
                ".threex-output-{}-{}.tmp",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            match OpenOptions::new().create_new(true).write(true).open(&path) {
                Ok(f) => Some(Ok((Temporary(path), f))),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(_) => Some(Err(BuildFailure::Io)),
            }
        })
        .unwrap_or(Err(BuildFailure::Io))?;
    let mut input = File::open(source).map_err(|_| BuildFailure::Io)?;
    let mut buffer = [0u8; 65536];
    loop {
        cancelled(job)?;
        let n = input.read(&mut buffer).map_err(|_| BuildFailure::Io)?;
        if n == 0 {
            break;
        }
        output
            .write_all(&buffer[..n])
            .map_err(|_| BuildFailure::Io)?;
    }
    output.sync_all().map_err(|_| BuildFailure::Io)?;
    drop(output);
    if !verify(&temp.0, reference) {
        return Err(BuildFailure::Io);
    }
    cancelled(job)?;
    match fs::hard_link(&temp.0, destination) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if !verify(destination, reference) {
                return Err(BuildFailure::OutputConflict);
            }
        }
        Err(_) => return Err(BuildFailure::Io),
    }
    drop(temp);
    File::open(parent)
        .and_then(|f| f.sync_all())
        .map_err(|_| BuildFailure::Io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Condvar, atomic::AtomicUsize};
    use threex_core::{AssetId, simplify::SimplifyBudget};
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "threex-builder-test-{}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn fixture() -> (Arc<BuildInput>, BuildSettings) {
        let reference = AssetRef::new(AssetId::from_bytes([93; 16]).unwrap(), 1, [2; 32]).unwrap();
        (
            Arc::new(BuildInput {
                reference,
                vertices: vec![
                    [-0.5, -0.5, 0.],
                    [0.5, -0.5, 0.],
                    [0.5, 0.5, 0.],
                    [-0.5, 0.5, 0.],
                    [0., 0., 0.1],
                ]
                .into_boxed_slice(),
                triangles: vec![[0, 1, 4], [1, 2, 4], [2, 3, 4], [3, 0, 4]].into_boxed_slice(),
                locks: Box::new([]),
            }),
            BuildSettings {
                cluster: ClusterBudget {
                    leaf_triangles: 2,
                    max_triangles: 4,
                    max_nodes: 3,
                },
                parents: ParentGeometryBudget {
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
            },
        )
    }
    fn wait(ticket: BuildTicket) -> BuildArtifact {
        ticket.wait(Duration::from_secs(10)).unwrap()
    }
    #[test]
    fn repeated_requests_share_builds_and_keys_include_geometry_settings_and_identity() {
        let t = Temp::new();
        let builder = HierarchyBuilder::start(t.0.join("cache"), BuilderConfig::default()).unwrap();
        let (input, settings) = fixture();
        let a = builder
            .try_submit(input.clone(), settings, t.0.join("a"))
            .unwrap();
        let b = builder
            .try_submit(input.clone(), settings, t.0.join("b"))
            .unwrap();
        let a = wait(a);
        let b = wait(b);
        assert!(!a.cache_hit());
        assert!(b.cache_hit());
        assert_eq!(a.key(), b.key());
        assert_eq!(a.reference(), b.reference());
        assert!(verify(a.path(), a.reference()));
        let mut changed = settings;
        changed.parents.simplify.max_error = 0.;
        let c = wait(
            builder
                .try_submit(input.clone(), changed, t.0.join("c"))
                .unwrap(),
        );
        assert_ne!(a.key(), c.key());
        assert!(!c.cache_hit());
        let mut geometry = BuildInput {
            reference: input.reference,
            vertices: input.vertices.clone(),
            triangles: input.triangles.clone(),
            locks: input.locks.clone(),
        };
        geometry.vertices[4][2] = 0.2;
        let d = wait(
            builder
                .try_submit(Arc::new(geometry), settings, t.0.join("d"))
                .unwrap(),
        );
        assert_ne!(a.key(), d.key());
        let identity = BuildInput {
            reference: AssetRef::new(input.reference.id(), 2, input.reference.hash()).unwrap(),
            vertices: input.vertices.clone(),
            triangles: input.triangles.clone(),
            locks: input.locks.clone(),
        };
        let e = wait(
            builder
                .try_submit(Arc::new(identity), settings, t.0.join("e"))
                .unwrap(),
        );
        assert_ne!(a.key(), e.key());
        assert_eq!(builder.stats().builds, 4);
        assert_eq!(builder.stats().cache_hits, 1);
        let again = wait(
            builder
                .try_submit(input.clone(), settings, t.0.join("a"))
                .unwrap(),
        );
        assert_eq!(again.reference(), a.reference());
        fs::write(t.0.join("conflict"), b"keep me").unwrap();
        assert!(matches!(
            builder
                .try_submit(input, settings, t.0.join("conflict"))
                .unwrap()
                .wait(Duration::from_secs(10)),
            Err(BuildFailure::OutputConflict)
        ));
        assert_eq!(fs::read(t.0.join("conflict")).unwrap(), b"keep me");
        builder.shutdown().unwrap();
        assert!(verify(a.path(), a.reference()));
        assert_eq!(fs::read_dir(t.0.join("cache")).unwrap().count(), 0);
    }
    struct Gate {
        entered: mpsc::SyncSender<()>,
        open: Mutex<bool>,
        changed: Condvar,
    }
    #[test]
    fn admission_cancellation_and_unread_results_keep_leases_bounded_off_owner_thread() {
        let t = Temp::new();
        let (tx, rx) = mpsc::sync_channel(1);
        let gate = Arc::new(Gate {
            entered: tx,
            open: Mutex::new(false),
            changed: Condvar::new(),
        });
        let g = gate.clone();
        let build = Arc::new(move |i: &BuildInput, s: BuildSettings| {
            assert_eq!(std::thread::current().name(), Some("threex-lod-builder"));
            g.entered.send(()).unwrap();
            let mut open = g.open.lock().unwrap();
            while !*open {
                open = g.changed.wait(open).unwrap();
            }
            SimplifiedClusters::build(&i.vertices, &i.triangles, &i.locks, s.cluster, s.parents)
        });
        let (input, settings) = fixture();
        let builder = HierarchyBuilder::start_with(
            &t.0.join("cache"),
            BuilderConfig {
                max_jobs: 1,
                ..Default::default()
            },
            build,
        )
        .unwrap();
        let mut ticket = builder
            .try_submit(input.clone(), settings, t.0.join("cancelled"))
            .unwrap();
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(ticket.try_recv().unwrap().is_none());
        assert!(matches!(
            builder.try_submit(input.clone(), settings, t.0.join("busy")),
            Err(BuildFailure::Busy)
        ));
        assert_eq!(builder.stats().jobs, 1);
        ticket.cancel();
        *gate.open.lock().unwrap() = true;
        gate.changed.notify_all();
        assert_eq!(
            ticket.wait(Duration::from_secs(5)).unwrap_err(),
            BuildFailure::Cancelled
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while builder.stats().jobs != 0 {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(!t.0.join("cancelled").exists());
        assert_eq!(builder.stats().jobs, 0);
        assert_eq!(builder.stats().reserved_input_bytes, 0);
        let ticket = builder
            .try_submit(input, settings, t.0.join("complete"))
            .unwrap();
        // Shutdown drains accepted work without needing the receiver to be consumed.
        builder.shutdown().unwrap();
        let artifact = wait(ticket);
        assert!(verify(artifact.path(), artifact.reference()));
    }
    #[test]
    fn lru_and_corruption_rebuild_do_not_remove_durable_outputs() {
        let t = Temp::new();
        let builder = HierarchyBuilder::start(
            t.0.join("cache"),
            BuilderConfig {
                max_cache_entries: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let (input, settings) = fixture();
        let a = wait(
            builder
                .try_submit(input.clone(), settings, t.0.join("a"))
                .unwrap(),
        );
        let session = fs::read_dir(t.0.join("cache"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let file = fs::read_dir(&session)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        fs::write(file, b"corrupt").unwrap();
        let b = wait(
            builder
                .try_submit(input.clone(), settings, t.0.join("b"))
                .unwrap(),
        );
        assert!(!b.cache_hit());
        assert_eq!(builder.stats().corrupt_rebuilds, 1);
        assert_eq!(a.reference(), b.reference());
        let mut changed = settings;
        changed.parents.simplify.max_error = 0.;
        wait(
            builder
                .try_submit(input.clone(), changed, t.0.join("c"))
                .unwrap(),
        );
        assert_eq!(builder.stats().cache_entries, 1);
        let again = wait(
            builder
                .try_submit(input, settings, t.0.join("again"))
                .unwrap(),
        );
        assert!(!again.cache_hit());
        assert_eq!(builder.stats().builds, 4);
        assert!(builder.stats().cache_bytes <= BuilderConfig::default().max_cache_bytes);
        builder.shutdown().unwrap();
        assert!(verify(a.path(), a.reference()));
        assert!(verify(b.path(), b.reference()));
    }
    #[test]
    fn guard_rejects_undo_aba_and_binding_survives_saved_document_restart() {
        let t = Temp::new();
        let builder = HierarchyBuilder::start(t.0.join("cache"), BuilderConfig::default()).unwrap();
        let (input, settings) = fixture();
        let mut owner =
            crate::AutosaveDocument::open(crate::DocumentStore::new(t.0.join("doc"))).unwrap();
        let mut commands = [0u8; 64];
        commands[..4].copy_from_slice(&1u32.to_le_bytes());
        commands[4..8].copy_from_slice(&1u32.to_le_bytes());
        let mut commands = commands.to_vec();
        commands.extend(asset_command(1, Some(input.reference)));
        owner.commit(0, &commands).unwrap();
        let ticket = builder
            .try_submit_for(
                owner.document(),
                1,
                input.clone(),
                settings,
                t.0.join("generated"),
            )
            .unwrap();
        owner.commit(1, &asset_command(1, None)).unwrap();
        owner.undo(2).unwrap();
        let artifact = wait(ticket);
        assert_eq!(
            artifact.bind_command(owner.document()),
            Err(BuildFailure::Stale)
        );
        let artifact = wait(
            builder
                .try_submit_for(
                    owner.document(),
                    1,
                    input.clone(),
                    settings,
                    t.0.join("generated"),
                )
                .unwrap(),
        );
        assert!(artifact.cache_hit());
        let command = artifact.bind_command(owner.document()).unwrap();
        owner.commit(3, &command).unwrap();
        owner.save().unwrap();
        drop(owner);
        builder.shutdown().unwrap();
        let owner =
            crate::AutosaveDocument::open(crate::DocumentStore::new(t.0.join("doc"))).unwrap();
        assert_eq!(
            owner.document().asset_job_guard(1).unwrap().reference(),
            artifact.reference()
        );
        assert!(verify(artifact.path(), artifact.reference()));
    }
    #[test]
    fn limits_and_panics_do_not_publish_or_kill_the_lane() {
        let t = Temp::new();
        let (input, settings) = fixture();
        let tiny = HierarchyBuilder::start(
            t.0.join("tiny"),
            BuilderConfig {
                max_cache_bytes: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            tiny.try_submit(input.clone(), settings, t.0.join("no-output"))
                .unwrap()
                .wait(Duration::from_secs(5))
                .unwrap_err(),
            BuildFailure::Limit
        );
        assert_eq!(tiny.stats().cache_bytes, 0);
        assert!(!t.0.join("no-output").exists());
        tiny.shutdown().unwrap();
        let small = HierarchyBuilder::start(
            t.0.join("small"),
            BuilderConfig {
                max_input_bytes: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(matches!(
            small.try_submit(input.clone(), settings, t.0.join("small-output")),
            Err(BuildFailure::Limit)
        ));
        small.shutdown().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let build = Arc::new(move |i: &BuildInput, s: BuildSettings| {
            if count.fetch_add(1, Ordering::Relaxed) == 0 {
                panic!("injected builder panic");
            }
            SimplifiedClusters::build(&i.vertices, &i.triangles, &i.locks, s.cluster, s.parents)
        });
        let builder =
            HierarchyBuilder::start_with(&t.0.join("panic"), BuilderConfig::default(), build)
                .unwrap();
        assert_eq!(
            builder
                .try_submit(input.clone(), settings, t.0.join("panic-output"))
                .unwrap()
                .wait(Duration::from_secs(5))
                .unwrap_err(),
            BuildFailure::Panic
        );
        assert!(!t.0.join("panic-output").exists());
        let artifact = wait(
            builder
                .try_submit(input, settings, t.0.join("after-panic"))
                .unwrap(),
        );
        assert!(verify(artifact.path(), artifact.reference()));
        builder.shutdown().unwrap();
    }
    #[test]
    fn actor_owned_document_binds_with_revision_and_replay_remains_idempotent() {
        use crate::{
            DocumentStore, JournalWorker, ProtocolAction, ProtocolRequest, WorkerAction,
            WorkerClient, WorkerConfig, WorkerOutcome, WorkerRequest,
        };
        fn call(client: &WorkerClient, next: &mut u64, action: WorkerAction) -> WorkerOutcome {
            *next += 1;
            client
                .try_submit(WorkerRequest { id: *next, action })
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .result
                .unwrap()
        }
        let t = Temp::new();
        let (input, settings) = fixture();
        let builder = HierarchyBuilder::start(t.0.join("cache"), BuilderConfig::default()).unwrap();
        let worker = JournalWorker::start(
            DocumentStore::new(t.0.join("actor")),
            WorkerConfig::default(),
        )
        .unwrap();
        let id = worker.initial_state.document_id;
        let client = worker.client();
        let mut next = 0;
        let session = crate::new_session_id().unwrap();
        let mut create = [0u8; 64];
        create[..4].copy_from_slice(&1u32.to_le_bytes());
        create[4..8].copy_from_slice(&1u32.to_le_bytes());
        let mut commands = create.to_vec();
        commands.extend(asset_command(1, Some(input.reference)));
        call(
            &client,
            &mut next,
            WorkerAction::Protocol(ProtocolRequest {
                protocol_version: 1,
                document_id: id,
                session_id: session,
                sequence: 1,
                action: ProtocolAction::Commit {
                    expected_revision: 0,
                    commands,
                },
            }),
        );
        let WorkerOutcome::AssetJob {
            ticket: Some(job), ..
        } = call(&client, &mut next, WorkerAction::BeginAssetJob { id: 1 })
        else {
            panic!()
        };
        let artifact = wait(
            builder
                .try_submit(input.clone(), settings, t.0.join("durable"))
                .unwrap(),
        );
        let WorkerOutcome::AssetFinished {
            accepted: true,
            state,
            ..
        } = call(
            &client,
            &mut next,
            WorkerAction::FinishAssetJob {
                ticket: job,
                reference: input.reference,
            },
        )
        else {
            panic!()
        };
        let request = ProtocolRequest {
            protocol_version: 1,
            document_id: id,
            session_id: session,
            sequence: 2,
            action: ProtocolAction::Commit {
                expected_revision: state.revision,
                commands: asset_command(1, Some(artifact.reference())).to_vec(),
            },
        };
        call(&client, &mut next, WorkerAction::Protocol(request.clone()));
        call(&client, &mut next, WorkerAction::Save);
        worker.shutdown().unwrap();
        builder.shutdown().unwrap();
        let worker = JournalWorker::start(
            DocumentStore::new(t.0.join("actor")),
            WorkerConfig::default(),
        )
        .unwrap();
        let client = worker.client();
        let mut next = 0;
        let WorkerOutcome::Protocol {
            replayed: true,
            state,
            ..
        } = call(&client, &mut next, WorkerAction::Protocol(request))
        else {
            panic!()
        };
        assert_eq!(state.revision, 2);
        let WorkerOutcome::Object {
            object: Some(object),
            ..
        } = call(&client, &mut next, WorkerAction::ReadObject { id: 1 })
        else {
            panic!()
        };
        assert_eq!(object.asset, Some(artifact.reference()));
        let WorkerOutcome::AssetFinished {
            accepted: false, ..
        } = call(
            &client,
            &mut next,
            WorkerAction::FinishAssetJob {
                ticket: job,
                reference: input.reference,
            },
        )
        else {
            panic!()
        };
        assert!(verify(artifact.path(), artifact.reference()));
        worker.shutdown().unwrap();
    }
    #[test]
    fn cache_write_failure_stops_future_writes_without_losing_output_files() {
        let t = Temp::new();
        let root = t.0.join("cache");
        let r = root.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let n = calls.clone();
        let build = Arc::new(move |input: &BuildInput, settings: BuildSettings| {
            n.fetch_add(1, Ordering::Relaxed);
            let key = generation_key(
                input.reference,
                &input.vertices,
                &input.triangles,
                &input.locks,
                settings.cluster,
                settings.parents,
            );
            let name = key.iter().map(|b| format!("{b:02x}")).collect::<String>();
            let session = fs::read_dir(&r).unwrap().next().unwrap().unwrap().path();
            fs::create_dir(session.join(format!("{name}.a2"))).unwrap();
            SimplifiedClusters::build(
                &input.vertices,
                &input.triangles,
                &input.locks,
                settings.cluster,
                settings.parents,
            )
        });
        let builder = HierarchyBuilder::start_with(&root, BuilderConfig::default(), build).unwrap();
        let (input, settings) = fixture();
        for name in ["a", "b"] {
            assert_eq!(
                builder
                    .try_submit(input.clone(), settings, t.0.join(name))
                    .unwrap()
                    .wait(Duration::from_secs(5))
                    .unwrap_err(),
                BuildFailure::Io
            );
            assert!(!t.0.join(name).exists());
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(builder.stats().cache_bytes > 0);
        assert!(builder.stats().cache_bytes <= BuilderConfig::default().max_cache_bytes);
        builder.shutdown().unwrap();
        assert_eq!(fs::read_dir(root).unwrap().count(), 0);
    }
}
