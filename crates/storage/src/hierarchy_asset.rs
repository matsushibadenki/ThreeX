//! Immutable THREEXA2 publication and bounded seek/read. Explicit offline API.
use crate::asset_pages::PageReadError;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use threex_core::{
    AssetId, AssetRef,
    asset_pages::AssetPageError,
    cluster_builder::SimplifiedClusters,
    hierarchy_asset::{HEADER_BYTES, HierarchyManifest, HierarchyPage, encode_page},
};
pub struct HierarchyAssetReader {
    file: File,
    manifest: HierarchyManifest,
}
impl HierarchyAssetReader {
    pub fn open(path: impl AsRef<Path>, expected: AssetRef) -> Result<Self, PageReadError> {
        let mut file = File::open(path)?;
        let mut header = [0; HEADER_BYTES];
        file.read_exact(&mut header)?;
        let len = HierarchyManifest::encoded_len(&header)?;
        let mut bytes = vec![0; len];
        bytes[..HEADER_BYTES].copy_from_slice(&header);
        file.read_exact(&mut bytes[HEADER_BYTES..])?;
        let manifest = HierarchyManifest::decode(&bytes, expected)?;
        if file.metadata()?.len() != manifest.file_len() {
            return Err(AssetPageError::Length.into());
        }
        Ok(Self { file, manifest })
    }
    pub fn manifest(&self) -> &HierarchyManifest {
        &self.manifest
    }
    /// Caller must admit residency and validate its Document/surface job guard.
    /// Expected reference is checked before seeking or allocating page bytes.
    pub fn read_page(
        &mut self,
        expected: AssetRef,
        node: u32,
    ) -> Result<HierarchyPage, PageReadError> {
        if expected != self.manifest.reference() {
            return Err(AssetPageError::Format.into());
        }
        let p = self.manifest.page(node).ok_or(AssetPageError::Format)?;
        self.file.seek(SeekFrom::Start(p.offset))?;
        let mut bytes = vec![0; p.length as usize];
        self.file.read_exact(&mut bytes)?;
        Ok(self.manifest.decode_page(node, bytes)?)
    }
}
static SERIAL: AtomicU64 = AtomicU64::new(1);
struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
/// Writes one compact page at a time, fsyncs, then publishes without replacing
/// an existing destination. Parent-directory fsync confirms publication durability.
/// An error after publication can leave a complete destination; it never rolls it back.
pub fn write_hierarchy_asset(
    path: impl AsRef<Path>,
    id: AssetId,
    revision: u64,
    source: &SimplifiedClusters,
    max_file_bytes: u64,
) -> Result<AssetRef, PageReadError> {
    let path = path.as_ref();
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let (manifest, metadata) = HierarchyManifest::prepare(id, revision, source)?;
    if manifest.file_len() > max_file_bytes {
        return Err(AssetPageError::Limit.into());
    }
    let (temporary, mut file) = (0..32)
        .find_map(|_| {
            let name = parent.join(format!(
                ".threex-asset-{}-{}.tmp",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            match OpenOptions::new().write(true).create_new(true).open(&name) {
                Ok(f) => Some(Ok((Temporary(name), f))),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(e) => Some(Err(e)),
            }
        })
        .unwrap_or_else(|| {
            Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "asset temporary names occupied",
            ))
        })?;
    file.write_all(&metadata)?;
    for node in 0..manifest.hierarchy().nodes().len() as u32 {
        let (_, bytes) = encode_page(source, node)?;
        let page = manifest.decode_page(node, bytes)?;
        file.write_all(page.vertex_bytes())?;
        file.write_all(page.index_bytes())?;
    }
    file.sync_all()?;
    drop(file);
    std::fs::hard_link(&temporary.0, path)?;
    drop(temporary);
    File::open(parent)?.sync_all()?;
    Ok(manifest.reference())
}

