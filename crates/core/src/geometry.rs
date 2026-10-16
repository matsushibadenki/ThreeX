/// Shared unit geometry. Dimensions remain in the object's scale, as for Box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum GeometryKind {
    #[default]
    Box = 0,
    Plane = 1,
    Sphere = 2,
}
impl GeometryKind {
    pub fn from_flags(flags: u32) -> Option<Self> {
        if flags & !15 != 0 {
            return None;
        }
        match (flags >> 2) & 3 {
            0 => Some(Self::Box),
            1 => Some(Self::Plane),
            2 => Some(Self::Sphere),
            _ => None,
        }
    }
}
impl crate::Object {
    pub fn flags(self) -> u32 {
        u32::from(self.visible) | (u32::from(self.mesh) << 1) | ((self.geometry as u32) << 2)
    }
}
impl crate::Instance {
    /// Color alpha is an internal geometry tag; the fragment output remains opaque.
    pub fn geometry_kind(&self) -> GeometryKind {
        match self.color[3] as u32 {
            2 => GeometryKind::Plane,
            3 => GeometryKind::Sphere,
            _ => GeometryKind::Box,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::record;
    use crate::{Document, Object};
    #[test]
    fn shape_only_edits_are_incremental_and_roundtrip_history_and_hash() {
        let mut d = Document::default();
        let mut o = Object {
            mesh: true,
            ..Default::default()
        };
        d.commit(0, &[record(1, 1, o), record(2, 1, o)].concat())
            .unwrap();
        let box_hash = d.content_hash();
        let stamp = d.scene().stamp();
        o.geometry = GeometryKind::Plane;
        d.commit(1, &record(2, 1, o)).unwrap();
        assert_eq!(d.scene().update_stats().world_transforms, 0);
        assert_eq!(d.scene().update_stats().local_transforms, 0);
        let updates = d.scene().instance_updates_since(Some(&stamp), false);
        assert!(!updates.full);
        assert_eq!(updates.ranges, vec![0..1]);
        assert_eq!(
            d.scene().instances()[0].geometry_kind(),
            GeometryKind::Plane
        );
        assert_ne!(d.content_hash(), box_hash);
        let mut restored = Document::from_checkpoint(&d.checkpoint().unwrap()).unwrap();
        assert_eq!(
            restored.scene().object(1).unwrap().geometry,
            GeometryKind::Plane
        );
        restored.undo(2).unwrap();
        assert_eq!(restored.content_hash(), box_hash);
        restored.redo(3).unwrap();
        assert_eq!(
            restored.scene().object(1).unwrap().geometry,
            GeometryKind::Plane
        );
        o.geometry = GeometryKind::Sphere;
        restored.commit(4, &record(2, 1, o)).unwrap();
        let before = restored.scene().stamp();
        let mut bad = record(2, 1, o);
        bad[56..60].copy_from_slice(&15u32.to_le_bytes());
        assert!(restored.commit(5, &bad).is_err());
        assert!(restored.scene().is_current(Some(&before)));
    }
}
