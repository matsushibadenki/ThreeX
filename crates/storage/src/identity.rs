use super::*;
use threex_core::{DocumentId, SessionId};
fn crc(b: &[u8]) -> u32 {
    let mut c = !0u32;
    for &v in b {
        c ^= v as u32;
        for _ in 0..8 {
            c = (c >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(c & 1));
        }
    }
    !c
}
#[cfg(unix)]
fn random() -> std::io::Result<[u8; 16]> {
    let mut b = [0; 16];
    File::open("/dev/urandom")?.read_exact(&mut b)?;
    if b == [0; 16] {
        return Err(std::io::Error::other("Invalid random identity"));
    }
    Ok(b)
}
#[cfg(not(unix))]
fn random() -> std::io::Result<[u8; 16]> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Native identity generation is not implemented",
    ))
}
pub fn new_session_id() -> std::io::Result<SessionId> {
    Ok(SessionId::from_bytes(random()?).unwrap())
}
/// Host-generated persistent asset identity; content revisions are supplied by the importer.
pub fn new_asset_id() -> std::io::Result<threex_core::AssetId> {
    Ok(threex_core::AssetId::from_bytes(random()?).unwrap())
}
impl DocumentStore {
    pub(super) fn read_identity(&self) -> Result<Option<DocumentId>, StorageError> {
        let f = match File::open(self.root.join("document.id")) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut b = Vec::new();
        f.take(29).read_to_end(&mut b)?;
        if b.len() != 28
            || &b[..8] != b"THREEXI1"
            || crc(&b[..24]) != u32::from_le_bytes(b[24..].try_into().unwrap())
        {
            return Err(StorageError::IdentityMismatch);
        }
        Ok(Some(
            DocumentId::from_bytes(b[8..24].try_into().unwrap())
                .ok_or(StorageError::IdentityMismatch)?,
        ))
    }
    pub(super) fn ensure_identity(
        &self,
        preferred: Option<DocumentId>,
    ) -> Result<DocumentId, StorageError> {
        if let Some(id) = self.read_identity()? {
            if preferred.is_some_and(|other| other != id) {
                return Err(StorageError::IdentityMismatch);
            }
            return Ok(id);
        }
        let id = match preferred {
            Some(id) => id,
            None => DocumentId::from_bytes(random()?).unwrap(),
        };
        let mut b = b"THREEXI1".to_vec();
        b.extend(id.bytes());
        b.extend(crc(&b).to_le_bytes());
        let tmp = self.root.join("document.id.tmp");
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        f.write_all(&b)?;
        f.sync_all()?;
        drop(f);
        fs::rename(tmp, self.root.join("document.id"))?;
        sync_directory(&self.root)?;
        Ok(id)
    }
}
