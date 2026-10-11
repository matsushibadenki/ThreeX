//! Write-ahead transaction log with an immutable, self-contained checkpoint anchor.
use super::*;
use std::io::{Seek, SeekFrom};
const MAGIC: &[u8; 8] = b"THREEXJ1";
const MAX_LOG: u64 = 256 * 1024 * 1024;
const MAX_RECORD: usize = 256 * 1024 + 21;
fn crc(b: &[u8]) -> u32 {
    let mut c = !0u32;
    for &v in b {
        c ^= v as u32;
        for _ in 0..8 {
            c = (c >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(c & 1));
        }
    }
    !c
}
fn frame(payload: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(payload.len() + 12);
    b.extend((payload.len() as u64).to_le_bytes());
    b.extend(payload);
    b.extend(crc(payload).to_le_bytes());
    b
}
/// Rust owner that journals every accepted commit/undo/redo before publishing it.
/// It holds the writer lock for its lifetime; UI/renderers receive read-only Document.
pub struct AutosaveDocument {
    store: DocumentStore,
    document: Document,
    _lock: File,
    valid_bytes: u64,
    pub discarded_bytes: u64,
    append_ready: bool,
}
impl AutosaveDocument {
    pub fn open(store: DocumentStore) -> Result<Self, StorageError> {
        fs::create_dir_all(&store.root)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(store.root.join("writer.lock"))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Err(StorageError::Busy),
            Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
        }
        if !store.root.join("autosave.txj").exists() {
            let document = match store.load_checkpoint() {
                Ok(loaded) => loaded.document,
                Err(StorageError::NoValidGeneration) if store.generations()?.is_empty() => {
                    Document::default()
                }
                Err(e) => return Err(e),
            };
            replace_log(&store, &document)?;
        }
        let (loaded, mut valid_bytes) = read_log(&store)?;
        // Only a writer repairs the invalid suffix. Read-only recovery never modifies files.
        if valid_bytes == 0 {
            fs::copy(
                store.root.join("autosave.txj"),
                store.root.join("autosave.damaged"),
            )?;
            valid_bytes = replace_log(&store, &loaded.document)?;
        } else if loaded.discarded_journal_bytes > 0 {
            let file = OpenOptions::new()
                .write(true)
                .open(store.root.join("autosave.txj"))?;
            file.set_len(valid_bytes)?;
            file.sync_all()?;
        }
        Ok(Self {
            store,
            document: loaded.document,
            _lock: lock,
            valid_bytes,
            discarded_bytes: loaded.discarded_journal_bytes,
            append_ready: true,
        })
    }
    pub fn document(&self) -> &Document {
        &self.document
    }
    pub fn commit(&mut self, expected: u64, commands: &[u8]) -> Result<u64, StorageError> {
        self.edit(expected, 1, commands)
    }
    /// Bounded synchronous group: one journal sync, separate transaction/Undo boundaries.
    /// No edits become visible before synchronization. Callers choose the batching policy.
    pub fn commit_group(
        &mut self,
        expected: u64,
        commands: &[&[u8]],
    ) -> Result<Vec<u64>, StorageError> {
        self.group_with(expected, commands, |_| Ok(()))
    }
    fn group_with(
        &mut self,
        expected: u64,
        commands: &[&[u8]],
        mut hook: impl FnMut(u8) -> std::io::Result<()>,
    ) -> Result<Vec<u64>, StorageError> {
        if !self.append_ready {
            return Err(StorageError::JournalCorrupt);
        }
        let group = self
            .document
            .prepare_group(expected, commands)
            .map_err(StorageError::Edit)?;
        let mut bytes = Vec::new();
        let mut base = expected;
        let revisions = group.revisions();
        for (&revision, &command) in revisions.iter().zip(commands) {
            if revision != base {
                let mut payload = vec![1];
                payload.extend(base.to_le_bytes());
                payload.extend(revision.to_le_bytes());
                payload.extend(command);
                if payload.len() > MAX_RECORD {
                    return Err(StorageError::JournalLimit);
                }
                bytes.extend(frame(&payload));
            }
            base = revision;
        }
        if self.valid_bytes + bytes.len() as u64 > MAX_LOG {
            return Err(StorageError::JournalLimit);
        }
        if !bytes.is_empty() {
            let mut file = OpenOptions::new()
                .write(true)
                .open(self.store.root.join("autosave.txj"))?;
            file.set_len(self.valid_bytes)?;
            file.seek(SeekFrom::Start(self.valid_bytes))?;
            file.write_all(&bytes[..bytes.len() / 2])?;
            hook(0)?;
            file.write_all(&bytes[bytes.len() / 2..])?;
            hook(1)?;
            file.sync_all()?;
            hook(2)?;
            self.valid_bytes += bytes.len() as u64;
        }
        self.document.apply_group(group).map_err(StorageError::Edit)
    }
    pub fn undo(&mut self, expected: u64) -> Result<u64, StorageError> {
        self.edit(expected, 2, &[])
    }
    pub fn redo(&mut self, expected: u64) -> Result<u64, StorageError> {
        self.edit(expected, 3, &[])
    }
    fn edit(&mut self, expected: u64, op: u8, commands: &[u8]) -> Result<u64, StorageError> {
        self.edit_with(expected, op, commands, |_| Ok(()))
    }
    fn edit_with(
        &mut self,
        expected: u64,
        op: u8,
        commands: &[u8],
        mut hook: impl FnMut(u8) -> std::io::Result<()>,
    ) -> Result<u64, StorageError> {
        if !self.append_ready {
            return Err(StorageError::JournalCorrupt);
        }
        let prepared = match op {
            1 => self.document.prepare_commit(expected, commands),
            2 => self.document.prepare_undo(expected),
            3 => self.document.prepare_redo(expected),
            _ => unreachable!(),
        }
        .map_err(StorageError::Edit)?;
        let revision = prepared.revision();
        if revision == self.document.revision() {
            return Ok(revision);
        }
        let mut payload = vec![op];
        payload.extend(expected.to_le_bytes());
        payload.extend(revision.to_le_bytes());
        payload.extend(commands);
        let bytes = frame(&payload);
        if payload.len() > MAX_RECORD || self.valid_bytes + bytes.len() as u64 > MAX_LOG {
            return Err(StorageError::JournalLimit);
        }
        let mut file = OpenOptions::new()
            .write(true)
            .open(self.store.root.join("autosave.txj"))?;
        // Retry after any uncertain write: replace the unacknowledged suffix at the known boundary.
        file.set_len(self.valid_bytes)?;
        file.seek(SeekFrom::Start(self.valid_bytes))?;
        let half = bytes.len() / 2;
        file.write_all(&bytes[..half])?;
        hook(0)?;
        file.write_all(&bytes[half..])?;
        hook(1)?;
        file.sync_all()?;
        hook(2)?;
        self.valid_bytes += bytes.len() as u64;
        self.document
            .apply_prepared(prepared)
            .map_err(StorageError::Edit)
    }
    /// Save a generation, then atomically compact the log to a checkpoint anchor.
    pub fn save(&mut self) -> Result<SaveReceipt, StorageError> {
        self.save_compact(|_| Ok(()))
    }
    fn save_compact(
        &mut self,
        hook: impl FnMut(u8) -> std::io::Result<()>,
    ) -> Result<SaveReceipt, StorageError> {
        let receipt = self
            .store
            .save_locked(PreparedSave::capture(&self.document)?, |_| Ok(()))?;
        self.append_ready = false;
        match replace_with(&self.store, &self.document, hook) {
            Ok(bytes) => {
                self.valid_bytes = bytes;
                self.append_ready = true;
            }
            Err(error) => {
                // Rename can commit despite a later error. Refresh the append boundary before retry.
                let boundary = read_log(&self.store)?.1;
                self.valid_bytes = if boundary == 0 {
                    replace_log(&self.store, &self.document)?
                } else {
                    boundary
                };
                self.append_ready = true;
                return Err(error);
            }
        }
        Ok(receipt)
    }
}
fn replace_log(store: &DocumentStore, document: &Document) -> Result<u64, StorageError> {
    replace_with(store, document, |_| Ok(()))
}
fn replace_with(
    store: &DocumentStore,
    document: &Document,
    mut hook: impl FnMut(u8) -> std::io::Result<()>,
) -> Result<u64, StorageError> {
    let checkpoint = document.checkpoint()?;
    let mut bytes = MAGIC.to_vec();
    bytes.extend(frame(&checkpoint));
    let tmp = store.root.join("autosave.tmp");
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)?;
    f.write_all(&bytes[..bytes.len() / 2])?;
    hook(0)?;
    f.write_all(&bytes[bytes.len() / 2..])?;
    f.sync_all()?;
    hook(1)?;
    drop(f);
    fs::rename(tmp, store.root.join("autosave.txj"))?;
    hook(2)?;
    sync_directory(&store.root)?;
    Ok(bytes.len() as u64)
}
pub(super) fn recover(store: &DocumentStore) -> Result<LoadedDocument, StorageError> {
    Ok(read_log(store)?.0)
}
fn read_log(store: &DocumentStore) -> Result<(LoadedDocument, u64), StorageError> {
    let f = File::open(store.root.join("autosave.txj"))?;
    if f.metadata()?.len() > MAX_LOG {
        return Err(StorageError::JournalLimit);
    }
    let mut bytes = Vec::new();
    f.take(MAX_LOG + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_LOG {
        return Err(StorageError::JournalLimit);
    }
    if bytes.get(..7) == Some(&MAGIC[..7]) && bytes.get(..8) != Some(MAGIC.as_slice()) {
        return Err(StorageError::UnsupportedJournalVersion);
    }
    if bytes.get(..8) != Some(MAGIC.as_slice()) {
        return fallback(store, bytes.len() as u64);
    }
    let Some((base, mut at)) = decode(&bytes, 8, MAX_BYTES as usize) else {
        return fallback(store, bytes.len() as u64);
    };
    let mut document = match Document::from_checkpoint(base) {
        Ok(document) => document,
        Err(CheckpointError::UnsupportedVersion(v)) => {
            return Err(StorageError::Format(CheckpointError::UnsupportedVersion(v)));
        }
        Err(_) => return fallback(store, bytes.len() as u64),
    };
    let base_revision = document.revision();
    let mut transactions = 0;
    while at < bytes.len() {
        let Some((payload, end)) = decode(&bytes, at, MAX_RECORD) else {
            break;
        };
        if payload.len() < 17 {
            break;
        }
        let expected = u64::from_le_bytes(payload[1..9].try_into().unwrap());
        let revision = u64::from_le_bytes(payload[9..17].try_into().unwrap());
        if expected != document.revision() || expected.checked_add(1) != Some(revision) {
            break;
        }
        // Document methods reject failures atomically, so replay needs no full Scene clone.
        let result = match payload[0] {
            1 => document.commit(expected, &payload[17..]),
            2 if payload.len() == 17 => document.undo(expected),
            3 if payload.len() == 17 => document.redo(expected),
            _ => break,
        };
        if result != Ok(revision) {
            break;
        }
        transactions += 1;
        at = end;
    }
    // A saved generation must never be silently superseded by an older/corrupt journal.
    let generation = match store.load_checkpoint() {
        Ok(saved) => {
            if saved.document.revision() > document.revision() {
                return fallback(store, bytes.len() as u64);
            }
            saved.generation
        }
        Err(StorageError::NoValidGeneration) => 0,
        Err(e) => return Err(e),
    };
    let discarded = (bytes.len() - at) as u64;
    Ok((
        LoadedDocument {
            document,
            generation,
            recovered: discarded > 0 || transactions > 0 || base_revision > 0,
            journal_transactions: transactions,
            discarded_journal_bytes: discarded,
        },
        at as u64,
    ))
}
fn fallback(store: &DocumentStore, discarded: u64) -> Result<(LoadedDocument, u64), StorageError> {
    let mut loaded = match store.load_checkpoint() {
        Ok(loaded) => loaded,
        Err(StorageError::NoValidGeneration) => return Err(StorageError::JournalCorrupt),
        Err(error) => return Err(error),
    };
    loaded.recovered = true;
    loaded.discarded_journal_bytes = discarded;
    Ok((loaded, 0)) // writer must replace the invalid anchor, not append to it
}
fn decode(bytes: &[u8], at: usize, max: usize) -> Option<(&[u8], usize)> {
    let header = bytes.get(at..at.checked_add(8)?)?;
    let len = u64::from_le_bytes(header.try_into().ok()?);
    if len > max as u64 {
        return None;
    }
    let start = at + 8;
    let end = start.checked_add(len as usize)?;
    let payload = bytes.get(start..end)?;
    let sum = bytes.get(end..end.checked_add(4)?)?;
    if crc(payload) != u32::from_le_bytes(sum.try_into().ok()?) {
        return None;
    }
    Some((payload, end + 4))
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
                "threex-journal-{}-{}",
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
    fn command(id: u32) -> [u8; 64] {
        let mut b = [0; 64];
        b[..4].copy_from_slice(&1u32.to_le_bytes());
        b[4..8].copy_from_slice(&id.to_le_bytes());
        b
    }
    fn add(owner: &mut AutosaveDocument, id: u32) {
        owner
            .commit(owner.document().revision(), &command(id))
            .unwrap();
    }
    #[test]
    fn autosave_recovers_unsaved_edits_undo_redo_and_compacts() {
        let t = Temp::new();
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        add(&mut a, 1);
        a.save().unwrap();
        add(&mut a, 2);
        a.undo(2).unwrap();
        a.redo(3).unwrap();
        let expected = a.document().snapshot();
        let stamp = a.document().scene().stamp();
        add(&mut a, 3);
        assert!(
            a.document()
                .scene()
                .instance_updates_since(Some(&stamp), false)
                .ranges
                .is_empty()
        );
        a.undo(5).unwrap();
        drop(a);
        let loaded = DocumentStore::new(&t.0).load().unwrap();
        assert_eq!(loaded.document.snapshot().objects, expected.objects);
        assert_eq!(loaded.document.revision(), 6);
        assert_eq!(loaded.journal_transactions, 5);
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        a.redo(6).unwrap();
        assert!(a.document().scene().object(3).is_some());
        let before = fs::metadata(t.0.join("autosave.txj")).unwrap().len();
        a.save().unwrap();
        let loaded = DocumentStore::new(&t.0).load().unwrap();
        assert_eq!(loaded.journal_transactions, 0);
        assert_eq!(loaded.document.snapshot(), a.document().snapshot());
        // Compaction keeps history even though the transaction log itself is reset.
        a.undo(7).unwrap();
        assert!(a.document().scene().object(3).is_none());
        assert!(before > 0);
    }
    #[test]
    fn every_truncated_or_corrupt_tail_recovers_last_complete_transaction() {
        let t = Temp::new();
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        add(&mut a, 1);
        let boundary = a.valid_bytes as usize;
        add(&mut a, 2);
        drop(a);
        let path = t.0.join("autosave.txj");
        let complete = fs::read(&path).unwrap();
        for end in boundary..complete.len() {
            fs::write(&path, &complete[..end]).unwrap();
            let loaded = DocumentStore::new(&t.0).load().unwrap();
            assert_eq!(loaded.document.revision(), 1);
            assert_eq!(loaded.discarded_journal_bytes, (end - boundary) as u64);
        }
        for i in boundary..complete.len() {
            let mut b = complete.clone();
            b[i] ^= 0x80;
            fs::write(&path, b).unwrap();
            assert_eq!(
                DocumentStore::new(&t.0).load().unwrap().document.revision(),
                1
            );
        }
        let mut b = complete.clone();
        b.extend(b"garbage after complete transaction");
        fs::write(&path, b).unwrap();
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        assert!(a.discarded_bytes > 0);
        assert_eq!(a.document().revision(), 2);
        add(&mut a, 3);
        drop(a);
        assert_eq!(
            DocumentStore::new(&t.0).load().unwrap().document.revision(),
            3
        );
    }
    #[test]
    fn failed_append_does_not_publish_and_retry_does_not_duplicate() {
        for stage in 0..3 {
            let t = Temp::new();
            let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
            add(&mut a, 1);
            let before = a.document().snapshot();
            let cmd = command(2);
            let result = a.edit_with(1, 1, &cmd, |at| {
                if at == stage {
                    Err(std::io::Error::other("injected disk failure"))
                } else {
                    Ok(())
                }
            });
            assert!(result.is_err());
            assert_eq!(a.document().snapshot(), before);
            a.commit(1, &cmd).unwrap();
            drop(a);
            let loaded = DocumentStore::new(&t.0).load().unwrap();
            assert_eq!(loaded.document.revision(), 2);
            assert_eq!(loaded.document.scene().len(), 2);
        }
    }
    #[test]
    fn process_exit_during_append_or_compaction_preserves_transactions() {
        if let Ok(path) = std::env::var("THREEX_JOURNAL_CRASH") {
            let stage = std::env::var("THREEX_JOURNAL_STAGE")
                .unwrap()
                .parse::<u8>()
                .unwrap();
            let mut a = AutosaveDocument::open(DocumentStore::new(&path)).unwrap();
            add(&mut a, 1);
            if stage < 3 {
                let c = command(2);
                a.edit_with(1, 1, &c, |at| {
                    if at == stage {
                        std::process::exit(78)
                    }
                    Ok(())
                })
                .unwrap();
            } else {
                add(&mut a, 2);
                replace_with(&a.store, &a.document, |at| {
                    if at == stage - 3 {
                        std::process::exit(78)
                    }
                    Ok(())
                })
                .unwrap();
            }
            panic!("crash not reached");
        }
        for stage in 0..6 {
            let t = Temp::new();
            let status=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","journal::tests::process_exit_during_append_or_compaction_preserves_transactions"])
                .env("THREEX_JOURNAL_CRASH",&t.0).env("THREEX_JOURNAL_STAGE",stage.to_string()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().unwrap();
            assert_eq!(status.code(), Some(78));
            let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
            assert_eq!(a.document().revision(), if stage == 0 { 1 } else { 2 });
            add(&mut a, 3);
            drop(a);
            assert!(
                DocumentStore::new(&t.0)
                    .load()
                    .unwrap()
                    .document
                    .scene()
                    .object(3)
                    .is_some()
            );
        }
    }
    #[test]
    fn group_preserves_dependent_edits_undo_boundaries_and_single_sync() {
        let t = Temp::new();
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        let create = command(1);
        let mut update = [0; 64];
        update[..4].copy_from_slice(&2u32.to_le_bytes());
        update[4..8].copy_from_slice(&1u32.to_le_bytes());
        update[8..12].copy_from_slice(&7f32.to_le_bytes());
        update[56..60].copy_from_slice(&1u32.to_le_bytes());
        let mut delete = create;
        delete[..4].copy_from_slice(&3u32.to_le_bytes());
        let mut syncs = 0;
        assert_eq!(
            a.group_with(0, &[&create, &update, &[], &delete], |stage| {
                if stage == 2 {
                    syncs += 1
                }
                Ok(())
            })
            .unwrap(),
            vec![1, 2, 2, 3]
        );
        assert_eq!(syncs, 1);
        assert!(a.document().scene().is_empty());
        a.undo(3).unwrap();
        assert_eq!(a.document().scene().object(1).unwrap().position[0], 7.);
        a.undo(4).unwrap();
        assert_eq!(a.document().scene().object(1).unwrap().position[0], 0.);
        a.undo(5).unwrap();
        assert!(a.document().scene().is_empty());
        a.redo(6).unwrap();
        let expected = a.document().snapshot();
        drop(a);
        assert_eq!(
            DocumentStore::new(&t.0).load().unwrap().document.snapshot(),
            expected
        );
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        let size = a.valid_bytes;
        let mut syncs = 0;
        a.group_with(7, &[&[], &[]], |_| {
            syncs += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(syncs, 0);
        assert_eq!(a.valid_bytes, size);
        assert!(a.commit_group(7, &[&command(2), &command(1)]).is_err());
        assert_eq!(a.valid_bytes, size);
        assert!(a.document().scene().object(2).is_none());
        assert!(a.commit_group(7, &vec![&[][..]; 33]).is_err());
    }
    #[test]
    fn group_failed_sync_retry_and_every_truncation_keep_valid_prefix() {
        for stage in 0..3 {
            let t = Temp::new();
            let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
            let cmds = [command(1), command(2), command(3)];
            let refs = cmds.iter().map(|c| c.as_slice()).collect::<Vec<_>>();
            assert!(
                a.group_with(0, &refs, |at| if at == stage {
                    Err(std::io::Error::other("group IO failure"))
                } else {
                    Ok(())
                })
                .is_err()
            );
            assert_eq!(a.document().revision(), 0);
            assert!(a.document().scene().is_empty());
            a.commit_group(0, &refs).unwrap();
            drop(a);
            assert_eq!(
                DocumentStore::new(&t.0).load().unwrap().document.revision(),
                3
            );
        }
        let t = Temp::new();
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        let base = a.valid_bytes as usize;
        let cmds = [command(1), command(2), command(3)];
        let refs = cmds.iter().map(|c| c.as_slice()).collect::<Vec<_>>();
        a.commit_group(0, &refs).unwrap();
        drop(a);
        let path = t.0.join("autosave.txj");
        let bytes = fs::read(&path).unwrap();
        let length = (bytes.len() - base) / 3;
        for end in base..=bytes.len() {
            fs::write(&path, &bytes[..end]).unwrap();
            let loaded = DocumentStore::new(&t.0).load().unwrap();
            assert_eq!(loaded.document.revision(), ((end - base) / length) as u64);
        }
        for record in 0..3 {
            let mut corrupt = bytes.clone();
            corrupt[base + record * length + 20] ^= 1;
            fs::write(&path, &corrupt).unwrap();
            let loaded = DocumentStore::new(&t.0).load().unwrap();
            assert_eq!(loaded.document.revision(), record as u64);
        }
    }
    #[test]
    fn group_process_exit_recovers_complete_prefix_and_releases_writer() {
        if let Ok(path) = std::env::var("THREEX_GROUP_CRASH") {
            let stage = std::env::var("THREEX_GROUP_STAGE")
                .unwrap()
                .parse::<u8>()
                .unwrap();
            let mut a = AutosaveDocument::open(DocumentStore::new(path)).unwrap();
            let cmds = [command(1), command(2), command(3), command(4)];
            let refs = cmds.iter().map(|c| c.as_slice()).collect::<Vec<_>>();
            a.group_with(0, &refs, |at| {
                if at == stage {
                    std::process::exit(79)
                }
                Ok(())
            })
            .unwrap();
            panic!("hook missed");
        }
        for stage in 0..3 {
            let t = Temp::new();
            let status=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","journal::tests::group_process_exit_recovers_complete_prefix_and_releases_writer"])
                .env("THREEX_GROUP_CRASH",&t.0).env("THREEX_GROUP_STAGE",stage.to_string()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().unwrap();
            assert_eq!(status.code(), Some(79));
            let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
            assert_eq!(a.document().revision(), if stage == 0 { 2 } else { 4 });
            add(&mut a, 5);
            drop(a);
            assert!(
                DocumentStore::new(&t.0)
                    .load()
                    .unwrap()
                    .document
                    .scene()
                    .object(5)
                    .is_some()
            );
        }
    }
    #[test]
    fn failed_compaction_refreshes_boundary_for_same_owner_retry() {
        for stage in 0..3 {
            let t = Temp::new();
            let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
            add(&mut a, 1);
            add(&mut a, 2);
            assert!(
                a.save_compact(|at| if at == stage {
                    Err(std::io::Error::other("injected compact failure"))
                } else {
                    Ok(())
                })
                .is_err()
            );
            add(&mut a, 3);
            let expected = a.document().snapshot();
            drop(a);
            assert_eq!(
                DocumentStore::new(&t.0).load().unwrap().document.snapshot(),
                expected
            );
        }
    }
    #[test]
    fn bounded_journal_and_checksummed_invalid_commands_preserve_document() {
        let t = Temp::new();
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        add(&mut a, 1);
        let before = a.document().snapshot();
        let size = a.valid_bytes;
        a.valid_bytes = MAX_LOG;
        assert!(matches!(
            a.commit(1, &command(2)),
            Err(StorageError::JournalLimit)
        ));
        assert_eq!(a.document().snapshot(), before);
        a.valid_bytes = size;
        drop(a);
        let path = t.0.join("autosave.txj");
        let original = fs::read(&path).unwrap();
        let mut payload = vec![1];
        payload.extend(1u64.to_le_bytes());
        payload.extend(2u64.to_le_bytes());
        payload.extend(command(1));
        let mut bytes = original;
        bytes.extend(frame(&payload));
        fs::write(&path, bytes).unwrap();
        let loaded = DocumentStore::new(&t.0).load().unwrap();
        assert_eq!(loaded.document.snapshot(), before);
        assert!(loaded.discarded_journal_bytes > 0);
    }
    #[test]
    fn corrupt_anchor_falls_back_to_saved_generation_and_writer_restarts_safely() {
        let t = Temp::new();
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        add(&mut a, 1);
        a.save().unwrap();
        add(&mut a, 2);
        drop(a);
        let path = t.0.join("autosave.txj");
        let mut b = fs::read(&path).unwrap();
        b[20] ^= 1;
        fs::write(&path, b).unwrap();
        let loaded = DocumentStore::new(&t.0).load().unwrap();
        assert_eq!(loaded.document.revision(), 1);
        assert!(loaded.recovered);
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        add(&mut a, 3);
        drop(a);
        assert_eq!(
            DocumentStore::new(&t.0)
                .load()
                .unwrap()
                .document
                .scene()
                .len(),
            2
        );
        assert!(t.0.join("autosave.damaged").exists());
        let mut b = fs::read(&path).unwrap();
        b[7] = b'2';
        fs::write(&path, b).unwrap();
        assert!(matches!(
            DocumentStore::new(&t.0).load(),
            Err(StorageError::UnsupportedJournalVersion)
        ));
        assert!(matches!(
            AutosaveDocument::open(DocumentStore::new(&t.0)),
            Err(StorageError::UnsupportedJournalVersion)
        ));
    }
    #[test]
    fn conflicts_invalid_records_noops_and_writer_ownership_are_safe() {
        let t = Temp::new();
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        add(&mut a, 1);
        let size = a.valid_bytes;
        assert!(matches!(
            a.commit(0, &command(2)),
            Err(StorageError::Edit(_))
        ));
        assert!(a.commit(1, &command(1)).is_err());
        a.commit(1, &[]).unwrap();
        assert_eq!(a.valid_bytes, size);
        assert!(matches!(
            AutosaveDocument::open(DocumentStore::new(&t.0)),
            Err(StorageError::Busy)
        ));
        assert!(matches!(
            DocumentStore::new(&t.0).save(a.document()),
            Err(StorageError::Busy)
        ));
        drop(a);
        assert!(matches!(
            DocumentStore::new(&t.0).save(&Document::default()),
            Err(StorageError::JournalActive)
        ));
        // Correct CRC with a revision gap must still stop at the valid prefix.
        let mut payload = vec![1];
        payload.extend(99u64.to_le_bytes());
        payload.extend(100u64.to_le_bytes());
        payload.extend(command(2));
        OpenOptions::new()
            .append(true)
            .open(t.0.join("autosave.txj"))
            .unwrap()
            .write_all(&frame(&payload))
            .unwrap();
        let loaded = DocumentStore::new(&t.0).load().unwrap();
        assert_eq!(loaded.document.revision(), 1);
        assert!(loaded.discarded_journal_bytes > 0);
    }
}
