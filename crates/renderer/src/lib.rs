pub mod cpu_lod;
mod spatial;
use cpu_lod::{CpuLodPlanner, CpuLodPolicy, CpuLodStats, InvalidLodPolicy, LodView};
pub mod asset_pages;
pub mod batching;
mod gather;
pub mod geometry;
pub mod mesh_resources;
pub mod resource_handles;
use mesh_resources::{MeshBudget, MeshBudgetStatus, MeshCache};
use resource_handles::ResourceHandle;
#[cfg(not(target_arch = "wasm32"))]
pub mod native;
use threex_core::{GeometryKind, Instance, Scene, SceneStamp};
struct MeshBuffers {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    count: u32,
    format: wgpu::IndexFormat,
}
impl MeshBuffers {
    fn new(device: &wgpu::Device, data: geometry::MeshData) -> Self {
        let small = data.vertices.len() <= 65536;
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("shared geometry"),
            contents: bytemuck::cast_slice(&data.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let indices = if small {
            let values: Vec<u16> = data.indices.iter().map(|&i| i as u16).collect();
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("shared indices"),
                contents: bytemuck::cast_slice(&values),
                usage: wgpu::BufferUsages::INDEX,
            })
        } else {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("shared indices"),
                contents: bytemuck::cast_slice(&data.indices),
                usage: wgpu::BufferUsages::INDEX,
            })
        };
        Self {
            vertices,
            indices,
            count: data.indices.len() as u32,
            format: if small {
                wgpu::IndexFormat::Uint16
            } else {
                wgpu::IndexFormat::Uint32
            },
        }
    }
}
use asset_pages::{PageAdmission, PageFailure, PagePool, PagePoolStatus, PageRequest};
use std::collections::BTreeMap;
use threex_core::{
    AssetRef, Document,
    asset_pages::{AssetManifest, ValidatedPage},
};
use wgpu::util::DeviceExt;

pub struct Renderer {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    geometries: [MeshBuffers; 3],
    custom: MeshCache<MeshBuffers>,
    asset_pages: PagePool<MeshBuffers>,
    asset_manifests: BTreeMap<AssetRef, (AssetManifest, MeshBuffers)>,
    asset_document: Option<threex_core::DocumentId>,
    asset_views: asset_pages::AssetDrawLayout,
    cpu_lod: CpuLodPlanner,
    cpu_lod_policy: CpuLodPolicy,
    asset_page_budget: (u64, u32),
    asset_views_dirty: bool,

