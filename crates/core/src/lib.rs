//! Retained scene database. Commands are little-endian fixed 64-byte records.
mod geometry;
pub use geometry::{GeometryKind, GeometrySpec};
mod camera;
pub use camera::{CameraError, Perspective};
mod content_hash;
pub use content_hash::{CONTENT_HASH_VERSION, ContentHash};
mod checkpoint;
pub use checkpoint::CheckpointError;
mod document;
mod request;
pub use document::{
    CommitGroupBuilder, Document, DocumentError, DocumentSnapshot, PreparedGroup,
    PreparedTransaction,
};
pub use request::{
    DocumentId, MAX_SESSION_RECORDS, PreparedRequest, PreparedRequestGroup, RequestCommit,
    RequestError, RequestGroupBuilder, RequestPreparation, SessionId, SessionStatus,
};
pub use request::{
    PreparedRejection, RecordedRejection, RequestRejection, decode_request_error,
    encode_request_error,
};

mod instance_updates;
mod scene;
pub use instance_updates::{InstanceUpdates, SceneStamp};
mod transform_store;
pub use scene::{Scene, UpdateStats};
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Object {
    pub position: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: [f32; 3],
    pub color: [f32; 3],
    pub visible: bool,
    pub parent: u32,
    pub mesh: bool,
    pub geometry: GeometryKind,
    pub segments: [u16; 2],
}
impl Default for Object {
    fn default() -> Self {
        Self {
            position: [0.; 3],
            rotation: [0.; 3],
            scale: [1.; 3],
            color: [1.; 3],
            visible: true,
            parent: 0,
            mesh: false,
            geometry: GeometryKind::Box,
            segments: [0, 0],
        }
    }
}
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Instance {
    pub model: [[f32; 4]; 4],
    pub color: [f32; 4],
}
pub fn multiply(a: [[f32; 4]; 4], b: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut c = [[0.; 4]; 4];
    for j in 0..4 {
        for i in 0..4 {
            for k in 0..4 {
                c[j][i] += a[k][i] * b[j][k];
            }
        }
    }
    c
}
pub fn identity() -> [[f32; 4]; 4] {
    [
        [1., 0., 0., 0.],
        [0., 1., 0., 0.],
        [0., 0., 1., 0.],
        [0., 0., 0., 1.],
    ]
}
fn local(o: &Object) -> [[f32; 4]; 4] {
    let (sx, cx) = o.rotation[0].sin_cos();
    let (sy, cy) = o.rotation[1].sin_cos();
    let (sz, cz) = o.rotation[2].sin_cos();
    let x = [
        [1., 0., 0., 0.],
        [0., cx, sx, 0.],
        [0., -sx, cx, 0.],
        [0., 0., 0., 1.],
    ];
    let y = [
        [cy, 0., -sy, 0.],
        [0., 1., 0., 0.],
        [sy, 0., cy, 0.],
        [0., 0., 0., 1.],
    ];
    let z = [
        [cz, sz, 0., 0.],
        [-sz, cz, 0., 0.],
        [0., 0., 1., 0.],
        [0., 0., 0., 1.],
    ];
    let mut m = multiply(multiply(x, y), z);
    for (column, scale) in m.iter_mut().take(3).zip(o.scale) {
        for value in column {
            *value *= scale;
        }
    }
    m[3] = [o.position[0], o.position[1], o.position[2], 1.];
    m
}
/// Converts Universal-3D's degree-based sequential X/Y/Z rotations to
/// ThreeX's XYZ Euler radians. The composition differs (Rz*Ry*Rx vs Rx*Ry*Rz).
pub fn universal_rotation(degrees: [f32; 3]) -> Result<[f32; 3], String> {
    if degrees.iter().any(|v| !v.is_finite()) {
        return Err("Non-finite rotation".into());
    }
    let angles = degrees.map(f32::to_radians);
    let mut m = identity();
    for (axis, angle) in angles.into_iter().enumerate() {
        let mut o = Object::default();
        o.rotation[axis] = angle;
        m = multiply(local(&o), m);
    }
    let y = m[2][0].clamp(-1., 1.).asin();
    let (x, z) = if m[2][0].abs() < 0.9999999 {
        ((-m[2][1]).atan2(m[2][2]), (-m[1][0]).atan2(m[0][0]))
    } else {
        (m[1][2].atan2(m[1][1]), 0.)
    };
    Ok([x, y, z])
}
#[cfg(test)]
mod tests {
    use super::*;
    fn command(op: u32, id: u32) -> [u8; 64] {
        let mut b = [0; 64];
        b[..4].copy_from_slice(&op.to_le_bytes());
        b[4..8].copy_from_slice(&id.to_le_bytes());
        b
    }
    #[test]
    fn atomic_invalid_batch() {
        let mut s = Scene::default();
        let mut b = command(1, 1).to_vec();
        b.extend(command(7, 2));
        assert!(s.apply(&b).is_err());
        assert!(s.is_empty());
    }
    #[test]
    fn lifecycle() {
        let mut s = Scene::default();
        s.apply(&command(1, 1)).unwrap();
        assert_eq!(s.len(), 1);
        assert!(s.apply(&command(1, 1)).is_err());
        s.apply(&command(3, 1)).unwrap();
        assert!(s.is_empty());
    }
    #[test]
    fn idle_revision() {
        let mut s = Scene::default();
        s.apply(&[]).unwrap();
        assert_eq!(s.revision, 0);
    }
    fn update(id: u32, parent: u32) -> [u8; 64] {
        let mut b = command(2, id);
        for n in [32, 36, 40, 44, 48, 52] {
            b[n..n + 4].copy_from_slice(&1f32.to_le_bytes());
        }
        b[56..60].copy_from_slice(&3u32.to_le_bytes());
        b[60..64].copy_from_slice(&parent.to_le_bytes());
        b
    }
    #[test]
    fn atomic_cycle_orphan_and_delta() {
        let mut s = Scene::default();
        let batch = [command(1, 1), command(1, 2), update(1, 0), update(2, 1)].concat();
        s.apply(&batch).unwrap();
        let revision = s.revision;
        assert!(s.apply(&update(1, 2)).is_err());
        assert!(s.apply(&command(3, 1)).is_err());
        assert_eq!(s.revision, revision);
        let mut b = update(1, 0);
        b[8..12].copy_from_slice(&5f32.to_le_bytes());
        s.apply(&b).unwrap();
        assert_eq!(s.instances()[1].model[3][0], 5.);
        s.apply(&[command(3, 1), update(2, 0)].concat()).unwrap();
        assert_eq!(s.len(), 1);
    }
    #[test]
    fn universal_transform_contract() {
        for degrees in [
            [0., 0., 0.],
            [25., 40., -15.],
            [0., 90., 0.],
            [90., -90., 25.],
        ] {
            let position = [2., -3., 1.];
            let scale = [2., 0.5, -1.5];
            let point = [0.3, -0.4, 0.7];
            let o = Object {
                position,
                scale,
                rotation: universal_rotation(degrees).unwrap(),
                ..Default::default()
            };
            let m = local(&o);
            let mut expected = std::array::from_fn::<_, 3, _>(|i| point[i] * scale[i]);
            // Independent reference matching the inspected Universal-3D CPU transform.
            for (axis, degree) in degrees.into_iter().enumerate() {
                let a = (axis + 1) % 3;
                let b = (axis + 2) % 3;
                let (s, c) = degree.to_radians().sin_cos();
                let x = expected[a] * c - expected[b] * s;
                expected[b] = expected[a] * s + expected[b] * c;
                expected[a] = x;
            }
            for i in 0..3 {
                expected[i] += position[i];
                let actual = m[0][i] * point[0] + m[1][i] * point[1] + m[2][i] * point[2] + m[3][i];
                assert!(
                    (actual - expected[i]).abs() < 0.00001,
                    "axis {i}: {actual} != {}",
                    expected[i]
                );
            }
            let mut mirror = identity();
            mirror[0][0] = -1.;
            let mirrored = multiply(mirror, m);
            assert_eq!(mirrored[3][0], -position[0]); // Reflection includes translation.
        }
        assert!(universal_rotation([f32::NAN, 0., 0.]).is_err());
    }
}
