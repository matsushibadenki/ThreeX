//! Native targets remain in Rust. The worker borrows its authoritative Scene only for a frame.
use crate::Renderer;
use crate::asset_pages::{PageAdmission, PageFailure};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
};
use threex_core::{
    Document, Scene,
    asset_pages::{AssetManifest, ValidatedPage},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SurfaceHandle {
    pub id: u64,
    pub generation: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SurfaceOp {
    Resize {
        width: u32,
        height: u32,
    },
    Suspend,
    Resume,
    Detach,
    /// IEEE float bits keep the transport envelope exactly comparable, including rejection of NaN.
    Draw {
        camera: [u32; 16],
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurfaceRequest {
    pub handle: SurfaceHandle,
    pub op: SurfaceOp,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceFailure {
    Stale,
    Capacity,
    InvalidSize,
    InvalidCamera,
    Unavailable,
    DeviceLost,
    OutOfMemory,
    Poisoned,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameStatus {
    Presented,
    Suspended,
    Timeout,
    Reconfigure,
    Detached,
    Ready,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcquireFailure {
    Lost,
    Outdated,
    Timeout,
    DeviceLost,
    OutOfMemory,
    Other,
}
/// Small outcome after guarded page upload or fallback selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AssetDelivery {
    pub changed: bool,
    pub ready: bool,
    pub failure: Option<PageFailure>,
}
/// A backend owns the surface and renderer, never a Document or Scene clone.
pub trait SurfaceBackend: Send {
    fn upload_asset(
        &mut self,
        _document: &Document,
        _entity: u32,
        _manifest: AssetManifest,
        _level: u8,
        _page: Result<ValidatedPage, PageFailure>,
    ) -> Result<AssetDelivery, SurfaceFailure> {
        Err(SurfaceFailure::Unavailable)
    }

    fn configure(&mut self, width: u32, height: u32) -> Result<(), SurfaceFailure>;
    fn draw(&mut self, scene: &Scene, camera: &[f32; 16]) -> Result<(), AcquireFailure>;
}
struct Target {
    generation: u64,
    size: (u32, u32),
    suspended: bool,
    failed: Option<SurfaceFailure>,
    backend: Box<dyn SurfaceBackend>,
}
#[derive(Default)]
struct Targets {
    next: u64,
    entries: BTreeMap<u64, Target>,
}
/// Shared control registry; only the worker's ordered render command receives a Scene reference.
type SceneWake = Arc<dyn Fn() + Send + Sync>;
type AssetWake = Arc<dyn Fn(SurfaceHandle) + Send + Sync>;
#[derive(Clone, Default)]
pub struct NativeSurfaces(
    Arc<Mutex<Targets>>,
    Arc<Mutex<Option<SceneWake>>>,
    Arc<Mutex<Option<AssetWake>>>,
);
impl NativeSurfaces {
    pub fn set_asset_waker(&self, wake: AssetWake) {
        if let Ok(mut signal) = self.2.lock() {
            *signal = Some(wake);
        }
    }
    pub fn asset_changed(&self, handle: SurfaceHandle) {
        let wake = self.2.lock().ok().and_then(|s| s.clone());
        if let Some(wake) = wake {
            wake(handle);
        } else {
            self.scene_changed();
        }
    }
    pub fn upload_asset(
        &self,
        document: &Document,
        handle: SurfaceHandle,
        entity: u32,
        manifest: AssetManifest,
        level: u8,
        page: Result<ValidatedPage, PageFailure>,
    ) -> Result<AssetDelivery, SurfaceFailure> {
        let mut targets = self.0.lock().map_err(|_| SurfaceFailure::Poisoned)?;
        let target = targets
            .entries
            .get_mut(&handle.id)
            .filter(|t| t.generation == handle.generation)
            .ok_or(SurfaceFailure::Stale)?;
        if let Some(failure) = target.failed {
            return Err(failure);
        }
        let result = target
            .backend
            .upload_asset(document, entity, manifest, level, page);
        if let Err(error @ (SurfaceFailure::DeviceLost | SurfaceFailure::OutOfMemory)) = result {
            target.failed = Some(error);
        }
        result
    }

    pub fn size(&self, handle: SurfaceHandle) -> Result<(u32, u32), SurfaceFailure> {
        let targets = self.0.lock().map_err(|_| SurfaceFailure::Poisoned)?;
        targets
            .entries
            .get(&handle.id)
            .filter(|v| v.generation == handle.generation)
            .map(|v| v.size)
            .ok_or(SurfaceFailure::Stale)
    }
    pub fn set_scene_waker(&self, wake: SceneWake) {
        if let Ok(mut signal) = self.1.lock() {
            *signal = Some(wake);
        }
    }
    /// Called only after durable publication, without holding the GPU registry lock.
    pub fn scene_changed(&self) {
        let wake = self.1.lock().ok().and_then(|s| s.clone());
        if let Some(wake) = wake {
            wake();
        }
    }

    /// Prepare/configure before replacing the old target, preserving it on a failed attachment.
    /// Call on the platform's required native event-loop thread. IDs are trusted Rust view identities.
    pub fn attach(
        &self,
        id: u64,
        mut backend: Box<dyn SurfaceBackend>,
        width: u32,
        height: u32,
    ) -> Result<SurfaceHandle, SurfaceFailure> {
        let mut targets = self.0.lock().map_err(|_| SurfaceFailure::Poisoned)?;
        if id == 0 || (!targets.entries.contains_key(&id) && targets.entries.len() >= 16) {
            return Err(SurfaceFailure::Capacity);
        }
        let generation = targets
            .next
            .checked_add(1)
            .ok_or(SurfaceFailure::Capacity)?;
        if width > 0 && height > 0 {
            backend.configure(width, height)?;
        }
        targets.entries.insert(
            id,
            Target {
                generation,
                size: (width, height),
                suspended: false,
                failed: None,
                backend,
            },
        );
        targets.next = generation;
        Ok(SurfaceHandle { id, generation })
    }
    pub fn execute(
        &self,
        scene: &Scene,
        request: &SurfaceRequest,
    ) -> Result<FrameStatus, SurfaceFailure> {
        let mut targets = self.0.lock().map_err(|_| SurfaceFailure::Poisoned)?;
        let target = targets
            .entries
            .get_mut(&request.handle.id)
            .filter(|t| t.generation == request.handle.generation)
            .ok_or(SurfaceFailure::Stale)?;
        if matches!(request.op, SurfaceOp::Detach) {
            targets.entries.remove(&request.handle.id);
            return Ok(FrameStatus::Detached);
        }
        if let Some(failure) = target.failed {
            return Err(failure);
        }
        match request.op {
            SurfaceOp::Resize { width, height } => {
                if width > 0 && height > 0 && !target.suspended {
                    target.backend.configure(width, height)?;
                }
                target.size = (width, height);
                Ok(if width == 0 || height == 0 || target.suspended {
                    FrameStatus::Suspended
                } else {
                    FrameStatus::Ready
                })
            }
            SurfaceOp::Suspend => {
                target.suspended = true;
                Ok(FrameStatus::Suspended)
            }
            SurfaceOp::Resume => {
                if target.size.0 > 0 && target.size.1 > 0 {
                    target.backend.configure(target.size.0, target.size.1)?;
                }
                target.suspended = false;
                Ok(if target.size.0 == 0 || target.size.1 == 0 {
                    FrameStatus::Suspended
                } else {
                    FrameStatus::Ready
                })
            }
            SurfaceOp::Draw { camera } => {
                let camera = camera.map(f32::from_bits);
                if !camera.iter().all(|v| v.is_finite()) {
                    return Err(SurfaceFailure::InvalidCamera);
                }
                if target.suspended || target.size.0 == 0 || target.size.1 == 0 {
                    return Ok(FrameStatus::Suspended);
                }
                for attempt in 0..2 {
                    match target.backend.draw(scene, &camera) {
                        Ok(()) => return Ok(FrameStatus::Presented),
                        Err(AcquireFailure::Lost | AcquireFailure::Outdated) => {
                            if attempt == 1 {
                                return Ok(FrameStatus::Reconfigure);
                            }
                            target.backend.configure(target.size.0, target.size.1)?;
                        }
                        Err(AcquireFailure::Timeout) => return Ok(FrameStatus::Timeout),
                        Err(AcquireFailure::Other) => return Err(SurfaceFailure::Unavailable),
                        Err(AcquireFailure::DeviceLost) => {
                            target.failed = Some(SurfaceFailure::DeviceLost);
                            return Err(SurfaceFailure::DeviceLost);
                        }
                        Err(AcquireFailure::OutOfMemory) => {
                            target.failed = Some(SurfaceFailure::OutOfMemory);
                            return Err(SurfaceFailure::OutOfMemory);
                        }
                    }
                }
                unreachable!()
            }
            SurfaceOp::Detach => unreachable!(),
        }
    }
    /// Destroy only the matching generation; late close events cannot remove a replacement.
    pub fn detach(&self, handle: SurfaceHandle) -> Result<(), SurfaceFailure> {
        let mut targets = self.0.lock().map_err(|_| SurfaceFailure::Poisoned)?;
        if !targets
            .entries
            .get(&handle.id)
            .is_some_and(|t| t.generation == handle.generation)
        {
            return Err(SurfaceFailure::Stale);
        }
        targets.entries.remove(&handle.id);
        Ok(())
    }
    pub fn clear(&self) -> Result<(), SurfaceFailure> {
        self.0
            .lock()
            .map_err(|_| SurfaceFailure::Poisoned)?
            .entries
            .clear();
        Ok(())
    }
}
/// Actual wgpu native surface. Surface creation stays with the native event-loop caller.
pub struct WgpuSurface {
    surface: wgpu::Surface<'static>,
    renderer: Renderer,
    config: wgpu::SurfaceConfiguration,
    lost: Arc<AtomicBool>,
    fault: Arc<AtomicU8>,
}
impl WgpuSurface {
    pub async fn prepare(
        instance: &wgpu::Instance,
        surface: wgpu::Surface<'static>,
    ) -> Result<Self, SurfaceFailure> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .map_err(|_| SurfaceFailure::Unavailable)?;
        let capabilities = surface.get_capabilities(&adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .or_else(|| capabilities.formats.first().copied())
            .ok_or(SurfaceFailure::Unavailable)?;
        let alpha_mode = *capabilities
            .alpha_modes
            .first()
            .ok_or(SurfaceFailure::Unavailable)?;
        if !capabilities
            .usages
            .contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
            || !capabilities
                .present_modes
                .contains(&wgpu::PresentMode::Fifo)
        {
            return Err(SurfaceFailure::Unavailable);
        }
        let (device, queue) = adapter
            .request_device(&Default::default())
            .await
            .map_err(|_| SurfaceFailure::Unavailable)?;
        let lost = Arc::new(AtomicBool::new(false));
        let signal = lost.clone();
        device.set_device_lost_callback(move |_, _| {
            signal.store(true, Ordering::Release);
        });
        let fault = Arc::new(AtomicU8::new(0));
        let signal = fault.clone();
        device.on_uncaptured_error(Arc::new(move |error| {
            signal.store(
                if matches!(error, wgpu::Error::OutOfMemory { .. }) {
                    1
                } else {
                    2
                },
                Ordering::Release,
            );
        }));
        let renderer = Renderer::new(device, queue, format);
        if fault.load(Ordering::Acquire) != 0 {
            return Err(SurfaceFailure::Unavailable);
        }
        Ok(Self {
            surface,
            renderer,
            lost,
            fault,
            config: wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format,
                width: 1,
                height: 1,
                present_mode: wgpu::PresentMode::Fifo,
                desired_maximum_frame_latency: 2,
                alpha_mode,
                view_formats: vec![],
            },
        })
    }
}
impl SurfaceBackend for WgpuSurface {
    fn upload_asset(
        &mut self,
        document: &Document,
        entity: u32,
        manifest: AssetManifest,
        level: u8,
        page: Result<ValidatedPage, PageFailure>,
    ) -> Result<AssetDelivery, SurfaceFailure> {
        if self.lost.load(Ordering::Acquire) {
            return Err(SurfaceFailure::DeviceLost);
        }
        match self.fault.load(Ordering::Acquire) {
            0 => (),
            1 => return Err(SurfaceFailure::OutOfMemory),
            _ => return Err(SurfaceFailure::Unavailable),
        }
        let reference = manifest.reference();
        let before = self.renderer.asset_manifests.contains_key(&reference);
        let mut uploaded = false;
        let result = match self
            .renderer
            .request_asset_page(document, entity, manifest, level)
        {
            Ok(PageAdmission::Resident(_)) => Ok(()),
            Ok(PageAdmission::Load(request)) => self
                .renderer
                .complete_asset_page(document, request, page)
                .map(|_| {
                    uploaded = true;
                }),
            Err(e) => Err(e),
        };
        let registered = self.renderer.asset_manifests.contains_key(&reference);
        Ok(AssetDelivery {
            changed: uploaded || (!before && registered),
            ready: result.is_ok(),
            failure: result.err(),
        })
    }

    fn configure(&mut self, width: u32, height: u32) -> Result<(), SurfaceFailure> {
        if self.lost.load(Ordering::Acquire) {
            return Err(SurfaceFailure::DeviceLost);
        }
        match self.fault.load(Ordering::Acquire) {
            0 => {}
            1 => return Err(SurfaceFailure::OutOfMemory),
            _ => return Err(SurfaceFailure::Unavailable),
        }
        let limit = self.renderer.device.limits().max_texture_dimension_2d;
        if width == 0 || height == 0 || width > limit || height > limit {
            return Err(SurfaceFailure::InvalidSize);
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.renderer.device, &self.config);
        Ok(())
    }
    fn draw(&mut self, scene: &Scene, camera: &[f32; 16]) -> Result<(), AcquireFailure> {
        if self.lost.load(Ordering::Acquire) {
            return Err(AcquireFailure::DeviceLost);
        }
        match self.fault.load(Ordering::Acquire) {
            0 => {}
            1 => return Err(AcquireFailure::OutOfMemory),
            _ => return Err(AcquireFailure::Other),
        }
        let frame = self.surface.get_current_texture().map_err(|e| match e {
            wgpu::SurfaceError::Lost => AcquireFailure::Lost,
            wgpu::SurfaceError::Outdated => AcquireFailure::Outdated,
            wgpu::SurfaceError::Timeout => AcquireFailure::Timeout,
            wgpu::SurfaceError::OutOfMemory => AcquireFailure::OutOfMemory,
            wgpu::SurfaceError::Other => AcquireFailure::Other,
        })?;
        self.renderer.render(
            scene,
            &frame.texture.create_view(&Default::default()),
            self.config.width,
            self.config.height,
            camera,
        );
        match self.fault.load(Ordering::Acquire) {
            0 => {}
            1 => return Err(AcquireFailure::OutOfMemory),
            _ => return Err(AcquireFailure::Other),
        }
        let suboptimal = frame.suboptimal;
        frame.present();
        if suboptimal {
            self.surface.configure(&self.renderer.device, &self.config);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    #[derive(Default)]
    struct Log {
        sizes: Vec<(u32, u32)>,
        draws: usize,
        drops: usize,
        failures: VecDeque<AcquireFailure>,
        configure_fail: bool,
    }
    struct Fake(Arc<Mutex<Log>>);
    impl Drop for Fake {
        fn drop(&mut self) {
            self.0.lock().unwrap().drops += 1;
        }
    }
    impl SurfaceBackend for Fake {
        fn configure(&mut self, w: u32, h: u32) -> Result<(), SurfaceFailure> {
            let mut log = self.0.lock().unwrap();
            if log.configure_fail || w > 4096 || h > 4096 {
                return Err(SurfaceFailure::InvalidSize);
            }
            log.sizes.push((w, h));
            Ok(())
        }
        fn draw(&mut self, _: &Scene, _: &[f32; 16]) -> Result<(), AcquireFailure> {
            let mut log = self.0.lock().unwrap();
            log.draws += 1;
            log.failures.pop_front().map_or(Ok(()), Err)
        }
    }
    fn attach(manager: &NativeSurfaces, id: u64) -> (SurfaceHandle, Arc<Mutex<Log>>) {
        let log = Arc::new(Mutex::new(Log::default()));
        (
            manager
                .attach(id, Box::new(Fake(log.clone())), 64, 48)
                .unwrap(),
            log,
        )
    }
    fn run(
        manager: &NativeSurfaces,
        h: SurfaceHandle,
        op: SurfaceOp,
    ) -> Result<FrameStatus, SurfaceFailure> {
        manager.execute(&Scene::default(), &SurfaceRequest { handle: h, op })
    }
    fn draw() -> SurfaceOp {
        SurfaceOp::Draw {
            camera: bytemuck::cast::<_, [f32; 16]>(threex_core::identity()).map(f32::to_bits),
        }
    }
    #[test]
    fn zero_extent_suspend_resume_resize_and_detach_release_resources() {
        let m = NativeSurfaces::default();
        let (h, log) = attach(&m, 1);
        assert_eq!(
            run(
                &m,
                h,
                SurfaceOp::Resize {
                    width: 0,
                    height: 48
                }
            ),
            Ok(FrameStatus::Suspended)
        );
        assert_eq!(run(&m, h, draw()), Ok(FrameStatus::Suspended));
        assert_eq!(log.lock().unwrap().draws, 0);
        run(
            &m,
            h,
            SurfaceOp::Resize {
                width: 100,
                height: 60,
            },
        )
        .unwrap();
        run(&m, h, SurfaceOp::Suspend).unwrap();
        run(
            &m,
            h,
            SurfaceOp::Resize {
                width: 120,
                height: 80,
            },
        )
        .unwrap();
        assert_eq!(run(&m, h, draw()), Ok(FrameStatus::Suspended));
        assert_eq!(run(&m, h, SurfaceOp::Resume), Ok(FrameStatus::Ready));
        assert_eq!(run(&m, h, draw()), Ok(FrameStatus::Presented));
        assert_eq!(
            log.lock().unwrap().sizes,
            vec![(64, 48), (100, 60), (120, 80)]
        );
        assert_eq!(run(&m, h, SurfaceOp::Detach), Ok(FrameStatus::Detached));
        assert_eq!(log.lock().unwrap().drops, 1);
        assert_eq!(run(&m, h, draw()), Err(SurfaceFailure::Stale));
    }
    #[test]
    fn replacement_is_transactional_and_stale_events_cannot_affect_new_surface() {
        let m = NativeSurfaces::default();
        let (old, log) = attach(&m, 1);
        let bad = Arc::new(Mutex::new(Log {
            configure_fail: true,
            ..Default::default()
        }));
        assert_eq!(
            m.attach(1, Box::new(Fake(bad.clone())), 20, 20),
            Err(SurfaceFailure::InvalidSize)
        );
        assert_eq!(run(&m, old, draw()), Ok(FrameStatus::Presented));
        assert_eq!(log.lock().unwrap().drops, 0);
        assert_eq!(bad.lock().unwrap().drops, 1);
        let (new, next) = attach(&m, 1);
        assert!(new.generation > old.generation);
        assert_eq!(log.lock().unwrap().drops, 1);
        for op in [
            SurfaceOp::Detach,
            SurfaceOp::Resize {
                width: 1,
                height: 1,
            },
            SurfaceOp::Suspend,
            draw(),
        ] {
            assert_eq!(run(&m, old, op), Err(SurfaceFailure::Stale));
        }
        assert_eq!(run(&m, new, draw()), Ok(FrameStatus::Presented));
        m.clear().unwrap();
        assert_eq!(next.lock().unwrap().drops, 1);
    }
    #[test]
    fn lost_outdated_retry_is_bounded_and_timeout_does_not_recreate() {
        let m = NativeSurfaces::default();
        let (h, log) = attach(&m, 1);
        log.lock().unwrap().failures.push_back(AcquireFailure::Lost);
        assert_eq!(run(&m, h, draw()), Ok(FrameStatus::Presented));
        assert_eq!(log.lock().unwrap().draws, 2);
        assert_eq!(log.lock().unwrap().sizes.len(), 2);
        log.lock()
            .unwrap()
            .failures
            .extend([AcquireFailure::Outdated, AcquireFailure::Lost]);
        assert_eq!(run(&m, h, draw()), Ok(FrameStatus::Reconfigure));
        assert_eq!(log.lock().unwrap().draws, 4);
        assert_eq!(log.lock().unwrap().sizes.len(), 3);
        log.lock()
            .unwrap()
            .failures
            .push_back(AcquireFailure::Timeout);
        assert_eq!(run(&m, h, draw()), Ok(FrameStatus::Timeout));
        assert_eq!(log.lock().unwrap().sizes.len(), 3);
        assert_eq!(run(&m, h, draw()), Ok(FrameStatus::Presented));
    }
    #[test]
    fn device_loss_and_oom_are_terminal_until_explicit_replacement() {
        for (error, expected) in [
            (AcquireFailure::DeviceLost, SurfaceFailure::DeviceLost),
            (AcquireFailure::OutOfMemory, SurfaceFailure::OutOfMemory),
        ] {
            let m = NativeSurfaces::default();
            let (h, log) = attach(&m, 1);
            log.lock().unwrap().failures.push_back(error);
            assert_eq!(run(&m, h, draw()), Err(expected));
            assert_eq!(run(&m, h, draw()), Err(expected));
            assert_eq!(run(&m, h, SurfaceOp::Resume), Err(expected));
            assert_eq!(log.lock().unwrap().draws, 1);
            let (new, _) = attach(&m, 1);
            assert_eq!(run(&m, new, draw()), Ok(FrameStatus::Presented));
        }
    }
    #[test]
    fn bounded_registry_invalid_size_and_camera_do_not_draw_or_destroy_old_target() {
        let m = NativeSurfaces::default();
        let (h, log) = attach(&m, 1);
        assert_eq!(
            run(
                &m,
                h,
                SurfaceOp::Resize {
                    width: 9999,
                    height: 1
                }
            ),
            Err(SurfaceFailure::InvalidSize)
        );
        assert_eq!(
            run(
                &m,
                h,
                SurfaceOp::Draw {
                    camera: [f32::NAN.to_bits(); 16]
                }
            ),
            Err(SurfaceFailure::InvalidCamera)
        );
        assert_eq!(log.lock().unwrap().draws, 0);
        assert_eq!(run(&m, h, draw()), Ok(FrameStatus::Presented));
        for id in 2..=16 {
            attach(&m, id);
        }
        let log = Arc::new(Mutex::new(Log::default()));
        assert_eq!(
            m.attach(17, Box::new(Fake(log)), 1, 1),
            Err(SurfaceFailure::Capacity)
        );
        let (new, _) = attach(&m, 1);
        assert_ne!(h, new);
    }
}
