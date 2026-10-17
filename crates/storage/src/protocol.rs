use crate::*;
use threex_core::{DocumentId, SessionId};
pub const REQUEST_PROTOCOL_VERSION: u32 = 1;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProtocolAction {
    EndSession {
        expected_revision: u64,
    },
    Commit {
        expected_revision: u64,
        commands: Vec<u8>,
    },
    Undo {
        expected_revision: u64,
    },
    Redo {
        expected_revision: u64,
    },
}
impl ProtocolAction {
    pub(crate) fn parts(&self) -> (u8, u64, &[u8]) {
        match self {
            Self::EndSession { expected_revision } => (4, *expected_revision, &[]),
            Self::Commit {
                expected_revision,
                commands,
            } => (1, *expected_revision, commands),
            Self::Undo { expected_revision } => (2, *expected_revision, &[]),
            Self::Redo { expected_revision } => (3, *expected_revision, &[]),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolRequest {
    pub protocol_version: u32,
    pub document_id: DocumentId,
    pub session_id: SessionId,
    pub sequence: u64,
    pub action: ProtocolAction,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolReceipt {
    pub revision: u64,
    pub replayed: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolFailure {
    pub error: threex_core::DocumentError,
    pub revision: u64,
    pub replayed: bool,
}
impl AutosaveDocument {
    pub fn request(&mut self, request: &ProtocolRequest) -> Result<ProtocolReceipt, StorageError> {
        self.request_with(request, |_| Ok(()))
    }
}

pub(crate) fn push_protocol_candidate(
    builder: &mut threex_core::RequestGroupBuilder<'_>,
    request: &ProtocolRequest,
) -> Result<(u64, bool), StorageError> {
    if request.protocol_version != REQUEST_PROTOCOL_VERSION {
        return Err(StorageError::UnsupportedRequestProtocol);
    }
    let ProtocolAction::Commit {
        expected_revision,
        commands,
    } = &request.action
    else {
        return Err(StorageError::Edit(
            threex_core::DocumentError::InvalidCommand(
                "Undo/Redo require a request barrier".into(),
            ),
        ));
    };
    builder
        .push(&threex_core::RequestCommit {
            document: request.document_id,
            session: request.session_id,
            sequence: request.sequence,
            expected: *expected_revision,
            commands,
        })
        .map_err(StorageError::Request)
}
/// Prepare commit envelopes only; Undo/Redo are explicit batching barriers.
pub(crate) fn prepare_protocol_group(
    document: &threex_core::Document,
    requests: &[&ProtocolRequest],
) -> Result<threex_core::PreparedRequestGroup, StorageError> {
    let mut input = Vec::with_capacity(requests.len().min(32));
    if requests.len() > 32 {
        return Err(StorageError::Edit(
            threex_core::DocumentError::CommandTooLarge,
        ));
    }
    for r in requests {
        if r.protocol_version != REQUEST_PROTOCOL_VERSION {
            return Err(StorageError::UnsupportedRequestProtocol);
        }
        let ProtocolAction::Commit {
            expected_revision,
            commands,
        } = &r.action
        else {
            return Err(StorageError::Edit(
                threex_core::DocumentError::InvalidCommand(
                    "Undo/Redo require a request barrier".into(),
                ),
            ));
        };
        input.push(threex_core::RequestCommit {
            document: r.document_id,
            session: r.session_id,
            sequence: r.sequence,
            expected: *expected_revision,
            commands,
        });
    }
    document
        .prepare_request_group(&input)
        .map_err(StorageError::Request)
}
impl AutosaveDocument {
    /// Commit up to 32 request envelopes / 1 MiB with one journal sync.
    /// Retains per-request revision, Undo and durable deduplication boundaries.
    /// A crash can recover a complete prefix; this is not a disk-atomic batch.
    pub fn request_group(
        &mut self,
        requests: &[&ProtocolRequest],
    ) -> Result<Vec<ProtocolReceipt>, StorageError> {
        self.request_group_with(requests, |_| Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };
    use threex_core::{RequestError, SessionId};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "threex-protocol-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
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
    fn command(id: u32) -> Vec<u8> {
        let mut b = vec![0; 64];
        b[..4].copy_from_slice(&1u32.to_le_bytes());
        b[4..8].copy_from_slice(&id.to_le_bytes());
        b
    }
    fn envelope(
        d: &threex_core::Document,
        s: SessionId,
        n: u64,
        expected: u64,
        commands: Vec<u8>,
    ) -> ProtocolRequest {
        ProtocolRequest {
            protocol_version: 1,
            document_id: d.document_id().unwrap(),
            session_id: s,
            sequence: n,
            action: ProtocolAction::Commit {
                expected_revision: expected,
                commands,
            },
        }
    }
    #[test]
    fn durable_receipts_survive_restart_save_compaction_and_undo() {
        let t = Temp::new();
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        let id = a.document().document_id().unwrap();
        let s = new_session_id().unwrap();
        let r = envelope(a.document(), s, 1, 0, command(1));
        assert!(!a.request(&r).unwrap().replayed);
        drop(a);
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        assert_eq!(a.document().document_id(), Some(id));
        assert_eq!(
            a.request(&r).unwrap(),
            ProtocolReceipt {
                revision: 1,
                replayed: true
            }
        );
        a.save().unwrap();
        drop(a);
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        assert!(a.request(&r).unwrap().replayed);
        let undo = ProtocolRequest {
            sequence: 2,
            action: ProtocolAction::Undo {
                expected_revision: 1,
            },
            ..r.clone()
        };
        a.request(&undo).unwrap();
        drop(a);
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        assert!(a.request(&r).unwrap().replayed);
        assert!(a.request(&undo).unwrap().replayed);
        assert!(a.document().scene().is_empty());
        assert_eq!(a.document().revision(), 2);
        let mut mismatch = r.clone();
        mismatch.action = ProtocolAction::Commit {
            expected_revision: 0,
            commands: command(2),
        };
        assert!(matches!(
            a.request(&mismatch),
            Err(StorageError::Request(RequestError::Mismatch))
        ));
        let other = Temp::new();
        let b = AutosaveDocument::open(DocumentStore::new(&other.0)).unwrap();
        mismatch.document_id = b.document().document_id().unwrap();
        assert!(matches!(
            a.request(&mismatch),
            Err(StorageError::Request(RequestError::DocumentMismatch))
        ));
        let mut future = r;
        future.protocol_version = 2;
        assert!(matches!(
            a.request(&future),
            Err(StorageError::UnsupportedRequestProtocol)
        ));
    }
    #[test]
    fn eviction_watermark_noop_and_session_limits_survive_checkpoint() {
        let t = Temp::new();
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        let s = new_session_id().unwrap();
        let first = envelope(a.document(), s, 1, 0, vec![]);
        a.request(&first).unwrap();
        let receipt = a.save().unwrap();
        for n in 2..=66 {
            let r = envelope(a.document(), s, n, 0, vec![]);
            a.request(&r).unwrap();
        }
        assert_eq!(a.document().revision(), 0);
        assert!(!receipt.is_current(a.document()));
        a.save().unwrap();
        drop(a);
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        assert!(matches!(
            a.request(&first),
            Err(StorageError::Request(RequestError::Expired))
        ));
        let latest = envelope(a.document(), s, 66, 0, vec![]);
        assert!(a.request(&latest).unwrap().replayed);
        let gap = envelope(a.document(), s, 68, 0, vec![]);
        assert!(matches!(
            a.request(&gap),
            Err(StorageError::Request(RequestError::SequenceGap))
        ));
        for _ in 0..63 {
            let r = envelope(a.document(), new_session_id().unwrap(), 1, 0, vec![]);
            a.request(&r).unwrap();
        }
        let excess = envelope(a.document(), new_session_id().unwrap(), 1, 0, vec![]);
        assert!(matches!(
            a.request(&excess),
            Err(StorageError::Request(RequestError::SessionLimit))
        ));
        a.save().unwrap();
        drop(a);
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        assert!(matches!(
            a.request(&excess),
            Err(StorageError::Request(RequestError::SessionLimit))
        ));
    }
    #[test]
    fn legacy_journal_migration_creates_stable_id_without_replaying_edits_twice() {
        let t = Temp::new();
        let crc = |bytes: &[u8]| {
            let mut c = !0u32;
            for &v in bytes {
                c ^= v as u32;
                for _ in 0..8 {
                    c = (c >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(c & 1));
                }
            }
            !c
        };
        let mut d = threex_core::Document::default();
        d.commit(0, &command(1)).unwrap();
        let mut old = d.checkpoint().unwrap();
        old.truncate(old.len() - 36);
        old[8..12].copy_from_slice(&1u32.to_le_bytes());
        old.extend(crc(&old).to_le_bytes());
        fs::write(t.0.join("generation-00000000000000000001.txd"), &old).unwrap();
        let mut journal = b"THREEXJ1".to_vec();
        journal.extend((old.len() as u64).to_le_bytes());
        journal.extend(&old);
        journal.extend(crc(&old).to_le_bytes());
        fs::write(t.0.join("autosave.txj"), journal).unwrap();
        let a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        let id = a.document().document_id().unwrap();
        assert_eq!(a.document().revision(), 1);
        drop(a);
        assert_eq!(
            &fs::read(t.0.join("autosave.txj")).unwrap()[..8],
            b"THREEXJ6"
        );
        let a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        assert_eq!(a.document().document_id(), Some(id));
        assert_eq!(a.document().scene().len(), 1);
    }
    #[test]
    fn worker_protocol_replays_after_restart_with_same_document_identity() {
        let t = Temp::new();
        let worker =
            JournalWorker::start(DocumentStore::new(&t.0), WorkerConfig::default()).unwrap();
        let id = worker.initial_state.document_id;
        let r = ProtocolRequest {
            protocol_version: 1,
            document_id: id,
            session_id: new_session_id().unwrap(),
            sequence: 1,
            action: ProtocolAction::Commit {
                expected_revision: 0,
                commands: command(7),
            },
        };
        let send = |worker: &JournalWorker, r: ProtocolRequest| {
            worker
                .client()
                .try_submit(WorkerRequest {
                    id: 1,
                    action: WorkerAction::Protocol(r),
                })
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
        };
        assert!(matches!(
            send(&worker, r.clone()).result,
            Ok(WorkerOutcome::Protocol {
                replayed: false,
                revision: 1,
                ..
            })
        ));
        worker.shutdown().unwrap();
        let worker =
            JournalWorker::start(DocumentStore::new(&t.0), WorkerConfig::default()).unwrap();
        assert_eq!(worker.initial_state.document_id, id);
        assert!(matches!(
            send(&worker, r.clone()).result,
            Ok(WorkerOutcome::Protocol {
                replayed: true,
                revision: 1,
                ..
            })
        ));
        worker.shutdown().unwrap();
        assert_eq!(
            DocumentStore::new(&t.0)
                .load()
                .unwrap()
                .document
                .scene()
                .len(),
            1
        );
    }
    #[test]
    fn groups_match_sequential_history_and_replay_after_restart_and_compaction() {
        let t = Temp::new();
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        let mut reference =
            threex_core::Document::from_checkpoint(&a.document().checkpoint().unwrap()).unwrap();
        let session = new_session_id().unwrap();
        let other = new_session_id().unwrap();
        let first = envelope(a.document(), session, 1, 0, command(1));
        let noop = envelope(a.document(), session, 2, 1, vec![]);
        let mut child = command(2);
        child[60..64].copy_from_slice(&1u32.to_le_bytes());
        let mut update = child.clone();
        update[..4].copy_from_slice(&2u32.to_le_bytes());
        child.extend(update);
        let second = envelope(a.document(), other, 1, 1, child);
        let third = envelope(a.document(), session, 3, 2, command(3));
        let input = [&first, &first, &noop, &second, &third];
        for r in input {
            let (kind, expected, commands) = r.action.parts();
            if let threex_core::RequestPreparation::Pending(p) = reference
                .prepare_request(
                    r.document_id,
                    r.session_id,
                    r.sequence,
                    kind,
                    expected,
                    commands,
                )
                .unwrap()
            {
                reference.apply_request(p).unwrap();
            }
        }
        let mut syncs = 0;
        let results = a
            .request_group_with(&input, |stage| {
                if stage == 2 {
                    syncs += 1;
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(syncs, 1);
        assert_eq!(a.document().scene().object(2).unwrap().parent, 1);
        assert_eq!(
            results
                .iter()
                .map(|r| (r.revision, r.replayed))
                .collect::<Vec<_>>(),
            vec![(1, false), (1, true), (1, false), (2, false), (3, false)]
        );
        assert_eq!(
            a.document().checkpoint().unwrap(),
            reference.checkpoint().unwrap()
        );
        assert!(
            a.request_group_with(&input, |_| panic!("replays must not write or sync"))
                .unwrap()
                .iter()
                .all(|r| r.replayed)
        );
        a.save().unwrap();
        drop(a);
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        assert!(a.request_group(&input).unwrap().iter().all(|r| r.replayed));
        let undo = ProtocolRequest {
            sequence: 4,
            action: ProtocolAction::Undo {
                expected_revision: 3,
            },
            ..first.clone()
        };
        a.request(&undo).unwrap();
        assert!(a.document().scene().object(3).is_none());
        assert!(a.request_group(&input).unwrap().iter().all(|r| r.replayed));
        assert_eq!(a.document().revision(), 4);
    }
    #[test]
    fn invalid_groups_reject_before_io_and_noop_groups_sync_once() {
        let t = Temp::new();
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        let session = new_session_id().unwrap();
        let first = envelope(a.document(), session, 1, 0, command(1));
        let bad = envelope(a.document(), session, 2, 0, command(2));
        let before = fs::read(t.0.join("autosave.txj")).unwrap();
        assert!(
            a.request_group_with(&[&first, &bad], |_| panic!("invalid group performed IO"))
                .is_err()
        );
        assert_eq!(a.document().revision(), 0);
        assert_eq!(a.document().session_sequence(session), None);
        assert_eq!(fs::read(t.0.join("autosave.txj")).unwrap(), before);
        let noop1 = envelope(a.document(), session, 1, 0, vec![]);
        let noop2 = envelope(a.document(), session, 2, 0, vec![]);
        let mut syncs = 0;
        a.request_group_with(&[&noop1, &noop2], |at| {
            if at == 2 {
                syncs += 1;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(syncs, 1);
        assert_eq!(a.document().revision(), 0);
        assert_eq!(a.document().session_sequence(session), Some(2));
        assert!(fs::read(t.0.join("autosave.txj")).unwrap().len() > before.len());
        let mut mismatch = noop1.clone();
        mismatch.action = first.action.clone();
        assert!(matches!(
            a.request_group(&[&mismatch]),
            Err(StorageError::Request(RequestError::Mismatch))
        ));
        let mut unsupported = noop1.clone();
        unsupported.protocol_version = 2;
        assert!(matches!(
            a.request_group(&[&unsupported]),
            Err(StorageError::UnsupportedRequestProtocol)
        ));
        let barrier = ProtocolRequest {
            sequence: 3,
            action: ProtocolAction::Undo {
                expected_revision: 0,
            },
            ..noop1.clone()
        };
        assert!(a.request_group(&[&barrier]).is_err());
        assert!(a.request_group(&[]).unwrap().is_empty());
        drop(a);
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        assert!(
            a.request_group(&[&noop1, &noop2])
                .unwrap()
                .iter()
                .all(|r| r.replayed)
        );
    }
    fn four(a: &AutosaveDocument) -> Vec<ProtocolRequest> {
        let session = SessionId::from_bytes([19; 16]).unwrap();
        (0..4)
            .map(|i| envelope(a.document(), session, i + 1, i, command(i as u32 + 1)))
            .collect()
    }
    #[test]
    fn every_group_suffix_cut_and_corruption_preserves_edit_receipt_prefix() {
        let t = Temp::new();
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        let path = t.0.join("autosave.txj");
        let base = fs::metadata(&path).unwrap().len() as usize;
        let requests = four(&a);
        a.request_group(&requests.iter().collect::<Vec<_>>())
            .unwrap();
        drop(a);
        let full = fs::read(&path).unwrap();
        let frame = (full.len() - base) / 4;
        for end in base..=full.len() {
            fs::write(&path, &full[..end]).unwrap();
            let d = DocumentStore::new(&t.0).load().unwrap().document;
            let count = (end - base) / frame;
            assert_eq!(d.revision(), count as u64);
            assert_eq!(d.scene().len(), count);
            assert_eq!(
                d.session_sequence(requests[0].session_id),
                if count == 0 { None } else { Some(count as u64) }
            );
        }
        for at in base + 3 * frame..full.len() {
            let mut damaged = full.clone();
            damaged[at] ^= 1;
            fs::write(&path, damaged).unwrap();
            let d = DocumentStore::new(&t.0).load().unwrap().document;
            assert_eq!(d.revision(), 3);
            assert_eq!(d.session_sequence(requests[0].session_id), Some(3));
        }
        for count in 0..=4 {
            fs::write(&path, &full[..base + count * frame]).unwrap();
            let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
            let results = a
                .request_group(&requests.iter().collect::<Vec<_>>())
                .unwrap();
            for (index, r) in results.iter().enumerate() {
                assert_eq!(r.replayed, index < count);
            }
            assert_eq!(a.document().revision(), 4);
            assert_eq!(a.document().scene().len(), 4);
            a.save().unwrap();
            drop(a);
            // Remove generations so the next artificial truncation tests journal-prefix recovery.
            for entry in fs::read_dir(&t.0).unwrap() {
                let entry = entry.unwrap();
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("generation-")
                {
                    fs::remove_file(entry.path()).unwrap();
                }
            }
        }
    }
    #[test]
    fn group_write_failures_publish_nothing_and_same_owner_retry_is_safe() {
        for stage in 0..3 {
            let t = Temp::new();
            let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
            let requests = four(&a);
            assert!(
                a.request_group_with(&requests.iter().collect::<Vec<_>>(), |at| if at == stage {
                    Err(std::io::Error::other("injected failure"))
                } else {
                    Ok(())
                })
                .is_err()
            );
            assert_eq!(a.document().revision(), 0);
            assert_eq!(a.document().request_generation(), 0);
            let results = a
                .request_group(&requests.iter().collect::<Vec<_>>())
                .unwrap();
            assert!(results.iter().all(|r| !r.replayed));
            drop(a);
            let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
            assert!(
                a.request_group(&requests.iter().collect::<Vec<_>>())
                    .unwrap()
                    .iter()
                    .all(|r| r.replayed)
            );
            assert_eq!(a.document().scene().len(), 4);
        }
    }
    #[test]
    fn process_exit_in_protocol_group_recovers_and_deduplicates_prefix() {
        if let Ok(path) = std::env::var("THREEX_PROTOCOL_GROUP_EXIT") {
            let stage = std::env::var("THREEX_PROTOCOL_GROUP_STAGE")
                .unwrap()
                .parse::<u8>()
                .unwrap();
            let mut a = AutosaveDocument::open(DocumentStore::new(path)).unwrap();
            let requests = four(&a);
            a.request_group_with(&requests.iter().collect::<Vec<_>>(), |at| {
                if at == stage {
                    std::process::exit(81)
                }
                Ok(())
            })
            .unwrap();
            panic!("missed exit");
        }
        for stage in 0..3 {
            let t = Temp::new();
            let status=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","protocol::tests::process_exit_in_protocol_group_recovers_and_deduplicates_prefix"])
                .env("THREEX_PROTOCOL_GROUP_EXIT",&t.0).env("THREEX_PROTOCOL_GROUP_STAGE",stage.to_string()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().unwrap();
            assert_eq!(status.code(), Some(81));
            let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
            let count = if stage == 0 { 2 } else { 4 };
            assert_eq!(a.document().revision(), count);
            let requests = four(&a);
            let results = a
                .request_group(&requests.iter().collect::<Vec<_>>())
                .unwrap();
            for (i, r) in results.iter().enumerate() {
                assert_eq!(r.replayed, (i as u64) < count);
            }
            assert_eq!(a.document().scene().len(), 4);
            a.save().unwrap();
            drop(a);
            let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
            assert!(
                a.request_group(&requests.iter().collect::<Vec<_>>())
                    .unwrap()
                    .iter()
                    .all(|r| r.replayed)
            );
        }
    }
    #[test]
    fn journal_sync_calls_reduce_from_request_count_to_one() {
        for size in [1, 8, 32] {
            let before = Temp::new();
            let after = Temp::new();
            let mut sequential = AutosaveDocument::open(DocumentStore::new(&before.0)).unwrap();
            let mut grouped = AutosaveDocument::open(DocumentStore::new(&after.0)).unwrap();
            let session = new_session_id().unwrap();
            let requests = (0..size)
                .map(|i| envelope(grouped.document(), session, i + 1, i, command(i as u32 + 1)))
                .collect::<Vec<_>>();
            let mut single_syncs = 0;
            for r in &requests {
                let mut r = r.clone();
                r.document_id = sequential.document().document_id().unwrap();
                sequential
                    .request_with(&r, |stage| {
                        if stage == 2 {
                            single_syncs += 1;
                        }
                        Ok(())
                    })
                    .unwrap();
            }
            let mut group_syncs = 0;
            grouped
                .request_group_with(&requests.iter().collect::<Vec<_>>(), |stage| {
                    if stage == 2 {
                        group_syncs += 1;
                    }
                    Ok(())
                })
                .unwrap();
            assert_eq!(single_syncs, size);
            assert_eq!(group_syncs, 1);
            assert_eq!(
                sequential.document().snapshot(),
                grouped.document().snapshot()
            );
            grouped
                .request_group_with(&requests.iter().collect::<Vec<_>>(), |_| {
                    panic!("replay performed IO")
                })
                .unwrap();
            drop(grouped);
            let loaded = DocumentStore::new(&after.0).load().unwrap();
            assert_eq!(loaded.journal_transactions, size as usize);
            assert_eq!(loaded.document.session_sequence(session), Some(size));
        }
    }
    #[test]
    fn session_end_survives_restart_undo_compaction_and_receipt_eviction() {
        let t = Temp::new();
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        let session = new_session_id().unwrap();
        let edit = envelope(a.document(), session, 1, 0, command(1));
        a.request(&edit).unwrap();
        let saved = a.save().unwrap();
        let close = ProtocolRequest {
            sequence: 2,
            action: ProtocolAction::EndSession {
                expected_revision: 1,
            },
            ..edit.clone()
        };
        a.request(&close).unwrap();
        assert!(!saved.is_current(a.document()));
        assert_eq!(a.document().revision(), 1);
        assert!(
            a.request_with(&close, |_| panic!("close replay performed IO"))
                .unwrap()
                .replayed
        );
        a.save().unwrap();
        drop(a);
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        assert!(a.request(&edit).unwrap().replayed);
        assert!(a.request(&close).unwrap().replayed);
        let future = envelope(a.document(), session, 3, 1, command(2));
        assert!(matches!(
            a.request(&future),
            Err(StorageError::Request(RequestError::SessionClosed))
        ));
        assert!(matches!(
            a.request_group(&[&future]),
            Err(StorageError::Request(RequestError::SessionClosed))
        ));
        let undo = ProtocolRequest {
            session_id: new_session_id().unwrap(),
            sequence: 1,
            action: ProtocolAction::Undo {
                expected_revision: 1,
            },
            ..edit.clone()
        };
        a.request(&undo).unwrap();
        assert!(a.request(&edit).unwrap().replayed);
        assert!(a.document().scene().is_empty());
        let other = new_session_id().unwrap();
        for start in [1, 33, 65] {
            let requests = (start..=(start + 31).min(70))
                .map(|n| envelope(a.document(), other, n, 2, vec![]))
                .collect::<Vec<_>>();
            a.request_group(&requests.iter().collect::<Vec<_>>())
                .unwrap();
        }
        a.save().unwrap();
        drop(a);
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        assert!(matches!(
            a.request(&edit),
            Err(StorageError::Request(RequestError::Expired))
        ));
        assert!(matches!(
            a.request(&close),
            Err(StorageError::Request(RequestError::Expired))
        ));
        assert!(matches!(
            a.request(&future),
            Err(StorageError::Request(RequestError::SessionClosed))
        ));
        assert_eq!(
            a.document().session_status(session),
            Some(threex_core::SessionStatus::Closed { sequence: 2 })
        );
        assert!(a.document().scene().is_empty());
    }
    #[test]
    fn worker_session_handshake_reports_closed_after_restart() {
        let t = Temp::new();
        let worker =
            JournalWorker::start(DocumentStore::new(&t.0), WorkerConfig::default()).unwrap();
        let session = new_session_id().unwrap();
        let edit = ProtocolRequest {
            protocol_version: 1,
            document_id: worker.initial_state.document_id,
            session_id: session,
            sequence: 1,
            action: ProtocolAction::Commit {
                expected_revision: 0,
                commands: command(1),
            },
        };
        let send = |w: &JournalWorker, id, action| {
            w.client()
                .try_submit(WorkerRequest { id, action })
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
        };
        send(&worker, 1, WorkerAction::Protocol(edit.clone()))
            .result
            .unwrap();
        let close = ProtocolRequest {
            sequence: 2,
            action: ProtocolAction::EndSession {
                expected_revision: 1,
            },
            ..edit.clone()
        };
        send(&worker, 2, WorkerAction::Protocol(close.clone()))
            .result
            .unwrap();
        worker.shutdown().unwrap();
        let worker =
            JournalWorker::start(DocumentStore::new(&t.0), WorkerConfig::default()).unwrap();
        assert!(matches!(
            send(
                &worker,
                1,
                WorkerAction::InspectSession {
                    session_id: session
                }
            )
            .result,
            Ok(WorkerOutcome::Session {
                status: Some(threex_core::SessionStatus::Closed { sequence: 2 }),
                ..
            })
        ));
        assert!(matches!(
            send(&worker, 2, WorkerAction::Protocol(edit.clone())).result,
            Ok(WorkerOutcome::Protocol {
                replayed: true,
                revision: 1,
                ..
            })
        ));
        let future = ProtocolRequest {
            sequence: 3,
            action: ProtocolAction::Commit {
                expected_revision: 1,
                commands: command(2),
            },
            ..edit
        };
        assert_eq!(
            send(&worker, 3, WorkerAction::Protocol(future))
                .result
                .unwrap_err()
                .code,
            WorkerCode::SessionClosed
        );
        assert!(matches!(
            send(&worker, 4, WorkerAction::Protocol(close)).result,
            Ok(WorkerOutcome::Protocol { replayed: true, .. })
        ));
        worker.shutdown().unwrap();
    }
    #[test]
    fn every_close_suffix_cut_preserves_atomic_session_state() {
        let t = Temp::new();
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        let session = new_session_id().unwrap();
        let edit = envelope(a.document(), session, 1, 0, command(1));
        a.request(&edit).unwrap();
        let path = t.0.join("autosave.txj");
        let base = fs::metadata(&path).unwrap().len() as usize;
        let close = ProtocolRequest {
            sequence: 2,
            action: ProtocolAction::EndSession {
                expected_revision: 1,
            },
            ..edit.clone()
        };
        a.request(&close).unwrap();
        drop(a);
        let full = fs::read(&path).unwrap();
        for end in base..=full.len() {
            fs::write(&path, &full[..end]).unwrap();
            let loaded = DocumentStore::new(&t.0).load().unwrap();
            assert_eq!(
                loaded.document.session_status(session),
                Some(if end == full.len() {
                    threex_core::SessionStatus::Closed { sequence: 2 }
                } else {
                    threex_core::SessionStatus::Active { sequence: 1 }
                })
            );
            assert_eq!(loaded.document.scene().len(), 1);
        }
        for at in base..full.len() {
            let mut corrupt = full.clone();
            corrupt[at] ^= 1;
            fs::write(&path, corrupt).unwrap();
            let d = DocumentStore::new(&t.0).load().unwrap().document;
            assert_eq!(
                d.session_status(session),
                Some(threex_core::SessionStatus::Active { sequence: 1 })
            );
        }
        fs::write(&path, &full).unwrap();
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        assert!(a.request(&edit).unwrap().replayed);
        assert!(a.request(&close).unwrap().replayed);
    }
    #[test]
    fn process_exit_during_session_close_is_recoverable_and_retry_safe() {
        if let Ok(path) = std::env::var("THREEX_CLOSE_EXIT") {
            let stage = std::env::var("THREEX_CLOSE_STAGE")
                .unwrap()
                .parse::<u8>()
                .unwrap();
            let mut a = AutosaveDocument::open(DocumentStore::new(path)).unwrap();
            let session = SessionId::from_bytes([91; 16]).unwrap();
            let edit = envelope(a.document(), session, 1, 0, command(1));
            a.request(&edit).unwrap();
            a.save().unwrap();
            let close = ProtocolRequest {
                sequence: 2,
                action: ProtocolAction::EndSession {
                    expected_revision: 1,
                },
                ..edit
            };
            a.request_with(&close, |at| {
                if at == stage {
                    std::process::exit(82)
                }
                Ok(())
            })
            .unwrap();
            panic!("missed exit");
        }
        for stage in 0..3 {
            let t = Temp::new();
            let status=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","protocol::tests::process_exit_during_session_close_is_recoverable_and_retry_safe"]).env("THREEX_CLOSE_EXIT",&t.0).env("THREEX_CLOSE_STAGE",stage.to_string()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().unwrap();
            assert_eq!(status.code(), Some(82));
            let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
            let session = SessionId::from_bytes([91; 16]).unwrap();
            let edit = envelope(a.document(), session, 1, 0, command(1));
            let close = ProtocolRequest {
                sequence: 2,
                action: ProtocolAction::EndSession {
                    expected_revision: 1,
                },
                ..edit.clone()
            };
            assert_eq!(a.request(&close).unwrap().replayed, stage != 0);
            assert!(a.request(&edit).unwrap().replayed);
            assert!(matches!(
                a.request(&ProtocolRequest {
                    sequence: 3,
                    action: ProtocolAction::Commit {
                        expected_revision: 1,
                        commands: command(2)
                    },
                    ..edit
                }),
                Err(StorageError::Request(RequestError::SessionClosed))
            ));
            assert_eq!(a.document().revision(), 1);
            assert_eq!(a.document().scene().len(), 1);
        }
    }
    #[test]
    fn journal_v2_migration_reanchors_v3_before_accepting_close() {
        let t = Temp::new();
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        let session = new_session_id().unwrap();
        let edit = envelope(a.document(), session, 1, 0, command(1));
        a.request(&edit).unwrap();
        drop(a);
        let path = t.0.join("autosave.txj");
        let mut old = fs::read(&path).unwrap();
        old[7] = b'2';
        // The empty anchor has no session flags; its v3 layout otherwise equals v2.
        let length = u64::from_le_bytes(old[8..16].try_into().unwrap()) as usize;
        old[24..28].copy_from_slice(&2u32.to_le_bytes());
        fn crc(bytes: &[u8]) -> u32 {
            let mut c = !0u32;
            for &v in bytes {
                c ^= v as u32;
                for _ in 0..8 {
                    c = (c >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(c & 1));
                }
            }
            !c
        }
        let inner = 16 + length - 4;
        let c = crc(&old[16..inner]);
        old[inner..inner + 4].copy_from_slice(&c.to_le_bytes());
        let c = crc(&old[16..16 + length]);
        old[16 + length..20 + length].copy_from_slice(&c.to_le_bytes());
        fs::write(&path, old).unwrap();
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        assert_eq!(&fs::read(&path).unwrap()[..8], b"THREEXJ6");
        assert!(a.request(&edit).unwrap().replayed);
        let close = ProtocolRequest {
            sequence: 2,
            action: ProtocolAction::EndSession {
                expected_revision: 1,
            },
            ..edit
        };
        a.request(&close).unwrap();
        drop(a);
        let d = DocumentStore::new(&t.0).load().unwrap().document;
        assert_eq!(
            d.session_status(session),
            Some(threex_core::SessionStatus::Closed { sequence: 2 })
        );
    }
    fn rejection(result: Result<ProtocolReceipt, StorageError>) -> ProtocolFailure {
        let Err(StorageError::Rejected(failure)) = result else {
            panic!("expected durable failure: {result:?}")
        };
        failure
    }
    #[test]
    fn errors_survive_state_changes_restart_close_and_result_eviction() {
        let t = Temp::new();
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        let session = new_session_id().unwrap();
        let bad = ProtocolRequest {
            protocol_version: 1,
            document_id: a.document().document_id().unwrap(),
            session_id: session,
            sequence: 1,
            action: ProtocolAction::Undo {
                expected_revision: 0,
            },
        };
        let first = rejection(a.request(&bad));
        assert_eq!(
            first,
            ProtocolFailure {
                error: threex_core::DocumentError::NothingToUndo,
                revision: 0,
                replayed: false
            }
        );
        assert_eq!(a.document().revision(), 0);
        a.commit(0, &command(1)).unwrap();
        let replay = rejection(a.request_with(&bad, |_| panic!("replay wrote journal")));
        assert_eq!(replay.error, first.error);
        assert_eq!(replay.revision, 0);
        assert!(replay.replayed);
        assert_eq!(a.document().scene().len(), 1);
        a.save().unwrap();
        drop(a);
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        assert_eq!(rejection(a.request(&bad)), replay);
        let changed = ProtocolRequest {
            action: ProtocolAction::Commit {
                expected_revision: 1,
                commands: command(2),
            },
            ..bad.clone()
        };
        assert!(matches!(
            a.request(&changed),
            Err(StorageError::Request(RequestError::Mismatch))
        ));
        let good = ProtocolRequest {
            sequence: 2,
            action: ProtocolAction::Undo {
                expected_revision: 1,
            },
            ..bad.clone()
        };
        a.request(&good).unwrap();
        assert!(a.document().scene().is_empty());
        let close = ProtocolRequest {
            sequence: 3,
            action: ProtocolAction::EndSession {
                expected_revision: 2,
            },
            ..bad.clone()
        };
        a.request(&close).unwrap();
        assert_eq!(rejection(a.request(&bad)), replay);
        let other = new_session_id().unwrap();
        for start in [1, 33, 65] {
            let requests = (start..=(start + 31).min(70))
                .map(|n| envelope(a.document(), other, n, 2, vec![]))
                .collect::<Vec<_>>();
            a.request_group(&requests.iter().collect::<Vec<_>>())
                .unwrap();
        }
        a.save().unwrap();
        drop(a);
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        assert!(matches!(
            a.request(&bad),
            Err(StorageError::Request(RequestError::Expired))
        ));
        assert!(a.document().scene().is_empty());
    }
    #[test]
    fn malformed_commands_are_durable_but_admission_failures_do_not_consume_sequence() {
        let t = Temp::new();
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        let session = new_session_id().unwrap();
        let bad = envelope(a.document(), session, 1, 0, vec![1, 2, 3]);
        let original = rejection(a.request(&bad));
        assert!(matches!(
            original.error,
            threex_core::DocumentError::InvalidCommand(_)
        ));
        a.save().unwrap();
        drop(a);
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        let replay = rejection(a.request(&bad));
        assert_eq!(replay.error, original.error);
        assert_eq!(replay.revision, original.revision);
        assert!(replay.replayed);
        let other = new_session_id().unwrap();
        let gap = envelope(a.document(), other, 2, 0, vec![]);
        let before = fs::read(t.0.join("autosave.txj")).unwrap();
        assert!(matches!(
            a.request(&gap),
            Err(StorageError::Request(RequestError::SequenceGap))
        ));
        let oversized = envelope(a.document(), other, 1, 0, vec![0; 256 * 1024 + 1]);
        assert!(matches!(
            a.request(&oversized),
            Err(StorageError::Request(RequestError::Edit(
                threex_core::DocumentError::CommandTooLarge
            )))
        ));
        assert_eq!(a.document().session_sequence(other), None);
        assert_eq!(fs::read(t.0.join("autosave.txj")).unwrap(), before);
    }
    #[test]
    fn durable_error_worker_reply_keeps_original_revision_after_restart() {
        let t = Temp::new();
        let worker =
            JournalWorker::start(DocumentStore::new(&t.0), WorkerConfig::default()).unwrap();
        let bad = ProtocolRequest {
            protocol_version: 1,
            document_id: worker.initial_state.document_id,
            session_id: new_session_id().unwrap(),
            sequence: 1,
            action: ProtocolAction::Undo {
                expected_revision: 0,
            },
        };
        let send = |w: &JournalWorker, id, action| {
            w.client()
                .try_submit(WorkerRequest { id, action })
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
        };
        let first = send(&worker, 1, WorkerAction::Protocol(bad.clone()))
            .result
            .unwrap_err();
        assert_eq!(first.code, WorkerCode::NoHistory);
        assert_eq!(first.current_revision, 0);
        send(
            &worker,
            2,
            WorkerAction::Commit {
                expected_revision: 0,
                commands: command(1),
            },
        )
        .result
        .unwrap();
        assert_eq!(
            send(&worker, 3, WorkerAction::Protocol(bad.clone()))
                .result
                .unwrap_err(),
            first
        );
        send(&worker, 4, WorkerAction::Save).result.unwrap();
        worker.shutdown().unwrap();
        let worker =
            JournalWorker::start(DocumentStore::new(&t.0), WorkerConfig::default()).unwrap();
        assert_eq!(
            send(&worker, 1, WorkerAction::Protocol(bad.clone()))
                .result
                .unwrap_err(),
            first
        );
        let good = ProtocolRequest {
            sequence: 2,
            action: ProtocolAction::Commit {
                expected_revision: 1,
                commands: command(2),
            },
            ..bad.clone()
        };
        send(&worker, 2, WorkerAction::Protocol(good))
            .result
            .unwrap();
        assert_eq!(
            send(&worker, 3, WorkerAction::Protocol(bad))
                .result
                .unwrap_err(),
            first
        );
        worker.shutdown().unwrap();
        assert_eq!(
            DocumentStore::new(&t.0)
                .load()
                .unwrap()
                .document
                .scene()
                .len(),
            2
        );
    }
    #[test]
    fn rejection_record_all_cuts_and_corruptions_recover_last_outcome_boundary() {
        let t = Temp::new();
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        let path = t.0.join("autosave.txj");
        let base = fs::metadata(&path).unwrap().len() as usize;
        let bad = ProtocolRequest {
            protocol_version: 1,
            document_id: a.document().document_id().unwrap(),
            session_id: new_session_id().unwrap(),
            sequence: 1,
            action: ProtocolAction::Undo {
                expected_revision: 0,
            },
        };
        let expected = rejection(a.request(&bad));
        drop(a);
        let full = fs::read(&path).unwrap();
        for end in base..=full.len() {
            fs::write(&path, &full[..end]).unwrap();
            let d = DocumentStore::new(&t.0).load().unwrap().document;
            assert_eq!(
                d.session_sequence(bad.session_id),
                if end == full.len() { Some(1) } else { None }
            );
            assert_eq!(d.revision(), 0);
        }
        for at in base..full.len() {
            let mut damaged = full.clone();
            damaged[at] ^= 1;
            fs::write(&path, damaged).unwrap();
            assert_eq!(
                DocumentStore::new(&t.0)
                    .load()
                    .unwrap()
                    .document
                    .session_sequence(bad.session_id),
                None
            );
        }
        fs::write(&path, full).unwrap();
        let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        let replay = rejection(a.request(&bad));
        assert_eq!(replay.error, expected.error);
        assert!(replay.replayed);
    }
    #[test]
    fn process_exit_during_rejection_preserves_exact_result_and_retry_safety() {
        if let Ok(path) = std::env::var("THREEX_ERROR_EXIT") {
            let stage = std::env::var("THREEX_ERROR_STAGE")
                .unwrap()
                .parse::<u8>()
                .unwrap();
            let mut a = AutosaveDocument::open(DocumentStore::new(path)).unwrap();
            let bad = ProtocolRequest {
                protocol_version: 1,
                document_id: a.document().document_id().unwrap(),
                session_id: SessionId::from_bytes([111; 16]).unwrap(),
                sequence: 1,
                action: ProtocolAction::Undo {
                    expected_revision: 0,
                },
            };
            let _ = a.request_with(&bad, |at| {
                if at == stage {
                    std::process::exit(83)
                }
                Ok(())
            });
            panic!("missed exit");
        }
        for stage in 0..3 {
            let t = Temp::new();
            let status=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","protocol::tests::process_exit_during_rejection_preserves_exact_result_and_retry_safety"]).env("THREEX_ERROR_EXIT",&t.0).env("THREEX_ERROR_STAGE",stage.to_string()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().unwrap();
            assert_eq!(status.code(), Some(83));
            let mut a = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
            let bad = ProtocolRequest {
                protocol_version: 1,
                document_id: a.document().document_id().unwrap(),
                session_id: SessionId::from_bytes([111; 16]).unwrap(),
                sequence: 1,
                action: ProtocolAction::Undo {
                    expected_revision: 0,
                },
            };
            let result = rejection(a.request(&bad));
            assert_eq!(result.replayed, stage != 0);
            assert_eq!(result.error, threex_core::DocumentError::NothingToUndo);
            assert_eq!(a.document().revision(), 0);
            assert_eq!(a.document().session_sequence(bad.session_id), Some(1));
        }
    }
}
