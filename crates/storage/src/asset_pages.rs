//! Bounded seek/read helpers for caller-owned loader work. Never read the entire asset file.
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};
use threex_core::{
    AssetRef,
    asset_pages::{AssetManifest, AssetPageError, HEADER_BYTES, ValidatedPage},
};
#[derive(Debug)]
pub enum PageReadError {
    Io(std::io::Error),
    Format(AssetPageError),
}
impl From<std::io::Error> for PageReadError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<AssetPageError> for PageReadError {
    fn from(e: AssetPageError) -> Self {
        Self::Format(e)
    }
}
pub struct AssetReader {
    file: File,
    manifest: AssetManifest,
}
impl AssetReader {
    pub fn open(path: impl AsRef<Path>, expected: AssetRef) -> Result<Self, PageReadError> {
        let mut file = File::open(path)?;
        let mut header = [0u8; HEADER_BYTES];
        file.read_exact(&mut header)?;
        let size = AssetManifest::encoded_len(&header)?;
        let mut bytes = vec![0; size];
        bytes[..HEADER_BYTES].copy_from_slice(&header);
        file.read_exact(&mut bytes[HEADER_BYTES..])?;
        let manifest = AssetManifest::decode(&bytes, expected)?;
        if file.metadata()?.len() != manifest.file_len() {
            return Err(AssetPageError::Length.into());
        }
        Ok(Self { file, manifest })
    }
    pub fn manifest(&self) -> &AssetManifest {
        &self.manifest
    }
    /// Read only the page named by an admitted request; reject a mismatched asset before I/O.
    pub fn read_request(
        &mut self,
        request: threex_renderer::asset_pages::PageRequest,
    ) -> Result<ValidatedPage, PageReadError> {
        if request.reference() != self.manifest.reference() {
            return Err(AssetPageError::Format.into());
        }
        self.read_page(request.level())
    }
    pub fn read_page(&mut self, level: u8) -> Result<ValidatedPage, PageReadError> {
        let p = self.manifest.page(level).ok_or(AssetPageError::Format)?;
        // Manifest decode already bounded length to MAX_PAGE_BYTES (1 MiB).
        self.file.seek(SeekFrom::Start(p.offset))?;
        let mut bytes = vec![0; p.length as usize];
        self.file.read_exact(&mut bytes)?;
        Ok(self.manifest.decode_page(level, bytes)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use threex_core::{
        AssetId, Document, DocumentId, asset_command,
        asset_pages::{DESCRIPTOR_BYTES, PageDescriptor, page_hash},
    };
    use threex_renderer::asset_pages::{Lod, PageAdmission, PageFailure, PagePool};
    static SERIAL: AtomicU64 = AtomicU64::new(1);
    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "threex-g02-{}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    fn fixture() -> (AssetRef, Vec<u8>) {
        let mut payload = Vec::new();
        for v in [[0f32, 0., 0.], [1., 0., 0.], [0., 1., 0.]] {
            for x in v {
                payload.extend(x.to_le_bytes());
            }
        }
        for i in [0u32, 1, 2] {
            payload.extend(i.to_le_bytes());
        }
        let offset = (HEADER_BYTES + 2 * DESCRIPTOR_BYTES) as u64;
        let p = PageDescriptor {
            offset,
            length: payload.len() as u64,
            vertices: 3,
            indices: 3,
            error: 1.,
            hash: page_hash(&payload),
        };
        let detail = PageDescriptor {
            offset: offset + p.length,
            error: 0.,
            ..p
        };
        let (reference, mut bytes) = AssetManifest::encode(
            AssetId::from_bytes([8; 16]).unwrap(),
            1,
            [0., 0., 0., 1., 1., 0.],
            detail.offset + detail.length,
            &[p, detail],
        )
        .unwrap();
        bytes.extend(&payload);
        bytes.extend(payload);
        (reference, bytes)
    }
    fn document(reference: AssetRef) -> Document {
        let mut d = Document::default();
        d.bind_document_id(DocumentId::from_bytes([4; 16]).unwrap())
            .unwrap();
        let mut create = [0u8; 64];
        create[..4].copy_from_slice(&1u32.to_le_bytes());
        create[4..8].copy_from_slice(&1u32.to_le_bytes());
        d.commit(0, &[create, asset_command(1, Some(reference))].concat())
            .unwrap();
        d
    }
    #[test]
    fn seek_read_and_hash_failure_keep_root_and_reuse_capacity() {
        let t = Temp::new();
        let (reference, bytes) = fixture();
        std::fs::write(&t.0, &bytes).unwrap();
        let mut reader = AssetReader::open(&t.0, reference).unwrap();
        let manifest = reader.manifest().clone();
        let d = document(reference);
        let mut pool = PagePool::new(96, 2);
        let PageAdmission::Load(root) = pool.begin(&d, 1, &manifest, 0).unwrap() else {
            panic!()
        };
        let handle = pool
            .complete(
                &d,
                root,
                reader
                    .read_request(root)
                    .map_err(|_| PageFailure::ReadFailed),
                |p| p,
            )
            .unwrap();
        assert_eq!(pool.get(handle).unwrap().indices(), 3);
        let PageAdmission::Load(detail) = pool.begin(&d, 1, &manifest, 1).unwrap() else {
            panic!()
        };
        let mut corrupt = bytes.clone();
        let n = corrupt.len();
        corrupt[n - 1] ^= 1;
        std::fs::write(&t.0, corrupt).unwrap();
        assert!(
            pool.complete(
                &d,
                detail,
                reader
                    .read_request(detail)
                    .map_err(|_| PageFailure::ReadFailed),
                |_| panic!("corrupt page factory")
            )
            .is_err()
        );
        assert_eq!(pool.select(&manifest, true).lod, Lod::Root);
        assert_eq!(pool.status().reserved_bytes, 48);
        std::fs::write(&t.0, &bytes).unwrap();
        let PageAdmission::Load(retry) = pool.begin(&d, 1, &manifest, 1).unwrap() else {
            panic!()
        };
        pool.complete(
            &d,
            retry,
            reader
                .read_request(retry)
                .map_err(|_| PageFailure::ReadFailed),
            |p| p,
        )
        .unwrap();
        assert_eq!(pool.select(&manifest, true).lod, Lod::Detail);
        pool.evict_detail(reference);
        let PageAdmission::Load(missing) = pool.begin(&d, 1, &manifest, 1).unwrap() else {
            panic!()
        };
        std::fs::OpenOptions::new()
            .write(true)
            .open(&t.0)
            .unwrap()
            .set_len(manifest.page(1).unwrap().offset)
            .unwrap();
        assert!(
            pool.complete(
                &d,
                missing,
                reader
                    .read_request(missing)
                    .map_err(|_| PageFailure::ReadFailed),
                |_| panic!("short read factory")
            )
            .is_err()
        );
        assert_eq!(pool.select(&manifest, true).handle, Some(handle));
        pool.release_asset(reference);
        assert_eq!(pool.status().reserved_bytes, 0);
    }
    #[test]
    fn bounded_header_truncation_and_delayed_thread_result_after_document_edit() {
        let t = Temp::new();
        let (reference, bytes) = fixture();
        assert!(AssetReader::open(&t.0, reference).is_err());
        for end in [0, 71, 72, 130, 191, bytes.len() - 1] {
            std::fs::write(&t.0, &bytes[..end]).unwrap();
            assert!(AssetReader::open(&t.0, reference).is_err());
        }
        let mut huge = bytes.clone();
        huge[64..68].copy_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(&t.0, huge).unwrap();
        assert!(matches!(
            AssetReader::open(&t.0, reference),
            Err(PageReadError::Format(AssetPageError::Limit))
        ));
        std::fs::write(&t.0, &bytes).unwrap();
        let mut reader = AssetReader::open(&t.0, reference).unwrap();
        let manifest = reader.manifest().clone();
        let mut d = document(reference);
        let mut pool = PagePool::new(48, 1);
        let PageAdmission::Load(request) = pool.begin(&d, 1, &manifest, 0).unwrap() else {
            panic!()
        };
        let job = std::thread::spawn(move || reader.read_request(request));
        d.commit(1, &asset_command(1, None)).unwrap();
        d.undo(2).unwrap();
        let result = job.join().unwrap().map_err(|_| PageFailure::ReadFailed);
        assert_eq!(
            pool.complete(&d, request, result, |_| -> ValidatedPage {
                panic!("stale thread result")
            }),
            Err(PageFailure::Stale)
        );
        assert_eq!(pool.select(&manifest, true).lod, Lod::BoundsProxy);
        assert_eq!(pool.status().reserved_bytes, 0);
    }
}
