//! Persistent content references, independent of transient renderer handles.
use crate::{AssetId, Document, SceneStamp};
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AssetRef {
    id: AssetId,
    revision: u64,
    hash: [u8; 32],
}
impl AssetRef {
    pub fn new(id: AssetId, revision: u64, hash: [u8; 32]) -> Option<Self> {
        (revision != 0).then_some(Self { id, revision, hash })
    }
    pub fn id(self) -> AssetId {
        self.id
    }
    pub fn revision(self) -> u64 {
        self.revision
    }
    pub fn hash(self) -> [u8; 32] {
        self.hash
    }
}
/// 64-byte bind(4)/clear(5) record. Existing object update(2) preserves this reference.
pub fn asset_command(entity: u32, reference: Option<AssetRef>) -> [u8; 64] {
    let mut b = [0; 64];
    b[..4].copy_from_slice(&if reference.is_some() { 4u32 } else { 5 }.to_le_bytes());
    b[4..8].copy_from_slice(&entity.to_le_bytes());
    if let Some(r) = reference {
        b[8..24].copy_from_slice(&r.id.bytes());
        b[24..32].copy_from_slice(&r.revision.to_le_bytes());
        b[32..].copy_from_slice(&r.hash);
    }
    b
}
pub(crate) fn decode(b: &[u8; 64]) -> Result<Option<AssetRef>, String> {
    if b[..4] == 5u32.to_le_bytes() {
        if b[8..].iter().any(|&v| v != 0) {
            return Err("Noncanonical asset clear".into());
        }
        return Ok(None);
    }
    let id = AssetId::from_bytes(b[8..24].try_into().unwrap()).ok_or("Invalid asset ID")?;
    AssetRef::new(
        id,
        u64::from_le_bytes(b[24..32].try_into().unwrap()),
        b[32..].try_into().unwrap(),
    )
    .map(Some)
    .ok_or("Invalid asset revision".into())
}
/// Conservative guard: any Document edit invalidates it, including edit→Undo ABA.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetJobGuard {
    entity: u32,
    reference: AssetRef,
    stamp: SceneStamp,
    document: Option<crate::DocumentId>,
}
impl AssetJobGuard {
    pub fn entity(&self) -> u32 {
        self.entity
    }
    pub fn reference(&self) -> AssetRef {
        self.reference
    }
}
impl Document {
    pub fn asset_job_guard(&self, entity: u32) -> Option<AssetJobGuard> {
        Some(AssetJobGuard {
            entity,
            reference: self.scene().object(entity)?.asset?,
            stamp: self.scene().stamp(),
            document: self.document_id(),
        })
    }
    pub fn accepts_asset_result(&self, guard: &AssetJobGuard, result: AssetRef) -> bool {
        self.document_id() == guard.document
            && self.scene().is_current(Some(&guard.stamp))
            && guard.reference == result
            && self
                .scene()
                .object(guard.entity)
                .is_some_and(|o| o.asset == Some(result))
    }
}

