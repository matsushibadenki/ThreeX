//! Optional editing owner above Scene. No window, IPC, GPU, or filesystem types.
use crate::{Object, Scene};
use std::collections::BTreeMap;

/// Typed failures for hosts to translate into localized messages.
#[derive(Debug, PartialEq, Eq)]
pub enum DocumentError {
    RevisionConflict { current: u64 },
    CommandTooLarge,
    HistoryCapacity,
    RevisionExhausted,
    InvalidCommand(String),
    NothingToUndo,
    NothingToRedo,
}
pub(crate) struct Edit {
    pub(crate) forward: Vec<u8>,
    pub(crate) inverse: Vec<u8>,
}
impl Edit {
    pub(crate) fn bytes(&self) -> usize {
        self.forward.len() + self.inverse.len()
    }
}
/// Single Rust writer; renderers borrow Scene without owning editing history.
/// Object IDs survive undo. History is bounded and rejects oversized edits before mutation.
pub struct Document {
    pub(crate) scene: Scene,
    pub(crate) undo: Vec<Edit>,
    pub(crate) redo: Vec<Edit>,
    pub(crate) history_bytes: usize,
    pub(crate) history_limit: usize,
    pub(crate) command_limit: usize,
}
/// Opaque validated delta tied to the exact source Scene identity and revision.
pub struct PreparedTransaction {
    stamp: crate::SceneStamp,
    scene: crate::scene::SceneEdit,
    history: PreparedHistory,
    revision: u64,
}
/// Opaque group bound to its starting revision; intermediate plans cannot be applied independently.
pub struct PreparedGroup {
    stamp: crate::SceneStamp,
    plans: Vec<PreparedTransaction>,
}
impl PreparedGroup {
    pub fn revisions(&self) -> Vec<u64> {
        self.plans.iter().map(|p| p.revision).collect()
    }
}
enum PreparedHistory {
    Commit(Edit),
    Undo,
    Redo,
}
impl PreparedTransaction {
    pub fn revision(&self) -> u64 {
        self.revision
    }
}
impl Default for Document {
    fn default() -> Self {
        Self::new(8 * 1024 * 1024, 256 * 1024)
    }
}
impl Document {
    pub fn new(history_limit: usize, command_limit: usize) -> Self {
        Self {
            scene: Scene::default(),
            undo: Vec::new(),
            redo: Vec::new(),
            history_bytes: 0,
            history_limit,
            command_limit,
        }
    }
    pub fn scene(&self) -> &Scene {
        &self.scene
    }
    pub fn revision(&self) -> u64 {
        self.scene.revision
    }
    pub fn history_bytes(&self) -> usize {
        self.history_bytes
    }
    fn check_revision(&self, expected: u64) -> Result<(), DocumentError> {
        if expected != self.revision() {
            return Err(DocumentError::RevisionConflict {
                current: self.revision(),
            });
        }
        if expected == u64::MAX {
            return Err(DocumentError::RevisionExhausted);
        }
        Ok(())
    }
    /// Applies the existing 64-byte command protocol atomically with optimistic concurrency.
    /// No-op batches do not consume history or clear redo.
    pub fn commit(&mut self, expected: u64, commands: &[u8]) -> Result<u64, DocumentError> {
        let edit = self.prepare_commit(expected, commands)?;
        self.apply_prepared(edit)
    }
    pub fn prepare_commit(
        &self,
        expected: u64,
        commands: &[u8],
    ) -> Result<PreparedTransaction, DocumentError> {
        self.check_revision(expected)?;
        if commands.len() > self.command_limit {
            return Err(DocumentError::CommandTooLarge);
        }
        if !commands.len().is_multiple_of(64) {
            return Err(DocumentError::InvalidCommand(
                "Truncated command buffer".into(),
            ));
        }
        let mut before = BTreeMap::new();
        let mut exists = BTreeMap::new();
        for c in commands.as_chunks::<64>().0 {
            let op = u32::from_le_bytes(c[..4].try_into().unwrap());
            let id = u32::from_le_bytes(c[4..8].try_into().unwrap());
            before.entry(id).or_insert_with(|| self.scene.object(id));
            let present = exists.entry(id).or_insert(before[&id].is_some());
            match op {
                1 => *present = true,
                3 => *present = false,
                _ => {}
            }
        }
        let mut inverse = Vec::new();
        // Scene validates the final hierarchy, so restored parents need not be emitted first.
        for (&id, &old) in &before {
            match (old, exists[&id]) {
                (Some(object), true) => inverse.extend(record(2, id, object)),
                (Some(object), false) => {
                    inverse.extend(record(1, id, object));
                    inverse.extend(record(2, id, object));
                }
                (None, true) => inverse.extend(record(3, id, Object::default())),
                (None, false) => {}
            }
        }
        let edit_bytes = commands
            .len()
            .checked_add(inverse.len())
            .ok_or(DocumentError::HistoryCapacity)?;
        if edit_bytes > self.history_limit {
            return Err(DocumentError::HistoryCapacity);
        }
        let scene = self
            .scene
            .prepare(commands)
            .map_err(DocumentError::InvalidCommand)?;
        let revision = expected + u64::from(scene.changes_scene(&self.scene));
        Ok(PreparedTransaction {
            stamp: self.scene.stamp(),
            scene,
            revision,
            history: PreparedHistory::Commit(Edit {
                forward: commands.to_vec(),
                inverse,
            }),
        })
    }
    /// Prepare up to 32 dependent edits without cloning the Document or its history.
    /// Each edit retains its own revision and Undo boundary. Any invalid edit rejects the group.
    pub fn prepare_group(
        &self,
        expected: u64,
        commands: &[&[u8]],
    ) -> Result<PreparedGroup, DocumentError> {
        self.check_revision(expected)?;
        if commands.len() > 32 || commands.iter().map(|b| b.len()).sum::<usize>() > 1024 * 1024 {
            return Err(DocumentError::CommandTooLarge);
        }
        let mut overlay = BTreeMap::new();
        let mut plans: Vec<PreparedTransaction> = Vec::new();
        let mut revision = expected;
        for &bytes in commands {
            if bytes.len() > self.command_limit || !bytes.len().is_multiple_of(64) {
                return Err(DocumentError::CommandTooLarge);
            }
            let lookup = |id| {
                overlay
                    .get(&id)
                    .copied()
                    .unwrap_or_else(|| self.scene.object(id))
            };
            let mut before = BTreeMap::new();
            let mut exists = BTreeMap::new();
            for c in bytes.as_chunks::<64>().0 {
                let op = u32::from_le_bytes(c[..4].try_into().unwrap());
                let id = u32::from_le_bytes(c[4..8].try_into().unwrap());
                before.entry(id).or_insert_with(|| lookup(id));
                let present = exists.entry(id).or_insert(before[&id].is_some());
                match op {
                    1 => *present = true,
                    3 => *present = false,
                    _ => {}
                }
            }
            let mut inverse = Vec::new();
            for (&id, &old) in &before {
                match (old, exists[&id]) {
                    (Some(o), true) => inverse.extend(record(2, id, o)),
                    (Some(o), false) => {
                        inverse.extend(record(1, id, o));
                        inverse.extend(record(2, id, o));
                    }
                    (None, true) => inverse.extend(record(3, id, Object::default())),
                    (None, false) => {}
                }
            }
            if bytes.len() + inverse.len() > self.history_limit {
                return Err(DocumentError::HistoryCapacity);
            }
            let scene = self
                .scene
                .prepare_overlay(bytes, &overlay)
                .map_err(DocumentError::InvalidCommand)?;
            let changed = scene.changes_overlay(&self.scene, &overlay);
            scene.extend_overlay(&mut overlay);
            let mut stamp = self.scene.stamp();
            stamp.revision = revision;
            if changed {
                revision = revision
                    .checked_add(1)
                    .ok_or(DocumentError::RevisionExhausted)?;
                if revision == u64::MAX {
                    return Err(DocumentError::RevisionExhausted);
                }
            }
            plans.push(PreparedTransaction {
                stamp,
                scene,
                revision,
                history: PreparedHistory::Commit(Edit {
                    forward: bytes.to_vec(),
                    inverse,
                }),
            });
        }
        Ok(PreparedGroup {
            stamp: self.scene.stamp(),
            plans,
        })
    }
    pub fn apply_group(&mut self, group: PreparedGroup) -> Result<Vec<u64>, DocumentError> {
        if !self.scene.is_current(Some(&group.stamp)) {
            return Err(DocumentError::RevisionConflict {
                current: self.revision(),
            });
        }
        group
            .plans
            .into_iter()
            .map(|plan| self.apply_prepared(plan))
            .collect()
    }
    pub fn apply_prepared(&mut self, prepared: PreparedTransaction) -> Result<u64, DocumentError> {
        if !self.scene.is_current(Some(&prepared.stamp)) {
            return Err(DocumentError::RevisionConflict {
                current: self.revision(),
            });
        }
        let changed = prepared.revision != self.revision();
        self.scene.apply_edit(prepared.scene);
        if !changed {
            return Ok(self.revision());
        }
        match prepared.history {
            PreparedHistory::Commit(edit) => {
                let edit_bytes = edit.bytes();
                self.history_bytes -= self.redo.iter().map(Edit::bytes).sum::<usize>();
                self.redo.clear();
                while self.history_bytes + edit_bytes > self.history_limit {
                    let oldest = self.undo.remove(0);
                    self.history_bytes -= oldest.bytes();
                }
                self.history_bytes += edit_bytes;
                self.undo.push(edit);
            }
            PreparedHistory::Undo => self.redo.push(self.undo.pop().unwrap()),
            PreparedHistory::Redo => self.undo.push(self.redo.pop().unwrap()),
        }
        Ok(self.revision())
    }
    pub fn prepare_undo(&self, expected: u64) -> Result<PreparedTransaction, DocumentError> {
        self.check_revision(expected)?;
        let edit = self.undo.last().ok_or(DocumentError::NothingToUndo)?;
        let scene = self
            .scene
            .prepare(&edit.inverse)
            .map_err(DocumentError::InvalidCommand)?;
        Ok(PreparedTransaction {
            stamp: self.scene.stamp(),
            revision: expected + u64::from(scene.changes_scene(&self.scene)),
            scene,
            history: PreparedHistory::Undo,
        })
    }
    pub fn prepare_redo(&self, expected: u64) -> Result<PreparedTransaction, DocumentError> {
        self.check_revision(expected)?;
        let edit = self.redo.last().ok_or(DocumentError::NothingToRedo)?;
        let scene = self
            .scene
            .prepare(&edit.forward)
            .map_err(DocumentError::InvalidCommand)?;
        Ok(PreparedTransaction {
            stamp: self.scene.stamp(),
            revision: expected + u64::from(scene.changes_scene(&self.scene)),
            scene,
            history: PreparedHistory::Redo,
        })
    }
    pub fn undo(&mut self, expected: u64) -> Result<u64, DocumentError> {
        let prepared = self.prepare_undo(expected)?;
        self.apply_prepared(prepared)
    }
    pub fn redo(&mut self, expected: u64) -> Result<u64, DocumentError> {
        let prepared = self.prepare_redo(expected)?;
        self.apply_prepared(prepared)
    }
    /// Explicit immutable CPU snapshot; not created per frame or sent wholesale over IPC.
    pub fn snapshot(&self) -> DocumentSnapshot {
        DocumentSnapshot {
            revision: self.revision(),
            objects: self.scene.objects().collect(),
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct DocumentSnapshot {
    pub revision: u64,
    pub objects: BTreeMap<u32, Object>,
}
pub(crate) fn record(op: u32, id: u32, o: Object) -> [u8; 64] {
    let mut b = [0; 64];
    for (at, value) in [
        (0, op),
        (4, id),
        (56, u32::from(o.visible) | (u32::from(o.mesh) << 1)),
        (60, o.parent),
    ] {
        b[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    for (i, value) in o
        .position
        .into_iter()
        .chain(o.rotation)
        .chain(o.scale)
        .chain(o.color)
        .enumerate()
    {
        b[8 + i * 4..12 + i * 4].copy_from_slice(&value.to_le_bytes());
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    fn mesh(parent: u32, x: f32) -> Object {
        Object {
            parent,
            position: [x, 0., 0.],
            mesh: true,
            ..Default::default()
        }
    }
    fn create(id: u32, o: Object) -> Vec<u8> {
        [record(1, id, o), record(2, id, o)].concat()
    }
    fn commit(d: &mut Document, b: &[u8]) {
        d.commit(d.revision(), b).unwrap();
    }
    #[test]
    fn revisions_history_and_stable_ids_survive_hierarchy_deletion() {
        let mut d = Document::default();
        commit(
            &mut d,
            &[create(1, mesh(0, 1.)), create(2, mesh(1, 2.))].concat(),
        );
        let initial = d.snapshot();
        let stamp = d.scene().stamp();
        commit(
            &mut d,
            &[record(3, 1, Object::default()), record(2, 2, mesh(0, 2.))].concat(),
        );
        let edited = d.snapshot();
        d.undo(2).unwrap();
        assert_eq!(d.snapshot().objects, initial.objects);
        assert_eq!(d.revision(), 3);
        assert!(!d.scene().is_current(Some(&stamp)));
        d.redo(3).unwrap();
        assert_eq!(d.snapshot().objects, edited.objects);
        assert_eq!(d.revision(), 4);
        assert_eq!(initial.objects[&2].parent, 1); // snapshot stays immutable
    }
    #[test]
    fn failed_edits_conflicts_and_bounds_are_atomic() {
        let mut d = Document::new(256, 128);
        commit(&mut d, &create(1, mesh(0, 1.)));
        let before = d.snapshot();
        let bytes = d.history_bytes();
        assert_eq!(
            d.commit(0, &record(2, 1, mesh(0, 5.))),
            Err(DocumentError::RevisionConflict { current: 1 })
        );
        assert!(matches!(
            d.commit(1, &record(2, 1, mesh(1, 5.))),
            Err(DocumentError::InvalidCommand(_))
        ));
        assert_eq!(d.commit(1, &[0; 192]), Err(DocumentError::CommandTooLarge));
        assert_eq!(d.snapshot(), before);
        assert_eq!(d.history_bytes(), bytes);
        d.undo(1).unwrap();
        d.commit(2, &[]).unwrap();
        d.redo(2).unwrap(); // no-op preserves redo
        commit(&mut d, &record(2, 1, mesh(0, 3.)));
        assert!(d.history_bytes() <= 256);
        d.undo(d.revision()).unwrap();
        assert_eq!(d.undo(d.revision()), Err(DocumentError::NothingToUndo));
        let mut tiny = Document::new(1, 128);
        assert_eq!(
            tiny.commit(0, &create(1, mesh(0, 1.))),
            Err(DocumentError::HistoryCapacity)
        );
        assert!(tiny.scene().is_empty());
    }
    #[test]
    fn prepared_deltas_are_inert_until_applied_and_bound_to_revision_and_identity() {
        let mut d = Document::default();
        commit(&mut d, &create(1, mesh(0, 1.)));
        let before = d.snapshot();
        let stamp = d.scene().stamp();
        let stats = d.scene().update_stats();
        let bytes = d.history_bytes();
        let prepared = d.prepare_commit(1, &record(2, 1, mesh(0, 3.))).unwrap();
        assert_eq!(prepared.revision(), 2);
        assert_eq!(d.snapshot(), before);
        assert!(d.scene().is_current(Some(&stamp)));
        assert_eq!(d.scene().update_stats(), stats);
        assert_eq!(d.history_bytes(), bytes);
        let stale = d.prepare_commit(1, &record(2, 1, mesh(0, 7.))).unwrap();
        d.apply_prepared(prepared).unwrap();
        let edited = d.snapshot();
        assert_eq!(
            d.apply_prepared(stale),
            Err(DocumentError::RevisionConflict { current: 2 })
        );
        assert_eq!(d.snapshot(), edited);
        let other = Document::from_checkpoint(&d.checkpoint().unwrap()).unwrap();
        let wrong = other.prepare_undo(2).unwrap();
        assert_eq!(
            d.apply_prepared(wrong),
            Err(DocumentError::RevisionConflict { current: 2 })
        );
        assert!(d.prepare_commit(2, &record(2, 1, mesh(1, 0.))).is_err());
        assert_eq!(d.snapshot(), edited);
        let undo = d.prepare_undo(2).unwrap();
        assert_eq!(d.snapshot(), edited);
        d.apply_prepared(undo).unwrap();
        assert_eq!(d.snapshot().objects, before.objects);
        let redo = d.prepare_redo(3).unwrap();
        d.apply_prepared(redo).unwrap();
        assert_eq!(d.snapshot().objects, edited.objects);
        let first = record(2, 1, mesh(0, 5.));
        let second = record(2, 1, mesh(0, 6.));
        let group = d.prepare_group(4, &[&first, &second]).unwrap();
        assert_eq!(group.revisions(), vec![5, 6]);
        commit(&mut d, &record(2, 1, mesh(0, 9.)));
        let before = d.snapshot();
        assert_eq!(
            d.apply_group(group),
            Err(DocumentError::RevisionConflict { current: 5 })
        );
        assert_eq!(d.snapshot(), before);
    }
    #[test]
    fn delta_group_matches_sequential_history_with_overlay_hierarchy_and_slot_reuse() {
        let mut grouped = Document::default();
        let mut reference = Document::default();
        let create_parent = create(50, mesh(0, 1.));
        let create_child = create(2, mesh(50, 2.));
        let move_parent = record(2, 50, mesh(0, 3.)).to_vec();
        let records = [create_parent, create_child, move_parent];
        let refs = records.iter().map(Vec::as_slice).collect::<Vec<_>>();
        grouped
            .apply_group(grouped.prepare_group(0, &refs).unwrap())
            .unwrap();
        for b in &records {
            commit(&mut reference, b);
        }
        assert_eq!(grouped.snapshot(), reference.snapshot());
        assert_eq!(grouped.scene().update_stats().hierarchy_rebuilds, 0);
        let before = grouped.snapshot();
        let invalid = [record(2, 50, mesh(2, 1.)), record(2, 2, mesh(0, 2.))];
        assert!(
            grouped
                .prepare_group(grouped.revision(), &[&invalid[0], &invalid[1]])
                .is_err()
        );
        assert_eq!(grouped.snapshot(), before);
        let deleted = [
            record(3, 2, Object::default()).to_vec(),
            create(2, mesh(50, 9.)),
        ];
        let refs = deleted.iter().map(Vec::as_slice).collect::<Vec<_>>();
        grouped
            .apply_group(grouped.prepare_group(grouped.revision(), &refs).unwrap())
            .unwrap();
        for b in &deleted {
            commit(&mut reference, b);
        }
        for round in 0..100 {
            let mut commands = Vec::new();
            for step in 0..8 {
                let id = if step % 2 == 0 { 50 } else { 2 };
                let mut o = reference.scene().object(id).unwrap();
                match step % 4 {
                    0 => o.position[0] += 0.1,
                    1 => o.visible = !o.visible,
                    2 => o.color[1] = round as f32 / 100.,
                    _ => o.mesh = !o.mesh,
                }
                let b = record(2, id, o).to_vec();
                commit(&mut reference, &b);
                commands.push(b);
            }
            let refs = commands.iter().map(Vec::as_slice).collect::<Vec<_>>();
            let before = grouped.snapshot();
            let plan = grouped.prepare_group(grouped.revision(), &refs).unwrap();
            assert_eq!(grouped.snapshot(), before);
            grouped.apply_group(plan).unwrap();
            assert_eq!(grouped.snapshot(), reference.snapshot());
        }
        while reference.undo(reference.revision()).is_ok() {
            grouped.undo(grouped.revision()).unwrap();
            assert_eq!(grouped.snapshot(), reference.snapshot());
        }
        assert_eq!(
            grouped.undo(grouped.revision()),
            Err(DocumentError::NothingToUndo)
        );
    }
    #[test]
    fn mixed_history_roundtrips_and_retained_gpu_ranges_match() {
        let mut d = Document::default();
        commit(
            &mut d,
            &[create(1, mesh(0, 1.)), create(2, mesh(1, 2.))].concat(),
        );
        let mut snapshots = vec![d.snapshot().objects];
        let mut stamp = d.scene().stamp();
        let mut gpu = d.scene().instances().to_vec();
        for step in 0..80 {
            let id = if step % 2 == 0 { 1 } else { 2 };
            let mut o = d.scene().object(id).unwrap();
            match step % 5 {
                0 => o.position[0] += 1.,
                1 => o.visible = !o.visible,
                2 => o.mesh = !o.mesh,
                3 => o.color[0] = step as f32 / 100.,
                _ => o.rotation[1] += 0.1,
            }
            commit(&mut d, &record(2, id, o));
            snapshots.push(d.snapshot().objects);
        }
        // A renderer which exceeded journal retention safely receives a full plan.
        assert!(d.scene().instance_updates_since(Some(&stamp), false).full);
        for expected in snapshots[..snapshots.len() - 1].iter().rev() {
            d.undo(d.revision()).unwrap();
            assert_eq!(&d.snapshot().objects, expected);
            let plan = d.scene().instance_updates_since(Some(&stamp), false);
            gpu.resize(
                d.scene().instances().len(),
                crate::Instance {
                    model: crate::identity(),
                    color: [0.; 4],
                },
            );
            for range in plan.ranges {
                gpu[range.clone()].copy_from_slice(&d.scene().instances()[range]);
            }
            stamp = plan.stamp;
            assert_eq!(
                bytemuck::cast_slice::<_, u8>(&gpu),
                bytemuck::cast_slice::<_, u8>(d.scene().instances())
            );
        }
        for expected in snapshots.iter().skip(1) {
            d.redo(d.revision()).unwrap();
            assert_eq!(&d.snapshot().objects, expected);
        }
    }
    #[test]
    fn compound_lifecycle_and_branching_restore_semantics() {
        let mut d = Document::default();
        commit(&mut d, &create(1, mesh(0, 1.)));
        let old = d.snapshot();
        commit(
            &mut d,
            &[
                record(3, 1, Object::default()).to_vec(),
                create(1, mesh(0, 7.)),
                create(2, mesh(0, 2.)),
                record(3, 2, Object::default()).to_vec(),
            ]
            .concat(),
        );
        d.undo(d.revision()).unwrap();
        assert_eq!(d.snapshot().objects, old.objects);
        commit(&mut d, &record(2, 1, mesh(0, 3.)));
        assert_eq!(d.redo(d.revision()), Err(DocumentError::NothingToRedo));
        let mut hidden = mesh(0, 3.);
        hidden.visible = false;
        commit(&mut d, &record(2, 1, hidden));
        assert!(d.scene().instances().is_empty());
        d.undo(d.revision()).unwrap();
        assert_eq!(d.scene().instances().len(), 1);
    }
}
