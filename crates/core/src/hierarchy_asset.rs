//! THREEXA2: bounded parent-first tree metadata and independently verified pages.
use crate::{
    AssetId, AssetRef,
    asset_pages::{
        AssetPageError as Error, MAX_PAGE_BYTES, PageDescriptor, page_hash, validate_payload,
    },
    cluster_builder::SimplifiedClusters,
    lod_hierarchy::{LodHierarchy, LodNode},
};
use std::collections::{BTreeMap, BTreeSet};
pub const HEADER_BYTES: usize = 72;
pub const NODE_BYTES: usize = 88;
pub const MAX_NODES: usize = 4096;
pub const MAX_MANIFEST_BYTES: usize = HEADER_BYTES + NODE_BYTES * MAX_NODES;
const MAGIC: &[u8; 8] = b"THREEXA2";
#[derive(Clone, Debug)]
pub struct HierarchyManifest {
    reference: AssetRef,
    hierarchy: LodHierarchy,
    pages: Vec<PageDescriptor>,
    file_len: u64,
}
#[derive(Debug)]
pub struct HierarchyPage {
    reference: AssetRef,
    node: u32,
    descriptor: PageDescriptor,
    bytes: Box<[u8]>,
}
impl HierarchyPage {
    pub fn reference(&self) -> AssetRef {
        self.reference
    }
    pub fn node(&self) -> u32 {
        self.node
    }
    pub fn vertex_bytes(&self) -> &[u8] {
        &self.bytes[..self.descriptor.vertices as usize * 12]
    }
    pub fn index_bytes(&self) -> &[u8] {
        &self.bytes[self.descriptor.vertices as usize * 12..]
    }
    pub fn byte_len(&self) -> usize {
        self.bytes.len()
    }
}
impl HierarchyManifest {
    pub fn reference(&self) -> AssetRef {
        self.reference
    }
    pub fn hierarchy(&self) -> &LodHierarchy {
        &self.hierarchy
    }
    pub fn page(&self, node: u32) -> Option<PageDescriptor> {
        self.pages.get(node as usize).copied()
    }
    pub fn file_len(&self) -> u64 {
        self.file_len
    }
    pub fn encoded_len(header: &[u8]) -> Result<usize, Error> {
        if header.len() != HEADER_BYTES {
            return Err(Error::Length);
        }
        if &header[..8] != MAGIC {
            return Err(if header[..7] == MAGIC[..7] {
                Error::UnsupportedVersion
            } else {
                Error::Format
            });
        }
        if header[68..72] != [0; 4] {
            return Err(Error::Format);
        }
        let count = u32::from_le_bytes(header[64..68].try_into().unwrap()) as usize;
        if count == 0 || count > MAX_NODES {
            return Err(Error::Limit);
        }
        Ok(HEADER_BYTES + NODE_BYTES * count)
    }
    pub fn decode(bytes: &[u8], expected: AssetRef) -> Result<Self, Error> {
        let header = bytes.get(..HEADER_BYTES).ok_or(Error::Length)?;
        let size = Self::encoded_len(header)?;
        if bytes.len() != size {
            return Err(Error::Length);
        }
        if page_hash(bytes) != expected.hash() {
            return Err(Error::Checksum);
        }
        if header[8..24] != expected.id().bytes()
            || u64::from_le_bytes(header[24..32].try_into().unwrap()) != expected.revision()
        {
            return Err(Error::Format);
        }
        let root: [f32; 6] = std::array::from_fn(|i| {
            f32::from_le_bytes(header[32 + i * 4..36 + i * 4].try_into().unwrap())
        });
        let file_len = u64::from_le_bytes(header[56..64].try_into().unwrap());
        let mut nodes: Vec<LodNode> = Vec::new();
        let mut pages = Vec::new();
        let mut end = size as u64;
        for (i, b) in bytes[HEADER_BYTES..]
            .as_chunks::<NODE_BYTES>()
            .0
            .iter()
            .enumerate()
        {
            let p = PageDescriptor {
                offset: u64::from_le_bytes(b[..8].try_into().unwrap()),
                length: u64::from_le_bytes(b[8..16].try_into().unwrap()),
                vertices: u32::from_le_bytes(b[16..20].try_into().unwrap()),
                indices: u32::from_le_bytes(b[20..24].try_into().unwrap()),
                error: f32::from_le_bytes(b[24..28].try_into().unwrap()),
                hash: b[28..60].try_into().unwrap(),
            };
            p.size()?;
            if p.offset != end {
                return Err(Error::Length);
            }
            end = end
                .checked_add(p.length)
                .filter(|&n| n <= file_len)
                .ok_or(Error::Length)?;
            let bounds = std::array::from_fn(|a| {
                f32::from_le_bytes(b[60 + a * 4..64 + a * 4].try_into().unwrap())
            });
            let parent = u32::from_le_bytes(b[84..88].try_into().unwrap());
            if i == 0 {
                if parent != u32::MAX || bounds != root {
                    return Err(Error::Format);
                }
            } else {
                if parent as usize >= i {
                    return Err(Error::Format);
                }
                nodes[parent as usize].children.push(i);
            }
            nodes.push(LodNode {
                bounds,
                error: p.error,
                page: i as u32,
                children: vec![],
            });
            pages.push(p);
        }
        if end != file_len {
            return Err(Error::Length);
        }
        let hierarchy = LodHierarchy::new(nodes, MAX_NODES).map_err(|_| Error::Geometry)?;
        Ok(Self {
            reference: expected,
            hierarchy,
            pages,
            file_len,
        })
    }
    pub fn decode_page(&self, node: u32, bytes: Vec<u8>) -> Result<HierarchyPage, Error> {
        let descriptor = self.page(node).ok_or(Error::Format)?;
        validate_payload(
            self.hierarchy.nodes()[node as usize].bounds,
            descriptor,
            &bytes,
        )?;
        Ok(HierarchyPage {
            reference: self.reference,
            node,
            descriptor,
            bytes: bytes.into_boxed_slice(),
        })
    }
    /// Hashes one compact page at a time; no whole-asset payload allocation.
    pub fn prepare(
        id: AssetId,
        revision: u64,
        source: &SimplifiedClusters,
    ) -> Result<(Self, Vec<u8>), Error> {
        let nodes = source.hierarchy.nodes();
        if nodes.is_empty() || nodes.len() > MAX_NODES || source.meshes.len() != nodes.len() {
            return Err(Error::Limit);
        }
        let mut parents = vec![u32::MAX; nodes.len()];
        for (i, n) in nodes.iter().enumerate() {
            if n.page as usize != i || n.children.windows(2).any(|w| w[0] >= w[1]) {
                return Err(Error::Format);
            }
            for &child in &n.children {
                parents[child] = i as u32;
            }
        }
        let mut offset = (HEADER_BYTES + nodes.len() * NODE_BYTES) as u64;
        let mut records = Vec::with_capacity(nodes.len() * NODE_BYTES);
        for (i, n) in nodes.iter().enumerate() {
            if !source.meshes[i].error.is_finite()
                || source.meshes[i].error < 0.
                || source.meshes[i].error > n.error
            {
                return Err(Error::Geometry);
            }
            let (vertices, bytes) = encode_page(source, i as u32)?;
            records.extend(offset.to_le_bytes());
            records.extend((bytes.len() as u64).to_le_bytes());
            records.extend(vertices.to_le_bytes());
            records.extend((source.meshes[i].triangles.len() as u32 * 3).to_le_bytes());
            records.extend(n.error.to_le_bytes());
            records.extend(page_hash(&bytes));
            for x in n.bounds {
                records.extend(x.to_le_bytes());
            }
            records.extend(parents[i].to_le_bytes());
            offset += bytes.len() as u64;
        }
        let mut bytes = Vec::with_capacity(HEADER_BYTES + records.len());
        bytes.extend(MAGIC);
        bytes.extend(id.bytes());
        bytes.extend(revision.to_le_bytes());
        for x in nodes[0].bounds {
            bytes.extend(x.to_le_bytes());
        }
        bytes.extend(offset.to_le_bytes());
        bytes.extend((nodes.len() as u32).to_le_bytes());
        bytes.extend([0; 4]);
        bytes.extend(records);
        let reference = AssetRef::new(id, revision, page_hash(&bytes)).ok_or(Error::Format)?;
        Ok((Self::decode(&bytes, reference)?, bytes))
    }
}
/// Compact referenced source IDs without welding equal positions or changing winding.
pub fn encode_page(source: &SimplifiedClusters, node: u32) -> Result<(u32, Vec<u8>), Error> {
    let mesh = source.meshes.get(node as usize).ok_or(Error::Format)?;
    let bounds = source
        .hierarchy
        .nodes()
        .get(node as usize)
        .ok_or(Error::Format)?
        .bounds;
    let indices = mesh
        .triangles
        .len()
        .checked_mul(3)
        .filter(|&n| n >= 3 && n as u64 * 4 <= MAX_PAGE_BYTES)
        .ok_or(Error::Limit)?;
    let mut ids = BTreeSet::new();
    for &v in mesh.triangles.iter().flatten() {
        let p = source.vertices.get(v as usize).ok_or(Error::Geometry)?;
        if (0..3).any(|a| !p[a].is_finite() || p[a] < bounds[a] || p[a] > bounds[a + 3]) {
            return Err(Error::Bounds);
        }
        ids.insert(v);
        if (ids.len() * 12 + indices * 4) as u64 > MAX_PAGE_BYTES {
            return Err(Error::Limit);
        }
    }
    if ids.len() < 3 {
        return Err(Error::Geometry);
    }
    let mut bytes = Vec::with_capacity(ids.len() * 12 + indices * 4);
    let mut remap = BTreeMap::new();
    for (dense, id) in ids.into_iter().enumerate() {
        remap.insert(id, dense as u32);
        for x in source.vertices[id as usize] {
            bytes.extend(x.to_le_bytes());
        }
    }
    let count = remap.len() as u32;
    for v in mesh.triangles.iter().flatten() {
        bytes.extend(remap[v].to_le_bytes());
    }
    Ok((count, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cluster_builder::{ClusterBudget, ParentGeometryBudget},
        simplify::SimplifyBudget,
    };
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
    fn multilevel_roundtrip_compacts_and_preserves_cut_coverage() {
        let source = source();
        let id = AssetId::from_bytes([61; 16]).unwrap();
        let (m, bytes) = HierarchyManifest::prepare(id, 3, &source).unwrap();
        assert_eq!(
            HierarchyManifest::encoded_len(&bytes[..HEADER_BYTES]).unwrap(),
            bytes.len()
        );
        assert_eq!(m.hierarchy.nodes().len(), 7);
        let (other, again) = HierarchyManifest::prepare(id, 3, &source).unwrap();
        assert_eq!(bytes, again);
        assert_eq!(m.reference(), other.reference());
        for i in 0..7 {
            let (count, payload) = encode_page(&source, i).unwrap();
            let page = m.decode_page(i, payload.clone()).unwrap();
            assert_eq!(page.reference(), m.reference());
            assert_eq!(page.node(), i);
            assert_eq!(page.byte_len(), payload.len());
            assert_eq!(page.vertex_bytes().len(), count as usize * 12);
            assert_eq!(
                page.index_bytes().len(),
                source.meshes[i as usize].triangles.len() * 12
            );
            for (triangle, packed) in page.index_bytes().as_chunks::<12>().0.iter().enumerate() {
                for corner in 0..3 {
                    let dense =
                        u32::from_le_bytes(packed[corner * 4..corner * 4 + 4].try_into().unwrap())
                            as usize;
                    let original = source.meshes[i as usize].triangles[triangle][corner] as usize;
                    for axis in 0..3 {
                        let at = dense * 12 + axis * 4;
                        assert_eq!(
                            u32::from_le_bytes(page.vertex_bytes()[at..at + 4].try_into().unwrap()),
                            source.vertices[original][axis].to_bits()
                        );
                    }
                }
            }
        }
        assert_eq!(
            m.hierarchy.select(100, 100, |_, _| true, |p| p != 1).nodes,
            vec![0]
        );
        assert_eq!(
            m.hierarchy
                .select(100, 100, |_, _| true, |_| true)
                .nodes
                .len(),
            4
        );
        let unified = crate::asset_pages::AssetManifest::decode(&bytes, m.reference()).unwrap();
        assert!(unified.is_hierarchical());
        assert_eq!(unified.page_count(), 7);
    }
    #[test]
    fn metadata_and_payload_damage_are_rejected_before_publication() {
        let source = source();
        let (m, bytes) =
            HierarchyManifest::prepare(AssetId::from_bytes([62; 16]).unwrap(), 1, &source).unwrap();
        for at in [8, 24, 32, 56, 72, HEADER_BYTES + 84] {
            let mut corrupt = bytes.clone();
            corrupt[at] ^= 1;
            assert!(HierarchyManifest::decode(&corrupt, m.reference()).is_err());
        }
        // Rehash malformed metadata to exercise structural checks beyond SHA256.
        for (at, value) in [
            (64, (MAX_NODES as u32 + 1)),
            (HEADER_BYTES + NODE_BYTES + 84, 2),
            (HEADER_BYTES + 84, 0),
        ] {
            let mut corrupt = bytes.clone();
            corrupt[at..at + 4].copy_from_slice(&value.to_le_bytes());
            let r = AssetRef::new(m.reference().id(), 1, page_hash(&corrupt)).unwrap();
            assert!(HierarchyManifest::decode(&corrupt, r).is_err());
        }
        for (at, value) in [
            (HEADER_BYTES, 0),
            (HEADER_BYTES + 16, 2),
            (HEADER_BYTES + 24, f32::NAN.to_bits()),
            (HEADER_BYTES + NODE_BYTES + 60, f32::NEG_INFINITY.to_bits()),
        ] {
            let mut corrupt = bytes.clone();
            corrupt[at..at + 4].copy_from_slice(&value.to_le_bytes());
            let r = AssetRef::new(m.reference().id(), 1, page_hash(&corrupt)).unwrap();
            assert!(HierarchyManifest::decode(&corrupt, r).is_err());
        }
        let (_, mut payload) = encode_page(&source, 0).unwrap();
        payload[0] ^= 1;
        assert!(matches!(m.decode_page(0, payload), Err(Error::Checksum)));
        assert!(m.decode_page(99, vec![]).is_err());
        let mut invalid = source;
        invalid.meshes[0].triangles[0][0] = u32::MAX;
        assert!(HierarchyManifest::prepare(m.reference().id(), 1, &invalid).is_err());
    }
    #[test]
    fn unified_manifest_preserves_node_ids_above_255_and_shares_metadata() {
        use crate::{
            asset_pages::AssetManifest,
            lod_hierarchy::LodNode,
            simplify::{SimplifyBudget, simplify},
        };
        let vertices = vec![[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]];
        let mesh = simplify(
            &vertices,
            &[[0, 1, 2]],
            &[],
            SimplifyBudget {
                target_triangles: 1,
                max_error: 0.,
                max_attempts: 0,
                max_input_triangles: 1,
            },
        )
        .unwrap();
        let nodes = (0..300)
            .map(|i| LodNode {
                bounds: [0., 0., 0., 1., 1., 0.],
                error: 0.,
                page: i as u32,
                children: if i < 299 { vec![i + 1] } else { vec![] },
            })
            .collect();
        let source = SimplifiedClusters {
            vertices,
            hierarchy: LodHierarchy::new(nodes, 300).unwrap(),
            meshes: vec![mesh; 300],
        };
        let (m, bytes) =
            HierarchyManifest::prepare(AssetId::from_bytes([90; 16]).unwrap(), 1, &source).unwrap();
        let unified = AssetManifest::decode(&bytes, m.reference()).unwrap();
        let clone = unified.clone();
        assert!(std::ptr::eq(
            unified.hierarchy().unwrap(),
            clone.hierarchy().unwrap()
        ));
        let (_, payload) = encode_page(&source, 299).unwrap();
        assert_eq!(unified.decode_page(299, payload).unwrap().level(), 299);
        assert_eq!(
            crate::lod_hierarchy::LodHierarchy::from_manifest(&unified)
                .nodes()
                .len(),
            300
        );
    }
}
