pub mod geometry;
#[cfg(not(target_arch = "wasm32"))]
pub mod native;
use threex_core::{GeometryKind, Instance, Scene, SceneStamp};
struct MeshBuffers {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    count: u32,
}
use wgpu::util::DeviceExt;
pub struct Renderer {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    geometries: [MeshBuffers; 3],
    runs: geometry::GeometryRuns,
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
        let geometries =
            [GeometryKind::Box, GeometryKind::Plane, GeometryKind::Sphere].map(|kind| {
                let data = geometry::unit_geometry(kind);
                MeshBuffers {
                    vertices: buffer(
                        "shared geometry",
                        bytemuck::cast_slice(&data.vertices),
                        wgpu::BufferUsages::VERTEX,
                    ),
                    indices: buffer(
                        "shared indices",
                        bytemuck::cast_slice(&data.indices),
                        wgpu::BufferUsages::INDEX,
                    ),
                    count: data.indices.len() as u32,
                }
            });
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
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instances"),
            size: 80,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            device,
            queue,
            pipeline,
            geometries,
            runs: Default::default(),
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
        if !scene.is_current(self.stamp.as_ref()) {
            let data = scene.instances();
            let reallocated = data.len() > self.capacity;
            if reallocated {
                self.capacity = data.len().next_power_of_two();
                self.instances = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("instances"),
                    size: (self.capacity * std::mem::size_of::<Instance>()) as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            let updates = scene.instance_updates_since(self.stamp.as_ref(), reallocated);
            self.runs.sync(data, updates.ranges.clone());
            for range in updates.ranges {
                self.queue.write_buffer(
                    &self.instances,
                    (range.start * std::mem::size_of::<Instance>()) as u64,
                    bytemuck::cast_slice(&data[range]),
                );
            }
            self.count = data.len() as u32;
            self.stamp = Some(updates.stamp);
        }
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
        let mut encoder = self.device.create_command_encoder(&Default::default());
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
            pass.set_vertex_buffer(1, self.instances.slice(..));
            for (kind, range) in self.runs.draws() {
                let geometry = &self.geometries[kind as usize];
                pass.set_vertex_buffer(0, geometry.vertices.slice(..));
                pass.set_index_buffer(geometry.indices.slice(..), wgpu::IndexFormat::Uint16);
                pass.draw_indexed(0..geometry.count, 0, range);
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
            for (id, flags) in [(2u32, 7u32), (3, 11)] {
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
            assert_eq!(renderer.count, 3);
            assert_eq!(renderer.runs.draws().count(), 3);
            assert!(renderer.device.pop_error_scope().await.is_none());
        });
    }
}
