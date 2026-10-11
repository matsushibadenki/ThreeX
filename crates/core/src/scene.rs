use crate::{Instance, Object, local, multiply, transform_store::TransformStore};
use crate::{
    InstanceUpdates, SceneStamp,
    instance_updates::{InstanceChanges, InstanceJournal},
};
use std::{collections::BTreeMap, sync::Arc};
const WORLD: u8 = 1;
const VISIBILITY: u8 = 2;
const INSTANCE: u8 = 4;
/// Work performed by the last successful batch (empty/no-op batches report zero).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UpdateStats {
    pub local_transforms: usize,
    pub world_transforms: usize,
    pub visibility_updates: usize,
    pub instance_writes: usize,
    pub packing_rebuilds: usize,
    pub instance_additions: usize,
    pub instance_removals: usize,
    pub instance_moves: usize,
    pub hierarchy_rebuilds: usize,
}
#[derive(Default)]
pub struct Scene {
    indices: BTreeMap<u32, usize>,
    store: TransformStore,
    packed: Vec<Instance>,
    packed_slots: Vec<usize>,
    pub revision: u64,
    stats: UpdateStats,
    identity: Arc<()>,
    journal: InstanceJournal,
}
pub(crate) struct SceneEdit {
    changes: BTreeMap<u32, Option<Object>>,
    structural: bool,
}
impl SceneEdit {
    pub(crate) fn changes_overlay(
        &self,
        scene: &Scene,
        overlay: &BTreeMap<u32, Option<Object>>,
    ) -> bool {
        self.changes.iter().any(|(&id, &value)| {
            value
                != overlay
                    .get(&id)
                    .copied()
                    .unwrap_or_else(|| scene.object(id))
        })
    }
    pub(crate) fn extend_overlay(&self, overlay: &mut BTreeMap<u32, Option<Object>>) {
        overlay.extend(self.changes.iter().map(|(&id, &value)| (id, value)));
    }
    pub(crate) fn changes_scene(&self, scene: &Scene) -> bool {
        self.changes
            .iter()
            .any(|(&id, &value)| scene.object(id) != value)
    }
}
impl Scene {
    pub fn stamp(&self) -> SceneStamp {
        SceneStamp {
            identity: self.identity.clone(),
            revision: self.revision,
        }
    }
    pub fn is_current(&self, stamp: Option<&SceneStamp>) -> bool {
        stamp.is_some_and(|stamp| {
            stamp.revision == self.revision && Arc::ptr_eq(&stamp.identity, &self.identity)
        })
    }
    pub fn instance_updates_since(
        &self,
        previous: Option<&SceneStamp>,
        force_full: bool,
    ) -> InstanceUpdates {
        self.journal
            .updates(self.stamp(), previous, self.packed.len(), force_full)
    }
    pub fn len(&self) -> usize {
        self.indices.len()
    }
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }
    /// Explicit document snapshot iteration; rendering uses retained instances instead.
    pub fn objects(&self) -> impl Iterator<Item = (u32, Object)> + '_ {
        self.indices
            .iter()
            .map(|(&id, &slot)| (id, self.store.object(slot)))
    }
    pub fn object(&self, id: u32) -> Option<Object> {
        self.indices.get(&id).map(|&slot| self.store.object(slot))
    }
    pub fn world_transform(&self, id: u32) -> Option<[[f32; 4]; 4]> {
        self.indices.get(&id).map(|&slot| self.store.worlds[slot])
    }
    pub fn update_stats(&self) -> UpdateStats {
        self.stats
    }
    /// Retained render data; no allocation, matrix evaluation, or traversal.
    pub fn instances(&self) -> &[Instance] {
        &self.packed
    }
    /// Dense render indices are transient, never entity identifiers or draw-order keys.
    pub fn instance_index(&self, id: u32) -> Option<usize> {
        self.indices
            .get(&id)
            .and_then(|&slot| self.store.packed_indices[slot])
    }
    pub fn apply(&mut self, bytes: &[u8]) -> Result<(), String> {
        let edit = self.prepare(bytes)?;
        self.apply_edit(edit);
        Ok(())
    }
    pub(crate) fn prepare(&self, bytes: &[u8]) -> Result<SceneEdit, String> {
        self.prepare_overlay(bytes, &BTreeMap::new())
    }
    pub(crate) fn prepare_overlay(
        &self,
        bytes: &[u8],
        overlay: &BTreeMap<u32, Option<Object>>,
    ) -> Result<SceneEdit, String> {
        let prior = |id| overlay.get(&id).copied().unwrap_or_else(|| self.object(id));
        if !bytes.len().is_multiple_of(64) {
            return Err("Truncated command buffer".into());
        }
        let mut changes: BTreeMap<u32, Option<Object>> = BTreeMap::new();
        let mut structural = false;
        for c in bytes.as_chunks::<64>().0 {
            let u = |n: usize| u32::from_le_bytes(c[n..n + 4].try_into().unwrap());
            let f = |n: usize| f32::from_le_bytes(c[n..n + 4].try_into().unwrap());
            let id = u(4);
            if id == 0 {
                return Err("Reserved entity ID".into());
            }
            let current = changes.get(&id).copied().unwrap_or_else(|| prior(id));
            match u(0) {
                1 => {
                    if current.is_some() {
                        return Err("Duplicate entity".into());
                    }
                    changes.insert(id, Some(Object::default()));
                    structural = true;
                }
                2 => {
                    let mut o = current.ok_or("Unknown entity")?;
                    let values: [f32; 12] = std::array::from_fn(|i| f(8 + i * 4));
                    if values.iter().any(|v| !v.is_finite()) {
                        return Err("Non-finite transform".into());
                    }
                    if u(56) & !3 != 0 {
                        return Err("Unknown flags".into());
                    }
                    structural |= o.parent != u(60);
                    o.position.copy_from_slice(&values[..3]);
                    o.rotation.copy_from_slice(&values[3..6]);
                    o.scale.copy_from_slice(&values[6..9]);
                    o.color.copy_from_slice(&values[9..12]);
                    o.visible = u(56) & 1 != 0;
                    o.mesh = u(56) & 2 != 0;
                    o.parent = u(60);
                    changes.insert(id, Some(o));
                }
                3 => {
                    current.ok_or("Unknown entity")?;
                    changes.insert(id, None);
                    structural = true;
                }
                _ => return Err("Unknown opcode".into()),
            }
        }
        if structural {
            let lookup = |id: u32| changes.get(&id).copied().unwrap_or_else(|| prior(id));
            // Iterative coloring validates final hierarchy without recursion or
            // walking the same long ancestor chain once for every descendant.
            let mut states = BTreeMap::<u32, u8>::new();
            let mut path = Vec::new();
            for &id in self
                .indices
                .keys()
                .chain(overlay.keys())
                .chain(changes.keys())
            {
                let Some(object) = lookup(id) else {
                    continue;
                };
                if object.parent == 0 || states.get(&id) == Some(&2) {
                    continue;
                }
                let mut at = id;
                while at != 0 {
                    match states.get(&at) {
                        Some(1) => return Err("Hierarchy cycle".into()),
                        Some(2) => break,
                        _ => {}
                    }
                    let o = lookup(at).ok_or("Missing parent")?;
                    if o.parent == 0 {
                        break;
                    }
                    states.insert(at, 1);
                    path.push(at);
                    at = o.parent;
                }
                for id in path.drain(..) {
                    states.insert(id, 2);
                }
            }
        }
        Ok(SceneEdit {
            changes,
            structural,
        })
    }
    pub(crate) fn apply_edit(&mut self, edit: SceneEdit) {
        let SceneEdit {
            changes,
            structural,
        } = edit;
        // Validation complete. No fallible operation modifies live scene state.
        self.stats = UpdateStats::default();
        let new_count = changes
            .iter()
            .filter(|(id, value)| value.is_some() && !self.indices.contains_key(id))
            .count();
        if new_count > 0 {
            self.store.reserve_new(new_count);
        }
        let mut seeds = Vec::new();
        let mut instance_changes = InstanceChanges::new(false);
        let mut changed = false;
        for (id, value) in changes {
            let old = self.object(id);
            if old == value {
                continue;
            }
            changed = true;
            match value {
                None => {
                    let slot = self.indices.remove(&id).unwrap();
                    self.remove_instance(slot, &mut instance_changes);
                    self.store.remove(slot);
                }
                Some(o) => {
                    let slot = if let Some(&slot) = self.indices.get(&id) {
                        slot
                    } else {
                        let slot = self.store.allocate(id);
                        self.indices.insert(id, slot);
                        slot
                    };
                    let local_dirty = old.is_none_or(|old| {
                        old.position != o.position
                            || old.rotation != o.rotation
                            || old.scale != o.scale
                    });
                    let parent_dirty = old.is_none_or(|old| old.parent != o.parent);
                    let visibility_dirty =
                        parent_dirty || old.is_none_or(|old| old.visible != o.visible);
                    self.store.write(slot, o);
                    if local_dirty {
                        self.store.locals[slot] = local(&o);
                        self.stats.local_transforms += 1;
                    }
                    let flags = INSTANCE
                        | if local_dirty || parent_dirty {
                            WORLD
                        } else {
                            0
                        }
                        | if visibility_dirty { VISIBILITY } else { 0 };
                    seeds.push((slot, flags));
                }
            }
        }
        if !changed {
            return;
        }
        if structural {
            for children in &mut self.store.children {
                children.clear();
            }
            for &slot in self.indices.values() {
                let parent = self.store.parent_ids[slot];
                self.store.parents[slot] = if parent == 0 {
                    None
                } else {
                    Some(self.indices[&parent])
                };
                if let Some(parent) = self.store.parents[slot] {
                    self.store.children[parent].push(slot);
                }
            }
            self.stats.hierarchy_rebuilds = 1;
        }
        let mut touched = Vec::new();
        let mut queue = Vec::new();
        for (slot, flags) in seeds {
            self.mark(slot, flags, &mut touched, &mut queue);
        }
        let mut cursor = 0;
        while cursor < queue.len() {
            let (slot, flags) = queue[cursor];
            cursor += 1;
            for i in 0..self.store.children[slot].len() {
                let child = self.store.children[slot][i];
                self.mark(child, flags | INSTANCE, &mut touched, &mut queue);
            }
        }
        let mut path = Vec::new();
        for &slot in &touched {
            let mut at = Some(slot);
            while let Some(slot) = at {
                if self.store.dirty[slot] & (WORLD | VISIBILITY) == 0 {
                    break;
                }
                path.push(slot);
                at = self.store.parents[slot];
            }
            while let Some(slot) = path.pop() {
                let flags = self.store.dirty[slot];
                let parent = self.store.parents[slot];
                if flags & WORLD != 0 {
                    self.store.worlds[slot] = parent.map_or(self.store.locals[slot], |parent| {
                        multiply(self.store.worlds[parent], self.store.locals[slot])
                    });
                    self.stats.world_transforms += 1;
                }
                if flags & VISIBILITY != 0 {
                    let visible = self.store.visible[slot]
                        && parent.is_none_or(|parent| self.store.world_visible[parent]);
                    self.store.world_visible[slot] = visible;
                    self.stats.visibility_updates += 1;
                }
                self.store.dirty[slot] &= !(WORLD | VISIBILITY);
            }
        }
        for &slot in &touched {
            if self.store.meshes[slot] && self.store.world_visible[slot] {
                let index = if let Some(index) = self.store.packed_indices[slot] {
                    self.packed[index] = self.instance(slot);
                    index
                } else {
                    let index = self.packed.len();
                    self.packed.push(self.instance(slot));
                    self.packed_slots.push(slot);
                    self.store.packed_indices[slot] = Some(index);
                    self.stats.instance_additions += 1;
                    index
                };
                instance_changes.push(index);
                self.stats.instance_writes += 1;
            } else {
                self.remove_instance(slot, &mut instance_changes);
            }
        }
        for slot in touched {
            self.store.dirty[slot] = 0;
        }
        self.revision += 1;
        self.journal.record(self.revision, instance_changes);
    }
    fn remove_instance(&mut self, slot: usize, changes: &mut InstanceChanges) {
        let Some(index) = self.store.packed_indices[slot].take() else {
            return;
        };
        self.packed.swap_remove(index);
        self.packed_slots.swap_remove(index);
        self.stats.instance_removals += 1;
        if index < self.packed.len() {
            let moved = self.packed_slots[index];
            self.store.packed_indices[moved] = Some(index);
            changes.push(index);
            self.stats.instance_moves += 1;
            self.stats.instance_writes += 1;
        }
    }
    fn instance(&self, slot: usize) -> Instance {
        let color = self.store.colors[slot];
        Instance {
            model: self.store.worlds[slot],
            color: [color[0], color[1], color[2], 1.],
        }
    }
    fn mark(
        &mut self,
        slot: usize,
        flags: u8,
        touched: &mut Vec<usize>,
        queue: &mut Vec<(usize, u8)>,
    ) {
        let old = self.store.dirty[slot];
        if old == 0 {
            touched.push(slot);
        }
        self.store.dirty[slot] |= flags;
        let added = (flags & !old) & (WORLD | VISIBILITY);
        if added != 0 {
            queue.push((slot, added));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn command(op: u32, id: u32, o: Object) -> [u8; 64] {
        let mut b = [0; 64];
        for (at, v) in [
            (0, op),
            (4, id),
            (56, u32::from(o.visible) | (u32::from(o.mesh) << 1)),
            (60, o.parent),
        ] {
            b[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }
        let values = [o.position, o.rotation, o.scale, o.color].concat();
        for (i, v) in values.into_iter().enumerate() {
            b[8 + i * 4..12 + i * 4].copy_from_slice(&v.to_le_bytes());
        }
        b
    }
    fn add(scene: &mut Scene, objects: &[(u32, Object)]) {
        let mut bytes = Vec::new();
        for &(id, o) in objects {
            bytes.extend(command(1, id, Object::default()));
            bytes.extend(command(2, id, o));
        }
        scene.apply(&bytes).unwrap();
    }
    fn mesh(parent: u32, x: f32) -> Object {
        Object {
            parent,
            position: [x, 0., 0.],
            mesh: true,
            ..Default::default()
        }
    }
    #[test]
    fn one_leaf_and_material_do_not_rebuild_unrelated_data() {
        let mut scene = Scene::default();
        let objects: Vec<_> = (1..=100_000).map(|id| (id, mesh(0, 0.))).collect();
        add(&mut scene, &objects);
        let pointer = scene.instances().as_ptr();
        scene.apply(&command(2, 50_000, mesh(0, 7.))).unwrap();
        assert_eq!(
            scene.update_stats(),
            UpdateStats {
                local_transforms: 1,
                world_transforms: 1,
                instance_writes: 1,
                ..Default::default()
            }
        );
        assert_eq!(scene.instances().as_ptr(), pointer);
        assert_eq!(scene.instances()[49_999].model[3][0], 7.);
        let mut o = scene.object(50_000).unwrap();
        o.color = [0., 0.5, 1.];
        scene.apply(&command(2, 50_000, o)).unwrap();
        assert_eq!(
            scene.update_stats(),
            UpdateStats {
                instance_writes: 1,
                ..Default::default()
            }
        );
        let revision = scene.revision;
        scene.apply(&command(2, 50_000, o)).unwrap();
        assert_eq!(scene.revision, revision);
        assert_eq!(scene.update_stats(), UpdateStats::default());
        scene.apply(&[]).unwrap();
        assert_eq!(scene.instances().as_ptr(), pointer);
        assert_eq!(scene.update_stats(), UpdateStats::default());
        let mut reparent = Vec::new();
        for id in 2..=11 {
            reparent.extend(command(2, id, mesh(1, 0.)));
        }
        scene.apply(&reparent).unwrap();
        scene.apply(&command(2, 1, mesh(0, 3.))).unwrap();
        assert_eq!(
            scene.update_stats(),
            UpdateStats {
                local_transforms: 1,
                world_transforms: 11,
                instance_writes: 11,
                ..Default::default()
            }
        );
    }
    #[test]
    fn parent_and_child_in_one_batch_are_evaluated_once_in_parent_order() {
        let mut scene = Scene::default();
        add(
            &mut scene,
            &[
                (50, mesh(0, 2.)),
                (1, mesh(50, 3.)),
                (2, mesh(1, 4.)),
                (100, mesh(0, 8.)),
            ],
        );
        let unrelated = scene.world_transform(100).unwrap();
        scene
            .apply(&[command(2, 1, mesh(50, 5.)), command(2, 50, mesh(0, 7.))].concat())
            .unwrap();
        assert_eq!(scene.world_transform(2).unwrap()[3][0], 16.);
        assert_eq!(scene.world_transform(100).unwrap(), unrelated);
        assert_eq!(
            scene.update_stats(),
            UpdateStats {
                local_transforms: 2,
                world_transforms: 3,
                instance_writes: 3,
                ..Default::default()
            }
        );
    }
    #[test]
    fn reparent_delete_and_slot_reuse_preserve_worlds_and_entity_mapping() {
        let mut scene = Scene::default();
        add(
            &mut scene,
            &[
                (10, mesh(0, 2.)),
                (20, mesh(0, 10.)),
                (1, mesh(10, 3.)),
                (2, mesh(1, 4.)),
            ],
        );
        scene
            .apply(
                &[
                    command(3, 10, Object::default()),
                    command(2, 1, mesh(20, 3.)),
                ]
                .concat(),
            )
            .unwrap();
        assert_eq!(scene.world_transform(2).unwrap()[3][0], 17.);
        assert_eq!(scene.update_stats().local_transforms, 0);
        assert_eq!(scene.update_stats().world_transforms, 2);
        let before = scene.instances().to_vec();
        let rev = scene.revision;
        let stats = scene.update_stats();
        assert!(scene.apply(&command(3, 20, Object::default())).is_err());
        assert_eq!(scene.revision, rev);
        assert_eq!(scene.update_stats(), stats);
        assert_eq!(
            bytemuck::cast_slice::<_, u8>(scene.instances()),
            bytemuck::cast_slice::<_, u8>(&before)
        );
        add(&mut scene, &[(5, mesh(0, -4.))]);
        assert_eq!(
            scene.instances()[scene.instance_index(5).unwrap()].model[3][0],
            -4.
        );
        assert_eq!(scene.world_transform(5).unwrap()[3][0], -4.);
        assert_eq!(scene.instances().len(), scene.len());
    }
    #[test]
    fn visibility_propagates_without_matrix_work() {
        let mut scene = Scene::default();
        add(
            &mut scene,
            &[
                (1, mesh(0, 1.)),
                (2, mesh(1, 2.)),
                (3, mesh(2, 3.)),
                (4, mesh(0, 4.)),
            ],
        );
        let mut parent = scene.object(1).unwrap();
        parent.visible = false;
        scene.apply(&command(2, 1, parent)).unwrap();
        assert_eq!(scene.instances().len(), 1);
        assert_eq!(scene.update_stats().world_transforms, 0);
        assert_eq!(scene.update_stats().local_transforms, 0);
        assert_eq!(scene.update_stats().visibility_updates, 3);
        let mut child = scene.object(2).unwrap();
        child.visible = false;
        scene.apply(&command(2, 2, child)).unwrap();
        assert_eq!(scene.update_stats().packing_rebuilds, 0);
        parent.visible = true;
        scene.apply(&command(2, 1, parent)).unwrap();
        assert_eq!(scene.instances().len(), 2);
        child.visible = true;
        scene.apply(&command(2, 2, child)).unwrap();
        assert_eq!(scene.instances().len(), 4);
        assert_eq!(
            scene.instances()[scene.instance_index(3).unwrap()].model[3][0],
            6.
        );
    }
    #[test]
    fn deep_hierarchy_is_iterative() {
        let mut scene = Scene::default();
        let n = 20_000;
        let objects: Vec<_> = (1..=n)
            .map(|id| (id, mesh(if id == n { 0 } else { id + 1 }, 1.)))
            .collect();
        add(&mut scene, &objects);
        assert_eq!(scene.world_transform(1).unwrap()[3][0], n as f32);
        scene.apply(&command(2, n, mesh(0, 2.))).unwrap();
        assert_eq!(scene.world_transform(1).unwrap()[3][0], n as f32 + 1.);
        assert_eq!(scene.update_stats().local_transforms, 1);
        assert_eq!(scene.update_stats().world_transforms, n as usize);
    }
    #[test]
    fn incremental_results_match_full_reference_after_mixed_updates() {
        let mut scene = Scene::default();
        let mut reference = BTreeMap::new();
        let objects: Vec<_> = (1..=120)
            .map(|id| (id, mesh(if id > 1 { id / 2 } else { 0 }, id as f32 / 100.)))
            .collect();
        add(&mut scene, &objects);
        reference.extend(objects);
        let mut gpu = scene.instances().to_vec();
        let mut gpu_stamp = Some(scene.stamp());
        let mut seed = 0x12345678u32;
        for step in 0..600 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let id = seed % 120 + 1;
            let mut o = reference[&id];
            match step % 7 {
                0 => o.position[1] = step as f32 / 50.,
                1 => o.rotation = [0.15, step as f32 / 100., -0.3],
                2 => o.scale = [-1.2, 0.5, 2.],
                3 => o.color = [0.4, 0.2, (step % 3) as f32 / 3.],
                4 => o.visible = !o.visible,
                5 => o.parent = if id > 1 { seed % (id - 1) } else { 0 },
                _ => o.mesh = !o.mesh,
            }
            reference.insert(id, o);
            scene.apply(&command(2, id, o)).unwrap();
            if step % 7 == 0 {
                synchronize(&scene, &mut gpu_stamp, &mut gpu, false);
            }
            let mut expected = BTreeMap::new();
            for (&id, o) in &reference {
                let mut chain = Vec::new();
                let mut at = id;
                let mut visible = true;
                while at != 0 {
                    let o = reference[&at];
                    chain.push(o);
                    visible &= o.visible;
                    at = o.parent;
                }
                let mut world = crate::identity();
                for o in chain.iter().rev() {
                    world = multiply(world, local(o));
                }
                let actual = scene.world_transform(id).unwrap();
                for (a, b) in actual.iter().flatten().zip(world.iter().flatten()) {
                    assert!((a - b).abs() < 0.0001, "step {step}, entity {id}");
                }
                if o.mesh && visible {
                    expected.insert(
                        id,
                        Instance {
                            model: world,
                            color: [o.color[0], o.color[1], o.color[2], 1.],
                        },
                    );
                }
            }
            assert_eq!(scene.instances().len(), expected.len());
            for (id, b) in expected {
                let a = &scene.instances()[scene.instance_index(id).unwrap()];
                assert_eq!(a.color, b.color);
                for (a, b) in a.model.iter().flatten().zip(b.model.iter().flatten()) {
                    assert!((a - b).abs() < 0.0001);
                }
            }
        }
    }
    #[test]
    fn universal_rotation_and_world_mirror_survive_incremental_updates() {
        let mut scene = Scene::default();
        let root = Object {
            scale: [-1., 1., 1.],
            ..Default::default()
        };
        let mut object = Object {
            parent: 10,
            position: [2., -3., 1.],
            rotation: crate::universal_rotation([25., 40., -15.]).unwrap(),
            scale: [2., 0.5, -1.5],
            mesh: true,
            ..Default::default()
        };
        add(&mut scene, &[(10, root), (1, object)]);
        for degrees in [[25., 40., -15.], [0., 90., 0.], [90., -90., 25.]] {
            object.rotation = crate::universal_rotation(degrees).unwrap();
            scene.apply(&command(2, 1, object)).unwrap();
            let point = [0.3, -0.4, 0.7];
            let mut expected = std::array::from_fn::<_, 3, _>(|i| point[i] * object.scale[i]);
            for (axis, degree) in degrees.into_iter().enumerate() {
                let a = (axis + 1) % 3;
                let b = (axis + 2) % 3;
                let (s, c) = degree.to_radians().sin_cos();
                let x = expected[a] * c - expected[b] * s;
                expected[b] = expected[a] * s + expected[b] * c;
                expected[a] = x;
            }
            for (i, p) in expected.iter_mut().enumerate() {
                *p += object.position[i];
            }
            expected[0] *= -1.;
            let model = scene.instances()[0].model;
            for (i, expected) in expected.iter().enumerate() {
                let actual = model[0][i] * point[0]
                    + model[1][i] * point[1]
                    + model[2][i] * point[2]
                    + model[3][i];
                assert!((actual - expected).abs() < 0.00001);
            }
        }
    }
    fn synchronize(
        scene: &Scene,
        stamp: &mut Option<SceneStamp>,
        target: &mut Vec<Instance>,
        force: bool,
    ) -> InstanceUpdates {
        let plan = scene.instance_updates_since(stamp.as_ref(), force);
        target.resize(
            scene.instances().len(),
            Instance {
                model: crate::identity(),
                color: [0.; 4],
            },
        );
        for range in &plan.ranges {
            target[range.clone()].copy_from_slice(&scene.instances()[range.clone()]);
        }
        *stamp = Some(plan.stamp.clone());
        assert_eq!(
            bytemuck::cast_slice::<_, u8>(target),
            bytemuck::cast_slice::<_, u8>(scene.instances())
        );
        plan
    }
    #[test]
    fn delta_journal_merges_skipped_updates_for_independent_renderers() {
        let mut scene = Scene::default();
        let objects: Vec<_> = (1..=100_000).map(|id| (id, mesh(0, 0.))).collect();
        add(&mut scene, &objects);
        let mut fast = None;
        let mut slow = None;
        let mut a = Vec::new();
        let mut b = Vec::new();
        assert!(synchronize(&scene, &mut fast, &mut a, false).full);
        synchronize(&scene, &mut slow, &mut b, false);
        scene.apply(&command(2, 500, mesh(0, 2.))).unwrap();
        assert_eq!(
            synchronize(&scene, &mut fast, &mut a, false).ranges,
            vec![499..500]
        );
        scene.apply(&command(2, 501, mesh(0, 3.))).unwrap();
        scene.apply(&command(2, 800, mesh(0, 4.))).unwrap();
        scene.apply(&[]).unwrap();
        let plan = synchronize(&scene, &mut slow, &mut b, false);
        assert!(!plan.full);
        assert_eq!(plan.ranges, vec![499..501, 799..800]);
        synchronize(&scene, &mut fast, &mut a, false);
        assert!(
            scene
                .instance_updates_since(fast.as_ref(), false)
                .ranges
                .is_empty()
        );
        assert!(synchronize(&scene, &mut fast, &mut a, true).full);
    }
    #[test]
    fn repacking_empty_growth_scene_switch_and_expired_history_are_safe() {
        let mut scene = Scene::default();
        add(&mut scene, &[(1, mesh(0, 1.)), (2, mesh(0, 2.))]);
        let mut stamp = None;
        let mut target = Vec::new();
        synchronize(&scene, &mut stamp, &mut target, false);
        let mut o = mesh(0, 1.);
        o.visible = false;
        scene.apply(&command(2, 1, o)).unwrap();
        assert!(!synchronize(&scene, &mut stamp, &mut target, false).full);
        assert_eq!(target.len(), 1);
        scene.apply(&command(3, 2, Object::default())).unwrap();
        let plan = synchronize(&scene, &mut stamp, &mut target, false);
        assert!(!plan.full);
        assert!(plan.ranges.is_empty());
        assert!(target.is_empty());
        add(&mut scene, &[(3, mesh(0, 5.))]);
        assert!(synchronize(&scene, &mut stamp, &mut target, true).full);
        let saved = stamp.clone();
        for x in 6..76 {
            scene.apply(&command(2, 3, mesh(0, x as f32))).unwrap();
        }
        assert!(scene.instance_updates_since(saved.as_ref(), false).full);
        synchronize(&scene, &mut stamp, &mut target, false);
        let mut other = Scene::default();
        add(&mut other, &[(3, mesh(0, 99.))]);
        assert!(!other.is_current(stamp.as_ref()));
        assert!(synchronize(&other, &mut stamp, &mut target, false).full);
        let mut equal_revision = Scene::default();
        add(&mut equal_revision, &[(3, mesh(0, 100.))]);
        assert_eq!(equal_revision.revision, other.revision);
        assert!(!equal_revision.is_current(stamp.as_ref()));
        assert!(synchronize(&equal_revision, &mut stamp, &mut target, false).full);
    }
    #[test]
    fn membership_swaps_shrinks_and_reused_slots_sync_without_full_upload() {
        let mut scene = Scene::default();
        add(
            &mut scene,
            &(1..=100_000)
                .map(|id| (id, mesh(0, id as f32)))
                .collect::<Vec<_>>(),
        );
        let mut fast = Some(scene.stamp());
        let mut slow = fast.clone();
        let mut a = scene.instances().to_vec();
        let mut b = a.clone();
        let pointer = scene.instances().as_ptr();
        let mut hidden = mesh(0, 1.);
        hidden.visible = false;
        scene.apply(&command(2, 1, hidden)).unwrap();
        let stats = scene.update_stats();
        assert_eq!(stats.instance_removals, 1);
        assert_eq!(stats.instance_moves, 1);
        assert_eq!(stats.instance_writes, 1);
        assert_eq!(stats.packing_rebuilds, 0);
        assert_eq!(scene.instance_index(100_000), Some(0));
        let plan = synchronize(&scene, &mut fast, &mut a, false);
        assert!(!plan.full);
        assert_eq!(plan.ranges, vec![0..1]);
        // Update the old tail, then remove it before the slow renderer catches up.
        scene.apply(&command(2, 99_999, mesh(0, -7.))).unwrap();
        scene.apply(&command(3, 99_999, Object::default())).unwrap();
        assert_eq!(scene.update_stats().instance_writes, 0);
        let plan = synchronize(&scene, &mut slow, &mut b, false);
        assert!(!plan.full);
        assert_eq!(plan.ranges, vec![0..1]);
        // A delete and create in one batch reuses a SoA slot while preserving mappings.
        scene
            .apply(
                &[
                    command(3, 2, Object::default()),
                    command(1, 100_001, Object::default()),
                    command(2, 100_001, mesh(0, 17.)),
                ]
                .concat(),
            )
            .unwrap();
        synchronize(&scene, &mut fast, &mut a, false);
        synchronize(&scene, &mut slow, &mut b, false);
        assert_eq!(scene.instance_index(2), None);
        assert_eq!(
            scene.instances()[scene.instance_index(100_001).unwrap()].model[3][0],
            17.
        );
        assert_eq!(scene.instances().as_ptr(), pointer);
        assert_eq!(scene.packed_slots.len(), scene.instances().len());
        for (index, &slot) in scene.packed_slots.iter().enumerate() {
            assert_eq!(scene.store.packed_indices[slot], Some(index));
            assert!(scene.store.ids[slot].is_some());
        }
    }
    #[test]
    fn planned_but_failed_upload_can_retry_and_invalid_batches_do_not_alter_journal() {
        let mut scene = Scene::default();
        add(&mut scene, &[(1, mesh(0, 0.)), (2, mesh(0, 0.))]);
        let stamp = scene.stamp();
        scene.apply(&command(2, 1, mesh(0, 3.))).unwrap();
        let plan = scene.instance_updates_since(Some(&stamp), false);
        assert_eq!(plan.ranges, vec![0..1]);
        assert!(scene.apply(&command(2, 1, mesh(2, 3.))).is_ok());
        assert!(scene.apply(&command(2, 2, mesh(1, 4.))).is_err());
        let retry = scene.instance_updates_since(Some(&stamp), false);
        assert_eq!(retry.ranges, vec![0..1]);
        assert!(!retry.full);
        let mut target = vec![
            Instance {
                model: crate::identity(),
                color: [1.; 4]
            };
            2
        ];
        let mut cursor = Some(stamp);
        synchronize(&scene, &mut cursor, &mut target, false);
    }
}
