use super::*;
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use threex_core::{AssetId, AssetRef, asset_command};
static SERIAL: AtomicU64 = AtomicU64::new(1);
struct Temp(std::path::PathBuf);
impl Temp {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "threex-assets-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        )))
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn reference(n: u64) -> AssetRef {
    AssetRef::new(AssetId::from_bytes([3; 16]).unwrap(), n, [n as u8; 32]).unwrap()
}
fn create(id: u32) -> Vec<u8> {
    let mut b = vec![0; 64];
    b[..4].copy_from_slice(&1u32.to_le_bytes());
    b[4..8].copy_from_slice(&id.to_le_bytes());
    b
}
fn request(
    id: threex_core::DocumentId,
    session: threex_core::SessionId,
    sequence: u64,
    expected: u64,
    commands: Vec<u8>,
) -> ProtocolRequest {
    ProtocolRequest {
        protocol_version: 1,
        document_id: id,
        session_id: session,
        sequence,
        action: ProtocolAction::Commit {
            expected_revision: expected,
            commands,
        },
    }
}
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
fn begin(
    client: &WorkerClient,
    next: &mut u64,
    id: u32,
) -> threex_renderer::resource_handles::ResourceHandle {
    let WorkerOutcome::AssetJob {
        ticket,
        reference: Some(_),
        ..
    } = call(client, next, WorkerAction::BeginAssetJob { id })
    else {
        panic!()
    };
    ticket.unwrap()
}
fn finish(
    client: &WorkerClient,
    next: &mut u64,
    ticket: threex_renderer::resource_handles::ResourceHandle,
    reference: AssetRef,
) -> bool {
    let WorkerOutcome::AssetFinished { accepted, .. } = call(
        client,
        next,
        WorkerAction::FinishAssetJob { ticket, reference },
    ) else {
        panic!()
    };
    accepted
}
#[test]
fn worker_asset_restore_replay_and_stale_completion_after_undo_restart() {
    let t = Temp::new();
    let worker = JournalWorker::start(DocumentStore::new(&t.0), WorkerConfig::default()).unwrap();
    let client = worker.client();
    let mut next = 0;
    let id = worker.initial_state.document_id;
    let session = new_session_id().unwrap();
    let first = request(
        id,
        session,
        1,
        0,
        [create(1), asset_command(1, Some(reference(1))).to_vec()].concat(),
    );
    call(&client, &mut next, WorkerAction::Protocol(first.clone()));
    let initial = begin(&client, &mut next, 1);
    call(&client, &mut next, WorkerAction::Save);
    assert!(
        finish(&client, &mut next, initial, reference(1)),
        "save alone must preserve job"
    );
    let old = begin(&client, &mut next, 1);
    let replacement = request(
        id,
        session,
        2,
        1,
        asset_command(1, Some(reference(2))).to_vec(),
    );
    call(
        &client,
        &mut next,
        WorkerAction::Protocol(replacement.clone()),
    );
    assert!(!finish(&client, &mut next, old, reference(1)));
    let aba = begin(&client, &mut next, 1);
    for (sequence, action) in [
        (
            3,
            ProtocolAction::Undo {
                expected_revision: 2,
            },
        ),
        (
            4,
            ProtocolAction::Redo {
                expected_revision: 3,
            },
        ),
    ] {
        call(
            &client,
            &mut next,
            WorkerAction::Protocol(ProtocolRequest {
                protocol_version: 1,
                document_id: id,
                session_id: session,
                sequence,
                action,
            }),
        );
    }
    assert!(
        !finish(&client, &mut next, aba, reference(2)),
        "Undo/Redo must not revive a job"
    );
    let restart_old = begin(&client, &mut next, 1);
    call(&client, &mut next, WorkerAction::Save);
    worker.shutdown().unwrap();
    let worker = JournalWorker::start(DocumentStore::new(&t.0), WorkerConfig::default()).unwrap();
    let client = worker.client();
    next = 0;
    assert_eq!(worker.initial_state.revision, 4);
    assert_eq!(worker.initial_state.document_id, id);
    let WorkerOutcome::Object {
        object: Some(o), ..
    } = call(&client, &mut next, WorkerAction::ReadObject { id: 1 })
    else {
        panic!()
    };
    assert_eq!(o.asset, Some(reference(2)));
    for old in [first, replacement] {
        let WorkerOutcome::Protocol {
            replayed, state, ..
        } = call(&client, &mut next, WorkerAction::Protocol(old))
        else {
            panic!()
        };
        assert!(replayed);
        assert_eq!(state.revision, 4);
    }
    assert!(!finish(&client, &mut next, restart_old, reference(2)));
    let fresh = begin(&client, &mut next, 1);
    assert!(
        !finish(&client, &mut next, fresh, reference(1)),
        "wrong declared content rejected"
    );
    let hash_mismatch = begin(&client, &mut next, 1);
    let wrong_hash = AssetRef::new(reference(2).id(), 2, [99; 32]).unwrap();
    assert!(!finish(&client, &mut next, hash_mismatch, wrong_hash));
    let cancelled = begin(&client, &mut next, 1);
    let current = begin(&client, &mut next, 1);
    assert!(!finish(&client, &mut next, cancelled, reference(2)));
    assert!(finish(&client, &mut next, current, reference(2)));
    assert!(
        !finish(&client, &mut next, current, reference(2)),
        "one publication only"
    );
    worker.shutdown().unwrap();
}
#[test]
fn bounded_worker_jobs_release_stale_capacity_and_reject_deleted_entity() {
    let t = Temp::new();
    let worker = JournalWorker::start(DocumentStore::new(&t.0), WorkerConfig::default()).unwrap();
    let client = worker.client();
    let mut next = 0;
    let mut commands = Vec::new();
    for id in 1..=129 {
        commands.extend(create(id));
        commands.extend(asset_command(id, Some(reference(1))));
    }
    call(
        &client,
        &mut next,
        WorkerAction::Commit {
            expected_revision: 0,
            commands,
        },
    );
    for id in 1..=128 {
        begin(&client, &mut next, id);
    }
    let WorkerOutcome::AssetJob {
        ticket, reference, ..
    } = call(&client, &mut next, WorkerAction::BeginAssetJob { id: 129 })
    else {
        panic!()
    };
    assert!(ticket.is_none());
    assert!(reference.is_some());
    let old = begin(&client, &mut next, 1); // replacement reuses its own cancelled slot
    let mut delete = create(1);
    delete[..4].copy_from_slice(&3u32.to_le_bytes());
    call(
        &client,
        &mut next,
        WorkerAction::Commit {
            expected_revision: 1,
            commands: delete,
        },
    );
    assert!(!finish(&client, &mut next, old, self::reference(1)));
    let current = begin(&client, &mut next, 129); // prunes other old-revision jobs
    assert!(finish(&client, &mut next, current, self::reference(1)));
    let WorkerOutcome::AssetJob {
        ticket, reference, ..
    } = call(&client, &mut next, WorkerAction::BeginAssetJob { id: 1 })
    else {
        panic!()
    };
    assert!(ticket.is_none());
    assert!(reference.is_none());
    worker.shutdown().unwrap();
}
#[test]
fn asset_journal_group_tail_cuts_recover_atomic_edit_and_receipt_prefix() {
    let t = Temp::new();
    let store = DocumentStore::new(&t.0);
    let mut owner = AutosaveDocument::open(store).unwrap();
    let id = owner.document().document_id().unwrap();
    let session = new_session_id().unwrap();
    let first = request(
        id,
        session,
        1,
        0,
        [create(1), asset_command(1, Some(reference(1))).to_vec()].concat(),
    );
    owner.request(&first).unwrap();
    let path = t.0.join("autosave.txj");
    let boundary = fs::metadata(&path).unwrap().len() as usize;
    let second = request(
        id,
        session,
        2,
        1,
        asset_command(1, Some(reference(2))).to_vec(),
    );
    let third = request(id, session, 3, 2, asset_command(1, None).to_vec());
    owner.request_group(&[&second, &third]).unwrap();
    let complete = fs::read(&path).unwrap();
    drop(owner);
    // request frames are separate atomic recovery boundaries even when fsync is grouped.
    let frame_len =
        |at: usize| u64::from_le_bytes(complete[at..at + 8].try_into().unwrap()) as usize + 12;
    let second_end = boundary + frame_len(boundary);
    for end in boundary..=complete.len() {
        fs::write(&path, &complete[..end]).unwrap();
        let loaded = DocumentStore::new(&t.0).load().unwrap();
        let expected = if end < second_end {
            1
        } else if end < complete.len() {
            2
        } else {
            3
        };
        assert_eq!(loaded.document.revision(), expected);
        let wanted = match expected {
            1 => Some(reference(1)),
            2 => Some(reference(2)),
            _ => None,
        };
        assert_eq!(loaded.document.scene().object(1).unwrap().asset, wanted);
        assert_eq!(loaded.document.session_sequence(session), Some(expected));
    }
    let mut corrupt = complete.clone();
    let at = second_end + 20;
    corrupt[at] ^= 1;
    fs::write(&path, &corrupt).unwrap();
    let mut owner = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
    assert_eq!(owner.document().revision(), 2);
    assert!(owner.request(&second).unwrap().replayed);
    owner.request(&third).unwrap();
    owner.save().unwrap();
    drop(owner);
    let mut owner = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
    assert!(owner.request(&first).unwrap().replayed);
    assert!(owner.request(&third).unwrap().replayed);
    assert_eq!(owner.document().revision(), 3);
}

#[test]
fn v6_journal_reanchors_before_new_asset_edits_and_restart() {
    fn crc(b: &[u8]) -> u32 {
        let mut c = !0u32;
        for &v in b {
            c ^= u32::from(v);
            for _ in 0..8 {
                c = (c >> 1) ^ (0xedb88320u32 & 0u32.wrapping_sub(c & 1));
            }
        }
        !c
    }
    let t = Temp::new();
    let mut owner = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
    let id = owner.document().document_id().unwrap();
    owner.commit(0, &create(1)).unwrap();
    let mut base = owner.document().checkpoint().unwrap();
    drop(owner);
    base[8..12].copy_from_slice(&6u32.to_le_bytes());
    let n = base.len() - 4;
    let c = crc(&base[..n]);
    base[n..].copy_from_slice(&c.to_le_bytes());
    let mut journal = b"THREEXJ6".to_vec();
    journal.extend((base.len() as u64).to_le_bytes());
    journal.extend(&base);
    journal.extend(crc(&base).to_le_bytes());
    let path = t.0.join("autosave.txj");
    fs::write(&path, journal).unwrap();
    let mut owner = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
    assert_eq!(&fs::read(&path).unwrap()[..8], b"THREEXJ7");
    let asset = AssetRef::new(new_asset_id().unwrap(), 1, [7; 32]).unwrap();
    let r = request(
        id,
        new_session_id().unwrap(),
        1,
        1,
        asset_command(1, Some(asset)).to_vec(),
    );
    owner.request(&r).unwrap();
    owner.save().unwrap();
    drop(owner);
    let mut owner = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
    assert_eq!(
        owner.document().scene().object(1).unwrap().asset,
        Some(asset)
    );
    assert!(owner.request(&r).unwrap().replayed);
    assert_eq!(owner.document().revision(), 2);
}
