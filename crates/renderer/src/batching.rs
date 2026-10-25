//! Delta-maintained geometry banks. Only slot indices are retained on the CPU.
use std::{collections::BTreeMap, ops::Range};
use threex_core::{GeometryKind, GeometrySpec, Instance};

#[derive(Clone, Copy)]
struct Slot {
    kind: usize,
    offset: usize,
}
#[derive(Default)]
pub struct GeometryBatches {
    slots: Vec<Slot>,
    banks: [Vec<u32>; 3],
    dirty: [Vec<usize>; 3],
    revisions: [u64; 3],
    specs: [Vec<GeometrySpec>; 3],
    starts: [BTreeMap<usize, GeometrySpec>; 3],
    groups: [BTreeMap<GeometrySpec, Range<usize>>; 3],
}
/// Consecutive source and destination slots can share one GPU buffer copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstanceCopy {
    pub source: usize,
    pub destination: usize,
    pub count: usize,
}
#[derive(Debug)]
pub enum TransferPlan {
    Copies(Vec<InstanceCopy>),
    Gather(Vec<[u32; 2]>),
}
impl GeometryBatches {
    pub fn geometry_draws(&self) -> impl Iterator<Item = (GeometrySpec, Range<u32>)> + '_ {
        (0..3).flat_map(move |kind| {
            let mut starts = self.starts[kind].iter().peekable();
            std::iter::from_fn(move || {
                let (&start, &spec) = starts.next()?;
                let end = starts
                    .peek()
                    .map_or(self.specs[kind].len(), |entry| *entry.0);
                Some((spec, start as u32..end as u32))
            })
        })
    }
    fn bump(&mut self, kind: usize) {
        self.revisions[kind] = self.revisions[kind]
            .checked_add(1)
            .expect("bank revision exhausted");
    }
    pub fn revision(&self, kind: usize) -> u64 {
        self.revisions[kind]
    }
    pub fn whole_bank(&self, kind: usize, full: bool) -> bool {
        let len = self.banks[kind].len();
        len > 0 && (full || self.dirty[kind].get(len - 1) == Some(&(len - 1)))
    }
    /// Swap only boundary representatives, repairing both reverse indices.
    fn swap(&mut self, kind: usize, a: usize, b: usize) -> bool {
        if a == b {
            return false;
        }
        self.banks[kind].swap(a, b);
        self.specs[kind].swap(a, b);
        self.slots[self.banks[kind][a] as usize].offset = a;
        self.slots[self.banks[kind][b] as usize].offset = b;
        self.dirty[kind].extend([a, b]);
        true
    }
    fn remove(&mut self, index: usize) -> bool {
        let Slot { kind, offset } = self.slots[index];
        let spec = self.specs[kind][offset];
        let range = self.groups[kind][&spec].clone();
        let mut moved = self.swap(kind, offset, range.end - 1);
        if range.len() == 1 {
            self.groups[kind].remove(&spec);
            self.starts[kind].remove(&range.start);
        } else {
            self.groups[kind].get_mut(&spec).unwrap().end -= 1;
        }
        // Carry the hole through one tail element per following group.
        let following: Vec<_> = self.starts[kind]
            .range(range.end..)
            .map(|(&start, &spec)| (start, spec))
            .collect();
        let mut hole = range.end - 1;
        for (start, spec) in following {
            let end = self.groups[kind][&spec].end;
            moved |= self.swap(kind, hole, end - 1);
            self.starts[kind].remove(&start);
            self.starts[kind].insert(start - 1, spec);
            let group = self.groups[kind].get_mut(&spec).unwrap();
            group.start -= 1;
            group.end -= 1;
            hole = end - 1;
        }
        self.banks[kind].pop();
        self.specs[kind].pop();
        moved
    }
    fn insert(&mut self, index: usize, spec: GeometrySpec) -> bool {
        let kind = spec.kind as usize;
        let offset = self.banks[kind].len();
        self.banks[kind].push(index as u32);
        self.specs[kind].push(spec);
        self.slots[index] = Slot { kind, offset };
        self.dirty[kind].push(offset);
        let Some(range) = self.groups[kind].get(&spec).cloned() else {
            self.groups[kind].insert(spec, offset..offset + 1);
            self.starts[kind].insert(offset, spec);
            return false;
        };
        // Grow this group by carrying the inserted slot backwards through heads.
        let following: Vec<_> = self.starts[kind]
            .range(range.end..)
            .map(|(&start, &spec)| (start, spec))
            .collect();
        let mut hole = offset;
        let mut moved = false;
        for (start, spec) in following.into_iter().rev() {
            moved |= self.swap(kind, hole, start);
            self.starts[kind].remove(&start);
            self.starts[kind].insert(start + 1, spec);
            let group = self.groups[kind].get_mut(&spec).unwrap();
            group.start += 1;
            group.end += 1;
            hole = start;
        }
        self.groups[kind].get_mut(&spec).unwrap().end += 1;
        moved
    }
    /// Cold start groups lightweight indices once; no per-instance boundary rotation.
    fn initialize(&mut self, data: &[Instance]) {
        let mut grouped = BTreeMap::<GeometrySpec, Vec<u32>>::new();
        self.slots.resize(data.len(), Slot { kind: 0, offset: 0 });
        for (index, instance) in data.iter().enumerate() {
            grouped
                .entry(instance.geometry_spec())
                .or_default()
                .push(index as u32);
        }
        for (spec, indices) in grouped {
            let kind = spec.kind as usize;
            let start = self.banks[kind].len();
            for source in indices {
                let offset = self.banks[kind].len();
                self.slots[source as usize] = Slot { kind, offset };
                self.banks[kind].push(source);
                self.specs[kind].push(spec);
                self.dirty[kind].push(offset);
            }
            self.starts[kind].insert(start, spec);
            self.groups[kind].insert(spec, start..self.banks[kind].len());
            self.bump(kind);
        }
    }
    pub fn sync(&mut self, data: &[Instance], ranges: impl IntoIterator<Item = Range<usize>>) {
        for dirty in &mut self.dirty {
            dirty.clear();
        }
        while self.slots.len() > data.len() {
            let index = self.slots.len() - 1;
            let kind = self.slots[index].kind;
            self.remove(index);
            self.bump(kind);
            self.slots.pop();
        }
        if self.slots.is_empty() {
            self.initialize(data);
        }
        // New slots are initialized even if no dirty range was supplied.
        for (index, instance) in data.iter().enumerate().skip(self.slots.len()) {
            self.slots.push(Slot { kind: 0, offset: 0 });
            self.insert(index, instance.geometry_spec());
            self.bump(instance.geometry_kind() as usize);
        }
        for range in ranges {
            for index in range {
                let slot = self.slots[index];
                let spec = data[index].geometry_spec();
                if self.specs[slot.kind][slot.offset] != spec {
                    let removed = self.remove(index);
                    let inserted = self.insert(index, spec);
                    if slot.kind != spec.kind as usize {
                        self.bump(slot.kind);
                        self.bump(spec.kind as usize);
                    } else if removed || inserted {
                        self.bump(slot.kind);
                    }
                }
                let slot = self.slots[index];
                self.dirty[slot.kind].push(slot.offset);
            }
        }
        for (kind, dirty) in self.dirty.iter_mut().enumerate() {
            dirty.retain(|&offset| offset < self.banks[kind].len());
            dirty.sort_unstable();
            dirty.dedup();
        }
    }
    pub fn requires_banks(&self) -> bool {
        self.draws().nth(1).is_some() || self.starts.iter().any(|starts| starts.len() > 1)
    }
    pub(crate) fn bank_slot(&self, index: usize) -> (usize, u32) {
        let slot = self.slots[index];
        (slot.kind, slot.offset as u32)
    }
    pub fn indices(&self, kind: usize) -> &[u32] {
        &self.banks[kind]
    }
    pub fn draws(&self) -> impl Iterator<Item = (GeometryKind, Range<u32>)> + '_ {
        [GeometryKind::Box, GeometryKind::Plane, GeometryKind::Sphere]
            .into_iter()
            .filter_map(|kind| {
                let len = self.banks[kind as usize].len();
                (len > 0).then_some((kind, 0..len as u32))
            })
    }
    pub fn transfer_count(&self, kind: usize, full: bool) -> usize {
        if full {
            self.banks[kind].len()
        } else {
            self.dirty[kind].len()
        }
    }
    fn destinations(&self, kind: usize, full: bool) -> impl Iterator<Item = usize> + '_ {
        (0..if full { self.banks[kind].len() } else { 0 })
            .chain(self.dirty[kind].iter().copied().take(if full {
                0
            } else {
                self.dirty[kind].len()
            }))
            .filter(move |&destination| destination < self.banks[kind].len())
    }
    fn limited_copies(&self, kind: usize, full: bool, limit: usize) -> Option<Vec<InstanceCopy>> {
        let mut result: Vec<InstanceCopy> = Vec::new();
        for destination in self.destinations(kind, full) {
            let source = self.banks[kind][destination] as usize;
            if let Some(last) = result.last_mut()
                && last.destination + last.count == destination
                && last.source + last.count == source
            {
                last.count += 1;
            } else {
                if result.len() == limit {
                    return None;
                }
                result.push(InstanceCopy {
                    source,
                    destination,
                    count: 1,
                });
            }
        }
        Some(result)
    }
    pub fn copies(&self, kind: usize, full: bool) -> Vec<InstanceCopy> {
        self.limited_copies(kind, full, usize::MAX).unwrap()
    }
    /// Keep small/coalesced copies; use one dispatch when fragmentation exceeds 64 runs.
    pub fn transfers(&self, kind: usize, full: bool, gather_supported: bool) -> TransferPlan {
        if !gather_supported {
            return TransferPlan::Copies(self.copies(kind, full));
        }
        if let Some(copies) = self.limited_copies(kind, full, 64) {
            TransferPlan::Copies(copies)
        } else {
            TransferPlan::Gather(
                self.destinations(kind, full)
                    .map(|destination| [self.banks[kind][destination], destination as u32])
                    .collect(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn instance(index: usize, kind: usize) -> Instance {
        Instance {
            model: threex_core::identity(),
            color: [index as f32, 0., 0., (kind + 1) as f32],
        }
    }
    fn verify(batches: &GeometryBatches, data: &[Instance]) {
        let mut seen = vec![false; data.len()];
        for kind in 0..3 {
            for (offset, &index) in batches.indices(kind).iter().enumerate() {
                let index = index as usize;
                assert!(!seen[index]);
                seen[index] = true;
                assert_eq!(data[index].geometry_kind() as usize, kind);
                assert_eq!(batches.slots[index].offset, offset);
                assert_eq!(batches.slots[index].kind, kind);
            }
        }
        for kind in 0..3 {
            assert_eq!(batches.groups[kind].len(), batches.starts[kind].len());
            let mut end = 0;
            for (&start, &spec) in &batches.starts[kind] {
                assert_eq!(start, end);
                let range = &batches.groups[kind][&spec];
                assert_eq!(range.start, start);
                assert!(!range.is_empty());
                for offset in range.clone() {
                    assert_eq!(batches.specs[kind][offset], spec);
                    assert_eq!(
                        data[batches.banks[kind][offset] as usize].geometry_spec(),
                        spec
                    );
                }
                end = range.end;
            }
            assert_eq!(end, batches.banks[kind].len());
        }
        assert!(seen.into_iter().all(|v| v));
        assert!(batches.draws().count() <= 3);
    }
    #[test]
    fn delta_gpu_copies_survive_moves_shrink_regrowth_and_reallocation() {
        let mut batches = GeometryBatches::default();
        let mut data = Vec::new();
        let mut gpu: [Vec<Instance>; 3] = Default::default();
        let mut capacities = [0; 3];
        let mut seed = 17u32;
        let mut banked = false;
        for step in 0..3000 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let mut ranges = Vec::new();
            match step % 7 {
                0 | 1 => data.push(instance(step, seed as usize % 3)),
                2 if !data.is_empty() => {
                    let index = seed as usize % data.len();
                    data.swap_remove(index);
                    if index < data.len() {
                        ranges.push(index..index + 1);
                    }
                }
                3 | 4 if !data.is_empty() => {
                    let index = seed as usize % data.len();
                    data[index] = instance(step, seed as usize % 3);
                    ranges.push(index..index + 1);
                }
                5 if step % 35 == 5 => data.truncate(data.len() / 2),
                _ => {}
            }
            // Scene replacement / lost history requires the same full-range fallback.
            if step % 101 == 0 {
                ranges = std::iter::once(0..data.len()).collect();
            }
            batches.sync(&data, ranges);
            verify(&batches, &data);
            if !batches.requires_banks() {
                banked = false;
                continue;
            }
            for kind in 0..3 {
                let len = batches.indices(kind).len();
                let reallocated = len > capacities[kind];
                let full = reallocated || !banked;
                if reallocated {
                    capacities[kind] = len.next_power_of_two();
                    gpu[kind].clear();
                }
                gpu[kind].resize(len, instance(0, 0));
                for copy in batches.copies(kind, full) {
                    gpu[kind][copy.destination..copy.destination + copy.count]
                        .copy_from_slice(&data[copy.source..copy.source + copy.count]);
                }
                let expected: Vec<_> = batches
                    .indices(kind)
                    .iter()
                    .map(|&i| data[i as usize])
                    .collect();
                assert_eq!(
                    bytemuck::cast_slice::<_, u8>(&gpu[kind]),
                    bytemuck::cast_slice::<_, u8>(&expected)
                );
            }
            banked = true;
        }
        data.clear();
        batches.sync(&data, []);
        verify(&batches, &data);
        assert_eq!(batches.draws().count(), 0);
    }
    #[test]
    fn box_coalesces_and_single_edit_copies_only_one_instance() {
        let mut batches = GeometryBatches::default();
        let mut data: Vec<_> = (0..100_000).map(|i| instance(i, 0)).collect();
        batches.sync(&data, std::iter::once(0..data.len()));
        assert_eq!(batches.draws().count(), 1);
        assert!(!batches.requires_banks());
        assert_eq!(
            batches.copies(0, true),
            vec![InstanceCopy {
                source: 0,
                destination: 0,
                count: data.len()
            }]
        );
        data[500].color[0] = -1.;
        batches.sync(&data, std::iter::once(500..501));
        assert_eq!(
            batches.copies(0, false),
            vec![InstanceCopy {
                source: 500,
                destination: 500,
                count: 1
            }]
        );
        batches.sync(&data, []);
        assert!((0..3).all(|kind| batches.copies(kind, false).is_empty()));
        data[500].color[3] = 2.;
        batches.sync(&data, std::iter::once(500..501));
        assert_eq!(
            (0..3)
                .map(|k| batches
                    .copies(k, false)
                    .iter()
                    .map(|c| c.count)
                    .sum::<usize>())
                .sum::<usize>(),
            2
        );
        assert!(batches.requires_banks());
        verify(&batches, &data);
    }
    #[test]
    fn hybrid_plans_preserve_bytes_sparse_copies_and_full_gathers() {
        for supported in [true, false] {
            let mut data: Vec<_> = (0..5000).map(|i| instance(i, i % 3)).collect();
            let mut batches = GeometryBatches::default();
            let mut gpu: [Vec<Instance>; 3] = Default::default();
            for step in 0..180 {
                let index = step * 13 % 512;
                data[index].model[0][0] = f32::from_bits(0x7fc00000 + step as u32);
                data[index].color[0] = -0.;
                data[index].color[3] = (step % 3 + 1) as f32;
                let bulk = step % 5 == 0;
                let full = step % 17 == 0;
                let ranges = if full {
                    0..data.len()
                } else if bulk {
                    for instance in &mut data[..512] {
                        instance.color[1] = step as f32;
                    }
                    0..512
                } else {
                    index..index + 1
                };
                batches.sync(&data, std::iter::once(ranges));
                for (kind, bank) in gpu.iter_mut().enumerate() {
                    bank.resize(batches.indices(kind).len(), instance(0, 0));
                    let plan = batches.transfers(kind, full, supported);
                    match plan {
                        TransferPlan::Copies(copies) => {
                            assert!(!supported || (!full && !bulk));
                            if !full && !bulk {
                                assert!(copies.len() <= 2);
                            }
                            for copy in copies {
                                bank[copy.destination..copy.destination + copy.count]
                                    .copy_from_slice(&data[copy.source..copy.source + copy.count]);
                            }
                        }
                        TransferPlan::Gather(tasks) => {
                            assert!(supported && (full || bulk));
                            let mut destinations = std::collections::BTreeSet::new();
                            for [source, destination] in tasks {
                                assert!(destinations.insert(destination));
                                bank[destination as usize] = data[source as usize];
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
            // Coalesced full updates remain one copy, regardless of length.
            data.iter_mut().for_each(|i| i.color[3] = 1.);
            batches = GeometryBatches::default();
            batches.sync(&data, std::iter::once(0..data.len()));
            assert!(
                matches!(batches.transfers(0, true, supported), TransferPlan::Copies(c) if c.len() == 1)
            );
        }
    }
    #[test]
    #[ignore = "CPU transfer planner benchmark; no GPU execution"]
    fn gather_benchmark() {
        use std::{hint::black_box, time::Instant};
        let data: Vec<_> = (0..100_000).map(|i| instance(i, i % 3)).collect();
        let mut batches = GeometryBatches::default();
        batches.sync(&data, std::iter::once(0..data.len()));
        for (name, iterations, full) in [
            ("initial-plan", 1, true),
            ("full-update", 100, true),
            ("sparse-update", 1000, false),
        ] {
            let mut times = Vec::new();
            let mut commands = [0; 2];
            for hybrid in [false, true] {
                let start = Instant::now();
                for step in 0..iterations {
                    if name != "initial-plan" {
                        let range = if full { 0..data.len() } else { step..step + 1 };
                        batches.sync(&data, std::iter::once(range));
                    }
                    let mut count = 0;
                    for kind in 0..3 {
                        let plan = batches.transfers(kind, name == "initial-plan", hybrid);
                        count += match &plan {
                            TransferPlan::Copies(c) => c.len(),
                            TransferPlan::Gather(_) => 1,
                        };
                        black_box(plan);
                    }
                    commands[hybrid as usize] = count;
                }
                times.push(start.elapsed());
            }
            eprintln!(
                "{name}: iterations={iterations}, before={:?}, after={:?}, commands={} -> {}",
                times[0], times[1], commands[0], commands[1]
            );
        }
    }
    #[test]
    #[ignore = "CPU benchmark; run explicitly with --release --ignored --nocapture"]
    fn batching_benchmark() {
        use std::{hint::black_box, time::Instant};
        for mixed in [false, true] {
            let mut data: Vec<_> = (0..100_000)
                .map(|i| instance(i, if mixed { i % 3 } else { 0 }))
                .collect();
            let mut runs = crate::geometry::GeometryRuns::default();
            let start = Instant::now();
            runs.sync(&data, std::iter::once(0..data.len()));
            let before_init = start.elapsed();
            let mut batches = GeometryBatches::default();
            let start = Instant::now();
            batches.sync(&data, std::iter::once(0..data.len()));
            if batches.requires_banks() {
                black_box((0..3).map(|k| batches.copies(k, true).len()).sum::<usize>());
            }
            let after_init = start.elapsed();
            let start = Instant::now();
            for i in 0..1000 {
                data[i].color[0] += 1.;
                runs.sync(&data, std::iter::once(i..i + 1));
                for draw in runs.draws() {
                    black_box(draw);
                }
            }
            let before = start.elapsed();
            let start = Instant::now();
            for i in 0..1000 {
                data[i].color[0] += 1.;
                batches.sync(&data, std::iter::once(i..i + 1));
                if batches.requires_banks() {
                    for k in 0..3 {
                        black_box(batches.copies(k, false));
                    }
                }
                for draw in batches.draws() {
                    black_box(draw);
                }
            }
            eprintln!(
                "100k mixed={mixed}: init runs={before_init:?}, batches+copy-plan={after_init:?}; 1000 sparse updates+draw traversal: runs={before:?}, batches={:?}; draws {} -> {}; initial GPU copy commands={}",
                start.elapsed(),
                runs.draws().count(),
                batches.draws().count(),
                if mixed {
                    (0..3).map(|k| batches.copies(k, true).len()).sum::<usize>()
                } else {
                    0
                }
            );
        }
    }
    #[test]
    fn subdivision_draws_group_specs_and_invalidate_only_changed_gather_topology() {
        let mut data: Vec<_> = (0..1200).map(|i| instance(i, i % 3)).collect();
        let mut batches = GeometryBatches::default();
        batches.sync(&data, std::iter::once(0..data.len()));
        let mut plans = crate::gather::PlanCache::default();
        let limits = wgpu::Limits {
            max_storage_buffer_binding_size: 2560,
            ..Default::default()
        };
        assert!(matches!(
            plans.prepare(&limits, [160000, 160000], &batches, 1, true),
            crate::gather::Prepared::Gather { upload: true, .. }
        ));
        for step in 0..300 {
            let index = step % data.len();
            let before = batches.revision(1);
            let kind = data[index].geometry_kind();
            if kind != GeometryKind::Box {
                data[index].color[3] =
                    GeometrySpec::new(kind, [3 + (step % 13) as u16, 2 + (step % 9) as u16])
                        .unwrap()
                        .tag() as f32;
            }
            batches.sync(&data, std::iter::once(index..index + 1));
            let upload = match plans.prepare(&limits, [160000, 160000], &batches, 1, true) {
                crate::gather::Prepared::Gather { upload, .. } => upload,
                _ => panic!("fragmented bank should gather"),
            };
            assert_eq!(upload, batches.revision(1) != before);
            let unique: std::collections::BTreeSet<_> =
                data.iter().map(Instance::geometry_spec).collect();
            assert_eq!(batches.geometry_draws().count(), unique.len());
            for (spec, range) in batches.geometry_draws() {
                for offset in range {
                    let source = batches.indices(spec.kind as usize)[offset as usize] as usize;
                    assert_eq!(data[source].geometry_spec(), spec);
                }
            }
            assert_eq!(
                batches
                    .geometry_draws()
                    .map(|(_, r)| r.len())
                    .sum::<usize>(),
                data.len()
            );
        }
        // One kind with several specs must use bank order, including after swap-remove permutations.
        data.iter_mut().for_each(|i| {
            i.color[3] = GeometrySpec::new(GeometryKind::Plane, [2, 3])
                .unwrap()
                .tag() as f32
        });
        batches.sync(&data, std::iter::once(0..data.len()));
        assert!(!batches.requires_banks());
        data[1].color[3] = GeometrySpec::new(GeometryKind::Plane, [7, 9])
            .unwrap()
            .tag() as f32;
        batches.sync(&data, std::iter::once(1..2));
        assert!(batches.requires_banks());
        data.truncate(2);
        batches.sync(&data, []);
        for (spec, r) in batches.geometry_draws() {
            for offset in r {
                assert_eq!(
                    data[batches.indices(1)[offset as usize] as usize].geometry_spec(),
                    spec
                );
            }
        }
    }
    #[test]
    #[ignore = "CPU custom-spec benchmark; no GPU execution"]
    fn custom_spec_benchmark() {
        use std::time::Instant;
        let mut data: Vec<_> = (0..100000)
            .map(|i| {
                let mut value = instance(i, 1);
                value.color[3] = GeometrySpec::new(GeometryKind::Plane, [2 + (i % 4) as u16, 3])
                    .unwrap()
                    .tag() as f32;
                value
            })
            .collect();
        let mut batches = GeometryBatches::default();
        let start = Instant::now();
        batches.sync(&data, std::iter::once(0..data.len()));
        let initial = start.elapsed().as_micros();
        let draws = batches.geometry_draws().count();
        let start = Instant::now();
        for step in 0..100 {
            data[step].color[0] += 1.;
            batches.sync(&data, std::iter::once(step..step + 1));
            std::hint::black_box(batches.geometry_draws().count());
        }
        println!(
            "custom_specs,100000,{initial},{},{draws},{}",
            start.elapsed().as_micros(),
            batches.transfer_count(1, false)
        );
    }
    #[test]
    fn custom_spec_repacking_copies_preserve_bytes_and_sparse_edits() {
        let mut data = Vec::new();
        let mut batches = GeometryBatches::default();
        let mut gpu: [Vec<Instance>; 3] = Default::default();
        let mut seed = 7u32;
        for step in 0..600 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let mut ranges = Vec::new();
            if step % 5 == 0 && !data.is_empty() {
                let index = seed as usize % data.len();
                data.swap_remove(index);
                if index < data.len() {
                    ranges.push(index..index + 1);
                }
            } else {
                let kind = if seed.is_multiple_of(2) {
                    GeometryKind::Plane
                } else {
                    GeometryKind::Sphere
                };
                let mut value = instance(step, kind as usize);
                value.color[3] =
                    GeometrySpec::new(kind, [3 + (seed % 5) as u16, 2 + (seed % 3) as u16])
                        .unwrap()
                        .tag() as f32;
                if step % 3 == 0 && !data.is_empty() {
                    let index = seed as usize % data.len();
                    data[index] = value;
                    ranges.push(index..index + 1);
                } else {
                    data.push(value);
                }
            }
            batches.sync(&data, ranges);
            verify(&batches, &data);
            let unique: std::collections::BTreeSet<_> =
                data.iter().map(Instance::geometry_spec).collect();
            assert_eq!(batches.geometry_draws().count(), unique.len());
            for (kind, bank) in gpu.iter_mut().enumerate() {
                bank.resize(batches.indices(kind).len(), instance(0, kind));
                for copy in batches.copies(kind, false) {
                    bank[copy.destination..copy.destination + copy.count]
                        .copy_from_slice(&data[copy.source..copy.source + copy.count]);
                }
                for (offset, &source) in batches.indices(kind).iter().enumerate() {
                    assert_eq!(
                        bytemuck::bytes_of(&bank[offset]),
                        bytemuck::bytes_of(&data[source as usize])
                    );
                }
            }
        }
        let before = batches.revisions;
        data[0].color[0] += 1.;
        batches.sync(&data, std::iter::once(0..1));
        assert_eq!(batches.revisions, before);
        assert_eq!(
            (0..3)
                .map(|k| batches.transfer_count(k, false))
                .sum::<usize>(),
            1
        );
        batches.sync(&data, []);
        assert_eq!(
            (0..3)
                .map(|k| batches.transfer_count(k, false))
                .sum::<usize>(),
            0
        );
    }
    #[test]
    #[ignore = "CPU local spec repacking benchmark; no GPU execution"]
    fn local_spec_benchmark() {
        use std::time::Instant;
        for count in [10000, 100000] {
            let mut data: Vec<_> = (0..count)
                .map(|i| {
                    let mut value = instance(i, 1);
                    value.color[3] = GeometrySpec::new(GeometryKind::Plane, [2 + (i % 4) as u16, 3])
                        .unwrap()
                        .tag() as f32;
                    value
                })
                .collect();
            let mut batches = GeometryBatches::default();
            let start = Instant::now();
            batches.sync(&data, std::iter::once(0..count));
            println!(
                "local_spec,{count},initial,{},{}",
                start.elapsed().as_nanos(),
                batches.transfer_count(1, false)
            );
            for operation in ["spec", "append_remove", "rgb"] {
                let start = Instant::now();
                let mut transferred = 0;
                for step in 0..100 {
                    match operation {
                        "spec" => {
                            data[0].color[3] = GeometrySpec::new(
                                GeometryKind::Plane,
                                [if step % 2 == 0 { 5 } else { 2 }, 3],
                            )
                            .unwrap()
                            .tag() as f32
                        }
                        "append_remove" => data.push(data[0]),
                        _ => data[0].color[0] += 1.,
                    }
                    batches.sync(&data, std::iter::once(0..1));
                    transferred += batches.transfer_count(1, false);
                    if operation == "append_remove" {
                        data.pop();
                        batches.sync(&data, []);
                        transferred += batches.transfer_count(1, false);
                    }
                    std::hint::black_box(batches.geometry_draws().count());
                }
                println!(
                    "local_spec,{count},{operation},{},{transferred}",
                    start.elapsed().as_nanos()
                );
            }
        }
    }
    #[test]
    fn local_repacking_is_bounded_by_specs_and_preserves_tag_only_cache() {
        let tag = |width| {
            GeometrySpec::new(GeometryKind::Plane, [width, 3])
                .unwrap()
                .tag() as f32
        };
        let mut data: Vec<_> = (0..100000)
            .map(|i| {
                let mut value = instance(i, 1);
                value.color[3] = tag(2 + (i % 4) as u16);
                value
            })
            .collect();
        let mut batches = GeometryBatches::default();
        batches.sync(&data, []);
        for width in [5, 2, 17, 5, 17, 2] {
            data[0].color[3] = tag(width);
            batches.sync(&data, std::iter::once(0..1));
            verify(&batches, &data);
            let groups = batches.geometry_draws().count();
            assert!(batches.transfer_count(1, false) <= 4 * groups + 4);
            assert!(!batches.whole_bank(1, false));
        }
        data.push(data[0]);
        batches.sync(&data, []);
        assert!(batches.transfer_count(1, false) <= 10);
        data.pop();
        batches.sync(&data, []);
        assert!(batches.transfer_count(1, false) <= 10);
        data.clear();
        batches.sync(&data, []);
        assert_eq!(batches.geometry_draws().count(), 0);
        assert_eq!(batches.transfer_count(1, false), 0);
        data.push(instance(0, 1));
        batches.sync(&data, []);
        let revision = batches.revision(1);
        data[0].color[3] = tag(19);
        batches.sync(&data, std::iter::once(0..1));
        assert_eq!(batches.revision(1), revision);
        assert_eq!(batches.indices(1), &[0]);
        verify(&batches, &data);
    }
}
