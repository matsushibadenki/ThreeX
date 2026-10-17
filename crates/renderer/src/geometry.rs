use std::{collections::BTreeMap, ops::Range};
use threex_core::{GeometryKind, GeometrySpec, Instance};
pub struct MeshData {
    pub vertices: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
}
pub fn unit_geometry(kind: GeometryKind) -> MeshData {
    geometry(GeometrySpec::new(kind, [0, 0]).unwrap())
}
pub fn geometry(spec: GeometrySpec) -> MeshData {
    match spec.kind {
        GeometryKind::Box => MeshData {
            vertices: vec![
                [-0.5, -0.5, -0.5],
                [0.5, -0.5, -0.5],
                [0.5, 0.5, -0.5],
                [-0.5, 0.5, -0.5],
                [-0.5, -0.5, 0.5],
                [0.5, -0.5, 0.5],
                [0.5, 0.5, 0.5],
                [-0.5, 0.5, 0.5],
            ],
            indices: vec![
                0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 3, 7, 6, 3, 6, 2, 1, 2, 6, 1,
                6, 5, 0, 4, 7, 0, 7, 3,
            ],
        },
        GeometryKind::Plane if spec.width == 1 && spec.height == 1 => MeshData {
            vertices: vec![
                [-0.5, -0.5, 0.],
                [0.5, -0.5, 0.],
                [0.5, 0.5, 0.],
                [-0.5, 0.5, 0.],
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
        },
        GeometryKind::Plane => {
            let (w, h) = (u32::from(spec.width), u32::from(spec.height));
            let mut vertices = Vec::new();
            let mut indices = Vec::new();
            for y in 0..=h {
                for x in 0..=w {
                    vertices.push([x as f32 / w as f32 - 0.5, y as f32 / h as f32 - 0.5, 0.]);
                }
            }
            for y in 0..h {
                for x in 0..w {
                    let a = y * (w + 1) + x;
                    let b = a + 1;
                    let c = a + w + 1;
                    let d = c + 1;
                    indices.extend([a, b, d, a, d, c]);
                }
            }
            MeshData { vertices, indices }
        }
        GeometryKind::Sphere => {
            let (w, h) = (u32::from(spec.width), u32::from(spec.height));
            let mut vertices = Vec::new();
            let mut indices = Vec::new();
            for y in 0..=h {
                let theta = y as f32 * std::f32::consts::PI / h as f32;
                for x in 0..=w {
                    let phi = if x == w {
                        0.
                    } else {
                        x as f32 * std::f32::consts::TAU / w as f32
                    };
                    vertices.push(if y == 0 {
                        [0., 1., 0.]
                    } else if y == h {
                        [0., -1., 0.]
                    } else {
                        [
                            -phi.cos() * theta.sin(),
                            theta.cos(),
                            phi.sin() * theta.sin(),
                        ]
                    });
                }
            }
            for y in 0..h {
                for x in 0..w {
                    let a = y * (w + 1) + x;
                    let b = a + w + 1;
                    let c = b + 1;
                    let d = a + 1;
                    if y > 0 {
                        indices.extend([a, b, d]);
                    }
                    if y < h - 1 {
                        indices.extend([d, b, c]);
                    }
                }
            }
            MeshData { vertices, indices }
        }
    }
}
/// Only geometry tags are retained; no full Instance snapshot or per-frame scan.
#[derive(Default)]
pub struct GeometryRuns {
    kinds: Vec<GeometryKind>,
    starts: BTreeMap<usize, GeometryKind>,
}
impl GeometryRuns {
    pub fn sync(&mut self, data: &[Instance], ranges: impl IntoIterator<Item = Range<usize>>) {
        let old = self.kinds.len();
        let len = data.len();
        self.kinds.resize(len, GeometryKind::Box);
        if len < old {
            self.starts.split_off(&len);
        }
        if len > old && (old == 0 || self.kinds[old - 1] != GeometryKind::Box) {
            self.starts.insert(old, GeometryKind::Box);
        }
        for range in ranges {
            for i in range {
                let kind = data[i].geometry_kind();
                if self.kinds[i] == kind {
                    continue;
                }
                self.kinds[i] = kind;
                if i == 0 || self.kinds[i - 1] != kind {
                    self.starts.insert(i, kind);
                } else {
                    self.starts.remove(&i);
                }
                if i + 1 < len {
                    if self.kinds[i + 1] != kind {
                        self.starts.insert(i + 1, self.kinds[i + 1]);
                    } else {
                        self.starts.remove(&(i + 1));
                    }
                }
            }
        }
    }
    pub fn draws(&self) -> impl Iterator<Item = (GeometryKind, Range<u32>)> + '_ {
        let mut starts = self.starts.iter().peekable();
        std::iter::from_fn(move || {
            let (&start, &kind) = starts.next()?;
            let end = starts.peek().map_or(self.kinds.len(), |entry| *entry.0);
            Some((kind, start as u32..end as u32))
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn primitive_topology_bounds_and_outward_winding() {
        for kind in [GeometryKind::Box, GeometryKind::Plane, GeometryKind::Sphere] {
            let m = unit_geometry(kind);
            assert_eq!(m.indices.len() % 3, 0);
            for tri in m.indices.as_chunks::<3>().0 {
                let [a, b, c] = tri.map(|i| m.vertices[i as usize]);
                let u = std::array::from_fn::<_, 3, _>(|i| b[i] - a[i]);
                let v = std::array::from_fn::<_, 3, _>(|i| c[i] - a[i]);
                let cross = [
                    u[1] * v[2] - u[2] * v[1],
                    u[2] * v[0] - u[0] * v[2],
                    u[0] * v[1] - u[1] * v[0],
                ];
                assert!(cross.iter().map(|v| v * v).sum::<f32>() > 1e-12);
                if kind == GeometryKind::Plane {
                    assert!(cross[2] > 0.);
                } else {
                    assert!((0..3).map(|i| cross[i] * (a[i] + b[i] + c[i])).sum::<f32>() > 0.);
                }
            }
            if kind == GeometryKind::Sphere {
                assert_eq!(m.indices.len(), 2880);
                for v in m.vertices {
                    assert!((v.iter().map(|n| n * n).sum::<f32>() - 1.).abs() < 1e-5);
                }
            }
        }
    }
    fn instances(kinds: &[GeometryKind]) -> Vec<Instance> {
        kinds
            .iter()
            .map(|&k| Instance {
                model: threex_core::identity(),
                color: [1., 1., 1., 1. + k as u8 as f32],
            })
            .collect()
    }
    #[test]
    fn dirty_geometry_runs_merge_split_shrink_and_keep_box_single_draw() {
        use GeometryKind::*;
        let mut runs = GeometryRuns::default();
        let mut data = instances(&[Box; 100]);
        runs.sync(&data, std::iter::once(0..100));
        assert_eq!(runs.draws().collect::<Vec<_>>(), vec![(Box, 0..100)]);
        data[50].color[3] = 2.;
        runs.sync(&data, std::iter::once(50..51));
        assert_eq!(
            runs.draws().collect::<Vec<_>>(),
            vec![(Box, 0..50), (Plane, 50..51), (Box, 51..100)]
        );
        data[51].color[3] = 2.;
        runs.sync(&data, std::iter::once(51..52));
        assert_eq!(
            runs.draws().collect::<Vec<_>>(),
            vec![(Box, 0..50), (Plane, 50..52), (Box, 52..100)]
        );
        data[50].color[3] = 1.;
        data[51].color[3] = 1.;
        runs.sync(&data, std::iter::once(50..52));
        assert_eq!(runs.draws().collect::<Vec<_>>(), vec![(Box, 0..100)]);
        data.truncate(10);
        runs.sync(&data, []);
        assert_eq!(runs.draws().collect::<Vec<_>>(), vec![(Box, 0..10)]);
        data.clear();
        runs.sync(&data, []);
        assert_eq!(runs.draws().count(), 0);
        data = instances(&[Sphere, Plane, Box]);
        runs.sync(&data, std::iter::once(0..3));
        assert_eq!(
            runs.draws().collect::<Vec<_>>(),
            vec![(Sphere, 0..1), (Plane, 1..2), (Box, 2..3)]
        );
    }
    #[test]
    fn mixed_scene_incremental_draws_match_full_reference_after_membership_edits() {
        fn wire(op: u32, id: u32, o: threex_core::Object) -> Vec<u8> {
            let mut b = vec![0; 64];
            b[..4].copy_from_slice(&op.to_le_bytes());
            b[4..8].copy_from_slice(&id.to_le_bytes());
            for (i, v) in [o.position, o.rotation, o.scale, o.color]
                .concat()
                .iter()
                .enumerate()
            {
                b[8 + i * 4..12 + i * 4].copy_from_slice(&v.to_le_bytes());
            }
            b[56..60].copy_from_slice(&o.flags().to_le_bytes());
            b[60..64].copy_from_slice(&o.parent.to_le_bytes());
            b
        }
        let mut scene = threex_core::Scene::default();
        let mut runs = GeometryRuns::default();
        let mut stamp = None;
        let mut batches = crate::batching::GeometryBatches::default();
        for n in 0..240 {
            let id = 1 + n % 30;
            let mut o = scene.object(id).unwrap_or(threex_core::Object {
                mesh: true,
                ..Default::default()
            });
            let mut commands = Vec::new();
            if scene.object(id).is_none() {
                commands.extend(wire(1, id, o));
            }
            match n % 8 {
                0 => o.geometry = GeometryKind::Plane,
                1 => o.geometry = GeometryKind::Sphere,
                2 => o.visible = false,
                3 => o.visible = true,
                4 => o.mesh = !o.mesh,
                5 => o.position[0] += 1.,
                6 => o.color[0] = 0.5,
                _ => {
                    commands.extend(wire(3, id, o));
                    scene.apply(&commands).unwrap();
                    let updates = scene.instance_updates_since(stamp.as_ref(), false);
                    runs.sync(scene.instances(), updates.ranges.clone());
                    batches.sync(scene.instances(), updates.ranges);
                    for kind in 0..3 {
                        for &index in batches.indices(kind) {
                            assert_eq!(
                                scene.instances()[index as usize].geometry_kind() as usize,
                                kind
                            );
                        }
                    }
                    assert_eq!(
                        (0..3).map(|k| batches.indices(k).len()).sum::<usize>(),
                        scene.instances().len()
                    );
                    stamp = Some(updates.stamp);
                    continue;
                }
            }
            commands.extend(wire(2, id, o));
            scene.apply(&commands).unwrap();
            let updates = scene.instance_updates_since(stamp.as_ref(), false);
            runs.sync(scene.instances(), updates.ranges.clone());
            batches.sync(scene.instances(), updates.ranges);
            for kind in 0..3 {
                for &index in batches.indices(kind) {
                    assert_eq!(
                        scene.instances()[index as usize].geometry_kind() as usize,
                        kind
                    );
                }
            }
            assert_eq!(
                (0..3).map(|k| batches.indices(k).len()).sum::<usize>(),
                scene.instances().len()
            );
            stamp = Some(updates.stamp);
            let planned: Vec<_> = runs
                .draws()
                .flat_map(|(kind, range)| range.map(move |_| kind))
                .collect();
            let reference: Vec<_> = scene
                .instances()
                .iter()
                .map(Instance::geometry_kind)
                .collect();
            assert_eq!(planned, reference);
        }
    }
    #[test]
    fn arbitrary_subdivision_topology_and_u32_indices_are_valid() {
        for kind in [GeometryKind::Plane, GeometryKind::Sphere] {
            for [w, h] in [[3, 2], [7, 9], [32, 16], [256, 256]] {
                let spec = GeometrySpec::new(kind, [w, h]).unwrap();
                let m = geometry(spec);
                assert_eq!(m.vertices.len(), (w as usize + 1) * (h as usize + 1));
                let triangles = if kind == GeometryKind::Plane {
                    2 * w as usize * h as usize
                } else {
                    2 * w as usize * (h as usize - 1)
                };
                assert_eq!(m.indices.len(), triangles * 3);
                for tri in m.indices.as_chunks::<3>().0 {
                    let [a, b, c] = tri.map(|i| m.vertices[i as usize]);
                    let u = std::array::from_fn::<_, 3, _>(|i| b[i] - a[i]);
                    let v = std::array::from_fn::<_, 3, _>(|i| c[i] - a[i]);
                    let cross = [
                        u[1] * v[2] - u[2] * v[1],
                        u[2] * v[0] - u[0] * v[2],
                        u[0] * v[1] - u[1] * v[0],
                    ];
                    if kind == GeometryKind::Plane {
                        assert!(cross[2] > 0.);
                    } else {
                        assert!((0..3).map(|i| cross[i] * (a[i] + b[i] + c[i])).sum::<f32>() > 0.);
                    }
                }
                if w == 256 {
                    assert!(m.indices.iter().any(|&i| i > u16::MAX as u32));
                }
                if kind == GeometryKind::Sphere {
                    for p in m.vertices {
                        assert!((p.iter().map(|v| v * v).sum::<f32>() - 1.).abs() < 1e-5);
                    }
                }
            }
        }
    }
}