#[cfg(test)]
mod tests {
    use super::*;
    use threex_core::{
        cluster_builder::{ClusterBudget, ParentGeometryBudget},
        simplify::SimplifyBudget,
    };
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "threex-a2-{}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn source() -> SimplifiedClusters {
        let v = [
            [0., 0., 1.],
            [0., 0., -1.],
            [1., 0., 0.],
            [0., 1., 0.],
            [-1., 0., 0.],
            [0., -1., 0.],
        ];
        let t = [
            [0, 2, 3],
            [0, 3, 4],
            [0, 4, 5],
            [0, 5, 2],
            [1, 3, 2],
            [1, 4, 3],
            [1, 5, 4],
            [1, 2, 5],
        ];
        SimplifiedClusters::build(
            &v,
            &t,
            &[],
            ClusterBudget {
                leaf_triangles: 2,
                max_triangles: 8,
                max_nodes: 7,
            },
            ParentGeometryBudget {
                simplify: SimplifyBudget {
                    target_triangles: 4,
                    max_error: 2.,
                    max_attempts: 64,
                    max_input_triangles: 8,
                },
                max_source_vertices: 6,
                max_stored_triangles: 24,
                max_total_attempts: 64,
            },
        )
        .unwrap()
    }
    #[test]
    fn publish_reopen_and_overwrite_failure_keep_the_original_asset() {
        let temp = Temp::new();
        let path = temp.0.join("asset.a2");
        let source = source();
        let id = AssetId::from_bytes([71; 16]).unwrap();
        let reference = write_hierarchy_asset(&path, id, 1, &source, 1024 * 1024).unwrap();
        let original = std::fs::read(&path).unwrap();
        let mut reader = HierarchyAssetReader::open(&path, reference).unwrap();
        for node in 0..7 {
            let p = reader.read_page(reference, node).unwrap();
            assert_eq!(p.node(), node);
            assert!(p.byte_len() <= 1024 * 1024);
        }
        assert!(write_hierarchy_asset(&path, id, 2, &source, 1024 * 1024).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(std::fs::read_dir(&temp.0).unwrap().count(), 1);
        let mut unified = crate::asset_pages::AssetReader::open(&path, reference).unwrap();
        assert_eq!(unified.read_page(6).unwrap().level(), 6);
        let wrong = AssetRef::new(id, 2, reference.hash()).unwrap();
        assert!(reader.read_page(wrong, 0).is_err());
        assert!(HierarchyAssetReader::open(&path, wrong).is_err());
        assert!(reader.read_page(reference, u32::MAX).is_err());
    }
    #[test]
    fn corrupt_child_and_truncated_tail_leave_verified_root_usable() {
        let temp = Temp::new();
        let path = temp.0.join("asset.a2");
        let source = source();
        let reference = write_hierarchy_asset(
            &path,
            AssetId::from_bytes([72; 16]).unwrap(),
            1,
            &source,
            1024 * 1024,
        )
        .unwrap();
        let mut reader = HierarchyAssetReader::open(&path, reference).unwrap();
        let offset = reader.manifest().page(1).unwrap().offset;
        let mut file = OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(SeekFrom::Start(offset)).unwrap();
        file.write_all(&[0xff; 4]).unwrap();
        file.sync_all().unwrap();
        // Metadata opens without reading every page; damaged child is isolated.
        let mut reopened = HierarchyAssetReader::open(&path, reference).unwrap();
        assert!(reopened.read_page(reference, 1).is_err());
        assert!(reopened.read_page(reference, 0).is_ok());
        let cut = reopened
            .manifest()
            .hierarchy()
            .select(20, 20, |_, _| true, |p| p != 1);
        assert_eq!(cut.nodes, vec![0]);
        file.set_len(reader.manifest().file_len() - 1).unwrap();
        assert!(HierarchyAssetReader::open(&path, reference).is_err());
        assert!(reader.read_page(reference, 6).is_err());
        assert!(reader.read_page(reference, 0).is_ok());
    }
    #[test]
    fn budget_failure_and_interrupted_temporary_do_not_publish() {
        let temp = Temp::new();
        let path = temp.0.join("asset.a2");
        let source = source();
        let id = AssetId::from_bytes([73; 16]).unwrap();
        assert!(matches!(
            write_hierarchy_asset(&path, id, 1, &source, 1),
            Err(PageReadError::Format(AssetPageError::Limit))
        ));
        assert!(!path.exists());
        assert_eq!(std::fs::read_dir(&temp.0).unwrap().count(), 0);
        let (m, bytes) = HierarchyManifest::prepare(id, 1, &source).unwrap();
        std::fs::write(temp.0.join(".interrupted.tmp"), &bytes[..30]).unwrap();
        assert!(HierarchyAssetReader::open(&path, m.reference()).is_err());
        let reference = write_hierarchy_asset(&path, id, 1, &source, 1024 * 1024).unwrap();
        assert_eq!(reference, m.reference());
        assert!(HierarchyAssetReader::open(&path, reference).is_ok());
        std::fs::write(&path, &bytes[..30]).unwrap();
        assert!(HierarchyAssetReader::open(&path, reference).is_err());
    }
}
