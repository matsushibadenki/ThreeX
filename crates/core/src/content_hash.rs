//! Versioned authoring-content identity; never a substitute for revision/receipt guards.
use crate::{Document, DocumentSnapshot, Object};
use sha2::{Digest, Sha256};
use std::fmt;
pub const CONTENT_HASH_VERSION: u32 = 1;
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentHash([u8; 32]);
impl ContentHash {
    pub fn bytes(self) -> [u8; 32] {
        self.0
    }
}
impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}
fn hash_objects(count: usize, objects: impl Iterator<Item = (u32, Object)>) -> ContentHash {
    let mut hash = Sha256::new();
    hash.update(b"THREEX-CONTENT\0");
    hash.update(CONTENT_HASH_VERSION.to_le_bytes());
    hash.update((count as u64).to_le_bytes());
    // Both callers iterate their BTreeMap in stable object-ID order.
    for (id, o) in objects {
        hash.update(id.to_le_bytes());
        for value in o
            .position
            .into_iter()
            .chain(o.rotation)
            .chain(o.scale)
            .chain(o.color)
        {
            // Validated Document floats are finite; +0 and -0 have the same semantics.
            hash.update(if value == 0. { 0u32 } else { value.to_bits() }.to_le_bytes());
        }
        hash.update((u32::from(o.visible) | (u32::from(o.mesh) << 1)).to_le_bytes());
        hash.update(o.parent.to_le_bytes());
    }
    ContentHash(hash.finalize().into())
}
impl Document {
    /// O(objects), streaming SHA-256 of current authoring state, excluding identity/history/receipts.
    pub fn content_hash(&self) -> ContentHash {
        hash_objects(self.scene.len(), self.scene.objects())
    }
}
impl DocumentSnapshot {
    /// Same canonical digest for an immutable snapshot. Does not validate externally edited snapshots.
    pub fn content_hash(&self) -> ContentHash {
        hash_objects(
            self.objects.len(),
            self.objects.iter().map(|(&id, &object)| (id, object)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::record;
    #[test]
    fn content_is_independent_of_order_identity_history_and_receipts() {
        let mut a = Document::default();
        let mut b = Document::new(4096, 1024);
        a.commit(
            0,
            &[
                record(1, 2, Object::default()),
                record(1, 1, Object::default()),
            ]
            .concat(),
        )
        .unwrap();
        b.commit(0, &record(1, 1, Object::default())).unwrap();
        b.commit(1, &record(1, 2, Object::default())).unwrap();
        assert_eq!(a.content_hash(), b.content_hash());
        assert_ne!(a.revision(), b.revision());
        let before = a.content_hash();
        let id = crate::DocumentId::from_bytes([1; 16]).unwrap();
        a.bind_document_id(id).unwrap();
        let session = crate::SessionId::from_bytes([2; 16]).unwrap();
        let crate::RequestPreparation::Pending(p) =
            a.prepare_request(id, session, 1, 1, 1, &[]).unwrap()
        else {
            panic!()
        };
        a.apply_request(p).unwrap();
        assert_eq!(a.content_hash(), before);
        assert_eq!(a.snapshot().content_hash(), before);
        assert_eq!(
            Document::from_checkpoint(&a.checkpoint().unwrap())
                .unwrap()
                .content_hash(),
            before
        );
        a.commit(1, &record(3, 2, Object::default())).unwrap();
        assert_ne!(a.content_hash(), before);
        a.undo(2).unwrap();
        assert_eq!(a.content_hash(), before);
        a.redo(3).unwrap();
        assert_ne!(a.content_hash(), before);
    }
    #[test]
    fn every_authoring_field_and_id_matters_and_signed_zero_is_canonical() {
        let mut d = Document::default();
        d.commit(0, &record(1, 1, Object::default())).unwrap();
        let snapshot = d.snapshot();
        let initial = snapshot.content_hash();
        for field in 0..15 {
            let mut changed = snapshot.clone();
            let o = changed.objects.get_mut(&1).unwrap();
            match field {
                0..=2 => o.position[field] = 1.,
                3..=5 => o.rotation[field - 3] = 1.,
                6..=8 => o.scale[field - 6] = 2.,
                9..=11 => o.color[field - 9] = 0.5,
                12 => o.visible = !o.visible,
                13 => o.mesh = !o.mesh,
                _ => o.parent = 2,
            };
            assert_ne!(changed.content_hash(), initial, "field {field}");
        }
        let mut changed = snapshot.clone();
        let o = changed.objects.remove(&1).unwrap();
        changed.objects.insert(2, o);
        assert_ne!(changed.content_hash(), initial);
        let mut zero = snapshot;
        zero.objects.get_mut(&1).unwrap().position = [-0.; 3];
        assert_eq!(zero.content_hash(), initial);
    }
    #[test]
    fn empty_document_golden_vector() {
        assert_eq!(
            Document::default().content_hash().to_string(),
            "ed881b0c277e6221f0c6e7fb10ca5fe8cce5df8c3e3410629f00b819f3742d56"
        );
    }
    #[test]
    fn populated_canonical_golden_vector() {
        let snapshot = DocumentSnapshot {
            revision: 99,
            objects: std::collections::BTreeMap::from([(7, Object::default())]),
        };
        assert_eq!(
            snapshot.content_hash().to_string(),
            "29019df039ef90d09c88e78689c010e5ae42ba939f6ecef563f658f13d9ad6fb"
        );
    }
}
