//! Native persistence host. Document format lives in core; filesystem stays here.
mod identity;
mod journal;
mod protocol;
pub use identity::new_session_id;
pub use protocol::{
    ProtocolAction, ProtocolFailure, ProtocolReceipt, ProtocolRequest, REQUEST_PROTOCOL_VERSION,
};
mod worker;
pub use journal::AutosaveDocument;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use threex_core::{CheckpointError, Document, SceneStamp};
pub use worker::{
    JournalWorker, RejectedRequest, WorkerAction, WorkerClient, WorkerCode, WorkerConfig,
    WorkerFailure, WorkerOutcome, WorkerReply, WorkerRequest, WorkerStartError, WorkerState,
};
const MAX_BYTES: u64 = 128 * 1024 * 1024;
#[derive(Debug)]
pub enum StorageError {
    Io(std::io::Error),
    Format(CheckpointError),
    Busy,
    NoValidGeneration,
    TooManyGenerations,
    GenerationExhausted,
    StaleSave { latest_revision: u64 },
    JournalActive,
    JournalCorrupt,
    IdentityMismatch,
    UnsupportedRequestProtocol,
    Request(threex_core::RequestError),
    Rejected(ProtocolFailure),
    UnsupportedJournalVersion,
    JournalLimit,
    Edit(threex_core::DocumentError),
}
impl From<std::io::Error> for StorageError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<CheckpointError> for StorageError {
    fn from(e: CheckpointError) -> Self {
        Self::Format(e)
    }
}
/// Freeze before scheduling background IO; subsequent edits remain dirty.
pub struct PreparedSave {
    bytes: Vec<u8>,
    stamp: SceneStamp,
    revision: u64,
    request_generation: u128,
    content_hash: threex_core::ContentHash,
}
impl PreparedSave {
    pub fn capture(document: &Document) -> Result<Self, StorageError> {
        Ok(Self {
            bytes: document.checkpoint()?,
            content_hash: document.content_hash(),
            stamp: document.scene().stamp(),
            revision: document.revision(),
            request_generation: document.request_generation(),
        })
    }
}
pub struct SaveReceipt {
    pub content_hash: threex_core::ContentHash,
    pub document_id: threex_core::DocumentId,
    pub request_generation: u128,
    pub generation: u64,
    pub revision: u64,
    stamp: SceneStamp,
}
impl SaveReceipt {
    pub fn is_current(&self, document: &Document) -> bool {
        document.scene().is_current(Some(&self.stamp))
            && document
                .document_id()
                .is_none_or(|id| id == self.document_id)
            && document.request_generation() == self.request_generation
    }
}
pub struct LoadedDocument {
    pub document: Document,
    pub generation: u64,
    pub recovered: bool,
    pub journal_transactions: usize,
    pub discarded_journal_bytes: u64,
}
/// One directory per document. Final generations are immutable; .tmp files are never loaded.
pub struct DocumentStore {
    root: PathBuf,
}
impl DocumentStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { root: path.into() }
    }
    pub fn load(&self) -> Result<LoadedDocument, StorageError> {
        if self.root.join("autosave.txj").exists() {
            return journal::recover(self);
        }
        self.load_checkpoint()
    }
    fn load_checkpoint(&self) -> Result<LoadedDocument, StorageError> {
        let generations = self.generations()?;
        let mut recovered = false;
        for generation in generations.iter().rev() {
            match self.read(*generation) {
                Ok(document) => {
                    return Ok(LoadedDocument {
                        document,
                        generation: *generation,
                        recovered,
                        journal_transactions: 0,
                        discarded_journal_bytes: 0,
                    });
                }
                Err(StorageError::Format(CheckpointError::UnsupportedVersion(v))) => {
                    return Err(StorageError::Format(CheckpointError::UnsupportedVersion(v)));
                }
                Err(StorageError::Format(_)) => recovered = true,
                Err(e) => return Err(e),
            }
        }
        Err(StorageError::NoValidGeneration)
    }
    pub fn save(&self, document: &Document) -> Result<SaveReceipt, StorageError> {
        self.save_prepared(PreparedSave::capture(document)?)
    }
    pub fn save_prepared(&self, prepared: PreparedSave) -> Result<SaveReceipt, StorageError> {
        self.save_with(prepared, |_| Ok(()))
    }
    fn path(&self, g: u64) -> PathBuf {
        self.root.join(format!("generation-{g:020}.txd"))
    }
    fn generations(&self) -> Result<Vec<u64>, StorageError> {
        let mut result = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(number) = name
                .strip_prefix("generation-")
                .and_then(|s| s.strip_suffix(".txd"))
            else {
                continue;
            };
            if number.len() != 20 || !number.bytes().all(|c| c.is_ascii_digit()) {
                continue;
            }
            if let Ok(g) = number.parse() {
                result.push(g)
            }
            if result.len() > 4096 {
                return Err(StorageError::TooManyGenerations);
            }
        }
        result.sort_unstable();
        Ok(result)
    }
    fn read(&self, g: u64) -> Result<Document, StorageError> {
        let file = File::open(self.path(g))?;
        if file.metadata()?.len() > MAX_BYTES {
            return Err(StorageError::Format(CheckpointError::TooLarge));
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(StorageError::Format(CheckpointError::TooLarge));
        }
        let mut document = Document::from_checkpoint(&bytes)?;
        if let Some(id) = self.read_identity()? {
            document
                .bind_document_id(id)
                .map_err(|_| StorageError::IdentityMismatch)?;
        }
        Ok(document)
    }
    fn save_with(
        &self,
        prepared: PreparedSave,
        hook: impl FnMut(Stage) -> std::io::Result<()>,
    ) -> Result<SaveReceipt, StorageError> {
        fs::create_dir_all(&self.root)?;
        // OS advisory lock survives neither process exit nor crash; leave the lock file in place.
        let _lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.root.join("writer.lock"))?;
        match _lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Err(StorageError::Busy),
            Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
        }
        if self.root.join("autosave.txj").exists() {
            return Err(StorageError::JournalActive);
        }
        self.save_locked(prepared, hook)
    }
    fn save_locked(
        &self,
        mut prepared: PreparedSave,
        mut hook: impl FnMut(Stage) -> std::io::Result<()>,
    ) -> Result<SaveReceipt, StorageError> {
        let mut document = Document::from_checkpoint(&prepared.bytes)?;
        let id = self.ensure_identity(document.document_id())?;
        if document.document_id().is_none() {
            document
                .bind_document_id(id)
                .map_err(|_| StorageError::IdentityMismatch)?;
            prepared.bytes = document.checkpoint()?;
        }
        let request_generation = document.request_generation();
        let generations = self.generations()?;
        // Never downgrade/overwrite a newer schema. Recoverable corruption is allowed.
        let mut valid = Vec::new();
        for &g in generations.iter().rev() {
            match self.read(g) {
                Ok(document) => {
                    if valid.is_empty()
                        && (document.revision() > prepared.revision
                            || document.request_generation() > request_generation)
                    {
                        return Err(StorageError::StaleSave {
                            latest_revision: document.revision(),
                        });
                    }
                    valid.push(g);
                    if valid.len() == 2 {
                        break;
                    }
                }
                Err(StorageError::Format(CheckpointError::UnsupportedVersion(v))) => {
                    return Err(StorageError::Format(CheckpointError::UnsupportedVersion(v)));
                }
                Err(StorageError::Format(_)) => {}
                Err(e) => return Err(e),
            }
        }
        let generation = generations
            .last()
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(StorageError::GenerationExhausted)?;
        let temporary = self.root.join(format!("generation-{generation:020}.tmp"));
        // An abandoned temp from a crash is not committed and can safely be replaced under the lock.
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)?;
        let half = prepared.bytes.len() / 2;
        file.write_all(&prepared.bytes[..half])?;
        hook(Stage::PartialWrite)?;
        file.write_all(&prepared.bytes[half..])?;
        hook(Stage::Written)?;
        file.sync_all()?;
        hook(Stage::FileSynced)?;
        drop(file);
        fs::rename(&temporary, self.path(generation))?;
        hook(Stage::Renamed)?;
        sync_directory(&self.root)?;
        hook(Stage::DirectorySynced)?;
        // Keep new generation and the previous valid one; cleanup failure cannot invalidate the commit.
        let previous = valid.first().copied();
        for g in generations {
            if Some(g) != previous {
                let _ = fs::remove_file(self.path(g));
            }
        }
        Ok(SaveReceipt {
            content_hash: prepared.content_hash,
            document_id: id,
            request_generation: prepared.request_generation,
            generation,
            revision: prepared.revision,
            stamp: prepared.stamp,
        })
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    PartialWrite,
    Written,
    FileSynced,
    Renamed,
    DirectorySynced,
}
#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}
#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Directory durability is not implemented on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "threex-storage-{}-{}",
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
    fn create(d: &mut Document, id: u32) {
        let mut b = [0u8; 64];
        b[..4].copy_from_slice(&1u32.to_le_bytes());
        b[4..8].copy_from_slice(&id.to_le_bytes());
        d.commit(d.revision(), &b).unwrap();
    }
    #[test]
    fn save_load_history_dirty_tracking_and_corruption_recovery() {
        let temp = Temp::new();
        let store = DocumentStore::new(&temp.0);
        let mut d = Document::default();
        create(&mut d, 1);
        let first = store.save(&d).unwrap();
        assert!(first.is_current(&d));
        let pending = PreparedSave::capture(&d).unwrap();
        create(&mut d, 2);
        let receipt = store.save_prepared(pending).unwrap();
        assert!(!receipt.is_current(&d));
        let final_save = store.save(&d).unwrap();
        let mut loaded = store.load().unwrap();
        assert!(!loaded.recovered);
        assert_eq!(loaded.document.snapshot(), d.snapshot());
        assert!(!final_save.is_current(&loaded.document));
        loaded.document.undo(2).unwrap();
        assert_eq!(loaded.document.scene().len(), 1);
        loaded.document.redo(3).unwrap();
        assert_eq!(loaded.document.scene().len(), 2);
        fs::write(store.path(final_save.generation), b"broken").unwrap();
        let restored = store.load().unwrap();
        assert!(restored.recovered);
        assert_eq!(restored.document.revision(), 1);
        assert_eq!(restored.generation, receipt.generation);
        store.save(&restored.document).unwrap();
        assert!(!store.load().unwrap().recovered);
        assert_eq!(store.generations().unwrap().len(), 2);
    }
    #[test]
    fn interruption_at_each_commit_stage_keeps_a_complete_generation() {
        if let Ok(path) = std::env::var("THREEX_CRASH_DIRECTORY") {
            let store = DocumentStore::new(path);
            let mut d = Document::default();
            create(&mut d, 1);
            store.save(&d).unwrap();
            create(&mut d, 2);
            let stage = std::env::var("THREEX_CRASH_STAGE")
                .unwrap()
                .parse::<usize>()
                .unwrap();
            let stages = [
                Stage::PartialWrite,
                Stage::Written,
                Stage::FileSynced,
                Stage::Renamed,
                Stage::DirectorySynced,
            ];
            store
                .save_with(PreparedSave::capture(&d).unwrap(), |at| {
                    if at == stages[stage] {
                        std::process::exit(77);
                    }
                    Ok(())
                })
                .unwrap();
            panic!("crash hook was not reached");
        }
        // Abrupt subprocess exit skips Drop; the OS must release the writer lock.
        for stage in 0..5 {
            let temp = Temp::new();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tests::interruption_at_each_commit_stage_keeps_a_complete_generation",
                    "--nocapture",
                ])
                .env("THREEX_CRASH_DIRECTORY", &temp.0)
                .env("THREEX_CRASH_STAGE", stage.to_string())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(77));
            let store = DocumentStore::new(&temp.0);
            assert_eq!(
                store.load().unwrap().document.scene().len(),
                if stage >= 3 { 2 } else { 1 }
            );
            let mut d = Document::default();
            create(&mut d, 1);
            create(&mut d, 2);
            store.save(&d).unwrap(); // the abandoned lock file must not block recovery
        }
        for stage in [
            Stage::PartialWrite,
            Stage::Written,
            Stage::FileSynced,
            Stage::Renamed,
            Stage::DirectorySynced,
        ] {
            let temp = Temp::new();
            let store = DocumentStore::new(&temp.0);
            let mut d = Document::default();
            create(&mut d, 1);
            store.save(&d).unwrap();
            create(&mut d, 2);
            let result = store.save_with(PreparedSave::capture(&d).unwrap(), |at| {
                if at == stage {
                    Err(std::io::Error::other(
                        "injected storage failure / disk full",
                    ))
                } else {
                    Ok(())
                }
            });
            assert!(result.is_err());
            let loaded = store.load().unwrap();
            let expected = if stage == Stage::Renamed || stage == Stage::DirectorySynced {
                2
            } else {
                1
            };
            assert_eq!(loaded.document.scene().len(), expected);
            assert!(!loaded.recovered);
            store.save(&d).unwrap();
            assert_eq!(store.load().unwrap().document.snapshot(), d.snapshot());
        }
    }
    #[test]
    fn unknown_version_blocks_load_and_save_and_lock_blocks_writer() {
        let temp = Temp::new();
        let store = DocumentStore::new(&temp.0);
        let d = Document::default();
        let receipt = store.save(&d).unwrap();
        let path = store.path(receipt.generation);
        let mut b = fs::read(&path).unwrap();
        b[8..12].copy_from_slice(&7u32.to_le_bytes());
        fs::write(&path, &b).unwrap();
        assert!(matches!(
            store.load(),
            Err(StorageError::Format(CheckpointError::UnsupportedVersion(7)))
        ));
        assert!(matches!(
            store.save(&d),
            Err(StorageError::Format(CheckpointError::UnsupportedVersion(7)))
        ));
        assert_eq!(fs::read(path).unwrap(), b);
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(temp.0.join("writer.lock"))
            .unwrap();
        lock.lock().unwrap();
        assert!(matches!(store.save(&d), Err(StorageError::Busy)));
        drop(lock);
        // An abandoned lock file is harmless: the OS lock is released on drop/process exit.
        assert!(matches!(store.save(&d), Err(StorageError::Format(_))));
    }
    #[test]
    fn stale_background_save_cannot_replace_a_newer_committed_revision() {
        let temp = Temp::new();
        let store = DocumentStore::new(&temp.0);
        let mut d = Document::default();
        create(&mut d, 1);
        let old = PreparedSave::capture(&d).unwrap();
        create(&mut d, 2);
        store.save(&d).unwrap();
        assert!(matches!(
            store.save_prepared(old),
            Err(StorageError::StaleSave { latest_revision: 2 })
        ));
        assert_eq!(store.load().unwrap().document.snapshot(), d.snapshot());
    }
    #[test]
    fn orphan_temp_and_all_corrupt_files_do_not_become_documents() {
        let temp = Temp::new();
        let store = DocumentStore::new(&temp.0);
        fs::write(
            temp.0.join("generation-00000000000000000001.tmp"),
            b"partial",
        )
        .unwrap();
        assert!(matches!(store.load(), Err(StorageError::NoValidGeneration)));
        let d = Document::default();
        let r = store.save(&d).unwrap();
        fs::write(store.path(r.generation), b"corrupt").unwrap();
        assert!(matches!(store.load(), Err(StorageError::NoValidGeneration)));
    }
    #[test]
    fn save_receipt_hash_tracks_frozen_content_without_weakening_revision_guards() {
        let temp = Temp::new();
        let store = DocumentStore::new(&temp.0);
        let mut d = Document::default();
        create(&mut d, 1);
        let original = d.content_hash();
        let pending = PreparedSave::capture(&d).unwrap();
        create(&mut d, 2);
        assert_ne!(d.content_hash(), original);
        let receipt = store.save_prepared(pending).unwrap();
        assert_eq!(receipt.content_hash, original);
        assert!(!receipt.is_current(&d));
        assert_eq!(store.load().unwrap().document.content_hash(), original);
        d.undo(2).unwrap();
        assert_eq!(d.content_hash(), original);
        assert!(!receipt.is_current(&d));
        let current = store.save(&d).unwrap();
        assert!(current.is_current(&d));
        assert_eq!(current.content_hash, original);
    }
}
