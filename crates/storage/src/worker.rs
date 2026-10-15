//! Bounded native host service. Commands enter; small state/results leave.
use crate::{AutosaveDocument, DocumentStore, StorageError};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};
use threex_core::{DocumentError, Object};
const MAX_COMMAND: usize = 256 * 1024;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerConfig {
    pub queue_capacity: usize,
    pub batch_size: usize,
    pub batch_window: Duration,
    pub result_capacity: usize,
}
impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            queue_capacity: 32,
            batch_size: 32,
            batch_window: Duration::from_millis(2),
            result_capacity: 64,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkerAction {
    Render(threex_renderer::native::SurfaceRequest),
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
    Protocol(crate::ProtocolRequest),
    InspectSession {
        session_id: threex_core::SessionId,
    },
    Save,
    Inspect,
    InspectContentHash,
    ReadObject {
        id: u32,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerRequest {
    pub id: u64,
    pub action: WorkerAction,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerCode {
    QueueFull,
    Stopped,
    InvalidId,
    CommandTooLarge,
    RequestMismatch,
    Expired,
    RevisionConflict,
    InvalidCommand,
    HistoryCapacity,
    NoHistory,
    JournalLimit,
    StorageFailure,
    DocumentMismatch,
    SequenceGap,
    SessionLimit,
    SessionClosed,
    SessionUnknown,
    UnsupportedProtocol,
}
impl WorkerCode {
    /// Stable codes stay locale-independent; hosts may replace these default translations.
    pub fn message(self, locale: &str) -> &'static str {
        let (en, ja, zh) = match self {
            Self::SessionClosed => ("Session is closed", "sessionは終了済みです", "会话已关闭"),
            Self::SessionUnknown => (
                "Session has not started",
                "sessionは開始されていません",
                "会话尚未开始",
            ),
            Self::DocumentMismatch => ("Wrong document", "文書IDが一致しません", "文档 ID 不匹配"),
            Self::SequenceGap => (
                "Session sequence gap",
                "sessionの連番が欠落しています",
                "会话序号不连续",
            ),
            Self::SessionLimit => (
                "Document session limit reached",
                "文書のsession上限に達しました",
                "文档会话达到上限",
            ),
            Self::UnsupportedProtocol => (
                "Unsupported request protocol",
                "未対応のrequest protocolです",
                "不支持的请求协议",
            ),
            Self::QueueFull => ("Edit queue is full", "編集キューが満杯です", "编辑队列已满"),
            Self::Stopped => (
                "Worker has stopped",
                "workerは停止しています",
                "工作线程已停止",
            ),
            Self::InvalidId => (
                "Request ID must be nonzero",
                "request IDは0以外を指定してください",
                "请求 ID 不能为零",
            ),
            Self::CommandTooLarge => (
                "Command exceeds the limit",
                "命令が上限を超えています",
                "命令超过限制",
            ),
            Self::RequestMismatch => (
                "Request ID has different content",
                "同じrequest IDの内容が一致しません",
                "相同请求 ID 的内容不一致",
            ),
            Self::Expired => (
                "Request result expired; resynchronize",
                "結果が期限切れです。状態を再同期してください",
                "请求结果已过期，请重新同步状态",
            ),
            Self::RevisionConflict => (
                "Document revision changed",
                "文書のrevisionが変わりました",
                "文档版本已改变",
            ),
            Self::InvalidCommand => ("Invalid edit", "無効な編集です", "无效编辑"),
            Self::HistoryCapacity => (
                "Edit history limit exceeded",
                "編集履歴の上限を超えています",
                "编辑历史超过限制",
            ),
            Self::NoHistory => (
                "No undo or redo entry",
                "UndoまたはRedoの履歴がありません",
                "没有撤销或重做记录",
            ),
            Self::JournalLimit => (
                "Journal is full; save a checkpoint",
                "journalが満杯です。保存してください",
                "日志已满，请保存检查点",
            ),
            Self::StorageFailure => (
                "Storage operation failed; resynchronize",
                "保存処理に失敗しました。状態を再同期してください",
                "存储操作失败，请重新同步状态",
            ),
        };
        match locale {
            "ja" => ja,
            "zh-CN" => zh,
            _ => en,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerFailure {
    pub code: WorkerCode,
    pub current_revision: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerState {
    pub document_id: threex_core::DocumentId,
    pub revision: u64,
    pub objects: usize,
    pub saved_revision: Option<u64>,
    pub dirty: bool,
    pub discarded_journal_bytes: u64,
    pub successful_sync_groups: u64,
}
#[derive(Clone, Debug, PartialEq)]
pub enum WorkerOutcome {
    Rendered {
        result:
            Result<threex_renderer::native::FrameStatus, threex_renderer::native::SurfaceFailure>,
        state: WorkerState,
    },
    ContentHash {
        hash: threex_core::ContentHash,
        state: WorkerState,
    },
    Session {
        session_id: threex_core::SessionId,
        sequence: Option<u64>,
        status: Option<threex_core::SessionStatus>,
        state: WorkerState,
    },
    Protocol {
        revision: u64,
        replayed: bool,
        state: WorkerState,
    },
    Changed {
        revision: u64,
        state: WorkerState,
    },
    Saved {
        generation: u64,
        state: WorkerState,
    },
    State(WorkerState),
    Object {
        id: u32,
        object: Option<Object>,
        state: WorkerState,
    },
}
#[derive(Clone, Debug, PartialEq)]
pub struct WorkerReply {
    pub id: u64,
    pub replayed: bool,
    pub result: Result<WorkerOutcome, WorkerFailure>,
}
#[derive(Debug)]
pub struct RejectedRequest {
    pub request: WorkerRequest,
    pub code: WorkerCode,
}
#[derive(Debug)]
pub enum WorkerStartError {
    InvalidConfig,
    Thread(std::io::Error),
    Storage(StorageError),
    Stopped,
}
struct Message {
    request: WorkerRequest,
    reply: mpsc::Sender<WorkerReply>,
}
/// Window-independent client. Submission never waits for queue space or disk IO.
#[derive(Clone)]
pub struct WorkerClient {
    sender: Arc<Mutex<Option<mpsc::SyncSender<Message>>>>,
}
impl WorkerClient {
    pub fn try_submit(
        &self,
        request: WorkerRequest,
    ) -> Result<mpsc::Receiver<WorkerReply>, RejectedRequest> {
        let reject = |request, code| Err(RejectedRequest { request, code });
        if request.id == 0 {
            return reject(request, WorkerCode::InvalidId);
        }
        if matches!(&request.action,WorkerAction::Commit {commands,..} if commands.len()>MAX_COMMAND)
        {
            return reject(request, WorkerCode::CommandTooLarge);
        }
        if matches!(&request.action,WorkerAction::Protocol(r) if r.action.parts().2.len()>MAX_COMMAND)
        {
            return reject(request, WorkerCode::CommandTooLarge);
        }
        let Ok(sender) = self.sender.lock() else {
            return reject(request, WorkerCode::Stopped);
        };
        let Some(sender) = sender.as_ref() else {
            return reject(request, WorkerCode::Stopped);
        };
        let (reply, receiver) = mpsc::channel();
        match sender.try_send(Message { request, reply }) {
            Ok(()) => Ok(receiver),
            Err(mpsc::TrySendError::Full(m)) => reject(m.request, WorkerCode::QueueFull),
            Err(mpsc::TrySendError::Disconnected(m)) => reject(m.request, WorkerCode::Stopped),
        }
    }
}
/// The application owns this service; windows retain clients only. Drop closes ingress
/// and drains accepted work asynchronously. Explicit shutdown joins after that drain.
pub struct JournalWorker {
    client: WorkerClient,
    join: Option<thread::JoinHandle<()>>,
    pub initial_state: WorkerState,
}
impl JournalWorker {
    pub fn start(store: DocumentStore, config: WorkerConfig) -> Result<Self, WorkerStartError> {
        Self::start_with_surfaces(
            store,
            config,
            threex_renderer::native::NativeSurfaces::default(),
        )
    }
    pub fn start_with_surfaces(
        store: DocumentStore,
        config: WorkerConfig,
        surfaces: threex_renderer::native::NativeSurfaces,
    ) -> Result<Self, WorkerStartError> {
        if !(1..=64).contains(&config.queue_capacity)
            || !(1..=32).contains(&config.batch_size)
            || !(1..=256).contains(&config.result_capacity)
            || config.batch_window > Duration::from_millis(50)
        {
            return Err(WorkerStartError::InvalidConfig);
        }
        let (sender, receiver) = mpsc::sync_channel(config.queue_capacity);
        let (ready, startup) = mpsc::channel();
        let join = thread::Builder::new()
            .name("threex-document".into())
            .spawn(move || {
                let saved = match store.load_checkpoint() {
                    Ok(d) => Some(d.document.revision()),
                    Err(StorageError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => None,
                    Err(StorageError::NoValidGeneration) => None,
                    Err(e) => {
                        let _ = ready.send(Err(e));
                        return;
                    }
                };
                let owner = match AutosaveDocument::open(store) {
                    Ok(d) => d,
                    Err(e) => {
                        let _ = ready.send(Err(e));
                        return;
                    }
                };
                let render_stamp = Some(owner.document().scene().stamp());
                let mut service = Service {
                    render_stamp,
                    surfaces,
                    owner,
                    saved,
                    sync_groups: 0,
                    highest: 0,
                    cache: BTreeMap::new(),
                    order: VecDeque::new(),
                    config,
                };
                if ready.send(Ok(service.state())).is_err() {
                    return;
                }
                service.run(receiver);
            })
            .map_err(WorkerStartError::Thread)?;
        let initial_state = startup
            .recv()
            .map_err(|_| WorkerStartError::Stopped)?
            .map_err(WorkerStartError::Storage)?;
        Ok(Self {
            client: WorkerClient {
                sender: Arc::new(Mutex::new(Some(sender))),
            },
            join: Some(join),
            initial_state,
        })
    }
    pub fn client(&self) -> WorkerClient {
        self.client.clone()
    }
    fn close(&self) {
        if let Ok(mut sender) = self.client.sender.lock() {
            sender.take();
        }
    }
    pub fn shutdown(mut self) -> thread::Result<()> {
        self.close();
        self.join.take().unwrap().join()
    }
}
impl Drop for JournalWorker {
    fn drop(&mut self) {
        self.close();
    }
}
type ResultValue = Result<WorkerOutcome, WorkerFailure>;
struct Service {
    render_stamp: Option<threex_core::SceneStamp>,
    surfaces: threex_renderer::native::NativeSurfaces,
    owner: AutosaveDocument,
    saved: Option<u64>,
    sync_groups: u64,
    highest: u64,
    cache: BTreeMap<u64, (WorkerAction, ResultValue)>,
    order: VecDeque<u64>,
    config: WorkerConfig,
}
impl Service {
    fn state(&self) -> WorkerState {
        let d = self.owner.document();
        WorkerState {
            document_id: d.document_id().unwrap(),
            revision: d.revision(),
            objects: d.scene().len(),
            saved_revision: self.saved,
            dirty: self.saved != Some(d.revision()),
            discarded_journal_bytes: self.owner.discarded_bytes,
            successful_sync_groups: self.sync_groups,
        }
    }
    fn failure(&self, code: WorkerCode) -> WorkerFailure {
        WorkerFailure {
            code,
            current_revision: self.owner.document().revision(),
        }
    }
    fn storage_error(&self, error: StorageError) -> WorkerFailure {
        let code = match error {
            StorageError::Rejected(failure) => {
                let mut result = self.storage_error(StorageError::Edit(failure.error));
                result.current_revision = failure.revision;
                return result;
            }
            StorageError::Request(threex_core::RequestError::SessionClosed) => {
                WorkerCode::SessionClosed
            }
            StorageError::Request(threex_core::RequestError::SessionUnknown) => {
                WorkerCode::SessionUnknown
            }
            StorageError::Request(threex_core::RequestError::DocumentMismatch)
            | StorageError::IdentityMismatch => WorkerCode::DocumentMismatch,
            StorageError::Request(threex_core::RequestError::Expired) => WorkerCode::Expired,
            StorageError::Request(threex_core::RequestError::Mismatch) => {
                WorkerCode::RequestMismatch
            }
            StorageError::Request(threex_core::RequestError::InvalidSequence) => {
                WorkerCode::InvalidId
            }
            StorageError::Request(threex_core::RequestError::SequenceGap) => {
                WorkerCode::SequenceGap
            }
            StorageError::Request(threex_core::RequestError::SessionLimit) => {
                WorkerCode::SessionLimit
            }
            StorageError::UnsupportedRequestProtocol => WorkerCode::UnsupportedProtocol,
            StorageError::Request(threex_core::RequestError::Edit(e)) => {
                return self.storage_error(StorageError::Edit(e));
            }
            StorageError::Edit(DocumentError::RevisionConflict { .. }) => {
                WorkerCode::RevisionConflict
            }
            StorageError::Edit(DocumentError::InvalidCommand(_)) => WorkerCode::InvalidCommand,
            StorageError::Edit(DocumentError::CommandTooLarge) => WorkerCode::CommandTooLarge,
            StorageError::Edit(DocumentError::HistoryCapacity) => WorkerCode::HistoryCapacity,
            StorageError::Edit(DocumentError::NothingToUndo | DocumentError::NothingToRedo) => {
                WorkerCode::NoHistory
            }
            StorageError::JournalLimit => WorkerCode::JournalLimit,
            _ => WorkerCode::StorageFailure,
        };
        self.failure(code)
    }
    fn finish(&mut self, message: Message, result: ResultValue) {
        if !self
            .owner
            .document()
            .scene()
            .is_current(self.render_stamp.as_ref())
        {
            self.render_stamp = Some(self.owner.document().scene().stamp());
            self.surfaces.scene_changed();
        }
        self.highest = self.highest.max(message.request.id);
        self.order.push_back(message.request.id);
        self.cache
            .insert(message.request.id, (message.request.action, result.clone()));
        while self.order.len() > self.config.result_capacity {
            self.cache.remove(&self.order.pop_front().unwrap());
        }
        let _ = message.reply.send(WorkerReply {
            id: message.request.id,
            replayed: false,
            result,
        });
    }
    fn replay(&self, message: &Message) -> bool {
        let result = if let Some((action, result)) = self.cache.get(&message.request.id) {
            if action == &message.request.action {
                let _ = message.reply.send(WorkerReply {
                    id: message.request.id,
                    replayed: true,
                    result: result.clone(),
                });
                return true;
            }
            Some(Err(self.failure(WorkerCode::RequestMismatch)))
        } else if message.request.id <= self.highest {
            Some(Err(self.failure(WorkerCode::Expired)))
        } else {
            None
        };
        if let Some(result) = result {
            let _ = message.reply.send(WorkerReply {
                id: message.request.id,
                replayed: false,
                result,
            });
            return true;
        }
        false
    }
    fn run(&mut self, receiver: mpsc::Receiver<Message>) {
        while let Ok(first) = receiver.recv() {
            let mut chunk = VecDeque::from([first]);
            let deadline = Instant::now() + self.config.batch_window;
            while chunk.len() < self.config.batch_size {
                match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(m) => chunk.push_back(m),
                    Err(_) => break,
                }
            }
            while let Some(message) = chunk.pop_front() {
                if self.replay(&message) {
                    continue;
                }
                if matches!(message.request.action, WorkerAction::Commit { .. }) {
                    self.group(message, &mut chunk);
                    continue;
                }
                if matches!(&message.request.action, WorkerAction::Protocol(r) if matches!(r.action, crate::ProtocolAction::Commit { .. }))
                {
                    self.protocol_group(message, &mut chunk);
                    continue;
                }
                let result = match &message.request.action {
                    WorkerAction::Render(request) => Ok(WorkerOutcome::Rendered {
                        result: self
                            .surfaces
                            .execute(self.owner.document().scene(), request),
                        state: self.state(),
                    }),
                    WorkerAction::InspectSession { session_id } => Ok(WorkerOutcome::Session {
                        session_id: *session_id,
                        sequence: self.owner.document().session_sequence(*session_id),
                        status: self.owner.document().session_status(*session_id),
                        state: self.state(),
                    }),
                    WorkerAction::Protocol(request) => {
                        let generation = self.owner.document().request_generation();
                        let result = self.owner.request(request);
                        if self.owner.document().request_generation() != generation {
                            self.sync_groups += 1;
                        }
                        result.map(|receipt| WorkerOutcome::Protocol {
                            revision: receipt.revision,
                            replayed: receipt.replayed,
                            state: self.state(),
                        })
                    }
                    WorkerAction::Undo { expected_revision } => {
                        self.owner.undo(*expected_revision).map(|revision| {
                            self.sync_groups += 1;
                            WorkerOutcome::Changed {
                                revision,
                                state: self.state(),
                            }
                        })
                    }
                    WorkerAction::Redo { expected_revision } => {
                        self.owner.redo(*expected_revision).map(|revision| {
                            self.sync_groups += 1;
                            WorkerOutcome::Changed {
                                revision,
                                state: self.state(),
                            }
                        })
                    }
                    WorkerAction::Save => self.owner.save().map(|receipt| {
                        self.saved = Some(receipt.revision);
                        WorkerOutcome::Saved {
                            generation: receipt.generation,
                            state: self.state(),
                        }
                    }),
                    WorkerAction::InspectContentHash => Ok(WorkerOutcome::ContentHash {
                        hash: self.owner.document().content_hash(),
                        state: self.state(),
                    }),
                    WorkerAction::Inspect => Ok(WorkerOutcome::State(self.state())),
                    WorkerAction::ReadObject { id } => Ok(WorkerOutcome::Object {
                        id: *id,
                        object: self.owner.document().scene().object(*id),
                        state: self.state(),
                    }),
                    WorkerAction::Commit { .. } => unreachable!(),
                }
                .map_err(|e| self.storage_error(e));
                self.finish(message, result);
            }
        }
    }
    fn protocol_group(&mut self, first: Message, chunk: &mut VecDeque<Message>) {
        let mut group = vec![first];
        let mut builder = match self.owner.document().request_group_builder() {
            Ok(builder) => builder,
            Err(error) => {
                let failure = self.storage_error(StorageError::Request(error));
                self.finish(group.pop().unwrap(), Err(failure));
                return;
            }
        };
        let WorkerAction::Protocol(request) = &group[0].request.action else {
            unreachable!()
        };
        if crate::protocol::push_protocol_candidate(&mut builder, request).is_err() {
            drop(builder);
            let generation = self.owner.document().request_generation();
            let result = self.owner.request(request);
            if self.owner.document().request_generation() != generation {
                self.sync_groups += 1;
            }
            let result = result
                .map(|receipt| WorkerOutcome::Protocol {
                    revision: receipt.revision,
                    replayed: receipt.replayed,
                    state: self.state(),
                })
                .map_err(|error| self.storage_error(error));
            self.finish(group.pop().unwrap(), result);
            return;
        }
        while let Some(next) = chunk.front() {
            let WorkerAction::Protocol(request) = &next.request.action else {
                break;
            };
            if !matches!(request.action, crate::ProtocolAction::Commit { .. })
                || group.len() >= 32
                || next.request.id <= group.last().unwrap().request.id
                || self.cache.contains_key(&next.request.id)
            {
                break;
            }
            if crate::protocol::push_protocol_candidate(&mut builder, request).is_err() {
                break;
            }
            group.push(chunk.pop_front().unwrap());
        }
        // Consume exactly the validated prefix; no final full-prefix preparation.
        let prepared = builder.finish();
        match self.owner.append_prepared_requests(prepared) {
            Ok(receipts) => {
                if receipts.iter().any(|r| !r.replayed) {
                    self.sync_groups += 1;
                }
                let state = self.state();
                for (message, receipt) in group.into_iter().zip(receipts) {
                    self.finish(
                        message,
                        Ok(WorkerOutcome::Protocol {
                            revision: receipt.revision,
                            replayed: receipt.replayed,
                            state: state.clone(),
                        }),
                    );
                }
            }
            Err(error) => {
                let failure = self.storage_error(error);
                for message in group {
                    self.finish(message, Err(failure.clone()));
                }
            }
        }
    }
    fn group(&mut self, first: Message, chunk: &mut VecDeque<Message>) {
        let base = self.owner.document().revision();
        let mut group = vec![first];
        let WorkerAction::Commit {
            expected_revision,
            commands,
        } = &group[0].request.action
        else {
            unreachable!()
        };
        let mut builder = match self
            .owner
            .document()
            .commit_group_builder(*expected_revision)
        {
            Ok(builder) => builder,
            Err(error) => {
                let failure = self.storage_error(StorageError::Edit(error));
                self.finish(group.pop().unwrap(), Err(failure));
                return;
            }
        };
        if let Err(error) = builder.push(*expected_revision, commands) {
            drop(builder);
            let failure = self.storage_error(StorageError::Edit(error));
            self.finish(group.pop().unwrap(), Err(failure));
            return;
        }
        while let Some(next) = chunk.front() {
            let WorkerAction::Commit {
                expected_revision,
                commands,
            } = &next.request.action
            else {
                break;
            };
            if next.request.id <= group.last().unwrap().request.id
                || self.cache.contains_key(&next.request.id)
            {
                break;
            }
            if builder.push(*expected_revision, commands).is_err() {
                break;
            }
            group.push(chunk.pop_front().unwrap());
        }
        let prepared = builder.finish();
        match self.owner.append_prepared_commits(prepared) {
            Ok(applied) => {
                if *applied.last().unwrap() != base {
                    self.sync_groups += 1
                }
                let state = self.state();
                for (m, revision) in group.into_iter().zip(applied) {
                    self.finish(
                        m,
                        Ok(WorkerOutcome::Changed {
                            revision,
                            state: state.clone(),
                        }),
                    );
                }
            }
            Err(e) => {
                let error = self.storage_error(e);
                for m in group {
                    self.finish(m, Err(error.clone()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "threex-worker-{}-{}",
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
    fn command(id: u32) -> Vec<u8> {
        let mut b = vec![0; 64];
        b[..4].copy_from_slice(&1u32.to_le_bytes());
        b[4..8].copy_from_slice(&id.to_le_bytes());
        b
    }
    fn request(id: u64, action: WorkerAction) -> WorkerRequest {
        WorkerRequest { id, action }
    }
    fn commit(id: u64, expected: u64, entity: u32) -> WorkerRequest {
        request(
            id,
            WorkerAction::Commit {
                expected_revision: expected,
                commands: command(entity),
            },
        )
    }
    fn wait(rx: mpsc::Receiver<WorkerReply>) -> WorkerReply {
        rx.recv_timeout(Duration::from_secs(5)).unwrap()
    }
    fn send(c: &WorkerClient, r: WorkerRequest) -> WorkerReply {
        wait(c.try_submit(r).unwrap())
    }
    fn config() -> WorkerConfig {
        WorkerConfig {
            batch_window: Duration::from_millis(20),
            ..Default::default()
        }
    }
    #[test]
    fn window_clients_group_revisions_replay_and_save_barriers() {
        let t = Temp::new();
        let worker = JournalWorker::start(DocumentStore::new(&t.0), config()).unwrap();
        let a = worker.client();
        let b = worker.client();
        assert_eq!(worker.initial_state.revision, 0);
        let mut replies = Vec::new();
        for i in 0..8 {
            replies.push(a.try_submit(commit(i + 1, i, (i + 1) as u32)).unwrap());
        }
        for (i, rx) in replies.into_iter().enumerate() {
            let reply = wait(rx);
            let WorkerOutcome::Changed { revision, state } = reply.result.unwrap() else {
                panic!()
            };
            assert_eq!(revision, i as u64 + 1);
            assert_eq!(state.revision, 8);
            assert_eq!(state.successful_sync_groups, 1);
            assert!(state.dirty);
        }
        let repeat = send(&b, commit(1, 0, 1));
        assert!(repeat.replayed);
        assert!(matches!(
            repeat.result,
            Ok(WorkerOutcome::Changed { revision: 1, .. })
        ));
        let wrong = send(&b, commit(1, 0, 99));
        assert_eq!(wrong.result.unwrap_err().code, WorkerCode::RequestMismatch);
        assert_eq!(
            send(&a, commit(9, 0, 9)).result.unwrap_err().code,
            WorkerCode::RevisionConflict
        );
        let WorkerOutcome::Saved { state, .. } =
            send(&a, request(10, WorkerAction::Save)).result.unwrap()
        else {
            panic!()
        };
        assert!(!state.dirty);
        assert_eq!(state.saved_revision, Some(8));
        send(&a, commit(11, 8, 9)).result.unwrap();
        let WorkerOutcome::Changed { state, .. } = send(
            &b,
            request(
                12,
                WorkerAction::Undo {
                    expected_revision: 9,
                },
            ),
        )
        .result
        .unwrap() else {
            panic!()
        };
        assert_eq!(state.objects, 8);
        assert!(state.dirty);
        send(
            &b,
            request(
                13,
                WorkerAction::Redo {
                    expected_revision: 10,
                },
            ),
        )
        .result
        .unwrap();
        let WorkerOutcome::Object { object, .. } =
            send(&a, request(14, WorkerAction::ReadObject { id: 9 }))
                .result
                .unwrap()
        else {
            panic!()
        };
        assert!(object.is_some());
        worker.shutdown().unwrap();
        assert_eq!(
            a.try_submit(request(15, WorkerAction::Inspect))
                .unwrap_err()
                .code,
            WorkerCode::Stopped
        );
        let loaded = DocumentStore::new(&t.0).load().unwrap();
        assert_eq!(loaded.document.revision(), 11);
        assert_eq!(loaded.document.scene().len(), 9);
    }
    #[test]
    fn queue_backpressure_returns_request_and_limits_before_enqueuing() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let c = WorkerClient {
            sender: Arc::new(Mutex::new(Some(sender))),
        };
        let first = c.try_submit(commit(1, 0, 1)).unwrap();
        let rejected = c.try_submit(commit(2, 1, 2)).unwrap_err();
        assert_eq!(rejected.code, WorkerCode::QueueFull);
        assert_eq!(rejected.request, commit(2, 1, 2));
        assert_eq!(
            c.try_submit(request(0, WorkerAction::Inspect))
                .unwrap_err()
                .code,
            WorkerCode::InvalidId
        );
        assert_eq!(
            c.try_submit(request(
                3,
                WorkerAction::Commit {
                    expected_revision: 0,
                    commands: vec![0; MAX_COMMAND + 1]
                }
            ))
            .unwrap_err()
            .code,
            WorkerCode::CommandTooLarge
        );
        drop(receiver);
        assert_eq!(
            c.try_submit(rejected.request).unwrap_err().code,
            WorkerCode::Stopped
        );
        assert!(first.recv().is_err());
    }
    #[test]
    fn pending_duplicates_and_noops_do_not_add_transactions() {
        let t = Temp::new();
        let worker = JournalWorker::start(DocumentStore::new(&t.0), config()).unwrap();
        let c = worker.client();
        let a = c.try_submit(commit(1, 0, 1)).unwrap();
        let duplicate = c.try_submit(commit(1, 0, 1)).unwrap();
        let b = c
            .try_submit(request(
                2,
                WorkerAction::Commit {
                    expected_revision: 1,
                    commands: vec![],
                },
            ))
            .unwrap();
        let d = c.try_submit(commit(3, 1, 2)).unwrap();
        assert!(wait(a).result.is_ok());
        assert!(wait(duplicate).replayed);
        assert!(matches!(
            wait(b).result,
            Ok(WorkerOutcome::Changed { revision: 1, .. })
        ));
        assert!(wait(d).result.is_ok());
        worker.shutdown().unwrap();
        let loaded = DocumentStore::new(&t.0).load().unwrap();
        assert_eq!(loaded.document.revision(), 2);
        assert_eq!(loaded.journal_transactions, 2);
    }
    #[test]
    fn expired_request_is_rejected_and_shutdown_drains_accepted_work() {
        let t = Temp::new();
        let cfg = WorkerConfig {
            result_capacity: 2,
            ..config()
        };
        let worker = JournalWorker::start(DocumentStore::new(&t.0), cfg).unwrap();
        let c = worker.client();
        for id in 1..=3 {
            send(&c, request(id, WorkerAction::Inspect)).result.unwrap();
        }
        assert_eq!(
            send(&c, commit(1, 0, 1)).result.unwrap_err().code,
            WorkerCode::Expired
        );
        let one = c.try_submit(commit(4, 0, 1)).unwrap();
        let two = c.try_submit(commit(5, 1, 2)).unwrap();
        drop(one);
        worker.shutdown().unwrap();
        assert!(wait(two).result.is_ok());
        assert_eq!(
            DocumentStore::new(&t.0).load().unwrap().document.revision(),
            2
        );
    }
    #[test]
    fn invalid_request_does_not_poison_following_valid_edit() {
        let t = Temp::new();
        let worker = JournalWorker::start(DocumentStore::new(&t.0), config()).unwrap();
        let c = worker.client();
        let a = c.try_submit(commit(1, 0, 1)).unwrap();
        let invalid = c.try_submit(commit(2, 1, 1)).unwrap();
        let b = c.try_submit(commit(3, 1, 2)).unwrap();
        assert!(wait(a).result.is_ok());
        assert_eq!(
            wait(invalid).result.unwrap_err().code,
            WorkerCode::InvalidCommand
        );
        assert!(wait(b).result.is_ok());
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
    fn startup_lock_io_failures_and_recovery_are_reported_without_reexecution() {
        let t = Temp::new();
        let existing = AutosaveDocument::open(DocumentStore::new(&t.0)).unwrap();
        assert!(matches!(
            JournalWorker::start(DocumentStore::new(&t.0), config()),
            Err(WorkerStartError::Storage(StorageError::Busy))
        ));
        drop(existing);
        let worker = JournalWorker::start(DocumentStore::new(&t.0), config()).unwrap();
        let c = worker.client();
        let path = t.0.join("autosave.txj");
        let anchor = fs::read(&path).unwrap();
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        let failed = send(&c, commit(1, 0, 1));
        assert_eq!(failed.result.unwrap_err().code, WorkerCode::StorageFailure);
        fs::remove_dir(&path).unwrap();
        fs::write(&path, &anchor).unwrap();
        assert!(send(&c, commit(1, 0, 1)).replayed);
        send(&c, commit(2, 0, 1)).result.unwrap();
        worker.shutdown().unwrap();
        let mut log = fs::read(&path).unwrap();
        log.extend(b"broken tail");
        fs::write(&path, log).unwrap();
        let restored = JournalWorker::start(DocumentStore::new(&t.0), config()).unwrap();
        assert_eq!(restored.initial_state.revision, 1);
        assert!(restored.initial_state.discarded_journal_bytes > 0);
        restored.shutdown().unwrap();
    }
    #[test]
    fn configuration_and_default_localization_are_complete() {
        let t = Temp::new();
        assert!(matches!(
            JournalWorker::start(
                DocumentStore::new(&t.0),
                WorkerConfig {
                    queue_capacity: 0,
                    ..config()
                }
            ),
            Err(WorkerStartError::InvalidConfig)
        ));
        for code in [
            WorkerCode::QueueFull,
            WorkerCode::Stopped,
            WorkerCode::InvalidId,
            WorkerCode::CommandTooLarge,
            WorkerCode::RequestMismatch,
            WorkerCode::Expired,
            WorkerCode::RevisionConflict,
            WorkerCode::InvalidCommand,
            WorkerCode::HistoryCapacity,
            WorkerCode::NoHistory,
            WorkerCode::JournalLimit,
            WorkerCode::StorageFailure,
            WorkerCode::DocumentMismatch,
            WorkerCode::SequenceGap,
            WorkerCode::SessionLimit,
            WorkerCode::SessionClosed,
            WorkerCode::SessionUnknown,
            WorkerCode::UnsupportedProtocol,
        ] {
            for locale in ["en", "ja", "zh-CN"] {
                assert!(!code.message(locale).is_empty())
            }
            assert_eq!(code.message("unknown"), code.message("en"));
        }
    }
    #[test]
    fn protocol_worker_batches_commits_and_replays_without_sync_after_restart() {
        let t = Temp::new();
        let worker = JournalWorker::start(DocumentStore::new(&t.0), config()).unwrap();
        let session = crate::new_session_id().unwrap();
        let requests = (0..8)
            .map(|i| crate::ProtocolRequest {
                protocol_version: 1,
                document_id: worker.initial_state.document_id,
                session_id: session,
                sequence: i + 1,
                action: crate::ProtocolAction::Commit {
                    expected_revision: i,
                    commands: command(i as u32 + 1),
                },
            })
            .collect::<Vec<_>>();
        let client = worker.client();
        let replies = requests
            .iter()
            .enumerate()
            .map(|(i, r)| {
                client
                    .try_submit(request(i as u64 + 1, WorkerAction::Protocol(r.clone())))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        for (i, reply) in replies.into_iter().enumerate() {
            let WorkerOutcome::Protocol {
                revision,
                replayed,
                state,
            } = wait(reply).result.unwrap()
            else {
                panic!()
            };
            assert_eq!(revision, i as u64 + 1);
            assert!(!replayed);
            assert_eq!(state.successful_sync_groups, 1);
            assert_eq!(state.revision, 8);
        }
        let replay = send(
            &client,
            request(9, WorkerAction::Protocol(requests[0].clone())),
        );
        let WorkerOutcome::Protocol {
            replayed, state, ..
        } = replay.result.unwrap()
        else {
            panic!()
        };
        assert!(replayed);
        assert_eq!(state.successful_sync_groups, 1);
        let undo = crate::ProtocolRequest {
            sequence: 9,
            action: crate::ProtocolAction::Undo {
                expected_revision: 8,
            },
            ..requests[0].clone()
        };
        let outcome = send(&client, request(10, WorkerAction::Protocol(undo.clone())));
        let WorkerOutcome::Protocol {
            revision, state, ..
        } = outcome.result.unwrap()
        else {
            panic!()
        };
        assert_eq!(revision, 9);
        assert_eq!(state.objects, 7);
        assert_eq!(state.successful_sync_groups, 2);
        send(&client, request(11, WorkerAction::Save))
            .result
            .unwrap();
        worker.shutdown().unwrap();
        let worker = JournalWorker::start(DocumentStore::new(&t.0), config()).unwrap();
        let client = worker.client();
        let replies = requests
            .iter()
            .chain(std::iter::once(&undo))
            .enumerate()
            .map(|(i, r)| {
                client
                    .try_submit(request(i as u64 + 1, WorkerAction::Protocol(r.clone())))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        for reply in replies {
            let WorkerOutcome::Protocol {
                replayed, state, ..
            } = wait(reply).result.unwrap()
            else {
                panic!()
            };
            assert!(replayed);
            assert_eq!(state.successful_sync_groups, 0);
            assert_eq!(state.objects, 7);
            assert_eq!(state.revision, 9);
        }
        worker.shutdown().unwrap();
    }
    #[test]
    fn protocol_invalid_neighbor_preserves_valid_prefix_and_consumes_durable_error_sequence() {
        let t = Temp::new();
        let worker = JournalWorker::start(DocumentStore::new(&t.0), config()).unwrap();
        let session = crate::new_session_id().unwrap();
        let first = crate::ProtocolRequest {
            protocol_version: 1,
            document_id: worker.initial_state.document_id,
            session_id: session,
            sequence: 1,
            action: crate::ProtocolAction::Commit {
                expected_revision: 0,
                commands: command(1),
            },
        };
        let wrong = crate::ProtocolRequest {
            sequence: 2,
            action: crate::ProtocolAction::Commit {
                expected_revision: 0,
                commands: command(2),
            },
            ..first.clone()
        };
        let good = crate::ProtocolRequest {
            sequence: 3,
            action: crate::ProtocolAction::Commit {
                expected_revision: 1,
                commands: command(2),
            },
            ..wrong.clone()
        };
        let client = worker.client();
        let replies = [first, wrong, good]
            .into_iter()
            .enumerate()
            .map(|(i, r)| {
                client
                    .try_submit(request(i as u64 + 1, WorkerAction::Protocol(r)))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let mut replies = replies.into_iter();
        assert!(matches!(
            wait(replies.next().unwrap()).result,
            Ok(WorkerOutcome::Protocol { revision: 1, .. })
        ));
        assert_eq!(
            wait(replies.next().unwrap()).result.unwrap_err().code,
            WorkerCode::RevisionConflict
        );
        let WorkerOutcome::Protocol {
            revision, state, ..
        } = wait(replies.next().unwrap()).result.unwrap()
        else {
            panic!()
        };
        assert_eq!(revision, 2);
        assert_eq!(state.successful_sync_groups, 3);
        worker.shutdown().unwrap();
        let d = DocumentStore::new(&t.0).load().unwrap().document;
        assert_eq!(d.session_sequence(session), Some(3));
        assert_eq!(d.scene().len(), 2);
    }
    #[test]
    fn incremental_worker_prefix_handles_in_batch_retries_then_restart_replay() {
        let t = Temp::new();
        let worker = JournalWorker::start(
            DocumentStore::new(&t.0),
            WorkerConfig {
                batch_window: Duration::from_millis(50),
                ..Default::default()
            },
        )
        .unwrap();
        let session = crate::new_session_id().unwrap();
        let requests = (0..16)
            .map(|i| crate::ProtocolRequest {
                protocol_version: 1,
                document_id: worker.initial_state.document_id,
                session_id: session,
                sequence: i + 1,
                action: crate::ProtocolAction::Commit {
                    expected_revision: i,
                    commands: command(i as u32 + 1),
                },
            })
            .collect::<Vec<_>>();
        let client = worker.client();
        let replies = requests
            .iter()
            .flat_map(|r| [r, r])
            .enumerate()
            .map(|(i, r)| {
                client
                    .try_submit(request(i as u64 + 1, WorkerAction::Protocol(r.clone())))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        for (i, reply) in replies.into_iter().enumerate() {
            let WorkerOutcome::Protocol {
                revision,
                replayed,
                state,
            } = wait(reply).result.unwrap()
            else {
                panic!()
            };
            assert_eq!(revision, i as u64 / 2 + 1);
            assert_eq!(replayed, i % 2 == 1);
            assert_eq!(state.successful_sync_groups, 1);
            assert_eq!(state.objects, 16);
        }
        worker.shutdown().unwrap();
        let worker = JournalWorker::start(DocumentStore::new(&t.0), config()).unwrap();
        let client = worker.client();
        let replies = requests
            .iter()
            .enumerate()
            .map(|(i, r)| {
                client
                    .try_submit(request(i as u64 + 1, WorkerAction::Protocol(r.clone())))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        for reply in replies {
            let WorkerOutcome::Protocol {
                replayed, state, ..
            } = wait(reply).result.unwrap()
            else {
                panic!()
            };
            assert!(replayed);
            assert_eq!(state.successful_sync_groups, 0);
            assert_eq!(state.objects, 16);
        }
        worker.shutdown().unwrap();
    }
    #[test]
    fn bare_prefix_keeps_noops_cache_and_invalid_candidate_boundaries() {
        let t = Temp::new();
        let worker = JournalWorker::start(DocumentStore::new(&t.0), config()).unwrap();
        let client = worker.client();
        let first = commit(1, 0, 1);
        let noop = request(
            2,
            WorkerAction::Commit {
                expected_revision: 1,
                commands: vec![],
            },
        );
        let invalid = commit(3, 0, 2);
        let corrected = commit(4, 1, 2);
        let replies = [
            first.clone(),
            noop.clone(),
            first.clone(),
            invalid,
            corrected,
        ]
        .into_iter()
        .map(|r| client.try_submit(r).unwrap())
        .collect::<Vec<_>>();
        let mut replies = replies.into_iter();
        let WorkerOutcome::Changed { revision, state } =
            wait(replies.next().unwrap()).result.unwrap()
        else {
            panic!()
        };
        assert_eq!(revision, 1);
        assert_eq!(state.successful_sync_groups, 1);
        let WorkerOutcome::Changed { revision, .. } = wait(replies.next().unwrap()).result.unwrap()
        else {
            panic!()
        };
        assert_eq!(revision, 1);
        assert!(wait(replies.next().unwrap()).replayed);
        assert_eq!(
            wait(replies.next().unwrap()).result.unwrap_err().code,
            WorkerCode::RevisionConflict
        );
        let WorkerOutcome::Changed { revision, state } =
            wait(replies.next().unwrap()).result.unwrap()
        else {
            panic!()
        };
        assert_eq!(revision, 2);
        assert_eq!(state.successful_sync_groups, 2);
        assert!(send(&client, noop).replayed);
        assert_eq!(
            send(&client, commit(1, 0, 7)).result.unwrap_err().code,
            WorkerCode::RequestMismatch
        );
        worker.shutdown().unwrap();
        let d = DocumentStore::new(&t.0).load().unwrap().document;
        assert_eq!(d.revision(), 2);
        assert_eq!(d.scene().len(), 2);
        let worker = JournalWorker::start(DocumentStore::new(&t.0), config()).unwrap();
        // Bare transport cache is live-session only: stale expected revision prevents reapplying this old edit.
        assert_eq!(
            send(&worker.client(), first).result.unwrap_err().code,
            WorkerCode::RevisionConflict
        );
        worker.shutdown().unwrap();
    }
    #[test]
    fn explicit_content_hash_query_observes_queue_order_and_is_read_only() {
        let t = Temp::new();
        let worker = JournalWorker::start(DocumentStore::new(&t.0), config()).unwrap();
        let client = worker.client();
        let hash = |reply: WorkerReply| {
            let WorkerOutcome::ContentHash { hash, state } = reply.result.unwrap() else {
                panic!()
            };
            (hash, state)
        };
        let (empty, before) = hash(send(&client, request(1, WorkerAction::InspectContentHash)));
        let a = client.try_submit(commit(2, 0, 1)).unwrap();
        let b = client
            .try_submit(request(3, WorkerAction::InspectContentHash))
            .unwrap();
        let c = client
            .try_submit(request(
                4,
                WorkerAction::Undo {
                    expected_revision: 1,
                },
            ))
            .unwrap();
        let d = client
            .try_submit(request(5, WorkerAction::InspectContentHash))
            .unwrap();
        wait(a).result.unwrap();
        let (edited, state) = hash(wait(b));
        assert_ne!(edited, empty);
        assert_eq!(state.revision, 1);
        wait(c).result.unwrap();
        let (undone, state) = hash(wait(d));
        assert_eq!(undone, empty);
        assert_eq!(state.revision, 2);
        assert_eq!(before.successful_sync_groups, 0);
        assert_eq!(state.successful_sync_groups, 2);
        worker.shutdown().unwrap();
        let worker = JournalWorker::start(DocumentStore::new(&t.0), config()).unwrap();
        let (restored, state) = hash(send(
            &worker.client(),
            request(1, WorkerAction::InspectContentHash),
        ));
        assert_eq!(restored, empty);
        assert_eq!(state.revision, 2);
        worker.shutdown().unwrap();
    }
}
