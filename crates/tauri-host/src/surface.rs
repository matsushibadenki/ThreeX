//! Trusted Rust lifecycle API. Native handles and GPU resources never enter JSON.
use super::*;
use tauri::Manager;
use threex_renderer::native::{
    FrameStatus, SurfaceBackend, SurfaceFailure, SurfaceHandle, SurfaceOp, SurfaceRequest,
    WgpuSurface,
};

pub(crate) fn failure(error: SurfaceFailure, locale: Locale) -> ErrorDto {
    ErrorDto::host(
        match error {
            SurfaceFailure::Stale => "surface_stale",
            SurfaceFailure::Capacity => "surface_capacity",
            SurfaceFailure::InvalidSize => "surface_size",
            SurfaceFailure::InvalidCamera => "invalid_request",
            SurfaceFailure::DeviceLost => "device_lost",
            SurfaceFailure::OutOfMemory => "gpu_out_of_memory",
            SurfaceFailure::Unavailable | SurfaceFailure::Poisoned => "surface_unavailable",
        },
        locale,
    )
}
pub(crate) fn frame_reply(
    result: Result<FrameStatus, SurfaceFailure>,
    state: threex_storage::WorkerState,
    locale: Locale,
) -> Result<IpcReply, ErrorDto> {
    Ok(IpcReply::Frame {
        status: match result.map_err(|e| failure(e, locale))? {
            FrameStatus::Presented => "presented",
            FrameStatus::Suspended => "suspended",
            FrameStatus::Timeout => "timeout",
            FrameStatus::Reconfigure => "reconfigure",
            FrameStatus::Detached => "detached",
            FrameStatus::Ready => "ready",
        },
        state: state.into(),
    })
}
impl IpcHost {
    /// Trusted Rust file selection only. Queues bounded I/O, returns admission (not file bytes).
    pub async fn load_asset(
        &self,
        label: &str,
        handle: SurfaceHandle,
        entity: u32,
        path: std::path::PathBuf,
        detail: bool,
    ) -> Result<threex_storage::AssetLoadStatus, ErrorDto> {
        let _permit = self
            .pending
            .clone()
            .try_acquire_owned()
            .map_err(|_| ErrorDto::host("queue_full", Locale::En))?;
        let receiver = {
            let mut ingress = self
                .ingress
                .lock()
                .map_err(|_| ErrorDto::host("stopped", Locale::En))?;
            if ingress.stopped {
                return Err(ErrorDto::host("stopped", Locale::En));
            }
            let grant = ingress
                .grants
                .get(label)
                .ok_or_else(|| ErrorDto::host("forbidden", Locale::En))?;
            if grant.surface != Some(handle) {
                return Err(ErrorDto::host("surface_stale", Locale::En));
            }
            let id = ingress.next;
            let next = id
                .checked_add(1)
                .ok_or_else(|| ErrorDto::host("stopped", Locale::En))?;
            let receiver = self
                .client
                .try_submit(WorkerRequest {
                    id,
                    action: WorkerAction::LoadAsset {
                        target: handle,
                        entity,
                        path,
                        detail,
                    },
                })
                .map_err(|e| {
                    let mut error = ErrorDto::worker(
                        threex_storage::WorkerFailure {
                            code: e.code,
                            current_revision: 0,
                        },
                        Locale::En,
                    );
                    error.observed_revision = None;
                    error
                })?;
            ingress.next = next;
            receiver
        };
        let reply = tauri::async_runtime::spawn_blocking(move || receiver.recv())
            .await
            .map_err(|_| ErrorDto::host("stopped", Locale::En))?
            .map_err(|_| ErrorDto::host("stopped", Locale::En))?;
        match reply.result.map_err(|e| ErrorDto::worker(e, Locale::En))? {
            WorkerOutcome::AssetLoad { status, .. } => Ok(status),
            _ => Err(ErrorDto::host("stopped", Locale::En)),
        }
    }
    /// Trusted native event-loop code installs a fully prepared backend. A failed candidate preserves the old target.
    pub fn attach_surface(
        &self,
        label: &str,
        id: u64,
        backend: Box<dyn SurfaceBackend>,
        width: u32,
        height: u32,
    ) -> Result<SurfaceHandle, ErrorDto> {
        let mut ingress = self
            .ingress
            .lock()
            .map_err(|_| ErrorDto::host("stopped", Locale::En))?;
        if ingress.stopped {
            return Err(ErrorDto::host("stopped", Locale::En));
        }
        let grant = ingress
            .grants
            .get_mut(label)
            .ok_or_else(|| ErrorDto::host("forbidden", Locale::En))?;
        // Each registered window has its own stable native target identity.
        if grant.surface.is_some_and(|old| old.id != id) {
            return Err(ErrorDto::host("surface_stale", Locale::En));
        }
        if ingress
            .grants
            .iter()
            .any(|(other, g)| other != label && g.surface.is_some_and(|h| h.id == id))
        {
            return Err(ErrorDto::host("forbidden", Locale::En));
        }
        let handle = self
            .surfaces
            .attach(id, backend, width, height)
            .map_err(|e| failure(e, Locale::En))?;
        ingress.grants.get_mut(label).unwrap().surface = Some(handle);
        self.scheduler.replacement(label, handle, (width, height));
        Ok(handle)
    }
    /// Creates the surface on the Tauri event loop. The owned Arc keeps the window alive for wgpu.
    /// The caller creates its native Window on that event loop, separate from panel WebViews.
    pub async fn attach_native_window<R: Runtime>(
        &self,
        window: Arc<tauri::Window<R>>,
        id: u64,
    ) -> Result<SurfaceHandle, ErrorDto> {
        let label = window.label().to_owned();
        self.attach_native_window_for(&label, window, id).await
    }
    /// Rust may associate a native viewport with its authorized WebView controller.
    /// The controller label is trusted host configuration, never an IPC argument.
    pub async fn attach_native_window_for<R: Runtime>(
        &self,
        label: &str,
        window: Arc<tauri::Window<R>>,
        id: u64,
    ) -> Result<SurfaceHandle, ErrorDto> {
        {
            let ingress = self
                .ingress
                .lock()
                .map_err(|_| ErrorDto::host("stopped", Locale::En))?;
            if ingress.stopped {
                return Err(ErrorDto::host("stopped", Locale::En));
            }
            if !ingress.grants.contains_key(label) {
                return Err(ErrorDto::host("forbidden", Locale::En));
            }
        }
        let app = window.app_handle().clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let native_window = window.clone();
        app.run_on_main_thread(move || {
            let result = (|| {
                let size = native_window
                    .inner_size()
                    .map_err(|_| SurfaceFailure::Unavailable)?;
                let instance = wgpu::Instance::default();
                let surface = instance
                    .create_surface(native_window)
                    .map_err(|_| SurfaceFailure::Unavailable)?;
                Ok((instance, surface, size))
            })();
            let _ = tx.send(result);
        })
        .map_err(|_| ErrorDto::host("surface_unavailable", Locale::En))?;
        let (instance, surface, size) = rx
            .await
            .map_err(|_| ErrorDto::host("stopped", Locale::En))?
            .map_err(|e| failure(e, Locale::En))?;
        let backend = WgpuSurface::prepare(&instance, surface)
            .await
            .map_err(|e| failure(e, Locale::En))?;
        let handle = self.attach_surface(label, id, Box::new(backend), size.width, size.height)?;
        self.bind_native_window(label, handle, &window)?;
        Ok(handle)
    }
    /// Enqueue lifecycle/frame work in the same order as edits, retaining the caller's generation.
    pub fn try_surface_request(
        &self,
        label: &str,
        handle: SurfaceHandle,
        op: SurfaceOp,
    ) -> Result<QueuedFrame, ErrorDto> {
        let permit = self
            .pending
            .clone()
            .try_acquire_owned()
            .map_err(|_| ErrorDto::host("queue_full", Locale::En))?;
        let receiver = {
            let mut ingress = self
                .ingress
                .lock()
                .map_err(|_| ErrorDto::host("stopped", Locale::En))?;
            if ingress.stopped {
                return Err(ErrorDto::host("stopped", Locale::En));
            }
            let grant = ingress
                .grants
                .get(label)
                .ok_or_else(|| ErrorDto::host("forbidden", Locale::En))?;
            if grant.surface != Some(handle) {
                return Err(ErrorDto::host("surface_stale", Locale::En));
            }
            let id = ingress.next;
            let next = id
                .checked_add(1)
                .ok_or_else(|| ErrorDto::host("stopped", Locale::En))?;
            let rx = self
                .client
                .try_submit(WorkerRequest {
                    id,
                    action: WorkerAction::Render(SurfaceRequest { handle, op }),
                })
                .map_err(|e| {
                    ErrorDto::host(
                        if e.code == threex_storage::WorkerCode::QueueFull {
                            "queue_full"
                        } else {
                            "stopped"
                        },
                        Locale::En,
                    )
                })?;
            ingress.next = next;
            rx
        };
        Ok(QueuedFrame { receiver, permit })
    }
    pub async fn surface_request(
        &self,
        label: &str,
        handle: SurfaceHandle,
        op: SurfaceOp,
    ) -> Result<IpcReply, ErrorDto> {
        self.try_surface_request(label, handle, op)?.wait().await
    }
    /// Forward physical-pixel size changes and destruction from the native window event loop.
    /// QueueFull is returned to the caller, which should retry the latest size/event rather than dropping it.
    pub fn try_surface_window_event(
        &self,
        label: &str,
        handle: SurfaceHandle,
        event: &tauri::WindowEvent,
    ) -> Result<Option<QueuedFrame>, ErrorDto> {
        let op = match event {
            tauri::WindowEvent::Resized(size) => SurfaceOp::Resize {
                width: size.width,
                height: size.height,
            },
            tauri::WindowEvent::ScaleFactorChanged { new_inner_size, .. } => SurfaceOp::Resize {
                width: new_inner_size.width,
                height: new_inner_size.height,
            },
            tauri::WindowEvent::Destroyed => SurfaceOp::Detach,
            _ => return Ok(None),
        };
        self.try_surface_request(label, handle, op).map(Some)
    }
    pub async fn surface_window_event(
        &self,
        label: &str,
        handle: SurfaceHandle,
        event: &tauri::WindowEvent,
    ) -> Result<Option<IpcReply>, ErrorDto> {
        match self.try_surface_window_event(label, handle, event)? {
            Some(frame) => frame.wait().await.map(Some),
            None => Ok(None),
        }
    }
}

/// Accepted native work. Dropping its response does not cancel the operation.
pub struct QueuedFrame {
    receiver: std::sync::mpsc::Receiver<threex_storage::WorkerReply>,
    permit: tokio::sync::OwnedSemaphorePermit,
}
impl QueuedFrame {
    pub async fn wait(self) -> Result<IpcReply, ErrorDto> {
        let Self { receiver, permit } = self;
        let reply = tauri::async_runtime::spawn_blocking(move || {
            let _permit = permit;
            receiver.recv()
        })
        .await
        .map_err(|_| ErrorDto::host("stopped", Locale::En))?
        .map_err(|_| ErrorDto::host("stopped", Locale::En))?;
        match reply.result.map_err(|e| ErrorDto::worker(e, Locale::En))? {
            WorkerOutcome::Rendered { result, state } => frame_reply(result, state, Locale::En),
            _ => Err(ErrorDto::host("surface_unavailable", Locale::En)),
        }
    }
}
