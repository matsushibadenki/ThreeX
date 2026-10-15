//! Headless native host example; no WebView or full scene transfer.
use std::time::Duration;
use threex_storage::{
    DocumentStore, JournalWorker, WorkerAction, WorkerConfig, WorkerOutcome, WorkerRequest,
};
fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: worker_demo <new-document-directory>");
    let worker = JournalWorker::start(DocumentStore::new(path), WorkerConfig::default()).unwrap();
    assert_eq!(
        worker.initial_state.revision, 0,
        "use a new document directory"
    );
    let client = worker.client();
    let mut replies = Vec::new();
    for id in 1..=8u64 {
        let mut commands = vec![0; 64];
        commands[..4].copy_from_slice(&1u32.to_le_bytes());
        commands[4..8].copy_from_slice(&(id as u32).to_le_bytes());
        replies.push(
            client
                .try_submit(WorkerRequest {
                    id,
                    action: WorkerAction::Commit {
                        expected_revision: id - 1,
                        commands,
                    },
                })
                .unwrap(),
        );
    }
    for receiver in replies {
        let result = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(result.result.is_ok());
    }
    let receiver = client
        .try_submit(WorkerRequest {
            id: 9,
            action: WorkerAction::Inspect,
        })
        .unwrap();
    let WorkerOutcome::State(state) = receiver
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .result
        .unwrap()
    else {
        panic!()
    };
    println!(
        "revision={}, objects={}, journal_sync_groups={}",
        state.revision, state.objects, state.successful_sync_groups
    );
    let receiver = client
        .try_submit(WorkerRequest {
            id: 10,
            action: WorkerAction::Save,
        })
        .unwrap();
    assert!(
        receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .result
            .is_ok()
    );
    worker.shutdown().unwrap();
}
