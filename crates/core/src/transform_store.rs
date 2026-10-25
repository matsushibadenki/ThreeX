//! Structure-of-arrays storage. IDs map to reusable internal slots; slots never
//! escape the runtime. Entity/resource generation handles are a separate contract.
use crate::{Object, identity};
#[derive(Default)]
pub(crate) struct TransformStore {
    pub ids: Vec<Option<u32>>,
    pub positions: Vec<[f32; 3]>,
    pub rotations: Vec<[f32; 3]>,
    pub scales: Vec<[f32; 3]>,
    pub colors: Vec<[f32; 3]>,
    pub parent_ids: Vec<u32>,
    pub parents: Vec<Option<usize>>,
    pub child_positions: Vec<Option<usize>>,
    pub visible: Vec<bool>,
    pub meshes: Vec<bool>,
    pub geometries: Vec<crate::GeometryKind>,
    pub segments: Vec<[u16; 2]>,
    pub locals: Vec<[[f32; 4]; 4]>,
    pub worlds: Vec<[[f32; 4]; 4]>,
    pub world_visible: Vec<bool>,
    pub children: Vec<Vec<usize>>,
    pub dirty: Vec<u8>,
    pub packed_indices: Vec<Option<usize>>,
    pub assets: std::collections::BTreeMap<usize, crate::AssetRef>,
    free: Vec<usize>,
}
impl TransformStore {
    pub fn reserve_new(&mut self, count: usize) {
        let additional = count.saturating_sub(self.free.len());
        macro_rules! reserve { ($($field:ident),*) => { $(self.$field.reserve(additional);)* }; }
        reserve!(
            ids,
            positions,
            rotations,
            scales,
            colors,
            parent_ids,
            parents,
            child_positions,
            visible,
            meshes,
            geometries,
            segments,
            locals,
            worlds,
            world_visible,
            children,
            dirty,
            packed_indices
        );
    }
    pub fn allocate(&mut self, id: u32) -> usize {
        if let Some(slot) = self.free.pop() {
            self.ids[slot] = Some(id);
            self.parents[slot] = None;
            self.child_positions[slot] = None;
            self.dirty[slot] = 0;
            self.packed_indices[slot] = None;
            return slot;
        }
        let slot = self.ids.len();
        self.ids.push(Some(id));
        self.positions.push([0.; 3]);
        self.rotations.push([0.; 3]);
        self.scales.push([1.; 3]);
        self.colors.push([1.; 3]);
        self.parent_ids.push(0);
        self.parents.push(None);
        self.child_positions.push(None);
        self.visible.push(true);
        self.meshes.push(false);
        self.geometries.push(crate::GeometryKind::Box);
        self.segments.push([0, 0]);
        self.locals.push(identity());
        self.worlds.push(identity());
        self.world_visible.push(true);
        self.children.push(Vec::new());
        self.dirty.push(0);
        self.packed_indices.push(None);
        slot
    }
    pub fn remove(&mut self, slot: usize) {
        self.assets.remove(&slot);
        self.ids[slot] = None;
        self.dirty[slot] = 0;
        self.packed_indices[slot] = None;
        self.children[slot].clear();
        self.free.push(slot);
    }
    pub fn object(&self, slot: usize) -> Object {
        Object {
            position: self.positions[slot],
            rotation: self.rotations[slot],
            scale: self.scales[slot],
            color: self.colors[slot],
            parent: self.parent_ids[slot],
            visible: self.visible[slot],
            mesh: self.meshes[slot],
            geometry: self.geometries[slot],
            segments: self.segments[slot],
            asset: self.assets.get(&slot).copied(),
        }
    }
    pub fn write(&mut self, slot: usize, o: Object) {
        self.positions[slot] = o.position;
        self.rotations[slot] = o.rotation;
        self.scales[slot] = o.scale;
        self.colors[slot] = o.color;
        self.parent_ids[slot] = o.parent;
        self.visible[slot] = o.visible;
        self.meshes[slot] = o.mesh;
        self.geometries[slot] = o.geometry;
        self.segments[slot] = o.segments;
        match o.asset {
            Some(asset) => {
                self.assets.insert(slot, asset);
            }
            None => {
                self.assets.remove(&slot);
            }
        }
    }
}
