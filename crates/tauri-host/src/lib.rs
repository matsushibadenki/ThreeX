//! Async Tauri adapter. Only the application host owns the document worker.
mod dto;
pub mod redraw;
pub mod surface;
pub use dto::{EditAction, ErrorDto, IpcReply, IpcRequest, Locale, RetryAdvice, StateDto};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tauri::Runtime;
use threex_core::{DocumentId, SessionId, SessionStatus};
use threex_storage::{
    DocumentStore, JournalWorker, ProtocolRequest, WorkerAction, WorkerClient, WorkerConfig,
    WorkerOutcome, WorkerRequest, WorkerStartError,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    ReadOnly,
    Editor,
}
#[derive(Clone, Debug)]
pub struct WindowGrant {
    pub label: String,
    pub access: Access,
}
#[derive(Clone, Debug)]
pub struct HostConfig {
    pub worker: WorkerConfig,
    pub windows: Vec<WindowGrant>,
    pub dev_origin: Option<String>,
}
impl Default for HostConfig {
    fn default() -> Self {
        Self {
            worker: WorkerConfig::default(),
            windows: vec![WindowGrant {
                label: "main".into(),
                access: Access::Editor,
            }],
            dev_origin: None,
        }
    }
}
#[derive(Debug)]
pub enum HostOpenError {
    Worker(WorkerStartError),
    Io(std::io::Error),
    InvalidPolicy,
    InvalidBindings,
    Async,
    Scheduler(std::io::Error),
}
impl From<std::io::Error> for HostOpenError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
#[derive(Clone)]
struct Grant {
    access: Access,
    session: Option<SessionId>,
    surface: Option<threex_renderer::native::SurfaceHandle>,
}
struct Ingress {
    next: u64,
    grants: BTreeMap<String, Grant>,
    stopped: bool,
}
pub struct IpcHost {
    owner: Mutex<Option<JournalWorker>>,
    client: WorkerClient,
    document: DocumentId,
    ingress: Arc<Mutex<Ingress>>,
    pending: Arc<tokio::sync::Semaphore>,
    dev: Option<tauri::Url>,
    surfaces: threex_renderer::native::NativeSurfaces,
    scheduler: redraw::Scheduler,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Bindings {
    version: u32,
    document_id: String,
    sessions: BTreeMap<String, String>,
}
fn valid_label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn load_bindings(
    root: &Path,
    id: DocumentId,
    windows: &[WindowGrant],
) -> Result<BTreeMap<String, Grant>, HostOpenError> {
    let path = root.join("ipc-sessions.json");
    let mut bindings = match File::open(&path) {
        Ok(file) => {
            if file.metadata()?.len() > 64 * 1024 {
                return Err(HostOpenError::InvalidBindings);
            }
            let mut bytes = Vec::new();
            file.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
            serde_json::from_slice::<Bindings>(&bytes)
                .map_err(|_| HostOpenError::InvalidBindings)?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Bindings {
            version: 1,
            document_id: id.to_string(),
            sessions: BTreeMap::new(),
        },
        Err(e) => return Err(e.into()),
    };
    if bindings.version != 1
        || bindings.document_id != id.to_string()
        || bindings.sessions.len() > 64
        || bindings
            .sessions
            .iter()
            .any(|(label, id)| !valid_label(label) || id.parse::<SessionId>().is_err())
    {
        return Err(HostOpenError::InvalidBindings);
    }
    let mut identities = std::collections::BTreeSet::new();
    if bindings
        .sessions
        .values()
        .any(|id| !identities.insert(id.parse::<SessionId>().expect("validated session identity")))
    {
        return Err(HostOpenError::InvalidBindings);
    }
    let mut grants = BTreeMap::new();
    let mut changed = !path.exists();
    for window in windows {
        let session = if window.access == Access::Editor {
            if !bindings.sessions.contains_key(&window.label) {
                if bindings.sessions.len() >= 64 {
                    return Err(HostOpenError::InvalidBindings);
                }
                bindings.sessions.insert(
                    window.label.clone(),
                    threex_storage::new_session_id()?.to_string(),
                );
                changed = true;
            }
            Some(
                bindings.sessions[&window.label]
                    .parse()
                    .map_err(|_| HostOpenError::InvalidBindings)?,
            )
        } else {
            None
        };
        grants.insert(
            window.label.clone(),
            Grant {
                access: window.access,
                session,
                surface: None,
            },
        );
    }
    if changed {
        let bytes = serde_json::to_vec(&bindings).map_err(|_| HostOpenError::InvalidBindings)?;
        let temp = root.join("ipc-sessions.tmp");
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(temp, path)?;
        File::open(root)?.sync_all()?;
    }
    Ok(grants)
}
impl IpcHost {
    /// Startup filesystem work and the worker's recovery handshake run off the async executor.
    pub async fn open(root: PathBuf, config: HostConfig) -> Result<Self, HostOpenError> {
        tauri::async_runtime::spawn_blocking(move || Self::open_blocking(root, config))
            .await
            .map_err(|_| HostOpenError::Async)?
    }
    fn open_blocking(root: PathBuf, config: HostConfig) -> Result<Self, HostOpenError> {
        if config.windows.is_empty() || config.windows.len() > 64 {
            return Err(HostOpenError::InvalidPolicy);
        }
        let mut labels = std::collections::BTreeSet::new();
        if config
            .windows
            .iter()
            .any(|w| !valid_label(&w.label) || !labels.insert(w.label.clone()))
        {
            return Err(HostOpenError::InvalidPolicy);
        }
        let dev = config
            .dev_origin
            .map(|s| {
                s.parse::<tauri::Url>()
                    .map_err(|_| HostOpenError::InvalidPolicy)
            })
            .transpose()?;
        if dev.as_ref().is_some_and(|u| {
            !matches!(u.scheme(), "http" | "https")
                || !matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
                || !u.username().is_empty()
                || u.password().is_some()
                || u.path() != "/"
                || u.query().is_some()
                || u.fragment().is_some()
        }) {
            return Err(HostOpenError::InvalidPolicy);
        }
        let surfaces = threex_renderer::native::NativeSurfaces::default();
        let owner = JournalWorker::start_with_surfaces(
            DocumentStore::new(&root),
            config.worker,
            surfaces.clone(),
        )
        .map_err(HostOpenError::Worker)?;
        let document = owner.initial_state.document_id;
        let grants = load_bindings(&root, document, &config.windows)?;
        let client = owner.client();
        let ingress = Arc::new(Mutex::new(Ingress {
            next: 1,
            grants,
            stopped: false,
        }));
        let pending = Arc::new(tokio::sync::Semaphore::new(64));
        let scheduler = redraw::Scheduler::start(ingress.clone(), client.clone(), pending.clone())
            .map_err(HostOpenError::Scheduler)?;
        let asset_wake = scheduler.handle();
        surfaces.set_asset_waker(Arc::new(move |handle| asset_wake.invalidate_target(handle)));
        let wake = scheduler.handle();
        surfaces.set_scene_waker(Arc::new(move || wake.invalidate_all()));
        Ok(Self {
            client,
            owner: Mutex::new(Some(owner)),
            document,
            ingress,
            pending,
            dev,
            surfaces,
            scheduler,
        })
    }
    fn trusted_url(&self, url: &tauri::Url) -> bool {
        if !url.username().is_empty() || url.password().is_some() {
            return false;
        }
        (url.port().is_none()
            && ((url.scheme() == "tauri" && url.host_str() == Some("localhost"))
                || (matches!(url.scheme(), "http" | "https")
                    && url.host_str() == Some("tauri.localhost"))))
            || self.dev.as_ref().is_some_and(|dev| {
                url.scheme() == dev.scheme()
                    && url.host_str() == dev.host_str()
                    && url.port_or_known_default() == dev.port_or_known_default()
            })
    }
    /// Trusted Rust host may revoke a window. Previously accepted edits still finish.
    pub fn revoke_window(&self, label: &str) {
        if let Ok(mut ingress) = self.ingress.lock()
            && let Some(handle) = ingress.grants.remove(label).and_then(|g| g.surface)
        {
            self.scheduler.handle().remove(label, handle);
            let _ = self.surfaces.detach(handle);
        }
    }
    /// Stop ingress, drain accepted requests and join the worker off the async executor.
    pub async fn shutdown(&self) -> Result<(), ErrorDto> {
        self.ingress
            .lock()
            .map_err(|_| ErrorDto::host("stopped", Locale::En))?
            .stopped = true;
        let scheduler = self.scheduler.stop();
        tauri::async_runtime::spawn_blocking(move || {
            if let Some(join) = scheduler {
                let _ = join.join();
            }
        })
        .await
        .map_err(|_| ErrorDto::host("stopped", Locale::En))?;
        let owner = self
            .owner
            .lock()
            .map_err(|_| ErrorDto::host("stopped", Locale::En))?
            .take();
        if let Some(owner) = owner {
            tauri::async_runtime::spawn_blocking(move || owner.shutdown())
                .await
                .map_err(|_| ErrorDto::host("stopped", Locale::En))?
                .map_err(|_| ErrorDto::host("stopped", Locale::En))?;
        }
        self.surfaces
            .clear()
            .map_err(|_| ErrorDto::host("surface_unavailable", Locale::En))?;
        Ok(())
    }
    async fn dispatch(
        &self,
        label: &str,
        url: &tauri::Url,
        payload: serde_json::Value,
    ) -> Result<IpcReply, ErrorDto> {
        let locale = Locale::hint(&payload);
        if !self.trusted_url(url) {
            return Err(ErrorDto::host("forbidden", locale));
        }
        {
            let ingress = self
                .ingress
                .lock()
                .map_err(|_| ErrorDto::host("stopped", locale))?;
            if ingress.stopped {
                return Err(ErrorDto::host("stopped", locale));
            }
            if !ingress.grants.contains_key(label) {
                return Err(ErrorDto::host("forbidden", locale));
            }
        }
        let permit = self
            .pending
            .clone()
            .try_acquire_owned()
            .map_err(|_| ErrorDto::host("queue_full", locale))?;
        let request: IpcRequest = serde_json::from_value(payload)
            .map_err(|_| ErrorDto::host("invalid_request", locale))?;
        let (receiver, hello, session, scheduled) = {
            let mut ingress = self
                .ingress
                .lock()
                .map_err(|_| ErrorDto::host("stopped", locale))?;
            if ingress.stopped {
                return Err(ErrorDto::host("stopped", locale));
            }
            let grant = ingress
                .grants
                .get(label)
                .ok_or_else(|| ErrorDto::host("forbidden", locale))?
                .clone();
            let verify = |id: &str| {
                if id.parse::<DocumentId>().ok() == Some(self.document) {
                    Ok(())
                } else {
                    Err(ErrorDto::host("document_mismatch", locale))
                }
            };
            let edit_permission = || {
                if grant.access == Access::Editor {
                    Ok(())
                } else {
                    Err(ErrorDto::host("forbidden", locale))
                }
            };
            let hello = matches!(request, IpcRequest::Hello { .. });
            let mut scheduled = false;
            let action = match request {
                IpcRequest::Draw {
                    document_id,
                    camera,
                    ..
                } => {
                    verify(&document_id)?;
                    if !camera.iter().all(|v| v.is_finite()) {
                        return Err(ErrorDto::host("invalid_request", locale));
                    }
                    let handle = grant
                        .surface
                        .ok_or_else(|| ErrorDto::host("surface_stale", locale))?;
                    scheduled = self
                        .scheduler
                        .camera(label, handle, camera)
                        .map_err(|e| ErrorDto::host(&e.code, locale))?;
                    if scheduled {
                        WorkerAction::Inspect
                    } else {
                        WorkerAction::Render(threex_renderer::native::SurfaceRequest {
                            handle,
                            op: threex_renderer::native::SurfaceOp::Draw {
                                camera: camera.map(f32::to_bits),
                            },
                        })
                    }
                }
                IpcRequest::Hello { .. } => WorkerAction::Inspect,
                IpcRequest::Inspect { document_id, .. } => {
                    verify(&document_id)?;
                    WorkerAction::Inspect
                }
                IpcRequest::ContentHash { document_id, .. } => {
                    verify(&document_id)?;
                    WorkerAction::InspectContentHash
                }
                IpcRequest::Session { document_id, .. } => {
                    verify(&document_id)?;
                    edit_permission()?;
                    WorkerAction::InspectSession {
                        session_id: grant.session.unwrap(),
                    }
                }
                IpcRequest::Save { document_id, .. } => {
                    verify(&document_id)?;
                    edit_permission()?;
                    WorkerAction::Save
                }
                IpcRequest::Edit {
                    document_id,
                    session_id,
                    sequence,
                    action,
                    ..
                } => {
                    verify(&document_id)?;
                    edit_permission()?;
                    if session_id.parse::<SessionId>().ok() != grant.session {
                        return Err(ErrorDto::host("session_forbidden", locale));
                    }
                    let sequence = dto::number(&sequence)
                        .filter(|&n| n > 0)
                        .ok_or_else(|| ErrorDto::host("invalid_request", locale))?;
                    WorkerAction::Protocol(ProtocolRequest {
                        protocol_version: threex_storage::REQUEST_PROTOCOL_VERSION,
                        document_id: self.document,
                        session_id: grant.session.unwrap(),
                        sequence,
                        action: action.protocol(locale)?,
                    })
                }
            };
            let id = ingress.next;
            let next = id
                .checked_add(1)
                .ok_or_else(|| ErrorDto::host("stopped", locale))?;
            let receiver = self
                .client
                .try_submit(WorkerRequest { id, action })
                .map_err(|e| {
                    ErrorDto::host(
                        if e.code == threex_storage::WorkerCode::QueueFull {
                            "queue_full"
                        } else {
                            "stopped"
                        },
                        locale,
                    )
                })?;
            ingress.next = next;
            (receiver, hello, grant.session, scheduled)
        };
        // Dropping the frontend response does not cancel accepted durable operations.
        let reply = tauri::async_runtime::spawn_blocking(move || {
            let _permit = permit;
            receiver.recv()
        })
        .await
        .map_err(|_| ErrorDto::host("stopped", locale))?
        .map_err(|_| ErrorDto::host("stopped", locale))?;
        let outcome = reply
            .result
            .map_err(|failure| ErrorDto::worker(failure, locale))?;
        Ok(match outcome {
            WorkerOutcome::Rendered { result, state } => {
                surface::frame_reply(result, state, locale)?
            }
            WorkerOutcome::State(state) if scheduled => IpcReply::Frame {
                status: "scheduled",
                state: state.into(),
            },
            WorkerOutcome::State(state) if hello => IpcReply::Hello {
                state: state.into(),
                session_id: session.map(|s| s.to_string()),
            },
            WorkerOutcome::State(state) => IpcReply::State {
                state: state.into(),
            },
            WorkerOutcome::Protocol {
                revision,
                replayed,
                state,
            } => IpcReply::Edited {
                revision: revision.to_string(),
                replayed,
                state: state.into(),
            },
            WorkerOutcome::Saved { generation, state } => IpcReply::Saved {
                generation: generation.to_string(),
                state: state.into(),
            },
            WorkerOutcome::Session {
                sequence,
                status,
                state,
                ..
            } => IpcReply::Session {
                sequence: sequence.map(|v| v.to_string()),
                status: status.map(|s| match s {
                    SessionStatus::Active { .. } => "active",
                    SessionStatus::Closed { .. } => "closed",
                }),
                state: state.into(),
            },
            WorkerOutcome::ContentHash { hash, state } => IpcReply::ContentHash {
                hash: hash.to_string(),
                version: threex_core::CONTENT_HASH_VERSION,
                state: state.into(),
            },
            _ => return Err(ErrorDto::host("stopped", locale)),
        })
    }
}
pub mod commands {
    use super::*;
    /// Tauri injects the caller Webview and managed host; neither comes from request JSON.
    #[tauri::command]
    pub async fn document_ipc<R: Runtime>(
        webview: tauri::Webview<R>,
        host: tauri::State<'_, IpcHost>,
        payload: serde_json::Value,
    ) -> Result<IpcReply, ErrorDto> {
        if webview.window().label() != webview.label() {
            return Err(ErrorDto::host("forbidden", Locale::hint(&payload)));
        }
        let url = webview
            .url()
            .map_err(|_| ErrorDto::host("forbidden", Locale::hint(&payload)))?;
        host.dispatch(webview.label(), &url, payload).await
    }
}
fn has_remote_scope(scopes: Option<&[tauri::utils::acl::resolved::ResolvedCommand]>) -> bool {
    scopes.is_some_and(|scopes| {
        scopes.iter().any(|s| {
            matches!(
                s.context,
                tauri::utils::acl::ExecutionContext::Remote { .. }
            )
        })
    })
}
/// Compose this handler with other app handlers if needed. It also rejects explicitly granted remote ACL contexts.
pub fn invoke_handler<R: Runtime>() -> impl Fn(tauri::ipc::Invoke<R>) -> bool + Send + Sync + 'static
{
    let generated: fn(tauri::ipc::Invoke<R>) -> bool =
        tauri::generate_handler![commands::document_ipc];
    move |invoke: tauri::ipc::Invoke<R>| {
        if invoke.message.command() == "document_ipc" && has_remote_scope(invoke.acl.as_deref()) {
            invoke
                .resolver
                .reject(ErrorDto::host("forbidden", Locale::En));
            return true;
        }
        generated(invoke)
    }
}
/// Registers this library's single command and managed application host.
pub fn attach<R: Runtime>(builder: tauri::Builder<R>, host: IpcHost) -> tauri::Builder<R> {
    builder.manage(host).invoke_handler(invoke_handler())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::{
        future::Future,
        sync::atomic::{AtomicU64, Ordering},
    };
    use tauri::{
        Manager,
        ipc::{CallbackFn, InvokeBody},
        test::{MockRuntime, mock_builder, mock_context, noop_assets},
        webview::InvokeRequest,
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "threex-ipc-{}-{}",
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
    fn config() -> HostConfig {
        HostConfig {
            windows: vec![
                WindowGrant {
                    label: "main".into(),
                    access: Access::Editor,
                },
                WindowGrant {
                    label: "panel".into(),
                    access: Access::Editor,
                },
                WindowGrant {
                    label: "viewer".into(),
                    access: Access::ReadOnly,
                },
            ],
            ..Default::default()
        }
    }
    fn open(t: &Temp) -> IpcHost {
        tauri::async_runtime::block_on(IpcHost::open(t.0.clone(), config())).unwrap()
    }
    fn app(t: &Temp) -> tauri::App<MockRuntime> {
        attach(mock_builder(), open(t))
            .build(mock_context(noop_assets()))
            .unwrap()
    }
    fn view(app: &tauri::App<MockRuntime>, label: &str) -> tauri::WebviewWindow<MockRuntime> {
        tauri::WebviewWindowBuilder::new(app, label, tauri::WebviewUrl::App("index.html".into()))
            .build()
            .unwrap()
    }
    fn ipc_origin(
        w: &tauri::WebviewWindow<MockRuntime>,
        payload: Value,
        origin: &str,
    ) -> Result<Value, Value> {
        tauri::test::get_ipc_response(
            w,
            InvokeRequest {
                cmd: "document_ipc".into(),
                callback: CallbackFn(0),
                error: CallbackFn(1),
                url: origin.parse().unwrap(),
                body: InvokeBody::Json(json!({"payload":payload})),
                headers: Default::default(),
                invoke_key: tauri::test::INVOKE_KEY.into(),
            },
        )
        .map(|b| b.deserialize::<Value>().unwrap())
    }
    fn ipc(w: &tauri::WebviewWindow<MockRuntime>, payload: Value) -> Result<Value, Value> {
        ipc_origin(w, payload, "tauri://localhost")
    }
    fn hello(w: &tauri::WebviewWindow<MockRuntime>) -> Value {
        ipc(w, json!({"type":"hello"})).unwrap()
    }
    fn command(id: u32) -> Vec<u8> {
        let mut b = vec![0; 64];
        b[..4].copy_from_slice(&1u32.to_le_bytes());
        b[4..8].copy_from_slice(&id.to_le_bytes());
        b
    }
    fn edit(hello: &Value, seq: &str, revision: &str, id: u32) -> Value {
        json!({"type":"edit","document_id":hello["state"]["document_id"],"session_id":hello["session_id"],"sequence":seq,"action":{"type":"commit","expected_revision":revision,"commands":command(id)}})
    }
    fn shutdown(app: &tauri::App<MockRuntime>) {
        tauri::async_runtime::block_on(app.state::<IpcHost>().shutdown()).unwrap();
    }
    #[test]
    fn real_tauri_invoke_shares_rust_owner_and_replays_after_restart() {
        let t = Temp::new();
        let a = app(&t);
        let main = view(&a, "main");
        let panel = view(&a, "panel");
        let h = hello(&main);
        let p = hello(&panel);
        assert_eq!(h["state"]["document_id"], p["state"]["document_id"]);
        assert_ne!(h["session_id"], p["session_id"]);
        let first = edit(&h, "1", "0", 1);
        let second = edit(&p, "1", "1", 2);
        assert_eq!(ipc(&main, first.clone()).unwrap()["revision"], "1");
        assert_eq!(ipc(&panel, second.clone()).unwrap()["revision"], "2");
        ipc(
            &panel,
            json!({"type":"save","document_id":h["state"]["document_id"]}),
        )
        .unwrap();
        main.close().unwrap();
        assert_eq!(
            ipc(
                &panel,
                json!({"type":"inspect","document_id":h["state"]["document_id"]})
            )
            .unwrap()["state"]["objects"],
            2
        );
        shutdown(&a);
        drop(a);
        let a = app(&t);
        let main = view(&a, "main");
        let panel = view(&a, "panel");
        assert_eq!(hello(&main)["session_id"], h["session_id"]);
        for (view, request, revision) in [(&main, first, "1"), (&panel, second, "2")] {
            let r = ipc(view, request).unwrap();
            assert_eq!(r["revision"], revision);
            assert_eq!(r["replayed"], true);
            assert_eq!(r["state"]["revision"], "2");
            assert_eq!(r["state"]["successful_sync_groups"], "0");
        }
        shutdown(&a);
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
    fn localized_durable_error_dto_is_identical_after_state_change_and_restart() {
        let t = Temp::new();
        let a = app(&t);
        let w = view(&a, "main");
        let h = hello(&w);
        let bad = json!({"type":"edit","document_id":h["state"]["document_id"],"session_id":h["session_id"],"sequence":"1","locale":"ja","action":{"type":"undo","expected_revision":"0"}});
        let original = ipc(&w, bad.clone()).unwrap_err();
        assert_eq!(original["code"], "no_history");
        assert_eq!(original["observed_revision"], "0");
        assert_eq!(original["retry"], "new_request");
        assert!(original["message"].as_str().unwrap().contains("履歴"));
        ipc(&w, edit(&h, "2", "0", 1)).unwrap();
        assert_eq!(ipc(&w, bad.clone()).unwrap_err(), original);
        shutdown(&a);
        drop(a);
        let a = app(&t);
        let w = view(&a, "main");
        assert_eq!(ipc(&w, bad).unwrap_err(), original);
        assert_eq!(hello(&w)["state"]["objects"], 1);
        shutdown(&a);
    }
    #[test]
    fn caller_roles_document_session_origin_and_revocation_are_enforced() {
        let t = Temp::new();
        let a = app(&t);
        let main = view(&a, "main");
        let panel = view(&a, "panel");
        let viewer = view(&a, "viewer");
        let stranger = view(&a, "stranger");
        let h = hello(&main);
        assert!(hello(&viewer)["session_id"].is_null());
        assert_eq!(
            ipc(&viewer, edit(&h, "1", "0", 1)).unwrap_err()["code"],
            "forbidden"
        );
        assert_eq!(
            ipc(
                &viewer,
                json!({"type":"save","document_id":h["state"]["document_id"]})
            )
            .unwrap_err()["code"],
            "forbidden"
        );
        assert_eq!(
            ipc(&panel, edit(&h, "1", "0", 1)).unwrap_err()["code"],
            "session_forbidden"
        );
        assert_eq!(
            ipc(&stranger, json!({"type":"hello","label":"main"})).unwrap_err()["code"],
            "forbidden"
        );
        assert_eq!(
            ipc(
                &main,
                json!({"type":"inspect","document_id":"00000000000000000000000000000000"})
            )
            .unwrap_err()["code"],
            "document_mismatch"
        );
        assert!(ipc_origin(&main, edit(&h, "1", "0", 1), "https://evil.example").is_err());
        a.state::<IpcHost>().revoke_window("main");
        assert_eq!(
            ipc(&main, edit(&h, "1", "0", 1)).unwrap_err()["code"],
            "forbidden"
        );
        assert_eq!(hello(&panel)["state"]["successful_sync_groups"], "0");
        shutdown(&a);
        assert!(
            DocumentStore::new(&t.0)
                .load()
                .unwrap()
                .document
                .scene()
                .is_empty()
        );
    }
    #[test]
    fn strict_json_and_numeric_precision_reject_before_worker_admission() {
        let t = Temp::new();
        let a = app(&t);
        let w = view(&a, "main");
        let h = hello(&w);
        for seq in ["01", "-1", "1.0", " 1", "18446744073709551616"] {
            assert_eq!(
                ipc(&w, edit(&h, seq, "0", 1)).unwrap_err()["code"],
                "invalid_request"
            );
        }
        let mut numeric = edit(&h, "1", "0", 1);
        numeric["sequence"] = json!(1);
        assert_eq!(ipc(&w, numeric).unwrap_err()["code"], "invalid_request");
        let mut unknown = edit(&h, "1", "0", 1);
        unknown["path"] = json!("/tmp/forbidden-save");
        assert_eq!(ipc(&w, unknown).unwrap_err()["code"], "invalid_request");
        let mut huge = edit(&h, "1", "0", 1);
        huge["action"]["commands"] = json!(vec![0u8; 256 * 1024 + 1]);
        assert_eq!(ipc(&w, huge).unwrap_err()["code"], "invalid_request");
        assert_eq!(ipc(&w, edit(&h, "1", "0", 1)).unwrap()["revision"], "1");
        shutdown(&a);
        assert_eq!(dto::number("18446744073709551615"), Some(u64::MAX));
        let state = StateDto::from(threex_storage::WorkerState {
            document_id: DocumentId::from_bytes([1; 16]).unwrap(),
            revision: 9007199254740993,
            objects: 0,
            saved_revision: Some(u64::MAX),
            dirty: false,
            discarded_journal_bytes: u64::MAX,
            successful_sync_groups: u64::MAX,
        });
        let value = serde_json::to_value(state).unwrap();
        assert_eq!(value["revision"], "9007199254740993");
        assert_eq!(value["saved_revision"], u64::MAX.to_string());
    }
    #[test]
    fn bounded_async_bridge_cancellation_and_stop_do_not_cancel_accepted_edit() {
        let t = Temp::new();
        let host = open(&t);
        let url = "tauri://localhost".parse().unwrap();
        let permits = host.pending.clone().try_acquire_many_owned(64).unwrap();
        assert_eq!(
            tauri::async_runtime::block_on(host.dispatch("main", &url, json!({"type":"hello"})))
                .unwrap_err()
                .code,
            "queue_full"
        );
        drop(permits);
        let hello = serde_json::to_value(
            tauri::async_runtime::block_on(host.dispatch("main", &url, json!({"type":"hello"})))
                .unwrap(),
        )
        .unwrap();
        let mut future = Box::pin(host.dispatch("main", &url, edit(&hello, "1", "0", 1)));
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let _ = future.as_mut().poll(&mut context);
        drop(future);
        host.revoke_window("main");
        tauri::async_runtime::block_on(host.shutdown()).unwrap();
        assert_eq!(
            DocumentStore::new(&t.0)
                .load()
                .unwrap()
                .document
                .scene()
                .len(),
            1
        );
        assert_eq!(
            tauri::async_runtime::block_on(host.dispatch("main", &url, json!({"type":"hello"})))
                .unwrap_err()
                .code,
            "stopped"
        );
    }
    #[test]
    fn session_end_and_retry_cross_tauri_and_restart_boundaries() {
        let t = Temp::new();
        let a = app(&t);
        let w = view(&a, "main");
        let h = hello(&w);
        let first = edit(&h, "1", "0", 1);
        ipc(&w, first.clone()).unwrap();
        let close = json!({"type":"edit","document_id":h["state"]["document_id"],"session_id":h["session_id"],"sequence":"2","action":{"type":"end_session","expected_revision":"1"}});
        ipc(&w, close.clone()).unwrap();
        shutdown(&a);
        drop(a);
        let a = app(&t);
        let w = view(&a, "main");
        assert_eq!(ipc(&w, first).unwrap()["replayed"], true);
        assert_eq!(ipc(&w, close).unwrap()["replayed"], true);
        let status = ipc(
            &w,
            json!({"type":"session","document_id":h["state"]["document_id"]}),
        )
        .unwrap();
        assert_eq!(status["status"], "closed");
        assert_eq!(status["sequence"], "2");
        assert_eq!(
            ipc(&w, edit(&h, "3", "1", 2)).unwrap_err()["code"],
            "session_closed"
        );
        shutdown(&a);
    }
    #[test]
    fn corrupt_session_bindings_fail_closed_without_minting_replacement_identity() {
        let t = Temp::new();
        let host = open(&t);
        tauri::async_runtime::block_on(host.shutdown()).unwrap();
        drop(host);
        let path = t.0.join("ipc-sessions.json");
        fs::write(&path, b"broken").unwrap();
        assert!(matches!(
            tauri::async_runtime::block_on(IpcHost::open(t.0.clone(), config())),
            Err(HostOpenError::InvalidBindings)
        ));
        assert_eq!(fs::read(&path).unwrap(), b"broken");
        let id = DocumentStore::new(&t.0)
            .load()
            .unwrap()
            .document
            .document_id()
            .unwrap();
        let duplicate = threex_storage::new_session_id().unwrap().to_string();
        fs::write(
            &path,
            serde_json::to_vec(&json!({
                "version": 1, "document_id": id.to_string(),
                "sessions": {"main": duplicate, "panel": duplicate}
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(matches!(
            tauri::async_runtime::block_on(IpcHost::open(t.0.clone(), config())),
            Err(HostOpenError::InvalidBindings)
        ));
    }
    #[test]
    fn policy_origin_and_remote_acl_context_checks_are_fail_closed() {
        let t = Temp::new();
        let host = open(&t);
        for origin in [
            "https://evil.example",
            "file:///tmp/index.html",
            "tauri://evil",
            "http://user@tauri.localhost",
            "http://tauri.localhost:5555",
        ] {
            assert!(!host.trusted_url(&origin.parse().unwrap()));
        }
        assert!(host.trusted_url(&"tauri://localhost/index.html".parse().unwrap()));
        let remote = tauri::utils::acl::resolved::ResolvedCommand {
            context: tauri::utils::acl::ExecutionContext::Remote {
                url: "https://evil.example".parse().unwrap(),
            },
            ..Default::default()
        };
        assert!(has_remote_scope(Some(&[remote])));
        assert!(!has_remote_scope(None));
        tauri::async_runtime::block_on(host.shutdown()).unwrap();
        let invalid = HostConfig {
            dev_origin: Some("https://evil.example".into()),
            ..config()
        };
        assert!(matches!(
            tauri::async_runtime::block_on(IpcHost::open(t.0.clone(), invalid)),
            Err(HostOpenError::InvalidPolicy)
        ));
    }
    #[test]
    fn dto_translation_and_retry_advice_cover_every_worker_code() {
        use threex_storage::WorkerCode::*;
        for code in [
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
        ] {
            let failure = threex_storage::WorkerFailure {
                code,
                current_revision: u64::MAX,
            };
            let mut messages = std::collections::BTreeSet::new();
            for locale in [Locale::En, Locale::Ja, Locale::ZhCn] {
                let dto = ErrorDto::worker(failure.clone(), locale);
                messages.insert(dto.message);
                assert_eq!(dto.observed_revision, Some(u64::MAX.to_string()));
            }
            assert_eq!(messages.len(), 3);
        }
    }
    #[derive(Default)]
    struct RenderLog {
        frames: Vec<(usize, usize, String)>,
        drops: usize,
        sizes: Vec<(u32, u32)>,
        lose: bool,
        asset_uploads: Vec<String>,
    }
    struct TestSurface(Arc<Mutex<RenderLog>>);
    impl Drop for TestSurface {
        fn drop(&mut self) {
            self.0.lock().unwrap().drops += 1;
        }
    }
    impl threex_renderer::native::SurfaceBackend for TestSurface {
        fn upload_asset(
            &mut self,
            document: &threex_core::Document,
            entity: u32,
            manifest: threex_core::asset_pages::AssetManifest,
            _level: u8,
            page: Result<
                threex_core::asset_pages::ValidatedPage,
                threex_renderer::asset_pages::PageFailure,
            >,
        ) -> Result<threex_renderer::native::AssetDelivery, threex_renderer::native::SurfaceFailure>
        {
            assert_eq!(
                document.scene().object(entity).unwrap().asset,
                Some(manifest.reference())
            );
            self.0
                .lock()
                .unwrap()
                .asset_uploads
                .push(std::thread::current().name().unwrap().to_owned());
            Ok(threex_renderer::native::AssetDelivery {
                changed: page.is_ok(),
                ready: page.is_ok(),
                failure: page.err(),
            })
        }

        fn configure(
            &mut self,
            w: u32,
            h: u32,
        ) -> Result<(), threex_renderer::native::SurfaceFailure> {
            if w > 4096 || h > 4096 {
                Err(threex_renderer::native::SurfaceFailure::InvalidSize)
            } else {
                self.0.lock().unwrap().sizes.push((w, h));
                Ok(())
            }
        }
        fn draw(
            &mut self,
            scene: &threex_core::Scene,
            _: &[f32; 16],
        ) -> Result<(), threex_renderer::native::AcquireFailure> {
            let mut log = self.0.lock().unwrap();
            log.frames.push((
                scene as *const _ as usize,
                scene.len(),
                std::thread::current().name().unwrap_or("").into(),
            ));
            if log.lose {
                Err(threex_renderer::native::AcquireFailure::DeviceLost)
            } else {
                Ok(())
            }
        }
    }
    fn camera() -> [f32; 16] {
        let mut m = [0.; 16];
        for i in [0, 5, 10, 15] {
            m[i] = 1.;
        }
        m
    }
    #[test]
    fn renderer_reads_worker_scene_and_restart_replay_cannot_duplicate_edits() {
        use threex_renderer::native::SurfaceOp;
        let t = Temp::new();
        let a = app(&t);
        let w = view(&a, "main");
        let h = hello(&w);
        let log = Arc::new(Mutex::new(RenderLog::default()));
        let host = a.state::<IpcHost>();
        let handle = host
            .attach_surface("main", 1, Box::new(TestSurface(log.clone())), 64, 64)
            .unwrap();
        let draw = json!({"type":"draw","document_id":h["state"]["document_id"],"camera":camera(),"locale":"ja"});
        let original = edit(&h, "1", "0", 1);
        ipc(&w, original.clone()).unwrap();
        let result = ipc(&w, draw.clone()).unwrap();
        assert_eq!(result["status"], "presented");
        assert_eq!(result["state"]["revision"], "1");
        ipc(&w, edit(&h, "2", "1", 2)).unwrap();
        ipc(&w, draw.clone()).unwrap();
        {
            let log = log.lock().unwrap();
            assert_eq!(log.frames.len(), 2);
            assert_eq!(log.frames[0].0, log.frames[1].0);
            assert_eq!((log.frames[0].1, log.frames[1].1), (1, 2));
            assert!(log.frames.iter().all(|f| f.2 == "threex-document"));
        }
        assert_eq!(
            serde_json::to_value(
                tauri::async_runtime::block_on(host.surface_request(
                    "main",
                    handle,
                    SurfaceOp::Suspend
                ))
                .unwrap()
            )
            .unwrap()["status"],
            "suspended"
        );
        assert_eq!(ipc(&w, draw.clone()).unwrap()["status"], "suspended");
        tauri::async_runtime::block_on(host.surface_request("main", handle, SurfaceOp::Resume))
            .unwrap();
        let panel = view(&a, "panel");
        let panel_handle = host
            .attach_surface("panel", 2, Box::new(TestSurface(log.clone())), 64, 64)
            .unwrap();
        assert_eq!(ipc(&panel, draw.clone()).unwrap()["status"], "presented");
        {
            let entries = &log.lock().unwrap().frames;
            assert_eq!(entries[0].0, entries.last().unwrap().0);
        }
        tauri::async_runtime::block_on(host.surface_request(
            "panel",
            panel_handle,
            SurfaceOp::Detach,
        ))
        .unwrap();
        drop(panel);
        log.lock().unwrap().lose = true;
        let error = ipc(&w, draw.clone()).unwrap_err();
        assert_eq!(error["code"], "device_lost");
        assert_ne!(error["message"], "device_lost");
        // GPU failure does not prevent durable editing or saving.
        ipc(&w, edit(&h, "3", "2", 3)).unwrap();
        ipc(
            &w,
            json!({"type":"save","document_id":h["state"]["document_id"]}),
        )
        .unwrap();
        shutdown(&a);
        assert_eq!(log.lock().unwrap().drops, 2);
        drop(w);
        drop(a);
        let a = app(&t);
        let w = view(&a, "main");
        let next = hello(&w);
        assert_eq!(h["session_id"], next["session_id"]);
        let replay = ipc(&w, original).unwrap();
        assert_eq!(replay["replayed"], true);
        assert_eq!(replay["revision"], "1");
        assert_eq!(replay["state"]["revision"], "3");
        assert_eq!(replay["state"]["objects"], 3);
        assert_eq!(ipc(&w, draw.clone()).unwrap_err()["code"], "surface_stale");
        let log = Arc::new(Mutex::new(RenderLog::default()));
        a.state::<IpcHost>()
            .attach_surface("main", 1, Box::new(TestSurface(log.clone())), 64, 64)
            .unwrap();
        assert_eq!(ipc(&w, draw).unwrap()["status"], "presented");
        assert_eq!(log.lock().unwrap().frames[0].1, 3);
        shutdown(&a);
    }
    #[test]
    fn native_event_generation_and_window_authorization_preserve_document() {
        use threex_renderer::native::SurfaceOp;
        let t = Temp::new();
        let a = app(&t);
        let w = view(&a, "main");
        let h = hello(&w);
        let host = a.state::<IpcHost>();
        let log = Arc::new(Mutex::new(RenderLog::default()));
        let old = host
            .attach_surface("main", 1, Box::new(TestSurface(log.clone())), 64, 64)
            .unwrap();
        assert!(
            host.attach_surface("panel", 1, Box::new(TestSurface(log.clone())), 64, 64)
                .is_err()
        );
        assert!(
            host.attach_surface("stranger", 2, Box::new(TestSurface(log.clone())), 64, 64)
                .is_err()
        );
        assert!(
            host.attach_surface("main", 1, Box::new(TestSurface(log.clone())), 9999, 64)
                .is_err()
        );
        let new = host
            .attach_surface("main", 1, Box::new(TestSurface(log.clone())), 64, 64)
            .unwrap();
        let stale = tauri::async_runtime::block_on(host.surface_window_event(
            "main",
            old,
            &tauri::WindowEvent::Destroyed,
        ))
        .unwrap_err();
        assert_eq!(stale.code, "surface_stale");
        let first = host
            .try_surface_window_event(
                "main",
                new,
                &tauri::WindowEvent::Resized(tauri::PhysicalSize {
                    width: 80,
                    height: 80,
                }),
            )
            .unwrap()
            .unwrap();
        let second = host
            .try_surface_window_event(
                "main",
                new,
                &tauri::WindowEvent::Resized(tauri::PhysicalSize {
                    width: 90,
                    height: 90,
                }),
            )
            .unwrap()
            .unwrap();
        // Waiting in reverse order must not reorder native events.
        tauri::async_runtime::block_on(second.wait()).unwrap();
        tauri::async_runtime::block_on(first.wait()).unwrap();
        assert!(log.lock().unwrap().sizes.ends_with(&[(80, 80), (90, 90)]));
        let resized = tauri::WindowEvent::Resized(tauri::PhysicalSize {
            width: 0,
            height: 0,
        });
        let suspended =
            tauri::async_runtime::block_on(host.surface_window_event("main", new, &resized))
                .unwrap()
                .unwrap();
        assert_eq!(
            serde_json::to_value(suspended).unwrap()["status"],
            "suspended"
        );
        tauri::async_runtime::block_on(host.surface_request(
            "main",
            new,
            SurfaceOp::Resize {
                width: 100,
                height: 100,
            },
        ))
        .unwrap();
        tauri::async_runtime::block_on(host.surface_window_event(
            "main",
            new,
            &tauri::WindowEvent::Destroyed,
        ))
        .unwrap();
        let draw = json!({"type":"draw","document_id":h["state"]["document_id"],"camera":camera()});
        assert_eq!(ipc(&w, draw).unwrap_err()["code"], "surface_stale");
        let current = ipc(
            &w,
            json!({"type":"inspect","document_id":h["state"]["document_id"]}),
        )
        .unwrap();
        assert_eq!(current["state"]["revision"], "0");
        assert_eq!(current["state"]["objects"], 0);
        assert_eq!(current["state"]["successful_sync_groups"], "0");
        shutdown(&a);
    }
    #[derive(Default)]
    struct ScheduledLog {
        frames: Vec<(usize, usize, [f32; 16], String)>,
        sizes: Vec<(u32, u32)>,
        drops: usize,
    }
    struct ScheduledBackend {
        log: Arc<Mutex<ScheduledLog>>,
        error: Option<threex_renderer::native::AcquireFailure>,
        gate: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
    }
    impl Drop for ScheduledBackend {
        fn drop(&mut self) {
            self.log.lock().unwrap().drops += 1;
        }
    }
    impl threex_renderer::native::SurfaceBackend for ScheduledBackend {
        fn configure(
            &mut self,
            w: u32,
            h: u32,
        ) -> Result<(), threex_renderer::native::SurfaceFailure> {
            if w > 4096 || h > 4096 {
                return Err(threex_renderer::native::SurfaceFailure::InvalidSize);
            }
            self.log.lock().unwrap().sizes.push((w, h));
            Ok(())
        }
        fn draw(
            &mut self,
            scene: &threex_core::Scene,
            camera: &[f32; 16],
        ) -> Result<(), threex_renderer::native::AcquireFailure> {
            self.log.lock().unwrap().frames.push((
                scene as *const _ as usize,
                scene.len(),
                *camera,
                std::thread::current().name().unwrap_or("").into(),
            ));
            if let Some((entered, release)) = self.gate.take() {
                entered.send(()).unwrap();
                release
                    .recv_timeout(std::time::Duration::from_secs(3))
                    .unwrap();
            }
            self.error.map_or(Ok(()), Err)
        }
    }
    fn scheduled_backend(log: &Arc<Mutex<ScheduledLog>>) -> Box<ScheduledBackend> {
        Box::new(ScheduledBackend {
            log: log.clone(),
            error: None,
            gate: None,
        })
    }
    fn inspect(w: &tauri::WebviewWindow<MockRuntime>, h: &Value) -> Value {
        ipc(
            w,
            json!({"type":"inspect","document_id":h["state"]["document_id"]}),
        )
        .unwrap()
    }
    #[test]
    fn scheduled_native_callback_coalesces_pressure_resize_and_retains_destroy() {
        let t = Temp::new();
        let a = app(&t);
        let w = view(&a, "main");
        let h = hello(&w);
        let host = a.state::<IpcHost>();
        let log = Arc::new(Mutex::new(ScheduledLog::default()));
        let handle = host
            .attach_surface("main", 1, scheduled_backend(&log), 64, 64)
            .unwrap();
        let permits = host.pending.clone().try_acquire_many_owned(64).unwrap();
        let redraw = host.schedule_surface("main", handle, 64, 64).unwrap();
        let mut event = None;
        redraw::install_callback(&redraw, |callback| event = Some(callback));
        let event = event.unwrap();
        for n in 1..=1000 {
            event(&tauri::WindowEvent::Resized(tauri::PhysicalSize {
                width: 100 + n % 200,
                height: 200 + n % 100,
            }));
        }
        event(&tauri::WindowEvent::Resized(tauri::PhysicalSize {
            width: 400,
            height: 300,
        }));
        assert_eq!(log.lock().unwrap().frames.len(), 0);
        drop(permits);
        assert!(redraw.wait_idle());
        assert_eq!(log.lock().unwrap().sizes, vec![(64, 64), (400, 300)]);
        assert_eq!(log.lock().unwrap().frames.len(), 1);
        let idle = redraw.status().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert_eq!(redraw.status().unwrap(), idle);
        event(&tauri::WindowEvent::Focused(false));
        assert!(redraw.wait_idle());
        assert_eq!(log.lock().unwrap().frames.len(), 1);
        event(&tauri::WindowEvent::Focused(true));
        assert!(redraw.wait_idle());
        assert_eq!(log.lock().unwrap().frames.len(), 2);
        event(&tauri::WindowEvent::Resized(tauri::PhysicalSize {
            width: 0,
            height: 0,
        }));
        assert!(redraw.wait_idle());
        redraw.request_redraw().unwrap();
        assert!(redraw.wait_idle());
        assert_eq!(log.lock().unwrap().frames.len(), 2);
        event(&tauri::WindowEvent::Resized(tauri::PhysicalSize {
            width: 200,
            height: 100,
        }));
        assert!(redraw.wait_idle());
        assert_eq!(log.lock().unwrap().frames.len(), 3);
        let permits = host.pending.clone().try_acquire_many_owned(64).unwrap();
        event(&tauri::WindowEvent::Destroyed);
        assert!(redraw.status().unwrap().closed);
        drop(permits);
        assert!(redraw.wait_idle());
        assert!(redraw.status().is_none());
        assert_eq!(log.lock().unwrap().drops, 1);
        let state = inspect(&w, &h);
        assert_eq!(state["state"]["revision"], "0");
        assert_eq!(state["state"]["successful_sync_groups"], "0");
        shutdown(&a);
    }
    #[test]
    fn scheduled_document_changes_notify_without_response_and_restart_replays_stay_idle() {
        let t = Temp::new();
        let a = app(&t);
        let w = view(&a, "main");
        let h = hello(&w);
        let host = a.state::<IpcHost>();
        let log = Arc::new(Mutex::new(ScheduledLog::default()));
        let handle = host
            .attach_surface("main", 1, scheduled_backend(&log), 64, 64)
            .unwrap();
        let redraw = host.schedule_surface("main", handle, 64, 64).unwrap();
        assert!(redraw.wait_idle());
        let original = edit(&h, "1", "0", 1);
        ipc(&w, original.clone()).unwrap();
        assert!(redraw.wait_idle());
        assert_eq!(log.lock().unwrap().frames.len(), 2);
        assert_eq!(ipc(&w, original.clone()).unwrap()["replayed"], true);
        assert!(redraw.wait_idle());
        assert_eq!(log.lock().unwrap().frames.len(), 2);
        let noop = json!({"type":"edit","document_id":h["state"]["document_id"],"session_id":h["session_id"],"sequence":"2","action":{"type":"commit","expected_revision":"1","commands":[]}});
        ipc(&w, noop).unwrap();
        assert!(redraw.wait_idle());
        assert_eq!(log.lock().unwrap().frames.len(), 2);
        let rejected = json!({"type":"edit","document_id":h["state"]["document_id"],"session_id":h["session_id"],"sequence":"3","action":{"type":"redo","expected_revision":"1"},"locale":"ja"});
        let error = ipc(&w, rejected.clone()).unwrap_err();
        assert_eq!(error["code"], "no_history");
        assert!(redraw.wait_idle());
        assert_eq!(log.lock().unwrap().frames.len(), 2);
        let url = "tauri://localhost".parse().unwrap();
        let mut future = Box::pin(host.dispatch("main", &url, edit(&h, "4", "1", 2)));
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let _ = future.as_mut().poll(&mut context);
        drop(future);
        assert_eq!(inspect(&w, &h)["state"]["objects"], 2);
        assert!(redraw.wait_idle());
        {
            let log = log.lock().unwrap();
            assert_eq!(log.frames.len(), 3);
            assert_eq!(log.frames.last().unwrap().1, 2);
            assert!(
                log.frames
                    .iter()
                    .all(|f| f.0 == log.frames[0].0 && f.3 == "threex-document")
            );
        }
        let mut camera = camera();
        camera[12] = 0.5;
        let drawn = ipc(
            &w,
            json!({"type":"draw","document_id":h["state"]["document_id"],"camera":camera}),
        )
        .unwrap();
        assert_eq!(drawn["status"], "scheduled");
        assert!(redraw.wait_idle());
        assert_eq!(log.lock().unwrap().frames.last().unwrap().2, camera);
        ipc(
            &w,
            json!({"type":"save","document_id":h["state"]["document_id"]}),
        )
        .unwrap();
        assert!(redraw.wait_idle());
        shutdown(&a);
        drop(w);
        drop(a);
        let a = app(&t);
        let w = view(&a, "main");
        let next = hello(&w);
        assert_eq!(h["session_id"], next["session_id"]);
        let log = Arc::new(Mutex::new(ScheduledLog::default()));
        let host = a.state::<IpcHost>();
        let handle = host
            .attach_surface("main", 1, scheduled_backend(&log), 64, 64)
            .unwrap();
        let redraw = host.schedule_surface("main", handle, 64, 64).unwrap();
        assert!(redraw.wait_idle());
        assert_eq!(ipc(&w, original).unwrap()["replayed"], true);
        assert_eq!(ipc(&w, rejected).unwrap_err(), error);
        assert!(redraw.wait_idle());
        assert_eq!(log.lock().unwrap().frames.len(), 1);
        assert_eq!(log.lock().unwrap().frames[0].1, 2);
        assert_eq!(inspect(&w, &next)["state"]["successful_sync_groups"], "0");
        shutdown(&a);
    }
    #[test]
    fn scheduled_inflight_invalidations_survive_and_latest_camera_is_drawn() {
        let t = Temp::new();
        let a = app(&t);
        let w = view(&a, "main");
        let h = hello(&w);
        let host = a.state::<IpcHost>();
        let log = Arc::new(Mutex::new(ScheduledLog::default()));
        let (entered, arrived) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let mut backend = scheduled_backend(&log);
        backend.gate = Some((entered, wait));
        let handle = host.attach_surface("main", 1, backend, 64, 64).unwrap();
        let redraw = host.schedule_surface("main", handle, 64, 64).unwrap();
        arrived
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        let mut matrix = camera();
        for n in 0..100 {
            matrix[12] = n as f32;
            redraw.set_camera(matrix).unwrap();
        }
        let url = "tauri://localhost".parse().unwrap();
        let mut future = Box::pin(host.dispatch("main", &url, edit(&h, "1", "0", 1)));
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let _ = future.as_mut().poll(&mut context);
        drop(future);
        release.send(()).unwrap();
        inspect(&w, &h);
        assert!(redraw.wait_idle());
        {
            let entries = &log.lock().unwrap().frames;
            assert_eq!(entries.last().unwrap().1, 1);
            assert_eq!(entries.last().unwrap().2, matrix);
            assert!(entries.len() <= 3);
        }
        shutdown(&a);
    }
    #[test]
    fn scheduled_retry_budget_fairness_and_gpu_fault_replacement() {
        use threex_renderer::native::AcquireFailure;
        let t = Temp::new();
        let a = app(&t);
        let host = a.state::<IpcHost>();
        let log = Arc::new(Mutex::new(ScheduledLog::default()));
        let mut backend = scheduled_backend(&log);
        backend.error = Some(AcquireFailure::Timeout);
        let handle = host.attach_surface("main", 1, backend, 64, 64).unwrap();
        let redraw = host.schedule_surface("main", handle, 64, 64).unwrap();
        let panel_log = Arc::new(Mutex::new(ScheduledLog::default()));
        let panel = host
            .attach_surface("panel", 2, scheduled_backend(&panel_log), 64, 64)
            .unwrap();
        let other = host.schedule_surface("panel", panel, 64, 64).unwrap();
        assert!(other.wait_idle());
        assert!(redraw.wait_idle());
        assert_eq!(log.lock().unwrap().frames.len(), 3);
        assert_eq!(panel_log.lock().unwrap().frames.len(), 1);
        let attempts = redraw.status().unwrap().attempts;
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert_eq!(redraw.status().unwrap().attempts, attempts);
        redraw.request_redraw().unwrap();
        assert!(redraw.wait_idle());
        assert_eq!(log.lock().unwrap().frames.len(), 6);
        let mut backend = scheduled_backend(&log);
        backend.error = Some(AcquireFailure::DeviceLost);
        let failed = host.attach_surface("main", 1, backend, 64, 64).unwrap();
        let failed_view = host.schedule_surface("main", failed, 64, 64).unwrap();
        assert!(failed_view.wait_idle());
        assert_eq!(
            failed_view.status().unwrap().last_error.as_deref(),
            Some("device_lost")
        );
        let attempts = failed_view.status().unwrap().attempts;
        failed_view.request_redraw().unwrap();
        assert!(failed_view.wait_idle());
        assert_eq!(failed_view.status().unwrap().attempts, attempts);
        let recovered = host
            .attach_surface("main", 1, scheduled_backend(&log), 64, 64)
            .unwrap();
        let recovered_view = host.schedule_surface("main", recovered, 64, 64).unwrap();
        assert!(recovered_view.wait_idle());
        assert_eq!(redraw.close().unwrap_err().code, "surface_stale");
        assert_eq!(failed_view.close().unwrap_err().code, "surface_stale");
        assert_eq!(recovered_view.status().unwrap().presented, 1);
        shutdown(&a);
    }
    #[test]
    fn scheduled_suspend_resume_and_shutdown_under_backpressure_release_ownership() {
        let t = Temp::new();
        let a = app(&t);
        let host = a.state::<IpcHost>();
        let log = Arc::new(Mutex::new(ScheduledLog::default()));
        let handle = host
            .attach_surface("main", 1, scheduled_backend(&log), 64, 64)
            .unwrap();
        let redraw = host.schedule_surface("main", handle, 64, 64).unwrap();
        assert!(redraw.wait_idle());
        redraw.suspend().unwrap();
        assert!(redraw.wait_idle());
        redraw.resize(300, 200).unwrap();
        redraw.request_redraw().unwrap();
        assert!(redraw.wait_idle());
        assert_eq!(log.lock().unwrap().frames.len(), 1);
        redraw.resume().unwrap();
        assert!(redraw.wait_idle());
        assert_eq!(log.lock().unwrap().frames.len(), 2);
        assert_eq!(log.lock().unwrap().sizes.last(), Some(&(300, 200)));
        let permits = host.pending.clone().try_acquire_many_owned(64).unwrap();
        redraw.close().unwrap();
        shutdown(&a);
        assert_eq!(log.lock().unwrap().drops, 1);
        assert_eq!(redraw.request_redraw().unwrap_err().code, "stopped");
        drop(permits);
        let reopened = open(&t);
        tauri::async_runtime::block_on(reopened.shutdown()).unwrap();
    }
    #[test]
    fn real_native_window_binding_installs_without_document_or_scene_ownership() {
        let t = Temp::new();
        let a = app(&t);
        let host = a.state::<IpcHost>();
        let log = Arc::new(Mutex::new(ScheduledLog::default()));
        let handle = host
            .attach_surface("main", 1, scheduled_backend(&log), 64, 64)
            .unwrap();
        let window = tauri::WindowBuilder::new(&a, "viewport").build().unwrap();
        let redraw = host.bind_native_window("main", handle, &window).unwrap();
        assert!(redraw.wait_idle());
        assert!(log.lock().unwrap().frames.is_empty());
        // MockRuntime has size zero and deliberately does not dispatch OS callbacks. The callback adapter is exercised above.
        redraw
            .window_event(&tauri::WindowEvent::Resized(tauri::PhysicalSize {
                width: 100,
                height: 80,
            }))
            .unwrap();
        assert!(redraw.wait_idle());
        assert_eq!(log.lock().unwrap().frames.last().unwrap().1, 0);
        shutdown(&a);
    }
    #[test]
    fn trusted_asset_load_drives_only_its_target_redraw_without_document_edit() {
        use threex_core::{
            AssetId, asset_command,
            asset_pages::{
                AssetManifest, DESCRIPTOR_BYTES, HEADER_BYTES, PageDescriptor, page_hash,
            },
        };
        let t = Temp::new();
        let a = app(&t);
        let w = view(&a, "main");
        let h = hello(&w);
        let mut page = Vec::new();
        for v in [[0f32, 0., 0.], [1., 0., 0.], [0., 1., 0.]] {
            for x in v {
                page.extend(x.to_le_bytes());
            }
        }
        for i in [0u32, 1, 2] {
            page.extend(i.to_le_bytes());
        }
        let offset = (HEADER_BYTES + DESCRIPTOR_BYTES) as u64;
        let p = PageDescriptor {
            offset,
            length: page.len() as u64,
            vertices: 3,
            indices: 3,
            error: 0.,
            hash: page_hash(&page),
        };
        let (reference, mut bytes) = AssetManifest::encode(
            AssetId::from_bytes([4; 16]).unwrap(),
            1,
            [0., 0., 0., 1., 1., 0.],
            offset + p.length,
            &[p],
        )
        .unwrap();
        bytes.extend(page);
        let path = t.0.join("asset.bin");
        fs::write(&path, bytes).unwrap();
        let mut edit = edit(&h, "1", "0", 1);
        let commands = [command(1), asset_command(1, Some(reference)).to_vec()].concat();
        edit["action"]["commands"] = json!(commands);
        ipc(&w, edit).unwrap();
        let host = a.state::<IpcHost>();
        let main_log = Arc::new(Mutex::new(RenderLog::default()));
        let panel_log = Arc::new(Mutex::new(RenderLog::default()));
        let main = host
            .attach_surface("main", 1, Box::new(TestSurface(main_log.clone())), 64, 64)
            .unwrap();
        let panel = host
            .attach_surface("panel", 2, Box::new(TestSurface(panel_log.clone())), 64, 64)
            .unwrap();
        let main_redraw = host.schedule_surface("main", main, 64, 64).unwrap();
        let panel_redraw = host.schedule_surface("panel", panel, 64, 64).unwrap();
        assert!(main_redraw.wait_idle());
        assert!(panel_redraw.wait_idle());
        let before = main_log.lock().unwrap().frames.len();
        let other = panel_log.lock().unwrap().frames.len();
        assert_eq!(
            tauri::async_runtime::block_on(host.load_asset("panel", main, 1, path.clone(), false))
                .unwrap_err()
                .code,
            "surface_stale"
        );
        assert_eq!(
            tauri::async_runtime::block_on(host.load_asset(
                "unknown",
                main,
                1,
                path.clone(),
                false
            ))
            .unwrap_err()
            .code,
            "forbidden"
        );
        assert_eq!(
            tauri::async_runtime::block_on(host.load_asset("main", main, 1, path, false)).unwrap(),
            threex_storage::AssetLoadStatus::Queued
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while main_log.lock().unwrap().frames.len() == before {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(main_redraw.wait_idle());
        assert_eq!(
            main_log.lock().unwrap().asset_uploads,
            vec!["threex-document"]
        );
        assert_eq!(panel_log.lock().unwrap().frames.len(), other);
        let current = inspect(&w, &h);
        assert_eq!(current["state"]["revision"], "1");
        assert_eq!(current["state"]["successful_sync_groups"], "1");
        let frames = main_log.lock().unwrap().frames.len();
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(main_log.lock().unwrap().frames.len(), frames);
        shutdown(&a);
    }
}