    mesh_budget: MeshBudget,
    mesh_budget_dirty: bool,
    batches: batching::GeometryBatches,
    banks: [wgpu::Buffer; 3],
    bank_capacities: [usize; 3],
    banked: bool,
    gather: Option<gather::Gather>,
    gather_plans: gather::PlanCache,
    instances: wgpu::Buffer,
    camera: wgpu::Buffer,
    bind: wgpu::BindGroup,
    capacity: usize,
    count: u32,
    stamp: Option<SceneStamp>,
    depth: Option<wgpu::Texture>,
    size: (u32, u32),
}
impl Renderer {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let buffer = |label, bytes: &[u8], usage| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytes,
                usage,
            })
        };
        let geometries = [GeometryKind::Box, GeometryKind::Plane, GeometryKind::Sphere]
            .map(|kind| MeshBuffers::new(&device, geometry::unit_geometry(kind)));
        let camera = buffer(
            "camera",
            bytemuck::cast_slice(&threex_core::identity()),
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera.as_entire_binding(),
            }],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cube"),
            source: wgpu::ShaderSource::Wgsl(include_str!("cube.wgsl").into()),
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline=device.create_render_pipeline(&wgpu::RenderPipelineDescriptor{label:None,layout:Some(&pl),vertex:wgpu::VertexState{module:&shader,entry_point:Some("vs"),compilation_options:Default::default(),buffers:&[wgpu::VertexBufferLayout{array_stride:12,step_mode:wgpu::VertexStepMode::Vertex,attributes:&wgpu::vertex_attr_array![0=>Float32x3]},wgpu::VertexBufferLayout{array_stride:80,step_mode:wgpu::VertexStepMode::Instance,attributes:&wgpu::vertex_attr_array![1=>Float32x4,2=>Float32x4,3=>Float32x4,4=>Float32x4,5=>Float32x4]}]},fragment:Some(wgpu::FragmentState{module:&shader,entry_point:Some("fs"),compilation_options:Default::default(),targets:&[Some(wgpu::ColorTargetState{format,blend:None,write_mask:wgpu::ColorWrites::ALL})]}),primitive:wgpu::PrimitiveState::default(),depth_stencil:Some(wgpu::DepthStencilState{format:wgpu::TextureFormat::Depth32Float,depth_write_enabled:true,depth_compare:wgpu::CompareFunction::Less,stencil:Default::default(),bias:Default::default()}),multisample:Default::default(),multiview:None,cache:None});
        let gather = gather::Gather::new(&device);
        let storage = if gather.is_some() {
            wgpu::BufferUsages::STORAGE
        } else {
            wgpu::BufferUsages::empty()
        };
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instances"),
            size: 80,
            usage: wgpu::BufferUsages::VERTEX
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST
                | storage,
            mapped_at_creation: false,
        });
        let banks = std::array::from_fn(|_| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("geometry instances"),
                size: 80,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST | storage,
                mapped_at_creation: false,
            })
        });
        Self {
            device,
            queue,
            pipeline,
            geometries,
            custom: Default::default(),
            asset_pages: PagePool::new(64 * 1024 * 1024, 128),
            asset_manifests: BTreeMap::new(),
            asset_document: None,
            asset_views: Default::default(),
            cpu_lod: Default::default(),
            cpu_lod_policy: Default::default(),
            asset_page_budget: (64 * 1024 * 1024, 128),
            asset_views_dirty: false,

            mesh_budget: MeshBudget::default(),
            mesh_budget_dirty: true,
            batches: Default::default(),
            banks,
            bank_capacities: [1; 3],
            banked: false,
            gather,
            gather_plans: Default::default(),
            instances,
            camera,
            bind,
            capacity: 1,
            count: 0,
            stamp: None,
            depth: None,
            size: (0, 0),
        }
    }
    /// Applied on the next nonzero render, independently of the Scene stamp.
    pub fn set_mesh_budget(&mut self, budget: MeshBudget) {
        if self.mesh_budget != budget {
            self.mesh_budget = budget;
            self.mesh_budget_dirty = true;
        }
    }
    /// Transient identity only; never store this handle in a Document or IPC receipt.
    pub fn custom_mesh_handle(&self, spec: threex_core::GeometrySpec) -> Option<ResourceHandle> {
        self.custom.handle(&spec)
    }
    pub fn is_custom_mesh_resident(&self, handle: ResourceHandle) -> bool {
        self.custom.resolve(handle).is_some()
    }
    pub fn mesh_budget_status(&self) -> &MeshBudgetStatus {
        self.custom.status()
    }
    /// Fixed page budget; changing it releases page buffers but retains bounded proxy metadata.
    pub fn set_asset_page_budget(&mut self, bytes: u64, pages: u32) {
        if self.asset_page_budget == (bytes, pages) {
            return;
        }
        self.asset_pages = PagePool::new(bytes, pages);
        self.asset_page_budget = (bytes, pages);
    }
    pub fn asset_page_status(&self) -> PagePoolStatus {
        self.asset_pages.status()
    }
    pub fn clear_assets(&mut self) {
        self.asset_pages.clear();
        self.asset_manifests.clear();
        self.asset_document = None;
        self.asset_views_dirty = true;
    }
    pub fn evict_asset_detail(&mut self, reference: AssetRef) {
        self.asset_pages.evict_detail(reference);
    }
    pub fn release_asset(&mut self, reference: AssetRef) {
        self.asset_pages.release_asset(reference);
        self.asset_manifests.remove(&reference);
        self.asset_views_dirty = true;
    }
    /// At most 128 manifests/proxies. Registering a valid root that cannot fit still enables bounds fallback.
    pub fn request_asset_page(
        &mut self,
        document: &Document,
        entity: u32,
        manifest: AssetManifest,
        level: u8,
    ) -> Result<PageAdmission, PageFailure> {
        let id = document
            .document_id()
            .ok_or(PageFailure::MissingReference)?;
        if self.asset_document.is_some_and(|old| old != id) {
            return Err(PageFailure::Stale);
        }
        let guard = document
            .asset_job_guard(entity)
            .ok_or(PageFailure::MissingReference)?;
        if guard.reference() != manifest.reference() {
            return Err(PageFailure::Stale);
        }
        if !self.asset_manifests.contains_key(&manifest.reference()) {
            if self.asset_manifests.len() >= 128 {
                return Err(PageFailure::Capacity);
            }
            let bounds = manifest.bounds();
            let mut mesh = geometry::unit_geometry(GeometryKind::Box);
            for v in &mut mesh.vertices {
                for axis in 0..3 {
                    v[axis] = if v[axis] < 0. {
                        bounds[axis]
                    } else {
                        bounds[axis + 3]
                    };
                }
            }
            let proxy = MeshBuffers::new(&self.device, mesh);
            self.asset_manifests
                .insert(manifest.reference(), (manifest.clone(), proxy));
            self.asset_document = Some(id);
            self.asset_views_dirty = true;
        }
        self.asset_pages.begin(document, entity, &manifest, level)
    }
    pub fn complete_asset_page(
        &mut self,
        document: &Document,
        request: PageRequest,
        result: Result<ValidatedPage, PageFailure>,
    ) -> Result<ResourceHandle, PageFailure> {
        let limit = self.device.limits().max_buffer_size;
        // Pre-validation page cap is 1 MiB; also observe device-specific limits at upload time.
        let result = result.and_then(|p| {
            if p.vertex_bytes().len() as u64 > limit || p.index_bytes().len() as u64 > limit {
                return Err(PageFailure::Capacity);
            }
            Ok(p)
        });
        self.asset_pages
            .complete(document, request, result, |page| MeshBuffers {
                vertices: self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("asset vertices"),
                        contents: page.vertex_bytes(),
                        usage: wgpu::BufferUsages::VERTEX,
                    }),
                indices: self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("asset indices"),
                        contents: page.index_bytes(),
                        usage: wgpu::BufferUsages::INDEX,
                    }),
                count: page.indices(),
                format: wgpu::IndexFormat::Uint32,
            })
    }
    pub fn set_cpu_lod_policy(&mut self, policy: CpuLodPolicy) -> Result<(), InvalidLodPolicy> {
        self.cpu_lod_policy = policy.validate()?;
        Ok(())
    }
    pub fn cpu_lod_status(&self) -> CpuLodStats {
        self.cpu_lod.status()
    }
    fn update_asset_views(&mut self, scene: &Scene) -> bool {
        let changed = self.asset_views.sync(
            scene,
            &self.batches,
            self.banked,
            self.asset_views_dirty,
            |r| self.asset_manifests.contains_key(&r),
        );
        self.asset_views_dirty = false;
        changed
    }
    pub fn render(
        &mut self,
        scene: &Scene,
        target: &wgpu::TextureView,
        width: u32,
        height: u32,
        vp: &[f32; 16],
    ) {
        if width == 0 || height == 0 {
            return;
        }
        let storage = if self.gather.is_some() {
            wgpu::BufferUsages::STORAGE
        } else {
            wgpu::BufferUsages::empty()
        };
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let mut changed_ranges = Vec::new();
        if !scene.is_current(self.stamp.as_ref()) {
            let data = scene.instances();
            let reallocated = data.len() > self.capacity;
            if reallocated {
                self.capacity = data.len().next_power_of_two();
                self.instances = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("instances"),
                    size: (self.capacity * std::mem::size_of::<Instance>()) as u64,
                    usage: wgpu::BufferUsages::VERTEX
                        | wgpu::BufferUsages::COPY_SRC
                        | wgpu::BufferUsages::COPY_DST
                        | storage,
                    mapped_at_creation: false,
                });
            }
            let updates = scene.instance_updates_since(self.stamp.as_ref(), reallocated);
            changed_ranges = updates.ranges;
            self.batches.sync(data, changed_ranges.iter().cloned());
            for range in &changed_ranges {
                self.queue.write_buffer(
                    &self.instances,
                    (range.start * std::mem::size_of::<Instance>()) as u64,
                    bytemuck::cast_slice(&data[range.clone()]),
                );
            }
            let banked = self.batches.requires_banks();
            if banked {
                for kind in 0..3 {
                    let len = self.batches.indices(kind).len();
                    let reallocated = len > self.bank_capacities[kind];
                    let full = reallocated || !self.banked;
                    if reallocated {
                        self.bank_capacities[kind] = len.next_power_of_two();
                        self.banks[kind] = self.device.create_buffer(&wgpu::BufferDescriptor {
                            label: Some("geometry instances"),
                            size: (self.bank_capacities[kind] * 80) as u64,
                            usage: wgpu::BufferUsages::VERTEX
                                | wgpu::BufferUsages::COPY_DST
                                | storage,
                            mapped_at_creation: false,
                        });
                    }
                    if let Some(gather) = &mut self.gather {
                        match self.gather_plans.prepare(
                            &self.device.limits(),
                            [self.instances.size(), self.banks[kind].size()],
                            &self.batches,
                            kind,
                            full,
                        ) {
                            gather::Prepared::Gather { plan, upload } => gather.encode(
                                &self.device,
                                &self.queue,
                                &mut encoder,
                                kind,
                                [&self.instances, &self.banks[kind]],
                                (plan, upload),
                            ),
                            gather::Prepared::Copies(copies) => {
                                for copy in copies {
                                    encoder.copy_buffer_to_buffer(
                                        &self.instances,
                                        (copy.source * 80) as u64,
                                        &self.banks[kind],
                                        (copy.destination * 80) as u64,
                                        (copy.count * 80) as u64,
                                    );
                                }
                            }
                        }
                    } else {
                        for copy in self.batches.copies(kind, full) {
                            encoder.copy_buffer_to_buffer(
                                &self.instances,
                                (copy.source * 80) as u64,
                                &self.banks[kind],
                                (copy.destination * 80) as u64,
                                (copy.count * 80) as u64,
                            );
                        }
                    }
                }
            }
            self.banked = banked;
            self.mesh_budget_dirty = true;
            self.count = data.len() as u32;
            self.stamp = Some(updates.stamp);
        }
        if self.mesh_budget_dirty {
            self.custom.sync(
                self.mesh_budget,
                self.device.limits().max_buffer_size,
                self.batches.geometry_draws().map(|(spec, _)| spec),
                |spec| MeshBuffers::new(&self.device, geometry::geometry(spec)),
            );
            self.mesh_budget_dirty = false;
        }
        let layout_changed = self.update_asset_views(scene);
        self.cpu_lod.sync(
            scene.instances(),
            &self.asset_views,
            layout_changed,
            &changed_ranges,
            LodView {
                vp: *vp,
                width,
                height,
                policy: self.cpu_lod_policy,
            },
            |r| &self.asset_manifests[&r].0,
        );
        self.queue
            .write_buffer(&self.camera, 0, bytemuck::cast_slice(vp));
        if self.size != (width, height) {
            self.depth = Some(self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("depth"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth32Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            }));
            self.size = (width, height);
        }
        let depth = self
            .depth
            .as_ref()
            .unwrap()
            .create_view(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("opaque"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.025,
                            g: 0.035,
                            b: 0.05,
                            a: 1.,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind, &[]);
            for (spec, range) in self.batches.geometry_draws() {
                let kind = spec.kind;
                pass.set_vertex_buffer(
                    1,
                    if self.banked {
                        self.banks[kind as usize].slice(..)
                    } else {
                        self.instances.slice(..)
                    },
                );
                let geometry = if spec.flags() & !15 == 0 {
                    &self.geometries[kind as usize]
                } else {
                    self.custom
                        .get(&spec)
                        .unwrap_or(&self.geometries[kind as usize])
                };
                pass.set_vertex_buffer(0, geometry.vertices.slice(..));
                pass.set_index_buffer(geometry.indices.slice(..), geometry.format);
                for range in
                    asset_pages::primitive_ranges(range, &self.asset_views.excluded[kind as usize])
                {
                    pass.draw_indexed(0..geometry.count, 0, range);
                }
            }
            pass.set_vertex_buffer(1, self.instances.slice(..));
            for draw in &self.cpu_lod.draws {
                let (manifest, proxy) = &self.asset_manifests[&draw.reference];
                let selected = self.asset_pages.select_hierarchy(
                    manifest,
                    self.cpu_lod.hierarchy(draw.reference),
                    draw.want_detail,
                );
                let mesh = selected
                    .handle
                    .and_then(|h| self.asset_pages.get(h))
                    .unwrap_or(proxy);
                pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                pass.set_index_buffer(mesh.indices.slice(..), mesh.format);
                pass.draw_indexed(0..mesh.count, 0, draw.range.clone());
            }
        }
        self.queue.submit([encoder.finish()]);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn shader_is_valid() {
        let module = naga::front::wgsl::parse_str(include_str!("cube.wgsl")).unwrap();
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .unwrap();
    }
    #[test]
    #[ignore = "Requires an accessible Metal/WebGPU adapter"]
    fn native_gpu_pipeline_and_draw() {
        pollster::block_on(async {
            let instance = wgpu::Instance::default();
            let adapter = instance
                .request_adapter(&Default::default())
                .await
                .expect("GPU adapter");
            let (device, queue) = adapter.request_device(&Default::default()).await.unwrap();
            device.push_error_scope(wgpu::ErrorFilter::Validation);
            let mut renderer = super::Renderer::new(device, queue, wgpu::TextureFormat::Rgba8Unorm);
            let target = renderer.device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: 32,
                    height: 32,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let mut scene = threex_core::Scene::default();
            let mut commands = [0u8; 128];
            commands[0..4].copy_from_slice(&1u32.to_le_bytes());
            commands[4..8].copy_from_slice(&1u32.to_le_bytes());
            commands[64..68].copy_from_slice(&2u32.to_le_bytes());
            commands[68..72].copy_from_slice(&1u32.to_le_bytes());
            for offset in [96, 100, 104, 108, 112, 116] {
                commands[offset..offset + 4].copy_from_slice(&1f32.to_le_bytes());
            }
            commands[120..124].copy_from_slice(&3u32.to_le_bytes());
            scene.apply(&commands).unwrap();
            let vp: [f32; 16] = bytemuck::cast(threex_core::identity());
            renderer.render(
                &scene,
                &target.create_view(&Default::default()),
                32,
                32,
                &vp,
            );
            assert_eq!(renderer.count, 1);
            for (id, flags) in [
                (2u32, 7u32),
                (3, 11),
                (4, 7 | (7 << 4) | (9 << 13)),
                (5, 11 | (256 << 4) | (256 << 13)),
            ] {
                let mut extra = commands;
                extra[4..8].copy_from_slice(&id.to_le_bytes());
                extra[68..72].copy_from_slice(&id.to_le_bytes());
                extra[120..124].copy_from_slice(&flags.to_le_bytes());
                scene.apply(&extra).unwrap();
            }
            renderer.render(
                &scene,
                &target.create_view(&Default::default()),
                32,
                32,
                &vp,
            );
            assert_eq!(renderer.count, 5);
            assert_eq!(renderer.mesh_budget_status().resident_meshes, 2);
            let largest =
                threex_core::GeometrySpec::new(threex_core::GeometryKind::Sphere, [256, 256])
                    .unwrap();
            assert_eq!(
                renderer.custom.get(&largest).unwrap().format,
                wgpu::IndexFormat::Uint32
            );
            let handle = renderer.custom_mesh_handle(largest).unwrap();
            assert!(renderer.is_custom_mesh_resident(handle));
            let revisions: Vec<_> = (0..3).map(|k| renderer.batches.revision(k)).collect();
            renderer.set_mesh_budget(super::MeshBudget {
                max_bytes: 0,
                max_meshes: 0,
            });
            renderer.render(
                &scene,
                &target.create_view(&Default::default()),
                32,
                32,
                &vp,
            );
            assert_eq!(renderer.mesh_budget_status().resident_meshes, 0);
            assert!(!renderer.is_custom_mesh_resident(handle));
            assert!(renderer.custom_mesh_handle(largest).is_none());
            assert_eq!(renderer.mesh_budget_status().fallback.len(), 2);
            assert_eq!(
                revisions,
                (0..3)
                    .map(|k| renderer.batches.revision(k))
                    .collect::<Vec<_>>()
            );
            renderer.set_mesh_budget(super::MeshBudget::default());
            renderer.render(
                &scene,
                &target.create_view(&Default::default()),
                32,
                32,
                &vp,
            );
            assert_eq!(renderer.mesh_budget_status().resident_meshes, 2);
            assert_ne!(renderer.custom_mesh_handle(largest), Some(handle));
            assert!(!renderer.is_custom_mesh_resident(handle));
            assert_eq!(renderer.batches.draws().count(), 3);
            let (manifest, payload) = crate::asset_pages::tests::fixture();
            let doc = crate::asset_pages::tests::document(manifest.reference());
            renderer.set_asset_page_budget(0, 0);
            assert_eq!(
                renderer.request_asset_page(&doc, 1, manifest.clone(), 0),
                Err(super::PageFailure::Capacity)
            );
            renderer.render(
                doc.scene(),
                &target.create_view(&Default::default()),
                32,
                32,
                &vp,
            );
            assert_eq!(renderer.asset_views.draws.len(), 1);
            assert_eq!(
                renderer.asset_pages.select(&manifest, true).lod,
                crate::asset_pages::Lod::BoundsProxy
            );
            let revisions: Vec<_> = (0..3).map(|k| renderer.batches.revision(k)).collect();
            renderer.set_asset_page_budget(48, 1);
            let super::PageAdmission::Load(request) = renderer
                .request_asset_page(&doc, 1, manifest.clone(), 0)
                .unwrap()
            else {
                panic!()
            };
            renderer
                .complete_asset_page(
                    &doc,
                    request,
                    Ok(manifest.decode_page(0, payload[0].clone()).unwrap()),
                )
                .unwrap();
            renderer.render(
                doc.scene(),
                &target.create_view(&Default::default()),
                32,
                32,
                &vp,
            );
            assert_eq!(
                renderer.asset_pages.select(&manifest, true).lod,
                crate::asset_pages::Lod::Root
            );
            assert_eq!(renderer.asset_page_status().resident_bytes, 48);
            assert_eq!(
                revisions,
                (0..3)
                    .map(|k| renderer.batches.revision(k))
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                renderer.request_asset_page(&doc, 1, manifest.clone(), 1),
                Err(super::PageFailure::Capacity)
            );
            renderer.set_asset_page_budget(168, 2);
            let super::PageAdmission::Load(request) = renderer
                .request_asset_page(&doc, 1, manifest.clone(), 0)
                .unwrap()
            else {
                panic!()
            };
            renderer
                .complete_asset_page(
                    &doc,
                    request,
                    Ok(manifest.decode_page(0, payload[0].clone()).unwrap()),
                )
                .unwrap();
            let super::PageAdmission::Load(request) = renderer
                .request_asset_page(&doc, 1, manifest.clone(), 1)
                .unwrap()
            else {
                panic!()
            };
            renderer
                .complete_asset_page(
                    &doc,
                    request,
                    Ok(manifest.decode_page(1, payload[1].clone()).unwrap()),
                )
                .unwrap();
            renderer.render(
                doc.scene(),
                &target.create_view(&Default::default()),
                32,
                32,
                &vp,
            );
            assert_eq!(
                renderer.asset_pages.select(&manifest, true).lod,
                crate::asset_pages::Lod::Detail
            );
            let mut offscreen = vp;
            offscreen[12] = 10.;
            renderer.render(
                doc.scene(),
                &target.create_view(&Default::default()),
                32,
                32,
                &offscreen,
            );
            assert_eq!(renderer.cpu_lod_status().culled, 1);
            assert!(renderer.cpu_lod.draws.is_empty());
            renderer.render(
                doc.scene(),
                &target.create_view(&Default::default()),
                32,
                32,
                &vp,
            );
            assert_eq!(renderer.cpu_lod_status().visible, 1);
            renderer.render(
                doc.scene(),
                &target.create_view(&Default::default()),
                32,
                32,
                &vp,
            );
            assert_eq!(renderer.cpu_lod_status().bounds_tests, 0);
            renderer.evict_asset_detail(manifest.reference());
            renderer.render(
                doc.scene(),
                &target.create_view(&Default::default()),
                32,
                32,
                &vp,
            );
            assert_eq!(
                renderer.asset_pages.select(&manifest, true).lod,
                crate::asset_pages::Lod::Root
            );
            renderer.clear_assets();
            renderer.render(
                doc.scene(),
                &target.create_view(&Default::default()),
                32,
                32,
                &vp,
            );
            assert!(renderer.asset_views.draws.is_empty());
            assert!(renderer.device.pop_error_scope().await.is_none());
        });
    }
}