pub(crate) fn has_commands(bytes: &[u8]) -> bool {
    bytes
        .as_chunks::<64>()
        .0
        .iter()
        .any(|c| matches!(u32::from_le_bytes(c[..4].try_into().unwrap()), 4 | 5))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Object, document::record};
    fn reference(revision: u64) -> AssetRef {
        AssetRef::new(
            AssetId::from_bytes([9; 16]).unwrap(),
            revision,
            [revision as u8; 32],
        )
        .unwrap()
    }
    fn create() -> Document {
        let mut d = Document::default();
        let o = Object {
            mesh: true,
            ..Default::default()
        };
        d.commit(0, &[record(1, 1, o), record(2, 1, o)].concat())
            .unwrap();
        d
    }
    #[test]
    fn refs_preserve_history_checkpoint_hash_and_zero_gpu_delta() {
        let mut d = create();
        let empty_hash = d.content_hash();
        let stamp = d.scene().stamp();
        d.commit(1, &asset_command(1, Some(reference(1)))).unwrap();
        assert_eq!(d.scene().update_stats(), crate::UpdateStats::default());
        assert!(
            d.scene()
                .instance_updates_since(Some(&stamp), false)
                .ranges
                .is_empty()
        );
        assert_ne!(d.content_hash(), empty_hash);
        assert_eq!(d.content_hash(), d.snapshot().content_hash());
        let guard = d.asset_job_guard(1).unwrap();
        assert!(d.accepts_asset_result(&guard, reference(1)));
        d.commit(2, &asset_command(1, Some(reference(2)))).unwrap();
        assert!(!d.accepts_asset_result(&guard, reference(1)));
        d.undo(3).unwrap();
        assert!(
            !d.accepts_asset_result(&guard, reference(1)),
            "Undo must not revive a pre-edit job"
        );
        let mut loaded = Document::from_checkpoint(&d.checkpoint().unwrap()).unwrap();
        assert_eq!(loaded.snapshot(), d.snapshot());
        assert_eq!(loaded.content_hash(), d.content_hash());
        assert!(!loaded.accepts_asset_result(&d.asset_job_guard(1).unwrap(), reference(1)));
        loaded.redo(4).unwrap();
        assert_eq!(loaded.scene().object(1).unwrap().asset, Some(reference(2)));
        loaded.commit(5, &record(3, 1, Object::default())).unwrap();
        loaded.undo(6).unwrap();
        assert_eq!(loaded.scene().object(1).unwrap().asset, Some(reference(2)));
        loaded.commit(7, &asset_command(1, None)).unwrap();
        loaded.undo(8).unwrap();
        assert_eq!(loaded.scene().object(1).unwrap().asset, Some(reference(2)));
    }
    #[test]
    fn malformed_bindings_and_group_prefixes_are_atomic() {
        let mut d = create();
        let before = d.snapshot();
        for at in [8, 24] {
            let mut bad = asset_command(1, Some(reference(1)));
            if at == 8 {
                bad[8..24].fill(0)
            } else {
                bad[24..32].fill(0)
            }
            assert!(d.commit(1, &bad).is_err());
            assert_eq!(d.snapshot(), before);
        }
        let mut bad_clear = asset_command(1, None);
        bad_clear[63] = 1;
        assert!(d.commit(1, &bad_clear).is_err());
        assert!(d.commit(1, &asset_command(2, Some(reference(1)))).is_err());
        let a = asset_command(1, Some(reference(1)));
        let b = asset_command(1, Some(reference(2)));
        let mut builder = d.commit_group_builder(1).unwrap();
        builder.push(1, &a).unwrap();
        assert!(builder.push(2, &bad_clear).is_err());
        builder.push(2, &b).unwrap();
        let prepared = builder.finish();
        assert_eq!(d.apply_group(prepared).unwrap(), vec![2, 3]);
        d.undo(3).unwrap();
        assert_eq!(d.scene().object(1).unwrap().asset, Some(reference(1)));
        d.undo(4).unwrap();
        assert_eq!(d.scene().object(1).unwrap().asset, None);
    }
    #[test]
    fn v6_import_and_v7_checksum_validation_cover_assets_and_histories() {
        fn seal(b: &mut [u8]) {
            let n = b.len() - 4;
            let mut crc = !0u32;
            for &v in &b[..n] {
                crc ^= u32::from(v);
                for _ in 0..8 {
                    crc = (crc >> 1) ^ (0xedb88320u32 & 0u32.wrapping_sub(crc & 1));
                }
            }
            b[n..].copy_from_slice(&(!crc).to_le_bytes());
        }
        let mut d = create();
        let mut old = d.checkpoint().unwrap();
        old[8..12].copy_from_slice(&6u32.to_le_bytes());
        seal(&mut old);
        assert_eq!(
            Document::from_checkpoint(&old).unwrap().snapshot(),
            d.snapshot()
        );
        d.commit(1, &asset_command(1, Some(reference(1)))).unwrap();
        let bytes = d.checkpoint().unwrap();
        assert_eq!(&bytes[8..12], &7u32.to_le_bytes());
        for end in 0..bytes.len() {
            assert!(Document::from_checkpoint(&bytes[..end]).is_err());
        }
        let mut forged = bytes.clone();
        forged[8..12].copy_from_slice(&6u32.to_le_bytes());
        seal(&mut forged);
        assert!(Document::from_checkpoint(&forged).is_err());
        // Final asset record targets a missing entity even with a valid checksum.
        let mut bad = bytes.clone();
        let n = bad.len();
        bad[n - 64..n - 60].copy_from_slice(&99u32.to_le_bytes());
        seal(&mut bad);
        assert!(Document::from_checkpoint(&bad).is_err());
        let mut bad = bytes.clone();
        let n = bad.len();
        bad[n - 76..n - 68].copy_from_slice(&u64::MAX.to_le_bytes());
        seal(&mut bad);
        assert!(Document::from_checkpoint(&bad).is_err());
    }
}
