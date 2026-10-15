//! Restart and resend the same protocol envelope without applying its edit twice.
use std::time::Duration;
use threex_storage::{
    DocumentStore, JournalWorker, ProtocolAction, ProtocolRequest, WorkerAction, WorkerConfig,
    WorkerOutcome, WorkerRequest, new_session_id,
};
fn send(worker: &JournalWorker, request: ProtocolRequest) -> WorkerOutcome {
    worker
        .client()
        .try_submit(WorkerRequest {
            id: 1,
            action: WorkerAction::Protocol(request),
        })
        .unwrap()
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .result
        .unwrap()
}
fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: request_demo <new-document-directory>");
    let worker = JournalWorker::start(DocumentStore::new(&path), WorkerConfig::default()).unwrap();
    assert_eq!(worker.initial_state.revision, 0, "use a new directory");
    let mut commands = vec![0; 64];
    commands[..4].copy_from_slice(&1u32.to_le_bytes());
    commands[4..8].copy_from_slice(&42u32.to_le_bytes());
    let request = ProtocolRequest {
        protocol_version: 1,
        document_id: worker.initial_state.document_id,
        session_id: new_session_id().unwrap(),
        sequence: 1,
        action: ProtocolAction::Commit {
            expected_revision: 0,
            commands,
        },
    };
    let WorkerOutcome::Protocol {
        revision, replayed, ..
    } = send(&worker, request.clone())
    else {
        panic!()
    };
    println!(
        "document={}, session={}, revision={revision}, replayed={replayed}",
        request.document_id, request.session_id
    );
    worker.shutdown().unwrap();
    let restarted =
        JournalWorker::start(DocumentStore::new(path), WorkerConfig::default()).unwrap();
    let WorkerOutcome::Protocol {
        revision,
        replayed,
        state,
    } = send(&restarted, request)
    else {
        panic!()
    };
    assert!(replayed);
    assert_eq!(state.objects, 1);
    assert_eq!(revision, 1);
    println!(
        "after restart: revision={revision}, objects={}, replayed={replayed}",
        state.objects
    );
    restarted.shutdown().unwrap();
}
