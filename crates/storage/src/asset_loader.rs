//! Fixed native loader lane. Only bounded internal completions carry page bytes.
use crate::worker::Envelope;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
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
    pub queued: u64,
    pub completed: u64,
    pub failed: u64,
    pub stale: u64,
    pub capacity_rejections: u64,
}
impl AssetLoaderStats {
    pub(crate) fn pending(&mut self, count: usize) {
        self.pending = count;
        self.reserved_payload_bytes = count as u64 * MAX_PAGE_BYTES;
    }
}
pub(crate) struct Loaded {
    pub manifest: Option<AssetManifest>,
    pub page: Result<ValidatedPage, PageFailure>,
}
pub(crate) trait PageSource: Send + Sync {
    fn read(&self, path: &Path, reference: AssetRef, level: u8) -> Loaded;
}
pub(crate) struct DiskSource;
impl PageSource for DiskSource {
    fn read(&self, path: &Path, reference: AssetRef, level: u8) -> Loaded {
        // Normal assets are regular files; do not intentionally open a pipe/device as an asset.
        if !std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
            return Loaded {
                manifest: None,
                page: Err(PageFailure::ReadFailed),
            };
        }
        match crate::asset_pages::AssetReader::open(path, reference) {
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
    pub ticket: ResourceHandle,
    pub path: PathBuf,
    pub reference: AssetRef,
    pub level: u8,
}
pub(crate) struct Completion {
    pub ticket: ResourceHandle,
    pub level: u8,
    pub loaded: Loaded,
}
pub(crate) struct Loader {
    sender: Option<mpsc::SyncSender<Task>>,
    threads: Vec<thread::JoinHandle<()>>,
}
impl Loader {
    pub fn new(
        completion: mpsc::SyncSender<Envelope>,
        source: Arc<dyn PageSource>,
    ) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Task>(ASSET_LOAD_CAPACITY as usize);
        let receiver = Arc::new(Mutex::new(receiver));
        let mut threads = Vec::new();
        for index in 0..ASSET_LOAD_THREADS {
            let rx = receiver.clone();
            let tx = completion.clone();
            let source = source.clone();
            let result = thread::Builder::new()
                .name(format!("threex-asset-{index}"))
                .spawn(move || {
                    loop {
                        let task = match rx.lock() {
                            Ok(rx) => rx.recv(),
                            Err(_) => break,
                        };
                        let Ok(task) = task else { break };
                        let loaded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            source.read(&task.path, task.reference, task.level)
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
                    drop(sender);
                    for t in threads {
                        let _ = t.join();
                    }
                    return Err(e);
                }
            }
        }
        Ok(Self {
            sender: Some(sender),
            threads,
        })
    }
    pub fn submit(&self, task: Task) -> Result<(), Task> {
        match self.sender.as_ref().unwrap().try_send(task) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(t) | mpsc::TrySendError::Disconnected(t)) => Err(t),
        }
    }
}
impl Drop for Loader {
    fn drop(&mut self) {
        self.sender.take();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}
