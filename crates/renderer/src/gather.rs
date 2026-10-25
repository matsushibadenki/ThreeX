//! Optional compute gather; unsupported sizes/devices keep the buffer-copy path.
pub(crate) struct Gather {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    tasks: [Option<wgpu::Buffer>; 3],
    capacities: [usize; 3],
}
pub(crate) fn supported(limits: &wgpu::Limits) -> bool {
    limits.max_bind_groups >= 1
        && limits.max_bindings_per_bind_group >= 3
        && limits.max_storage_buffers_per_shader_stage >= 3
        && limits.max_compute_invocations_per_workgroup >= 64
        && limits.max_compute_workgroup_size_x >= 64
        && limits.max_compute_workgroups_per_dimension > 0
        && limits.max_storage_buffer_binding_size >= 80
}
pub(crate) fn fits(limits: &wgpu::Limits, source: u64, destination: u64, tasks: usize) -> bool {
    supported(limits)
        && source <= limits.max_buffer_size
        && destination <= limits.max_buffer_size
        && tasks as u64 * 8 <= limits.max_buffer_size
        && source <= limits.max_storage_buffer_binding_size as u64
        && destination <= limits.max_storage_buffer_binding_size as u64
        && tasks as u64 * 8 <= limits.max_storage_buffer_binding_size as u64
        && (tasks as u64).div_ceil(64) <= limits.max_compute_workgroups_per_dimension as u64
}
#[derive(Debug)]
pub(crate) struct Dispatch {
    pub source: std::ops::Range<u64>,
    pub destination: std::ops::Range<u64>,
    pub tasks: std::ops::Range<usize>,
}
#[derive(Debug)]
pub(crate) struct Partition {
    pub tasks: Vec<[u32; 2]>,
    pub dispatches: Vec<Dispatch>,
}
fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}
fn window_slots(limits: &wgpu::Limits) -> u64 {
    let alignment = u64::from(limits.min_storage_buffer_offset_alignment).max(1);
    let unit = alignment / gcd(alignment, 80);
    u64::from(limits.max_storage_buffer_binding_size) / 80 / unit * unit
}
pub(crate) fn can_partition(limits: &wgpu::Limits, source: u64, destination: u64) -> bool {
    supported(limits)
        && source <= limits.max_buffer_size
        && destination <= limits.max_buffer_size
        && source >= 80
        && destination >= 80
        && window_slots(limits) > 0
}
/// Rebase whole instances into aligned binding windows, then split task/dispatch limits.
/// A single task upload serves every dispatch; later queue writes must not overwrite earlier chunks.
pub(crate) fn partition(
    limits: &wgpu::Limits,
    source: u64,
    destination: u64,
    tasks: Vec<[u32; 2]>,
) -> Option<Partition> {
    if !can_partition(limits, source, destination) || tasks.is_empty() {
        return None;
    }
    if tasks
        .iter()
        .any(|&[s, d]| (u64::from(s) + 1) * 80 > source || (u64::from(d) + 1) * 80 > destination)
    {
        return None;
    }
    if fits(limits, source, destination, tasks.len()) {
        let count = tasks.len();
        return Some(Partition {
            tasks,
            dispatches: vec![Dispatch {
                source: 0..source,
                destination: 0..destination,
                tasks: 0..count,
            }],
        });
    }
    let slots = window_slots(limits);
    let max_tasks = (u64::from(limits.max_storage_buffer_binding_size) / 8)
        .min(u64::from(limits.max_compute_workgroups_per_dimension) * 64)
        as usize;
    if max_tasks == 0 {
        return None;
    }
    let alignment = u64::from(limits.min_storage_buffer_offset_alignment).max(1);
    let task_unit = alignment / gcd(alignment, 8);
    let mut lookup = std::collections::HashMap::new();
    type WindowGroup = ((u64, u64), Vec<[u32; 2]>);
    let mut groups: Vec<WindowGroup> = Vec::new();
    let mut last: Option<((u64, u64), usize)> = None;
    for [source, destination] in tasks {
        let s = u64::from(source);
        let d = u64::from(destination);
        // Packed slots usually stay within the previous windows: avoid repeated division/hash.
        let (key, index, local_source, local_destination) = if let Some((key, index)) = last
            && s >= key.0 * slots
            && s < (key.0 + 1) * slots
            && d >= key.1 * slots
            && d < (key.1 + 1) * slots
        {
            (key, index, s - key.0 * slots, d - key.1 * slots)
        } else {
            let key = (s / slots, d / slots);
            let index = *lookup.entry(key).or_insert_with(|| {
                let index = groups.len();
                groups.push((key, Vec::new()));
                index
            });
            (key, index, s % slots, d % slots)
        };
        groups[index]
            .1
            .push([local_source as u32, local_destination as u32]);
        last = Some((key, index));
    }
    groups.sort_unstable_by_key(|(key, _)| *key);
    let mut result = Partition {
        tasks: Vec::new(),
        dispatches: Vec::new(),
    };
    for ((s, d), tasks) in groups {
        for chunk in tasks.chunks(max_tasks) {
            let start = (result.tasks.len() as u64).div_ceil(task_unit) * task_unit;
            let end = start.checked_add(chunk.len() as u64)?;
            if end.checked_mul(8)? > limits.max_buffer_size {
                return None;
            }
            result.tasks.resize(start as usize, [0; 2]);
            result.tasks.extend_from_slice(chunk);
            result.dispatches.push(Dispatch {
                source: s * slots * 80..((s + 1) * slots * 80).min(source),
                destination: d * slots * 80..((d + 1) * slots * 80).min(destination),
                tasks: start as usize..end as usize,
            });
        }
    }
    Some(result)
}
#[derive(PartialEq, Eq)]
struct PlanKey {
    revision: u64,
    source: u64,
    destination: u64,
    limits: wgpu::Limits,
}
enum FullPlan {
    Copies(Vec<crate::batching::InstanceCopy>),
    Gather(Partition),
    Fallback,
}
struct CachedPlan {
    key: PlanKey,
    plan: FullPlan,
}
#[derive(Default)]
pub(crate) struct PlanCache {
    banks: [Option<CachedPlan>; 3],
    resident: [bool; 3],
    partial: Option<Partition>,
}
pub(crate) enum Prepared<'a> {
    Copies(Vec<crate::batching::InstanceCopy>),
    Gather { plan: &'a Partition, upload: bool },
}
impl PlanCache {
    pub fn prepare(
        &mut self,
        limits: &wgpu::Limits,
        sizes: [u64; 2],
        batches: &crate::batching::GeometryBatches,
        kind: usize,
        full: bool,
    ) -> Prepared<'_> {
        use crate::batching::TransferPlan;
        let [source, destination] = sizes;
        if !can_partition(limits, source, destination) {
            return Prepared::Copies(batches.copies(kind, full));
        }
        if batches.whole_bank(kind, full) {
            let key = PlanKey {
                revision: batches.revision(kind),
                source,
                destination,
                limits: limits.clone(),
            };
            if self.banks[kind]
                .as_ref()
                .is_none_or(|cached| cached.key != key)
            {
                let plan = match batches.transfers(kind, true, true) {
                    TransferPlan::Copies(copies) => FullPlan::Copies(copies),
                    TransferPlan::Gather(tasks) => {
                        match partition(limits, source, destination, tasks) {
                            Some(plan) => FullPlan::Gather(plan),
                            None => FullPlan::Fallback,
                        }
                    }
                };
                self.banks[kind] = Some(CachedPlan { key, plan });
                self.resident[kind] = false;
            }
            match &self.banks[kind].as_ref().unwrap().plan {
                FullPlan::Copies(copies) => Prepared::Copies(copies.clone()),
                FullPlan::Fallback => Prepared::Copies(batches.copies(kind, true)),
                FullPlan::Gather(plan) => {
                    let upload = !self.resident[kind];
                    self.resident[kind] = true;
                    Prepared::Gather { plan, upload }
                }
            }
        } else {
            match batches.transfers(kind, full, true) {
                TransferPlan::Copies(copies) => Prepared::Copies(copies),
                TransferPlan::Gather(tasks) => {
                    self.partial = partition(limits, source, destination, tasks);
                    if let Some(plan) = &self.partial {
                        // Partial gather overwrites this bank's GPU index buffer, but not its CPU full plan.
                        self.resident[kind] = false;
                        Prepared::Gather { plan, upload: true }
                    } else {
                        Prepared::Copies(batches.copies(kind, full))
                    }
                }
            }
        }
    }
}
impl Gather {
    pub fn new(device: &wgpu::Device) -> Option<Self> {
        if !supported(&device.limits()) {
            return None;
        }
        let entries = std::array::from_fn::<_, 3, _>(|binding| wgpu::BindGroupLayoutEntry {
            binding: binding as u32,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage {
                    read_only: binding != 1,
                },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("instance gather"),
            entries: &entries,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("instance gather"),
            source: wgpu::ShaderSource::Wgsl(include_str!("gather.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("instance gather"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("instance gather"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("gather"),
            compilation_options: Default::default(),
            cache: None,
        });
        Some(Self {
            pipeline,
            layout,
            tasks: Default::default(),
            capacities: [0; 3],
        })
    }
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        kind: usize,
        buffers: [&wgpu::Buffer; 2],
        input: (&Partition, bool),
    ) {
        let (plan, upload) = input;
        let [source, destination] = buffers;
        let count = plan.tasks.len();
        assert!(count > 0);
        let reallocated = count > self.capacities[kind];
        if reallocated {
            self.capacities[kind] = count
                .next_power_of_two()
                .min((device.limits().max_buffer_size / 8) as usize);
            self.tasks[kind] = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("instance gather tasks"),
                size: (self.capacities[kind] * 8) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let buffer = self.tasks[kind].as_ref().unwrap();
        if upload || reallocated {
            queue.write_buffer(buffer, 0, bytemuck::cast_slice(&plan.tasks));
        }
        let binding = |buffer, range: std::ops::Range<u64>| {
            wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer,
                offset: range.start,
                size: std::num::NonZeroU64::new(range.end - range.start),
            })
        };
        for dispatch in &plan.dispatches {
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("instance gather window"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: binding(source, dispatch.source.clone()),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: binding(destination, dispatch.destination.clone()),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: binding(
                            buffer,
                            dispatch.tasks.start as u64 * 8..dispatch.tasks.end as u64 * 8,
                        ),
                    },
                ],
            });
            // Separate passes avoid overlapping read/write storage usage scopes between windows.
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("instance gather window"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups((dispatch.tasks.len() as u32).div_ceil(64), 1, 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gather_shader_validates_and_matches_raw_instance_layout() {
        let module = naga::front::wgsl::parse_str(include_str!("gather.wgsl")).unwrap();
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .unwrap();
        assert_eq!(module.entry_points[0].workgroup_size, [64, 1, 1]);
        let strides: Vec<_> = module
            .types
            .iter()
            .filter_map(|(_, ty)| match ty.inner {
                naga::TypeInner::Array { stride, .. } => Some(stride),
                _ => None,
            })
            .collect();
        assert!(strides.contains(&80));
        assert!(strides.contains(&8));
        assert_eq!(std::mem::size_of::<threex_core::Instance>(), 80);
    }
    #[test]
    fn limits_fail_closed_without_changing_copy_fallback() {
        let mut limits = wgpu::Limits::default();
        assert!(supported(&limits));
        let max = limits.max_storage_buffer_binding_size as u64;
        assert!(fits(&limits, max, max, 1));
        assert!(!fits(&limits, max + 80, 80, 1));
        assert!(!fits(&limits, 80, max + 80, 1));
        let tasks = limits.max_compute_workgroups_per_dimension as usize * 64;
        assert!(fits(&limits, 80, 80, tasks));
        assert!(!fits(&limits, 80, 80, tasks + 1));
        assert!(!fits(&limits, 80, 80, max as usize / 8 + 1));
        limits.max_storage_buffers_per_shader_stage = 2;
        assert!(!supported(&limits));
        assert!(!fits(&limits, 80, 80, 1));
        limits = wgpu::Limits::default();
        limits.max_compute_workgroup_size_x = 32;
        assert!(!supported(&limits));
        limits = wgpu::Limits::default();
        limits.max_buffer_size = 80;
        assert!(!fits(&limits, 160, 80, 1));
    }
    fn check_plan(
        limits: &wgpu::Limits,
        source: u64,
        destination: u64,
        tasks: Vec<[u32; 2]>,
    ) -> Partition {
        let mut expected = tasks.clone();
        expected.sort_unstable();
        let plan = partition(limits, source, destination, tasks).unwrap();
        let mut actual = Vec::new();
        let mut destinations = std::collections::HashSet::new();
        for dispatch in &plan.dispatches {
            for range in [&dispatch.source, &dispatch.destination] {
                assert_eq!(
                    range.start % u64::from(limits.min_storage_buffer_offset_alignment),
                    0
                );
                assert_eq!(range.start % 80, 0);
                assert!(range.end > range.start);
                assert!(
                    range.end - range.start <= u64::from(limits.max_storage_buffer_binding_size)
                );
            }
            assert!(dispatch.source.end <= source && dispatch.destination.end <= destination);
            assert_eq!(
                dispatch.tasks.start * 8 % limits.min_storage_buffer_offset_alignment as usize,
                0
            );
            assert!(dispatch.tasks.len() * 8 <= limits.max_storage_buffer_binding_size as usize);
            assert!(
                (dispatch.tasks.len() as u32).div_ceil(64)
                    <= limits.max_compute_workgroups_per_dimension
            );
            for &[src, dst] in &plan.tasks[dispatch.tasks.clone()] {
                assert!((u64::from(src) + 1) * 80 <= dispatch.source.end - dispatch.source.start);
                assert!(
                    (u64::from(dst) + 1) * 80
                        <= dispatch.destination.end - dispatch.destination.start
                );
                let src = src + (dispatch.source.start / 80) as u32;
                let dst = dst + (dispatch.destination.start / 80) as u32;
                assert!(destinations.insert(dst));
                actual.push([src, dst]);
            }
        }
        actual.sort_unstable();
        assert_eq!(actual, expected);
        plan
    }
    #[test]
    fn partitioned_windows_rebase_at_boundaries_and_split_dispatches() {
        for alignment in [16, 64, 256, 512] {
            let limits = wgpu::Limits {
                max_storage_buffer_binding_size: 5120,
                min_storage_buffer_offset_alignment: alignment,
                ..Default::default()
            };
            let tasks = (0..193).map(|i| [(i * 97) % 257, i]).collect();
            let plan = check_plan(&limits, 257 * 80, 193 * 80, tasks);
            assert!(plan.dispatches.len() > 1);
        }
        let limits = wgpu::Limits {
            max_storage_buffer_binding_size: 25600,
            max_compute_workgroups_per_dimension: 1,
            ..Default::default()
        };
        let plan = check_plan(
            &limits,
            300 * 80,
            300 * 80,
            (0..300).map(|i| [299 - i, i]).collect(),
        );
        assert_eq!(plan.dispatches.len(), 5);
        let limits = wgpu::Limits::default();
        let slots = window_slots(&limits) as u32;
        let source = limits.max_buffer_size;
        let tasks = vec![[slots - 1, 0], [slots, 1], [slots + 1, 2]];
        assert!(!fits(&limits, source, 240, 3));
        let plan = check_plan(&limits, source, 240, tasks);
        assert_eq!(plan.dispatches.len(), 2);
    }
    #[test]
    fn partitioned_sparse_membership_and_fallback_preserve_bytes() {
        use crate::batching::{GeometryBatches, TransferPlan};
        use threex_core::Instance;
        let limits = wgpu::Limits {
            max_storage_buffer_binding_size: 2560,
            max_compute_workgroups_per_dimension: 1,
            ..Default::default()
        };
        let mut batches = GeometryBatches::default();
        let mut data: Vec<_> = (0..1000)
            .map(|i| Instance {
                model: threex_core::identity(),
                color: [f32::from_bits(0x7fc00000 + i), -0., 1., (i % 3 + 1) as f32],
            })
            .collect();
        let mut gpu: [Vec<Instance>; 3] = Default::default();
        for step in 0..90 {
            if step > 0 {
                data[step].color[3] = ((step + 1) % 3 + 1) as f32;
                data[step].color[2] += 1.;
            }
            if step % 7 == 0 {
                data.swap_remove(step);
            }
            let full = step % 11 == 0;
            let range = if full { 0..data.len() } else { step..step + 1 };
            batches.sync(&data, std::iter::once(range));
            for (kind, bank) in gpu.iter_mut().enumerate() {
                bank.resize(batches.indices(kind).len(), data[0]);
                match batches.transfers(kind, full, true) {
                    TransferPlan::Copies(copies) => {
                        for c in copies {
                            bank[c.destination..c.destination + c.count]
                                .copy_from_slice(&data[c.source..c.source + c.count]);
                        }
                    }
                    TransferPlan::Gather(tasks) => {
                        let plan = check_plan(
                            &limits,
                            data.len() as u64 * 80,
                            bank.len() as u64 * 80,
                            tasks,
                        );
                        for d in plan.dispatches {
                            for &[s, t] in &plan.tasks[d.tasks] {
                                bank[t as usize + (d.destination.start / 80) as usize] =
                                    data[s as usize + (d.source.start / 80) as usize];
                            }
                        }
                    }
                }
                let reference: Vec<_> = batches
                    .indices(kind)
                    .iter()
                    .map(|&i| data[i as usize])
                    .collect();
                assert_eq!(
                    bytemuck::cast_slice::<_, u8>(bank),
                    bytemuck::cast_slice::<_, u8>(&reference)
                );
            }
        }
        let mut no_compute = limits.clone();
        no_compute.max_storage_buffers_per_shader_stage = 2;
        assert!(partition(&no_compute, 800, 800, vec![[0, 0]]).is_none());
        assert!(partition(&limits, 800, 800, vec![[10, 0]]).is_none());
        let tiny = wgpu::Limits {
            max_storage_buffer_binding_size: 80,
            ..Default::default()
        };
        assert!(!can_partition(&tiny, 800, 800));
        let tight = wgpu::Limits {
            max_buffer_size: 80000,
            max_storage_buffer_binding_size: 1280,
            ..Default::default()
        };
        assert!(
            partition(
                &tight,
                80000,
                80000,
                (0..1000).map(|i| [(i * 97) % 1000, i]).collect()
            )
            .is_none()
        );
        assert!(matches!(
            batches.transfers(0, false, false),
            TransferPlan::Copies(_)
        ));
    }
    #[test]
    fn cached_plans_and_gpu_indices_survive_full_partial_and_membership_updates() {
        use crate::batching::GeometryBatches;
        use threex_core::{GeometryKind, GeometrySpec, Instance};
        let limits = wgpu::Limits {
            max_storage_buffer_binding_size: 2560,
            ..Default::default()
        };
        let mut data: Vec<_> = (0..3000)
            .map(|i| Instance {
                model: threex_core::identity(),
                color: [
                    f32::from_bits(0x7fc00000 + i),
                    -0.,
                    1.,
                    GeometrySpec::new(
                        [GeometryKind::Box, GeometryKind::Plane, GeometryKind::Sphere]
                            [(i % 3) as usize],
                        if i % 3 == 0 {
                            [0, 0]
                        } else {
                            [3 + (i % 4) as u16, 2 + (i % 3) as u16]
                        },
                    )
                    .unwrap()
                    .tag() as f32,
                ],
            })
            .collect();
        let mut batches = GeometryBatches::default();
        let mut cache = PlanCache::default();
        let mut indices: [Vec<[u32; 2]>; 3] = Default::default();
        let mut gpu: [Vec<Instance>; 3] = Default::default();
        let (mut hits, mut uploads, mut copies) = (0, 0, 0);
        for step in 0..350 {
            let index = step * 17 % 700;
            data[index].color[2] = step as f32;
            if step % 29 == 0 {
                let kind =
                    [GeometryKind::Box, GeometryKind::Plane, GeometryKind::Sphere][(step + 1) % 3];
                data[index].color[3] = GeometrySpec::new(
                    kind,
                    if kind == GeometryKind::Box {
                        [0, 0]
                    } else {
                        [3 + (step % 7) as u16, 2 + (step % 5) as u16]
                    },
                )
                .unwrap()
                .tag() as f32;
            }
            if step % 47 == 0 {
                data.swap_remove(index);
            }
            if step % 53 == 0 {
                data.push(data[index]);
            }
            let full = step % 3 != 1;
            let range = if full {
                0..data.len()
            } else if step % 5 == 0 {
                0..900
            } else {
                index..index + 1
            };
            batches.sync(&data, std::iter::once(range));
            for kind in 0..3 {
                gpu[kind].resize(batches.indices(kind).len(), data[0]);
                match cache.prepare(&limits, [400000, 400000], &batches, kind, false) {
                    Prepared::Copies(plan) => {
                        copies += 1;
                        for c in plan {
                            gpu[kind][c.destination..c.destination + c.count]
                                .copy_from_slice(&data[c.source..c.source + c.count]);
                        }
                    }
                    Prepared::Gather { plan, upload } => {
                        if upload {
                            uploads += 1;
                            indices[kind] = plan.tasks.clone();
                        } else {
                            hits += 1;
                            assert_eq!(indices[kind], plan.tasks);
                        }
                        for d in &plan.dispatches {
                            for &[src, dst] in &indices[kind][d.tasks.clone()] {
                                gpu[kind][dst as usize + (d.destination.start / 80) as usize] =
                                    data[src as usize + (d.source.start / 80) as usize];
                            }
                        }
                    }
                }
                let reference: Vec<_> = batches
                    .indices(kind)
                    .iter()
                    .map(|&i| data[i as usize])
                    .collect();
                assert_eq!(
                    bytemuck::cast_slice::<_, u8>(&gpu[kind]),
                    bytemuck::cast_slice::<_, u8>(&reference)
                );
            }
        }
        assert!(hits > 100 && uploads > 20 && copies > 20);
    }
    #[test]
    fn cache_invalidates_sizes_limits_and_revisions_and_retains_negative_results() {
        use crate::batching::GeometryBatches;
        let mut data: Vec<_> = (0..1000)
            .map(|i| threex_core::Instance {
                model: threex_core::identity(),
                color: [1., 1., 1., (i % 3 + 1) as f32],
            })
            .collect();
        let mut batches = GeometryBatches::default();
        batches.sync(&data, std::iter::once(0..1000));
        let mut cache = PlanCache::default();
        let mut limits = wgpu::Limits::default();
        assert!(matches!(
            cache.prepare(&limits, [80000, 80000], &batches, 0, true),
            Prepared::Gather { upload: true, .. }
        ));
        assert!(matches!(
            cache.prepare(&limits, [80000, 80000], &batches, 0, true),
            Prepared::Gather { upload: false, .. }
        ));
        let revision = batches.revision(0);
        batches.sync(&data, std::iter::once(0..1));
        assert!(!batches.whole_bank(0, false));
        assert_eq!(batches.revision(0), revision);
        assert!(
            matches!(cache.prepare(&limits,[80000,80000],&batches,0,false),Prepared::Copies(c) if c.len()==1)
        );
        assert!(matches!(
            cache.prepare(&limits, [80000, 80000], &batches, 0, true),
            Prepared::Gather { upload: false, .. }
        ));
        batches.sync(&data, std::iter::once(0..700));
        assert!(matches!(
            cache.prepare(&limits, [80000, 80000], &batches, 0, false),
            Prepared::Gather { upload: true, .. }
        ));
        let pointer = match &cache.banks[0].as_ref().unwrap().plan {
            FullPlan::Gather(p) => p.tasks.as_ptr(),
            _ => panic!(),
        };
        match cache.prepare(&limits, [80000, 80000], &batches, 0, true) {
            Prepared::Gather { plan, upload } => {
                assert!(upload);
                assert_eq!(pointer, plan.tasks.as_ptr());
            }
            _ => panic!(),
        }
        for sizes in [[80080, 80000], [80080, 80080]] {
            assert!(matches!(
                cache.prepare(&limits, sizes, &batches, 0, true),
                Prepared::Gather { upload: true, .. }
            ));
        }
        limits.max_storage_buffer_binding_size = 2560;
        assert!(matches!(
            cache.prepare(&limits, [80080, 80080], &batches, 0, true),
            Prepared::Gather { upload: true, .. }
        ));
        data[0].color[3] = 2.;
        batches.sync(&data, std::iter::once(0..1));
        assert_ne!(batches.revision(0), revision);
        assert!(matches!(
            cache.prepare(&limits, [80080, 80080], &batches, 0, true),
            Prepared::Gather { upload: true, .. }
        ));
        let tight = wgpu::Limits {
            max_buffer_size: 80000,
            max_storage_buffer_binding_size: 327680,
            min_storage_buffer_offset_alignment: 65536,
            max_compute_workgroups_per_dimension: 1,
            ..Default::default()
        };
        for _ in 0..2 {
            assert!(matches!(
                cache.prepare(&tight, [80000, 80000], &batches, 0, true),
                Prepared::Copies(_)
            ));
            assert!(matches!(
                cache.banks[0].as_ref().unwrap().plan,
                FullPlan::Fallback
            ));
        }
        let mut unsupported = limits.clone();
        unsupported.max_storage_buffers_per_shader_stage = 2;
        assert!(matches!(
            cache.prepare(&unsupported, [80000, 80000], &batches, 0, true),
            Prepared::Copies(_)
        ));
        // A new Renderer owns a fresh cache and must upload again.
        assert!(matches!(
            PlanCache::default().prepare(&limits, [80000, 80000], &batches, 0, true),
            Prepared::Gather { upload: true, .. }
        ));
    }
    #[test]
    #[ignore = "CPU cache benchmark; no GPU execution"]
    fn cached_partition_benchmark() {
        use crate::batching::{GeometryBatches, TransferPlan};
        use std::{hint::black_box, time::Instant};
        let data: Vec<_> = (0..100000)
            .map(|i| threex_core::Instance {
                model: threex_core::identity(),
                color: [1., 1., 1., (i % 3 + 1) as f32],
            })
            .collect();
        for binding in [65536, 134217728] {
            for workload in ["warm-plan", "sync-full", "sparse"] {
                let limits = wgpu::Limits {
                    max_storage_buffer_binding_size: binding,
                    ..Default::default()
                };
                let mut batches = GeometryBatches::default();
                batches.sync(&data, std::iter::once(0..data.len()));
                let mut cache = PlanCache::default();
                for kind in 0..3 {
                    black_box(cache.prepare(
                        &limits,
                        [131072 * 80, 65536 * 80],
                        &batches,
                        kind,
                        true,
                    ));
                }
                let mut times = Vec::new();
                let mut bytes = [0usize; 2];
                let mut commands = [0; 2];
                for after in [false, true] {
                    let start = Instant::now();
                    for step in 0..100 {
                        if workload != "warm-plan" {
                            let range = if workload == "sparse" {
                                step..step + 1
                            } else {
                                0..data.len()
                            };
                            batches.sync(&data, std::iter::once(range));
                        }
                        let (mut sent, mut count) = (0, 0);
                        for kind in 0..3 {
                            if after {
                                match cache.prepare(
                                    &limits,
                                    [131072 * 80, 65536 * 80],
                                    &batches,
                                    kind,
                                    workload == "warm-plan",
                                ) {
                                    Prepared::Copies(c) => {
                                        count += c.len();
                                        black_box(c);
                                    }
                                    Prepared::Gather { plan, upload } => {
                                        count += plan.dispatches.len();
                                        if upload {
                                            sent += plan.tasks.len() * 8;
                                        }
                                        black_box(plan);
                                    }
                                }
                            } else {
                                let eligible = can_partition(&limits, 131072 * 80, 65536 * 80);
                                match batches.transfers(kind, workload == "warm-plan", eligible) {
                                    TransferPlan::Copies(c) => {
                                        count += c.len();
                                        black_box(c);
                                    }
                                    TransferPlan::Gather(t) => {
                                        let plan =
                                            partition(&limits, 131072 * 80, 65536 * 80, t).unwrap();
                                        count += plan.dispatches.len();
                                        sent += plan.tasks.len() * 8;
                                        black_box(plan);
                                    }
                                }
                            }
                        }
                        bytes[after as usize] = sent;
                        commands[after as usize] = count;
                    }
                    times.push(start.elapsed());
                }
                eprintln!(
                    "binding={binding}, workload={workload}, iterations=100, before={:?}, after={:?}, commands={} -> {}, task_bytes={} -> {}",
                    times[0], times[1], commands[0], commands[1], bytes[0], bytes[1]
                );
            }
        }
    }
    #[test]
    #[ignore = "CPU partition planner benchmark; no GPU execution"]
    fn partition_benchmark() {
        use crate::batching::{GeometryBatches, TransferPlan};
        use std::{hint::black_box, time::Instant};
        let data: Vec<_> = (0..100000)
            .map(|i| threex_core::Instance {
                model: threex_core::identity(),
                color: [1., 1., 1., (i % 3 + 1) as f32],
            })
            .collect();
        let mut batches = GeometryBatches::default();
        batches.sync(&data, std::iter::once(0..data.len()));
        for binding in [65536, 134217728] {
            let limits = wgpu::Limits {
                max_storage_buffer_binding_size: binding,
                ..Default::default()
            };
            for full in [true, false] {
                if !full {
                    batches.sync(&data, std::iter::once(400..401));
                } else {
                    batches.sync(&data, std::iter::once(0..data.len()));
                }
                let mut times = Vec::new();
                let mut counts = [0; 2];
                let mut upload = 0;
                for after in [false, true] {
                    let start = Instant::now();
                    for _ in 0..100 {
                        let mut count = 0;
                        let mut bytes = 0;
                        for kind in 0..3 {
                            let source = 131072 * 80;
                            let destination = 65536 * 80;
                            let eligible = if after {
                                can_partition(&limits, source, destination)
                            } else {
                                fits(
                                    &limits,
                                    source,
                                    destination,
                                    batches.transfer_count(kind, full),
                                )
                            };
                            match batches.transfers(kind, full, eligible) {
                                TransferPlan::Copies(c) => {
                                    count += c.len();
                                    black_box(c);
                                }
                                TransferPlan::Gather(t) => {
                                    if after {
                                        let p = partition(&limits, source, destination, t).unwrap();
                                        count += p.dispatches.len();
                                        bytes += p.tasks.len() * 8;
                                        black_box(p);
                                    } else {
                                        count += 1;
                                        black_box(t);
                                    }
                                }
                            }
                        }
                        if after {
                            upload = bytes;
                        }
                        counts[after as usize] = count;
                    }
                    times.push(start.elapsed());
                }
                eprintln!(
                    "binding={binding}, full={full}, iterations=100, before={:?}, after={:?}, commands={} -> {}, task_bytes={upload}",
                    times[0], times[1], counts[0], counts[1]
                );
            }
        }
    }
    #[test]
    #[ignore = "Requires an accessible GPU adapter; validates actual gather readback"]
    fn gpu_gather_readback_preserves_words_and_reused_task_lengths() {
        use wgpu::util::DeviceExt;
        pollster::block_on(async {
            let adapter = wgpu::Instance::default()
                .request_adapter(&Default::default())
                .await
                .expect("GPU adapter");
            let (device, queue) = adapter.request_device(&Default::default()).await.unwrap();
            device.push_error_scope(wgpu::ErrorFilter::Validation);
            let mut gather = Gather::new(&device).expect("compute gather support");
            // Includes arbitrary float/NaN bit patterns; shader must never convert them.
            let mut words: Vec<u32> = (0..20000).map(|i| 0x7f800001u32.wrapping_add(i)).collect();
            let source = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: bytemuck::cast_slice(&words),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            });
            let destination = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 80000,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            let staging = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 80000,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let mut expected = vec![0u32; 20000];
            // Grow, shrink the live binding, and reuse capacity. Tail must remain untouched.
            for count in [999usize, 65, 1000, 1] {
                let tasks: Vec<_> = (0..count)
                    .map(|i| [((i * 7 + count) % 1000) as u32, i as u32])
                    .collect();
                for &[src, dst] in &tasks {
                    expected[dst as usize * 20..dst as usize * 20 + 20]
                        .copy_from_slice(&words[src as usize * 20..src as usize * 20 + 20]);
                }
                let mut limits = device.limits();
                if count != 65 {
                    limits.max_storage_buffer_binding_size = 2560;
                    limits.max_compute_workgroups_per_dimension = 1;
                }
                let plan = partition(&limits, source.size(), destination.size(), tasks).unwrap();
                for upload in [true, false] {
                    if !upload {
                        for word in &mut words {
                            *word = word.wrapping_add(1);
                        }
                        queue.write_buffer(&source, 0, bytemuck::cast_slice(&words));
                    }
                    for d in &plan.dispatches {
                        for &[s, t] in &plan.tasks[d.tasks.clone()] {
                            let src = s as usize + (d.source.start / 80) as usize;
                            let dst = t as usize + (d.destination.start / 80) as usize;
                            expected[dst * 20..dst * 20 + 20]
                                .copy_from_slice(&words[src * 20..src * 20 + 20]);
                        }
                    }
                    let mut encoder = device.create_command_encoder(&Default::default());
                    gather.encode(
                        &device,
                        &queue,
                        &mut encoder,
                        0,
                        [&source, &destination],
                        (&plan, upload),
                    );
                    encoder.copy_buffer_to_buffer(&destination, 0, &staging, 0, 80000);
                    queue.submit([encoder.finish()]);
                    let (send, receive) = std::sync::mpsc::channel();
                    staging
                        .slice(..)
                        .map_async(wgpu::MapMode::Read, move |result| {
                            send.send(result).unwrap()
                        });
                    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                    receive.recv().unwrap().unwrap();
                    {
                        let view = staging.slice(..).get_mapped_range();
                        assert_eq!(&*view, bytemuck::cast_slice::<_, u8>(&expected));
                    }
                    staging.unmap();
                }
            }
            assert!(device.pop_error_scope().await.is_none());
        });
    }
}
