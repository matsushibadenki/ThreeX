//! Fixed native loader lane. Only bounded internal completions carry page bytes.
use crate::worker::Envelope;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, mpsc},
    thread,
};
use threex_core::{
    AssetRef,
    asset_pages::{AssetManifest, MAX_PAGE_BYTES, ValidatedPage},
};
use threex_renderer::{asset_pages::PageFailure, resource_handles::ResourceHandle};
pub const ASSET_LOAD_CAPACITY: u32 = 8;
pub const ASSET_LOAD_THREADS: usize = 2;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetLoadStatus {
    Queued,
    Busy,
    MissingReference,
    StaleTarget,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AssetLoaderStats {
    pub pending: usize,
    pub reserved_payload_bytes: u64,
    pub reserved_manifest_bytes: u64,
    pub queued: u64,
    pub completed: u64,
    pub failed: u64,
    pub stale: u64,
    pub capacity_rejections: u64,
    pub streaming_batches: u64,
    pub streaming_reserved_requests: u64,
    pub streaming_visits: u64,
}
impl AssetLoaderStats {
    pub(crate) fn pending(&mut self, count: usize) {
        self.pending = count;
        self.reserved_payload_bytes = count as u64 * MAX_PAGE_BYTES;
        self.reserved_manifest_bytes =
            count as u64 * threex_core::hierarchy_asset::MAX_MANIFEST_BYTES as u64;
    }
}
pub(crate) struct Loaded {
    pub manifest: Option<AssetManifest>,
    pub page: Result<ValidatedPage, PageFailure>,
}
pub(crate) trait PageSource: Send + Sync {
    fn read(&self, path: &Path, reference: AssetRef, level: u32) -> Loaded;
    fn read_known(
        &self,
        path: &Path,
        reference: AssetRef,
        level: u32,
        _known: Option<&AssetManifest>,
    ) -> Loaded {
        self.read(path, reference, level)
    }
}
pub(crate) struct DiskSource;
impl PageSource for DiskSource {
    fn read(&self, path: &Path, reference: AssetRef, level: u32) -> Loaded {
        self.read_known(path, reference, level, None)
    }
    fn read_known(
        &self,
        path: &Path,
        reference: AssetRef,
        level: u32,
        known: Option<&AssetManifest>,
    ) -> Loaded {
        // Normal assets are regular files; do not intentionally open a pipe/device as an asset.
        if !std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
            return Loaded {
                manifest: None,
                page: Err(PageFailure::ReadFailed),
            };
        }
        match known.map_or_else(
            || crate::asset_pages::AssetReader::open(path, reference),
            |m| crate::asset_pages::AssetReader::open_known(path, reference, m.clone()),
        ) {
            Err(_) => Loaded {
                manifest: None,
                page: Err(PageFailure::ReadFailed),
            },
            Ok(mut reader) => Loaded {
                manifest: Some(reader.manifest().clone()),
                page: reader.read_page(level).map_err(|_| PageFailure::ReadFailed),
            },
        }
    }
}
pub(crate) struct Task {
    pub priority: f64,
    pub cancelled: bool,
    pub ticket: ResourceHandle,
    pub path: PathBuf,
    pub reference: AssetRef,
    pub level: u32,
    pub manifest: Option<Arc<AssetManifest>>,
}
pub(crate) struct Completion {
    pub ticket: ResourceHandle,
    pub level: u32,
    pub loaded: Loaded,
}
type TaskQueue = Arc<(Mutex<(Vec<Task>, bool)>, Condvar)>;
pub(crate) struct Loader {
    queue: TaskQueue,
    threads: Vec<thread::JoinHandle<()>>,
}
impl Loader {
    pub fn new(
        completion: mpsc::SyncSender<Envelope>,
        source: Arc<dyn PageSource>,
    ) -> std::io::Result<Self> {
        let queue = Arc::new((Mutex::new((Vec::<Task>::new(), false)), Condvar::new()));
        let mut threads = Vec::new();
        for index in 0..ASSET_LOAD_THREADS {
            let rx = queue.clone();
            let tx = completion.clone();
            let source = source.clone();
            let result = thread::Builder::new()
                .name(format!("threex-asset-{index}"))
                .spawn(move || {
                    loop {
                        let task = {
                            let (lock, cv) = &*rx;
                            let mut state = lock.lock().unwrap();
                            while state.0.is_empty() && !state.1 {
                                state = cv.wait(state).unwrap();
                            }
                            if state.0.is_empty() {
                                break;
                            }
                            let index = best(&state.0, |t| t.priority);
                            state.0.remove(index)
                        };
                        let loaded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            if task.cancelled {
                                return Loaded {
                                    manifest: None,
                                    page: Err(PageFailure::Stale),
                                };
                            }
                            source.read_known(
                                &task.path,
                                task.reference,
                                task.level,
                                task.manifest.as_deref(),
                            )
                        }))
                        .unwrap_or(Loaded {
                            manifest: None,
                            page: Err(PageFailure::ReadFailed),
                        });
                        if tx
                            .send(Envelope::Asset(Completion {
                                ticket: task.ticket,
                                level: task.level,
                                loaded,
                            }))
                            .is_err()
                        {
                            break;
                        }
                    }
                });
            match result {
                Ok(t) => threads.push(t),
                Err(e) => {
                    queue.0.lock().unwrap().1 = true;
                    queue.1.notify_all();
                    for t in threads {
                        let _ = t.join();
                    }
                    return Err(e);
                }
            }
        }
        Ok(Self { queue, threads })
    }
    pub fn reprioritize(&self, mut update: impl FnMut(&mut Task)) {
        let mut state = self.queue.0.lock().unwrap();
        for task in &mut state.0 {
            update(task);
        }
    }
    pub fn submit(&self, task: Task) -> Result<(), ()> {
        let mut state = self.queue.0.lock().unwrap();
        if state.1 || state.0.len() >= ASSET_LOAD_CAPACITY as usize {
            return Err(());
        }
        state.0.push(task);
        self.queue.1.notify_one();
        Ok(())
    }
}
fn best<T>(items: &[T], priority: impl Fn(&T) -> f64) -> usize {
    let mut best = 0;
    for i in 1..items.len() {
        if priority(&items[i])
            .total_cmp(&priority(&items[best]))
            .is_gt()
        {
            best = i;
        }
    }
    best
}
impl Drop for Loader {
    fn drop(&mut self) {
        self.queue.0.lock().unwrap().1 = true;
        self.queue.1.notify_all();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod queue_tests {
    #[test]
    fn waiting_work_reprioritizes_across_assets_and_preserves_fifo_ties() {
        let mut items = vec![(1, 2.), (2, 8.), (3, 8.)];
        assert_eq!(super::best(&items, |v| v.1), 1);
        items[0].1 = 10.;
        assert_eq!(items.remove(super::best(&items, |v| v.1)).0, 1);
        assert_eq!(items.remove(super::best(&items, |v| v.1)).0, 2);
    }
}
