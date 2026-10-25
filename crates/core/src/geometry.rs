/// Shared unit geometry. Dimensions remain in the object's scale, as for Box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum GeometryKind {
    #[default]
    Box = 0,
    Plane = 1,
    Sphere = 2,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GeometrySpec {
    pub kind: GeometryKind,
    pub width: u16,
    pub height: u16,
}
impl GeometrySpec {
    pub fn new(kind: GeometryKind, segments: [u16; 2]) -> Option<Self> {
        let [width, height] = if segments == [0, 0] {
            match kind {
                GeometryKind::Box => [0, 0],
                GeometryKind::Plane => [1, 1],
                GeometryKind::Sphere => [32, 16],
            }
        } else {
            segments
        };
        let valid = match kind {
            GeometryKind::Box => width == 0 && height == 0,
            GeometryKind::Plane => (1..=256).contains(&width) && (1..=256).contains(&height),
            GeometryKind::Sphere => (3..=256).contains(&width) && (2..=256).contains(&height),
        };
        valid.then_some(Self {
            kind,
            width,
            height,
        })
    }
    pub fn flags(self) -> u32 {
        let default = Self::new(self.kind, [0, 0]).unwrap();
        ((self.kind as u32) << 2)
            | if self == default {
                0
            } else {
                (u32::from(self.width) << 4) | (u32::from(self.height) << 13)
            }
    }
    pub fn from_flags(flags: u32) -> Option<Self> {
        if flags & !0x3fffff != 0 {
            return None;
        }
        let kind = match (flags >> 2) & 3 {
            0 => GeometryKind::Box,
            1 => GeometryKind::Plane,
            2 => GeometryKind::Sphere,
            _ => return None,
        };
        let spec = Self::new(
            kind,
            [((flags >> 4) & 511) as u16, ((flags >> 13) & 511) as u16],
        )?;
        (spec.flags() == flags & !3).then_some(spec)
    }
    pub fn tag(self) -> u32 {
        (self.flags() >> 2) + 1
    }
}
impl GeometryKind {
    pub fn from_flags(flags: u32) -> Option<Self> {
        GeometrySpec::from_flags(flags).map(|s| s.kind)
    }
}
impl crate::Object {
    pub fn geometry_spec(self) -> GeometrySpec {
        GeometrySpec::new(self.geometry, self.segments).expect("Invalid geometry segments")
    }
    pub fn flags(self) -> u32 {
        u32::from(self.visible) | (u32::from(self.mesh) << 1) | self.geometry_spec().flags()
    }
}
impl crate::Instance {
    pub fn geometry_spec(&self) -> GeometrySpec {
        let tag = self.color[3] as u32;
        GeometrySpec::from_flags(tag.saturating_sub(1) << 2)
            .unwrap_or(GeometrySpec::new(GeometryKind::Box, [0, 0]).unwrap())
    }
    pub fn geometry_kind(&self) -> GeometryKind {
        self.geometry_spec().kind
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
    #[test]
    fn subdivision_flags_are_canonical_bounded_and_exact_in_instance_tags() {
        for kind in [GeometryKind::Plane, GeometryKind::Sphere] {
            for w in [3, 7, 32, 128, 256] {
                for h in [2, 9, 16, 128, 256] {
                    let spec = GeometrySpec::new(kind, [w, h]).unwrap();
                    assert_eq!(GeometrySpec::from_flags(spec.flags()), Some(spec));
                    let i = crate::Instance {
                        model: crate::identity(),
                        color: [1., 1., 1., spec.tag() as f32],
                    };
                    assert_eq!(i.geometry_spec(), spec);
                }
            }
        }
        for flags in [
            1 << 22,
            4 | (1 << 13),
            8 | (2 << 4) | (4 << 13),
            4 | (257 << 4) | (2 << 13),
            4 | (1 << 4) | (1 << 13),
        ] {
            assert!(GeometrySpec::from_flags(flags).is_none());
        }
        assert!(GeometrySpec::new(GeometryKind::Box, [1, 1]).is_none());
        assert!(GeometrySpec::new(GeometryKind::Sphere, [3, 1]).is_none());
    }
    #[test]
    fn subdivisions_preserve_delta_hash_history_and_v5_read_compatibility() {
        fn version(mut bytes: Vec<u8>, v: u32) -> Vec<u8> {
            bytes[8..12].copy_from_slice(&v.to_le_bytes());
            let n = bytes.len() - 4;
            let mut crc = !0u32;
            for &b in &bytes[..n] {
                crc ^= b as u32;
                for _ in 0..8 {
                    crc = (crc >> 1) ^ (0xedb88320u32 & 0u32.wrapping_sub(crc & 1));
                }
            }
            bytes[n..].copy_from_slice(&(!crc).to_le_bytes());
            bytes
        }
        let mut d = Document::default();
        let mut o = Object {
            mesh: true,
            geometry: GeometryKind::Plane,
            ..Default::default()
        };
        d.commit(0, &[record(1, 1, o), record(2, 1, o)].concat())
            .unwrap();
        let hash = d.content_hash();
        let legacy = version(d.checkpoint().unwrap(), 5);
        let mut restored = Document::from_checkpoint(&legacy).unwrap();
        assert_eq!(restored.content_hash(), hash);
        let stamp = restored.scene().stamp();
        o.segments = [7, 9];
        restored.commit(1, &record(2, 1, o)).unwrap();
        assert_eq!(restored.scene().update_stats().local_transforms, 0);
        assert_eq!(restored.scene().update_stats().world_transforms, 0);
        assert_eq!(
            restored
                .scene()
                .instance_updates_since(Some(&stamp), false)
                .ranges,
            vec![0..1]
        );
        assert_eq!(
            restored.scene().instances()[0].geometry_spec(),
            o.geometry_spec()
        );
        assert_ne!(restored.content_hash(), hash);
        let bytes = restored.checkpoint().unwrap();
        assert_eq!(&bytes[8..12], &7u32.to_le_bytes());
        assert!(Document::from_checkpoint(&version(bytes.clone(), 5)).is_err());
        let mut reopened = Document::from_checkpoint(&bytes).unwrap();
        assert_eq!(reopened.scene().object(1).unwrap().segments, [7, 9]);
        reopened.undo(2).unwrap();
        assert_eq!(reopened.content_hash(), hash);
        reopened.redo(3).unwrap();
        assert_eq!(reopened.scene().object(1).unwrap().segments, [7, 9]);
        let before = reopened.snapshot();
        let mut bad = record(2, 1, o);
        bad[56..60].copy_from_slice(&(8u32 | (2 << 4) | (4 << 13)).to_le_bytes());
        assert!(reopened.commit(4, &bad).is_err());
        assert_eq!(before, reopened.snapshot());
    }
}
