//! Portable checkpoint codec, independent of OS and GPU. Limits before allocation.
use crate::{
    Document, Scene,
    document::{Edit, record},
};
#[derive(Debug, PartialEq, Eq)]
pub enum CheckpointError {
    Corrupt,
    UnsupportedVersion(u32),
    TooLarge,
    InvalidScene(String),
}
const MAGIC: &[u8; 8] = b"THREEXD\0";
const VERSION: u32 = 4;
pub const MAX_CHECKPOINT_BYTES: usize = 128 * 1024 * 1024;
const MAX_HISTORY: usize = 8 * 1024 * 1024;
const MAX_COMMAND: usize = 256 * 1024;
const MAX_OBJECTS: usize = 500_000;
fn checksum(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320u32 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}
impl Document {
    /// Version 3: durable closed-session tombstones plus document identity, session watermarks, receipts, scene and bounded history.
    /// No GPU indices, renderer stamps or OS handles are persisted.
    pub fn checkpoint(&self) -> Result<Vec<u8>, CheckpointError> {
        if self.scene.len() > MAX_OBJECTS
            || self.history_bytes > MAX_HISTORY
            || self.command_limit > MAX_COMMAND
            || self.history_limit > MAX_HISTORY
        {
            return Err(CheckpointError::TooLarge);
        }
        let mut b = Vec::new();
        b.extend(MAGIC);
        b.extend(VERSION.to_le_bytes());
        for v in [
            self.revision(),
            self.history_limit as u64,
            self.command_limit as u64,
            self.scene.len() as u64,
            self.undo.len() as u64,
            self.redo.len() as u64,
        ] {
            b.extend(v.to_le_bytes());
        }
        for (id, o) in self.scene.objects() {
            b.extend(record(2, id, o));
        }
        for e in self.undo.iter().chain(&self.redo) {
            b.extend((e.forward.len() as u64).to_le_bytes());
            b.extend((e.inverse.len() as u64).to_le_bytes());
            b.extend(&e.forward);
            b.extend(&e.inverse);
        }
        b.extend(self.id.map_or([0; 16], |id| id.bytes()));
        b.extend((self.ledger.sessions.len() as u64).to_le_bytes());
        for (session, highest) in &self.ledger.sessions {
            b.extend(session.bytes());
            b.extend(highest.to_le_bytes());
            b.push(u8::from(self.ledger.closed.contains(session)));
        }
        b.extend((self.ledger.receipts.len() as u64).to_le_bytes());
        for r in &self.ledger.receipts {
            b.extend(r.session.bytes());
            for value in [r.sequence, r.expected, r.revision, r.commands.len() as u64] {
                b.extend(value.to_le_bytes())
            }
            b.push(r.kind);
            let error = match &r.failure {
                None => Vec::new(),
                Some(e) => crate::encode_request_error(e).ok_or(CheckpointError::TooLarge)?,
            };
            b.extend((error.len() as u32).to_le_bytes());
            b.extend(error);
            b.extend(&r.commands);
        }
        if b.len() + 4 > MAX_CHECKPOINT_BYTES {
            return Err(CheckpointError::TooLarge);
        }
        b.extend(checksum(&b).to_le_bytes());
        Ok(b)
    }
    /// Restores into a fresh Scene identity. Invalid files never modify a live Document.
    pub fn from_checkpoint(bytes: &[u8]) -> Result<Self, CheckpointError> {
        if bytes.len() > MAX_CHECKPOINT_BYTES {
            return Err(CheckpointError::TooLarge);
        }
        if bytes.len() < 64 || &bytes[..8] != MAGIC {
            return Err(CheckpointError::Corrupt);
        }
        let version = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        if !(1..=VERSION).contains(&version) {
            return Err(CheckpointError::UnsupportedVersion(version));
        }
        let end = bytes.len() - 4;
        if checksum(&bytes[..end]) != u32::from_le_bytes(bytes[end..].try_into().unwrap()) {
            return Err(CheckpointError::Corrupt);
        }
        let mut r = Reader {
            bytes: &bytes[12..end],
            at: 0,
        };
        let revision = r.u64()?;
        if revision == u64::MAX {
            return Err(CheckpointError::Corrupt);
        }
        let history_limit = r.size(MAX_HISTORY)?;
        let command_limit = r.size(MAX_COMMAND)?;
        let count = r.size(MAX_OBJECTS)?;
        let undo_count = r.size(MAX_HISTORY / 64)?;
        let redo_count = r.size(MAX_HISTORY / 64)?;
        let records = r.take(count.checked_mul(64).ok_or(CheckpointError::TooLarge)?)?;
        let mut initial = Vec::with_capacity(count * 128);
        let mut last = 0;
        for c in records.as_chunks::<64>().0 {
            let id = u32::from_le_bytes(c[4..8].try_into().unwrap());
            if id <= last || c[..4] != 2u32.to_le_bytes() {
                return Err(CheckpointError::Corrupt);
            }
            last = id;
            initial.extend(record(1, id, crate::Object::default()));
            initial.extend(c);
        }
        let mut document = Document::new(history_limit, command_limit);
        document
            .scene
            .apply(&initial)
            .map_err(CheckpointError::InvalidScene)?;
        document.scene.revision = revision;
        for i in 0..undo_count + redo_count {
            let f = r.size(command_limit)?;
            let inv = r.size(history_limit)?;
            if f == 0 || !f.is_multiple_of(64) || !inv.is_multiple_of(64) {
                return Err(CheckpointError::Corrupt);
            }
            let size = f.checked_add(inv).ok_or(CheckpointError::TooLarge)?;
            document.history_bytes = document
                .history_bytes
                .checked_add(size)
                .ok_or(CheckpointError::TooLarge)?;
            if document.history_bytes > history_limit {
                return Err(CheckpointError::TooLarge);
            }
            let edit = Edit {
                forward: r.take(f)?.to_vec(),
                inverse: r.take(inv)?.to_vec(),
            };
            if i < undo_count {
                document.undo.push(edit)
            } else {
                document.redo.push(edit)
            }
        }
        if version >= 2 {
            document.id = crate::DocumentId::from_bytes(r.take(16)?.try_into().unwrap());
            let sessions = r.size(if version == 2 {
                64
            } else {
                crate::MAX_SESSION_RECORDS
            })?;
            for _ in 0..sessions {
                let id = crate::SessionId::from_bytes(r.take(16)?.try_into().unwrap())
                    .ok_or(CheckpointError::Corrupt)?;
                let high = r.u64()?;
                if version >= 3 {
                    match r.take(1)?[0] {
                        0 => {}
                        1 => {
                            document.ledger.closed.insert(id);
                        }
                        _ => return Err(CheckpointError::Corrupt),
                    }
                }
                if high == 0 || document.ledger.sessions.insert(id, high).is_some() {
                    return Err(CheckpointError::Corrupt);
                }
            }
            if sessions - document.ledger.closed.len() > 64 {
                return Err(CheckpointError::Corrupt);
            }
            let receipts = r.size(64)?;
            let mut keys = std::collections::BTreeSet::new();
            for _ in 0..receipts {
                let session = crate::SessionId::from_bytes(r.take(16)?.try_into().unwrap())
                    .ok_or(CheckpointError::Corrupt)?;
                let sequence = r.u64()?;
                let expected = r.u64()?;
                let revision = r.u64()?;
                let count = r.size(MAX_COMMAND)?;
                let kind = r.take(1)?[0];
                let failure = if version >= 4 {
                    let count = u32::from_le_bytes(r.take(4)?.try_into().unwrap()) as usize;
                    if count > 1025 {
                        return Err(CheckpointError::TooLarge);
                    }
                    if count == 0 {
                        None
                    } else {
                        Some(Box::new(
                            crate::decode_request_error(r.take(count)?)
                                .ok_or(CheckpointError::Corrupt)?,
                        ))
                    }
                } else {
                    None
                };
                if sequence == 0
                    || !keys.insert((session, sequence))
                    || document
                        .ledger
                        .sessions
                        .get(&session)
                        .is_none_or(|&high| sequence > high)
                    || revision > document.revision()
                    || (failure.is_none()
                        && expected != revision
                        && expected.checked_add(1) != Some(revision))
                    || failure.as_deref().is_some_and(|e| match e {
                        crate::DocumentError::RevisionConflict { current } => {
                            *current != revision || expected == revision
                        }
                        _ => expected != revision,
                    })
                    || !(1..=if version >= 3 { 4 } else { 3 }).contains(&kind)
                    || (kind == 4
                        && failure.is_none()
                        && (expected != revision
                            || !document.ledger.closed.contains(&session)
                            || document.ledger.sessions[&session] != sequence))
                    || (document.ledger.closed.contains(&session)
                        && document.ledger.sessions[&session] == sequence
                        && (kind != 4 || failure.is_some()))
                    || (kind != 1 && count != 0)
                    || (failure.is_none() && !count.is_multiple_of(64))
                {
                    return Err(CheckpointError::Corrupt);
                }
                document.ledger.bytes += count;
                if document.ledger.bytes > MAX_HISTORY {
                    return Err(CheckpointError::TooLarge);
                }
                let commands = r.take(count)?.to_vec();
                document.ledger.receipts.push_back(crate::request::Receipt {
                    session,
                    sequence,
                    kind,
                    expected,
                    revision,
                    commands,
                    failure,
                });
            }
            if sessions > 0 && document.id.is_none() {
                return Err(CheckpointError::Corrupt);
            }
        }
        if r.at != r.bytes.len() {
            return Err(CheckpointError::Corrupt);
        }
        // Validate both stacks against real Scene semantics before accepting history.
        let current = document.snapshot().objects;
        let mut check = Scene::default();
        check
            .apply(&initial)
            .map_err(CheckpointError::InvalidScene)?;
        for e in document.undo.iter().rev() {
            check
                .apply(&e.inverse)
                .map_err(CheckpointError::InvalidScene)?;
        }
        for e in &document.undo {
            check
                .apply(&e.forward)
                .map_err(CheckpointError::InvalidScene)?;
        }
        if check
            .objects()
            .collect::<std::collections::BTreeMap<_, _>>()
            != current
        {
            return Err(CheckpointError::Corrupt);
        }
        for e in document.redo.iter().rev() {
            check
                .apply(&e.forward)
                .map_err(CheckpointError::InvalidScene)?;
        }
        for e in &document.redo {
            check
                .apply(&e.inverse)
                .map_err(CheckpointError::InvalidScene)?;
        }
        if check
            .objects()
            .collect::<std::collections::BTreeMap<_, _>>()
            != current
        {
            return Err(CheckpointError::Corrupt);
        }
        Ok(document)
    }
}
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], CheckpointError> {
        let end = self.at.checked_add(n).ok_or(CheckpointError::Corrupt)?;
        let b = self
            .bytes
            .get(self.at..end)
            .ok_or(CheckpointError::Corrupt)?;
        self.at = end;
        Ok(b)
    }
    fn u64(&mut self) -> Result<u64, CheckpointError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn size(&mut self, max: usize) -> Result<usize, CheckpointError> {
        let v = self.u64()?;
        if v > max as u64 {
            return Err(CheckpointError::TooLarge);
        }
        Ok(v as usize)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_v1_import_and_v2_identity_binding_roundtrip() {
        let mut d = Document::default();
        d.commit(0, &record(1, 42, crate::Object::default()))
            .unwrap();
        let mut bytes = d.checkpoint().unwrap();
        bytes.truncate(bytes.len() - 36);
        bytes[8..12].copy_from_slice(&1u32.to_le_bytes());
        bytes.extend(checksum(&bytes).to_le_bytes());
        let mut old = Document::from_checkpoint(&bytes).unwrap();
        assert_eq!(old.document_id(), None);
        assert_eq!(old.snapshot(), d.snapshot());
        let id = crate::DocumentId::from_bytes([8; 16]).unwrap();
        old.bind_document_id(id).unwrap();
        assert!(
            old.bind_document_id(crate::DocumentId::from_bytes([7; 16]).unwrap())
                .is_err()
        );
        let new = Document::from_checkpoint(&old.checkpoint().unwrap()).unwrap();
        assert_eq!(new.document_id(), Some(id));
        assert_eq!(new.snapshot(), d.snapshot());
        assert_eq!(id.to_string().parse::<crate::DocumentId>(), Ok(id));
        assert!(
            "00000000000000000000000000000000"
                .parse::<crate::DocumentId>()
                .is_err()
        );
    }
    #[test]
    fn roundtrip_history_and_fresh_scene_identity() {
        let mut d = Document::default();
        let a = record(1, 1, crate::Object::default());
        d.commit(0, &a).unwrap();
        let o = crate::Object {
            mesh: true,
            position: [3., 2., 1.],
            rotation: [0.1, 0.2, 0.3],
            scale: [-1., 2., 0.5],
            color: [0.2, 0.3, 0.4],
            ..Default::default()
        };
        d.commit(1, &record(2, 1, o)).unwrap();
        d.undo(2).unwrap();
        let mut loaded = Document::from_checkpoint(&d.checkpoint().unwrap()).unwrap();
        assert_eq!(loaded.snapshot(), d.snapshot());
        assert_eq!(loaded.history_bytes(), d.history_bytes());
        assert!(!loaded.scene().is_current(Some(&d.scene().stamp())));
        loaded.redo(3).unwrap();
        assert_eq!(loaded.scene().object(1), Some(o));
        loaded.undo(4).unwrap();
        loaded.undo(5).unwrap();
        assert!(loaded.scene().is_empty());
    }
    #[test]
    fn checksummed_invalid_scene_and_history_are_rejected() {
        let mut d = Document::default();
        d.commit(0, &record(1, 1, crate::Object::default()))
            .unwrap();
        let original = d.checkpoint().unwrap();
        let seal = |bytes: &mut Vec<u8>| {
            let end = bytes.len() - 4;
            let crc = checksum(&bytes[..end]);
            bytes[end..].copy_from_slice(&crc.to_le_bytes());
        };
        // Header is 60 bytes. A checksum is integrity detection, not a substitute for validation.
        let mut b = original.clone();
        b[68..72].copy_from_slice(&f32::NAN.to_le_bytes());
        seal(&mut b);
        assert!(matches!(
            Document::from_checkpoint(&b),
            Err(CheckpointError::InvalidScene(_))
        ));
        let mut b = original.clone();
        b[120..124].copy_from_slice(&1u32.to_le_bytes());
        seal(&mut b);
        assert!(matches!(
            Document::from_checkpoint(&b),
            Err(CheckpointError::InvalidScene(_))
        ));
        // Inverse starts after header + object + two lengths + forward (64 bytes).
        let mut b = original;
        b[204..208].copy_from_slice(&99u32.to_le_bytes());
        seal(&mut b);
        assert!(matches!(
            Document::from_checkpoint(&b),
            Err(CheckpointError::InvalidScene(_))
        ));
    }
    #[test]
    fn corrupt_unknown_version_and_unbounded_fields_rejected() {
        let d = Document::default();
        let bytes = d.checkpoint().unwrap();
        for end in 0..bytes.len() {
            assert!(Document::from_checkpoint(&bytes[..end]).is_err());
        }
        let mut b = bytes.clone();
        b[12] ^= 1;
        assert!(matches!(
            Document::from_checkpoint(&b),
            Err(CheckpointError::Corrupt)
        ));
        let mut b = bytes.clone();
        b[8..12].copy_from_slice(&5u32.to_le_bytes());
        assert!(matches!(
            Document::from_checkpoint(&b),
            Err(CheckpointError::UnsupportedVersion(5))
        ));
        let mut b = bytes.clone();
        b[36..44].copy_from_slice(&u64::MAX.to_le_bytes());
        let n = b.len() - 4;
        let c = checksum(&b[..n]);
        b[n..].copy_from_slice(&c.to_le_bytes());
        assert!(matches!(
            Document::from_checkpoint(&b),
            Err(CheckpointError::TooLarge)
        ));
    }
    #[test]
    fn v2_sessions_migrate_active_and_v3_closed_flags_are_validated() {
        let mut d = Document::default();
        let id = crate::DocumentId::from_bytes([81; 16]).unwrap();
        d.bind_document_id(id).unwrap();
        let s = crate::SessionId::from_bytes([82; 16]).unwrap();
        let crate::RequestPreparation::Pending(p) = d.prepare_request(id, s, 1, 1, 0, &[]).unwrap()
        else {
            panic!()
        };
        d.apply_request(p).unwrap();
        let original = d.checkpoint().unwrap();
        // Empty scene/history: session flag offset = header60 + ID16 + count8 + session16 + high8.
        let flag = 108;
        assert_eq!(original[flag], 0);
        let mut legacy = original[..original.len() - 4].to_vec();
        legacy.drain(166..170);
        legacy.remove(flag);
        legacy[8..12].copy_from_slice(&2u32.to_le_bytes());
        legacy.extend(checksum(&legacy).to_le_bytes());
        let mut restored = Document::from_checkpoint(&legacy).unwrap();
        assert_eq!(
            restored.session_status(s),
            Some(crate::SessionStatus::Active { sequence: 1 })
        );
        let crate::RequestPreparation::Pending(p) =
            restored.prepare_request(id, s, 2, 4, 0, &[]).unwrap()
        else {
            panic!()
        };
        restored.apply_request(p).unwrap();
        let closed = restored.checkpoint().unwrap();
        assert_eq!(
            Document::from_checkpoint(&closed)
                .unwrap()
                .session_status(s),
            Some(crate::SessionStatus::Closed { sequence: 2 })
        );
        for value in [2, 255] {
            let mut corrupt = original[..original.len() - 4].to_vec();
            corrupt[flag] = value;
            corrupt.extend(checksum(&corrupt).to_le_bytes());
            assert!(Document::from_checkpoint(&corrupt).is_err());
        }
        let mut corrupt = closed[..closed.len() - 4].to_vec();
        corrupt[flag] = 0;
        corrupt.extend(checksum(&corrupt).to_le_bytes());
        assert!(Document::from_checkpoint(&corrupt).is_err());
    }
    #[test]
    fn v3_session_tombstones_migrate_with_success_receipts() {
        let mut d = Document::default();
        let id = crate::DocumentId::from_bytes([104; 16]).unwrap();
        d.bind_document_id(id).unwrap();
        let session = crate::SessionId::from_bytes([105; 16]).unwrap();
        for (sequence, kind) in [(1, 1), (2, 4)] {
            let crate::RequestPreparation::Pending(p) = d
                .prepare_request(id, session, sequence, kind, 0, &[])
                .unwrap()
            else {
                panic!()
            };
            d.apply_request(p).unwrap();
        }
        let mut old = d.checkpoint().unwrap();
        old.truncate(old.len() - 4);
        old.drain(219..223);
        old.drain(166..170);
        old[8..12].copy_from_slice(&3u32.to_le_bytes());
        old.extend(checksum(&old).to_le_bytes());
        let restored = Document::from_checkpoint(&old).unwrap();
        assert_eq!(
            restored.session_status(session),
            Some(crate::SessionStatus::Closed { sequence: 2 })
        );
        assert!(matches!(
            restored.prepare_request(id, session, 1, 1, 0, &[]),
            Ok(crate::RequestPreparation::Replay(0))
        ));
    }
}
