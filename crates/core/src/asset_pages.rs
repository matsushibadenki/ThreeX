//! Minimal uncompressed, two-level asset container. All lengths checked before allocation.
use crate::{AssetId, AssetRef};
use sha2::{Digest, Sha256};
const MAGIC: &[u8; 8] = b"THREEXA1";
pub const HEADER_BYTES: usize = 72;
pub const DESCRIPTOR_BYTES: usize = 60;
pub const MAX_MANIFEST_BYTES: usize = HEADER_BYTES + 2 * DESCRIPTOR_BYTES;
pub const MAX_PAGE_BYTES: u64 = 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetPageError {
    Format,
    UnsupportedVersion,
    Limit,
    Length,
    Bounds,
    Checksum,
    Geometry,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageDescriptor {
    pub offset: u64,
    pub length: u64,
    pub vertices: u32,
    pub indices: u32,
    pub error: f32,
    pub hash: [u8; 32],
}
impl PageDescriptor {
    fn size(&self) -> Result<usize, AssetPageError> {
        if self.vertices < 3 || self.indices < 3 || !self.indices.is_multiple_of(3) {
            return Err(AssetPageError::Geometry);
        }
        let size = u64::from(self.vertices) * 12 + u64::from(self.indices) * 4;
        if size > MAX_PAGE_BYTES {
            return Err(AssetPageError::Limit);
        }
        if size != self.length {
            return Err(AssetPageError::Length);
        }
        Ok(size as usize)
    }
}
#[derive(Clone, Debug)]
pub struct AssetManifest {
    reference: AssetRef,
    bounds: [f32; 6],
    file_len: u64,
    pages: Vec<PageDescriptor>,
}
pub fn page_hash(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
impl AssetManifest {
    pub fn reference(&self) -> AssetRef {
        self.reference
    }
    pub fn bounds(&self) -> [f32; 6] {
        self.bounds
    }
    pub fn file_len(&self) -> u64 {
        self.file_len
    }
    pub fn page(&self, level: u8) -> Option<PageDescriptor> {
        self.pages.get(level as usize).copied()
    }
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }
    /// Validates the fixed header before a reader fetches at most 120 descriptor bytes.
    pub fn encoded_len(header: &[u8]) -> Result<usize, AssetPageError> {
        if header.len() != HEADER_BYTES {
            return Err(AssetPageError::Length);
        }
        if &header[..8] != MAGIC {
            return Err(if header[..7] == MAGIC[..7] {
                AssetPageError::UnsupportedVersion
            } else {
                AssetPageError::Format
            });
        }
        if header[68..72] != [0; 4] {
            return Err(AssetPageError::Format);
        }
        let count = u32::from_le_bytes(header[64..68].try_into().unwrap());
        if !(1..=2).contains(&count) {
            return Err(AssetPageError::Limit);
        }
        Ok(HEADER_BYTES + count as usize * DESCRIPTOR_BYTES)
    }
    pub fn decode(bytes: &[u8], expected: AssetRef) -> Result<Self, AssetPageError> {
        let header = bytes.get(..HEADER_BYTES).ok_or(AssetPageError::Length)?;
        let size = Self::encoded_len(header)?;
        if bytes.len() != size {
            return Err(AssetPageError::Length);
        }
        if page_hash(bytes) != expected.hash() {
            return Err(AssetPageError::Checksum);
        }
        let id =
            AssetId::from_bytes(bytes[8..24].try_into().unwrap()).ok_or(AssetPageError::Format)?;
        let revision = u64::from_le_bytes(bytes[24..32].try_into().unwrap());
        if id != expected.id() || revision != expected.revision() {
            return Err(AssetPageError::Format);
        }
        let bounds = std::array::from_fn(|i| {
            f32::from_le_bytes(bytes[32 + i * 4..36 + i * 4].try_into().unwrap())
        });
        if bounds.iter().any(|v| !v.is_finite()) || (0..3).any(|i| bounds[i] > bounds[i + 3]) {
            return Err(AssetPageError::Bounds);
        }
        let file_len = u64::from_le_bytes(bytes[56..64].try_into().unwrap());
        let mut pages = Vec::new();
        let mut end = size as u64;
        let mut prior_error = f32::INFINITY;
        for b in bytes[HEADER_BYTES..].as_chunks::<DESCRIPTOR_BYTES>().0 {
            let p = PageDescriptor {
                offset: u64::from_le_bytes(b[..8].try_into().unwrap()),
                length: u64::from_le_bytes(b[8..16].try_into().unwrap()),
                vertices: u32::from_le_bytes(b[16..20].try_into().unwrap()),
                indices: u32::from_le_bytes(b[20..24].try_into().unwrap()),
                error: f32::from_le_bytes(b[24..28].try_into().unwrap()),
                hash: b[28..60].try_into().unwrap(),
            };
            p.size()?;
            if !p.error.is_finite() || p.error < 0. || p.error > prior_error {
                return Err(AssetPageError::Geometry);
            }
            let next = p
                .offset
                .checked_add(p.length)
                .ok_or(AssetPageError::Length)?;
            if p.offset < end || next > file_len {
                return Err(AssetPageError::Length);
            }
            end = next;
            prior_error = p.error;
            pages.push(p);
        }
        if end != file_len {
            return Err(AssetPageError::Length);
        }
        Ok(Self {
            reference: expected,
            bounds,
            file_len,
            pages,
        })
    }
    /// Fixture/importer helper: serializes bounded metadata, not the entire asset file.
    pub fn encode(
        id: AssetId,
        revision: u64,
        bounds: [f32; 6],
        file_len: u64,
        pages: &[PageDescriptor],
    ) -> Result<(AssetRef, Vec<u8>), AssetPageError> {
        if !(1..=2).contains(&pages.len()) {
            return Err(AssetPageError::Limit);
        }
        let mut b = Vec::with_capacity(MAX_MANIFEST_BYTES);
        b.extend(MAGIC);
        b.extend(id.bytes());
        b.extend(revision.to_le_bytes());
        for v in bounds {
            b.extend(v.to_le_bytes());
        }
        b.extend(file_len.to_le_bytes());
        b.extend((pages.len() as u32).to_le_bytes());
        b.extend([0; 4]);
        for p in pages {
            b.extend(p.offset.to_le_bytes());
            b.extend(p.length.to_le_bytes());
            b.extend(p.vertices.to_le_bytes());
            b.extend(p.indices.to_le_bytes());
            b.extend(p.error.to_le_bytes());
            b.extend(p.hash);
        }
        let reference = AssetRef::new(id, revision, page_hash(&b)).ok_or(AssetPageError::Format)?;
        Self::decode(&b, reference)?;
        Ok((reference, b))
    }
    /// Takes ownership of a single bounded input buffer; no whole-asset copy or expansion.
    pub fn decode_page(&self, level: u8, bytes: Vec<u8>) -> Result<ValidatedPage, AssetPageError> {
        let p = self.page(level).ok_or(AssetPageError::Format)?;
        if bytes.len() != p.size()? {
            return Err(AssetPageError::Length);
        }
        if page_hash(&bytes) != p.hash {
            return Err(AssetPageError::Checksum);
        }
        let vertex_bytes = p.vertices as usize * 12;
        for v in bytes[..vertex_bytes].as_chunks::<12>().0 {
            for i in 0..3 {
                let x = f32::from_le_bytes(v[i * 4..i * 4 + 4].try_into().unwrap());
                if !x.is_finite() || x < self.bounds[i] || x > self.bounds[i + 3] {
                    return Err(AssetPageError::Bounds);
                }
            }
        }
        for b in bytes[vertex_bytes..].as_chunks::<4>().0 {
            if u32::from_le_bytes(*b) >= p.vertices {
                return Err(AssetPageError::Geometry);
            }
        }
        Ok(ValidatedPage {
            reference: self.reference,
            level,
            bytes: bytes.into_boxed_slice(),
            vertices: p.vertices,
            indices: p.indices,
        })
    }
}
#[derive(Debug)]
pub struct ValidatedPage {
    reference: AssetRef,
    level: u8,
    bytes: Box<[u8]>,
    vertices: u32,
    indices: u32,
}
impl ValidatedPage {
    pub fn reference(&self) -> AssetRef {
        self.reference
    }
    pub fn level(&self) -> u8 {
        self.level
    }
    pub fn byte_len(&self) -> u64 {
        self.bytes.len() as u64
    }
    pub fn vertices(&self) -> u32 {
        self.vertices
    }
    pub fn indices(&self) -> u32 {
        self.indices
    }
    pub fn vertex_bytes(&self) -> &[u8] {
        &self.bytes[..self.vertices as usize * 12]
    }
    pub fn index_bytes(&self) -> &[u8] {
        &self.bytes[self.vertices as usize * 12..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn payload() -> Vec<u8> {
        let mut b = Vec::new();
        for v in [[0f32, 0., 0.], [1., 0., 0.], [0., 1., 0.]] {
            for x in v {
                b.extend(x.to_le_bytes());
            }
        }
        for i in [0u32, 1, 2] {
            b.extend(i.to_le_bytes());
        }
        b
    }
    fn fixture(page: Vec<u8>) -> (AssetRef, Vec<u8>) {
        let offset = (HEADER_BYTES + DESCRIPTOR_BYTES) as u64;
        let p = PageDescriptor {
            offset,
            length: page.len() as u64,
            vertices: 3,
            indices: 3,
            error: 1.,
            hash: page_hash(&page),
        };
        AssetManifest::encode(
            AssetId::from_bytes([1; 16]).unwrap(),
            1,
            [0., 0., 0., 1., 1., 0.],
            offset + p.length,
            &[p],
        )
        .unwrap()
    }
    fn reseal(bytes: &[u8], reference: AssetRef) -> AssetRef {
        AssetRef::new(reference.id(), reference.revision(), page_hash(bytes)).unwrap()
    }
    #[test]
    fn bounded_manifest_and_page_integrity_roundtrip() {
        let bytes = payload();
        let (reference, manifest) = fixture(bytes.clone());
        let decoded = AssetManifest::decode(&manifest, reference).unwrap();
        assert_eq!(decoded.page_count(), 1);
        let page = decoded.decode_page(0, bytes.clone()).unwrap();
        assert_eq!(
            (page.vertices(), page.indices(), page.byte_len()),
            (3, 3, 48)
        );
        assert_eq!(page.vertex_bytes().len(), 36);
        assert_eq!(page.index_bytes().len(), 12);
        for n in 0..manifest.len() {
            assert!(AssetManifest::decode(&manifest[..n], reference).is_err());
        }
        for n in 0..bytes.len() {
            assert!(decoded.decode_page(0, bytes[..n].to_vec()).is_err());
        }
        let mut bad = bytes;
        bad[4] ^= 1;
        assert!(matches!(
            decoded.decode_page(0, bad),
            Err(AssetPageError::Checksum)
        ));
        let mut bad = manifest.clone();
        bad[32] ^= 1;
        assert!(matches!(
            AssetManifest::decode(&bad, reference),
            Err(AssetPageError::Checksum)
        ));
    }
    #[test]
    fn forged_valid_hashes_cannot_bypass_geometry_and_length_validation() {
        let (reference, manifest) = fixture(payload());
        for (range, value) in [
            (64..68, u32::MAX.to_le_bytes().to_vec()),
            (72..80, u64::MAX.to_le_bytes().to_vec()),
            (80..88, u64::MAX.to_le_bytes().to_vec()),
            (88..92, u32::MAX.to_le_bytes().to_vec()),
            (96..100, f32::NAN.to_le_bytes().to_vec()),
            (32..36, f32::INFINITY.to_le_bytes().to_vec()),
        ] {
            let mut bad = manifest.clone();
            bad[range].copy_from_slice(&value);
            assert!(AssetManifest::decode(&bad, reseal(&bad, reference)).is_err());
        }
        for at in [0, 36] {
            let mut bytes = payload();
            bytes[at..at + 4]
                .copy_from_slice(&if at == 0 { f32::NAN.to_bits() } else { 3u32 }.to_le_bytes());
            let (r, m) = fixture(bytes.clone());
            let manifest = AssetManifest::decode(&m, r).unwrap();
            assert!(manifest.decode_page(0, bytes).is_err());
        }
        let mut bad = manifest;
        bad[7] = b'2';
        assert!(matches!(
            AssetManifest::decode(&bad, reference),
            Err(AssetPageError::UnsupportedVersion)
        ));
    }
}
