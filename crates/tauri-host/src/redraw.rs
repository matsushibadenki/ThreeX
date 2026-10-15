//! On-demand native scheduler. Contains only view intent, never Document/Scene/frame payloads.
use super::*;
use std::{
    sync::{Condvar, Weak},
    thread,
    time::{Duration, Instant},
};
use threex_renderer::native::{FrameStatus, SurfaceHandle, SurfaceOp, SurfaceRequest};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedrawStatus {
    pub busy: bool,
    pub pending: bool,
    pub presented: u64,
    pub attempts: u64,
    pub last_error: Option<String>,
    pub closed: bool,
}
struct View {
    handle: SurfaceHandle,
    size: (u32, u32),
    applied_size: (u32, u32),
    suspended: bool,
    applied_suspended: bool,
    closing: bool,
    dirty: bool,
    busy: bool,
    camera: [u32; 16],
    epoch: u64,
    retries: u8,
    deadline: Option<Instant>,
    presented: u64,
    attempts: u64,
    error: Option<String>,
}
impl View {
    fn invalidate(&mut self) {
        self.dirty = true;
        self.epoch = self.epoch.wrapping_add(1);
        self.retries = 0;
        self.deadline = None;
    }
    fn op(&self) -> Option<SurfaceOp> {
        if self.busy {
            return None;
        }
        if self.closing {
            return Some(SurfaceOp::Detach);
        }
        if self.error.is_some() {
            return None;
        }
        if self.suspended && !self.applied_suspended {
            return Some(SurfaceOp::Suspend);
        }
        if self.size != self.applied_size {
            return Some(SurfaceOp::Resize {
                width: self.size.0,
                height: self.size.1,
            });
        }
        if self.suspended != self.applied_suspended {
            return Some(SurfaceOp::Resume);
        }
        if self.dirty && !self.suspended && self.size.0 > 0 && self.size.1 > 0 {
            return Some(SurfaceOp::Draw {
                camera: self.camera,
            });
        }
        None
    }
    fn status(&self) -> RedrawStatus {
        RedrawStatus {
            busy: self.busy,
            pending: self.op().is_some() || self.busy,
            presented: self.presented,
            attempts: self.attempts,
            last_error: self.error.clone(),
            closed: self.closing,
        }
    }
}
#[derive(Default)]
struct State {
    views: BTreeMap<String, View>,
    stopped: bool,
    cursor: Option<String>,
}
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    ingress: Arc<Mutex<Ingress>>,
    client: WorkerClient,
    pending: Arc<tokio::sync::Semaphore>,
}
/// Weak handles may be retained by native event callbacks without keeping the app/worker alive.
#[derive(Clone)]
pub struct RedrawHandle {
    shared: Weak<Shared>,
    label: String,
    handle: SurfaceHandle,
}
#[derive(Clone)]
pub(crate) struct Wake(Weak<Shared>);
impl Wake {
    pub fn invalidate_all(&self) {
        if let Some(shared) = self.0.upgrade()
            && let Ok(mut state) = shared.state.lock()
            && !state.stopped
        {
            for view in state.views.values_mut().filter(|v| !v.closing) {
                view.invalidate();
            }
            shared.wake.notify_all();
        }
    }
    pub fn remove(&self, label: &str, handle: SurfaceHandle) {
        if let Some(shared) = self.0.upgrade()
            && let Ok(mut state) = shared.state.lock()
        {
            if state.views.get(label).is_some_and(|v| v.handle == handle) {
                state.views.remove(label);
            }
            shared.wake.notify_all();
        }
    }
}
pub(crate) struct Scheduler {
    shared: Arc<Shared>,
    join: Mutex<Option<thread::JoinHandle<()>>>,
}
impl Scheduler {
    pub fn start(
        ingress: Arc<Mutex<Ingress>>,
        client: WorkerClient,
        pending: Arc<tokio::sync::Semaphore>,
    ) -> Result<Self, std::io::Error> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            ingress,
            client,
            pending,
        });
        let driver = shared.clone();
        let join = thread::Builder::new()
            .name("threex-redraw".into())
            .spawn(move || driver.run())?;
        Ok(Self {
            shared,
            join: Mutex::new(Some(join)),
        })
    }
    pub fn handle(&self) -> Wake {
        Wake(Arc::downgrade(&self.shared))
    }
    pub fn stop(&self) -> Option<thread::JoinHandle<()>> {
        if let Ok(mut state) = self.shared.state.lock() {
            state.stopped = true;
            self.shared.wake.notify_all();
        }
        self.join.lock().ok().and_then(|mut j| j.take())
    }
    pub fn register(
        &self,
        label: &str,
        handle: SurfaceHandle,
        size: (u32, u32),
    ) -> Result<RedrawHandle, ErrorDto> {
        self.register_size(label, handle, size, size)
    }
    fn register_size(
        &self,
        label: &str,
        handle: SurfaceHandle,
        configured: (u32, u32),
        desired: (u32, u32),
    ) -> Result<RedrawHandle, ErrorDto> {
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| ErrorDto::host("stopped", Locale::En))?;
        if state.stopped {
            return Err(ErrorDto::host("stopped", Locale::En));
        }
        if !state.views.contains_key(label) && state.views.len() >= 16 {
            return Err(ErrorDto::host("surface_capacity", Locale::En));
        }
        if !state.views.get(label).is_some_and(|v| v.handle == handle) {
            let camera = state.views.get(label).map(|v| v.camera).unwrap_or_else(|| {
                let mut m = [0; 16];
                for i in [0, 5, 10, 15] {
                    m[i] = 1f32.to_bits();
                }
                m
            });
            state.views.insert(
                label.into(),
                View {
                    handle,
                    size: desired,
                    applied_size: configured,
                    suspended: false,
                    applied_suspended: false,
                    closing: false,
                    dirty: true,
                    busy: false,
                    camera,
                    epoch: 0,
                    retries: 0,
                    deadline: None,
                    presented: 0,
                    attempts: 0,
                    error: None,
                },
            );
        }
        if let Some(view) = state.views.get_mut(label)
            && view.size != desired
        {
            view.size = desired;
            if view.error.as_deref() == Some("surface_size") {
                view.error = None;
            }
            view.invalidate();
        }
        self.shared.wake.notify_all();
        Ok(RedrawHandle {
            shared: Arc::downgrade(&self.shared),
            label: label.into(),
            handle,
        })
    }
    pub fn replacement(&self, label: &str, handle: SurfaceHandle, size: (u32, u32)) {
        let registered = self
            .shared
            .state
            .lock()
            .is_ok_and(|s| s.views.contains_key(label));
        if registered {
            let _ = self.register(label, handle, size);
        }
    }
    pub fn camera(
        &self,
        label: &str,
        handle: SurfaceHandle,
        camera: [f32; 16],
    ) -> Result<bool, ErrorDto> {
        let exists = self
            .shared
            .state
            .lock()
            .is_ok_and(|s| s.views.get(label).is_some_and(|v| v.handle == handle));
        if !exists {
            return Ok(false);
        }
        RedrawHandle {
            shared: Arc::downgrade(&self.shared),
            label: label.into(),
            handle,
        }
        .set_camera(camera)?;
        Ok(true)
    }
}
impl Drop for Scheduler {
    fn drop(&mut self) {
        if let Some(join) = self.stop() {
            let _ = join.join();
        }
    }
}
impl RedrawHandle {
    fn update(&self, f: impl FnOnce(&mut View)) -> Result<(), ErrorDto> {
        let shared = self
            .shared
            .upgrade()
            .ok_or_else(|| ErrorDto::host("stopped", Locale::En))?;
        let mut state = shared
            .state
            .lock()
            .map_err(|_| ErrorDto::host("stopped", Locale::En))?;
        if state.stopped {
            return Err(ErrorDto::host("stopped", Locale::En));
        }
        let view = state
            .views
            .get_mut(&self.label)
            .filter(|v| v.handle == self.handle && !v.closing)
            .ok_or_else(|| ErrorDto::host("surface_stale", Locale::En))?;
        f(view);
        shared.wake.notify_all();
        Ok(())
    }
    pub fn request_redraw(&self) -> Result<(), ErrorDto> {
        self.update(View::invalidate)
    }
    pub fn set_camera(&self, camera: [f32; 16]) -> Result<(), ErrorDto> {
        if !camera.iter().all(|v| v.is_finite()) {
            return Err(ErrorDto::host("invalid_request", Locale::En));
        }
        self.update(|v| {
            v.camera = camera.map(f32::to_bits);
            v.invalidate();
        })?;
        if let Some(code) = self.status().and_then(|s| s.last_error) {
            return Err(ErrorDto::host(&code, Locale::En));
        }
        Ok(())
    }
    pub fn resize(&self, width: u32, height: u32) -> Result<(), ErrorDto> {
        self.update(|v| {
            if v.size != (width, height) {
                v.size = (width, height);
                if v.error.as_deref() == Some("surface_size") {
                    v.error = None;
                }
                v.invalidate();
            }
        })
    }
    pub fn suspend(&self) -> Result<(), ErrorDto> {
        self.update(|v| {
            if !v.suspended {
                v.suspended = true;
                v.invalidate();
            }
        })
    }
    pub fn resume(&self) -> Result<(), ErrorDto> {
        self.update(|v| {
            v.suspended = false;
            v.invalidate();
        })
    }
    pub fn close(&self) -> Result<(), ErrorDto> {
        self.update(|v| {
            v.closing = true;
            v.deadline = None;
        })
    }
    /// Sync event recording preserves ordering before any async response processing.
    pub fn window_event(&self, event: &tauri::WindowEvent) -> Result<(), ErrorDto> {
        match event {
            tauri::WindowEvent::Resized(s) => self.resize(s.width, s.height),
            tauri::WindowEvent::ScaleFactorChanged { new_inner_size, .. } => {
                self.resize(new_inner_size.width, new_inner_size.height)
            }
            tauri::WindowEvent::Destroyed => self.close(),
            tauri::WindowEvent::Focused(true) => self.request_redraw(),
            _ => Ok(()),
        }
    }
    pub fn status(&self) -> Option<RedrawStatus> {
        let shared = self.shared.upgrade()?;
        let state = shared.state.lock().ok()?;
        state
            .views
            .get(&self.label)
            .filter(|v| v.handle == self.handle)
            .map(View::status)
    }
    #[cfg(test)]
    pub(crate) fn wait_idle(&self) -> bool {
        let Some(shared) = self.shared.upgrade() else {
            return true;
        };
        let state = shared.state.lock().unwrap();
        let (state, timeout) = shared
            .wake
            .wait_timeout_while(state, Duration::from_secs(3), |s| {
                s.views
                    .get(&self.label)
                    .filter(|v| v.handle == self.handle)
                    .is_some_and(|v| v.busy || v.op().is_some())
            })
            .unwrap();
        !timeout.timed_out() || state.stopped
    }
}
struct Plan {
    label: String,
    handle: SurfaceHandle,
    op: SurfaceOp,
    epoch: u64,
}
impl Shared {
    fn next(&self) -> Option<Plan> {
        let mut state = self.state.lock().ok()?;
        loop {
            if state.stopped {
                return None;
            }
            let now = Instant::now();
            let mut candidates: Vec<_> = state
                .views
                .iter()
                .filter(|(_, v)| v.op().is_some() && v.deadline.is_none_or(|d| d <= now))
                .map(|(label, v)| (label.clone(), v.closing))
                .collect();
            // Round robin across windows; pending destruction takes priority.
            candidates.sort_by_key(|(label, closing)| {
                (
                    !closing,
                    state.cursor.as_ref().is_some_and(|last| label <= last),
                    label.clone(),
                )
            });
            if let Some((label, _)) = candidates.into_iter().next() {
                state.cursor = Some(label.clone());
                let v = state.views.get_mut(&label).unwrap();
                let op = v.op().unwrap();
                v.busy = true;
                if matches!(op, SurfaceOp::Draw { .. }) {
                    v.dirty = false;
                    v.attempts += 1;
                }
                return Some(Plan {
                    label,
                    handle: v.handle,
                    op,
                    epoch: v.epoch,
                });
            }
            let delay = state
                .views
                .values()
                .filter(|v| v.op().is_some())
                .filter_map(|v| v.deadline)
                .min()
                .map(|d| d.saturating_duration_since(now));
            state = if let Some(delay) = delay {
                self.wake.wait_timeout(state, delay).ok()?.0
            } else {
                self.wake.wait(state).ok()?
            };
        }
    }
    fn submit(
        &self,
        plan: &Plan,
    ) -> Result<std::sync::mpsc::Receiver<threex_storage::WorkerReply>, ErrorDto> {
        let mut ingress = self
            .ingress
            .lock()
            .map_err(|_| ErrorDto::host("stopped", Locale::En))?;
        if ingress.stopped {
            return Err(ErrorDto::host("stopped", Locale::En));
        }
        if !ingress
            .grants
            .get(&plan.label)
            .is_some_and(|g| g.surface == Some(plan.handle))
        {
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
                action: WorkerAction::Render(SurfaceRequest {
                    handle: plan.handle,
                    op: plan.op.clone(),
                }),
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
        Ok(rx)
    }
    fn run(&self) {
        while let Some(plan) = self.next() {
            let result = match self.pending.clone().try_acquire_owned() {
                Ok(_permit) => self
                    .submit(&plan)
                    .and_then(|rx| rx.recv().map_err(|_| ErrorDto::host("stopped", Locale::En)))
                    .and_then(|reply| reply.result.map_err(|e| ErrorDto::worker(e, Locale::En)))
                    .and_then(|outcome| match outcome {
                        WorkerOutcome::Rendered { result, .. } => {
                            result.map_err(|e| surface::failure(e, Locale::En))
                        }
                        _ => Err(ErrorDto::host("surface_unavailable", Locale::En)),
                    }),
                Err(_) => Err(ErrorDto::host("queue_full", Locale::En)),
            };
            self.complete(plan, result);
        }
    }
    fn complete(&self, plan: Plan, result: Result<FrameStatus, ErrorDto>) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let Some(v) = state
            .views
            .get_mut(&plan.label)
            .filter(|v| v.handle == plan.handle)
        else {
            return;
        };
        v.busy = false;
        match result {
            Ok(FrameStatus::Detached) => {
                state.views.remove(&plan.label);
            }
            Err(e) if e.code == "queue_full" => {
                if matches!(plan.op, SurfaceOp::Draw { .. }) {
                    v.dirty = true;
                }
                v.deadline = Some(Instant::now() + Duration::from_millis(8));
            }
            Err(e) => {
                v.error = Some(e.code);
                v.dirty = false;
                v.deadline = None;
                if v.closing && matches!(plan.op, SurfaceOp::Detach) {
                    state.views.remove(&plan.label);
                }
            }
            Ok(status) => match plan.op {
                SurfaceOp::Resize { width, height } => v.applied_size = (width, height),
                SurfaceOp::Suspend => v.applied_suspended = true,
                SurfaceOp::Resume => v.applied_suspended = false,
                SurfaceOp::Draw { .. } => {
                    if status == FrameStatus::Presented {
                        v.presented += 1;
                        v.retries = 0;
                        v.deadline = None;
                    } else if matches!(status, FrameStatus::Timeout | FrameStatus::Reconfigure)
                        && v.epoch == plan.epoch
                        && !v.closing
                    {
                        v.retries += 1;
                        if v.retries < 3 {
                            v.dirty = true;
                            v.deadline =
                                Some(Instant::now() + Duration::from_millis(8u64 << v.retries));
                        }
                    }
                }
                SurfaceOp::Detach => {}
            },
        }
        self.wake.notify_all();
    }
}
impl IpcHost {
    /// Opt in a trusted target; no GPU adapter is required for event/scheduler tests.
    pub fn schedule_surface(
        &self,
        label: &str,
        handle: SurfaceHandle,
        width: u32,
        height: u32,
    ) -> Result<RedrawHandle, ErrorDto> {
        let ingress = self
            .ingress
            .lock()
            .map_err(|_| ErrorDto::host("stopped", Locale::En))?;
        if ingress.stopped {
            return Err(ErrorDto::host("stopped", Locale::En));
        }
        if !ingress
            .grants
            .get(label)
            .is_some_and(|g| g.surface == Some(handle))
        {
            return Err(ErrorDto::host("surface_stale", Locale::En));
        }
        let configured = self
            .surfaces
            .size(handle)
            .map_err(|e| surface::failure(e, Locale::En))?;
        self.scheduler
            .register_size(label, handle, configured, (width, height))
    }
    /// Installs a real native callback with a weak, generation-checked scheduler handle.
    pub fn bind_native_window<R: Runtime>(
        &self,
        label: &str,
        handle: SurfaceHandle,
        window: &tauri::Window<R>,
    ) -> Result<RedrawHandle, ErrorDto> {
        let size = window
            .inner_size()
            .map_err(|_| ErrorDto::host("surface_unavailable", Locale::En))?;
        let redraw = self.schedule_surface(label, handle, size.width, size.height)?;
        install_callback(&redraw, |callback| window.on_window_event(callback));
        // Re-read after binding so preparation-time resize cannot leave stale dimensions.
        let size = window
            .inner_size()
            .map_err(|_| ErrorDto::host("surface_unavailable", Locale::En))?;
        redraw.resize(size.width, size.height)?;
        Ok(redraw)
    }
}

/// The same callback adapter is exercised with a deterministic native event source in tests.
pub(crate) fn install_callback(
    redraw: &RedrawHandle,
    install: impl FnOnce(Box<dyn Fn(&tauri::WindowEvent) + Send>),
) {
    let callback = redraw.clone();
    install(Box::new(move |event| {
        let _ = callback.window_event(event);
    }));
}
