//! Right-handed, column-major camera matrices, WebGPU depth 0..1.
use crate::{Object, identity, local, multiply};
#[derive(Clone, Copy, Debug)]
pub struct Perspective {
    pub fov: f32,
    pub aspect: f32,
    pub near: f32,
    pub far: f32,
    pub zoom: f32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraError {
    InvalidProjection,
    InvalidTransform,
    SingularTransform,
}
impl Perspective {
    pub fn matrix(self) -> Result<[[f32; 4]; 4], CameraError> {
        let Self {
            fov,
            aspect,
            near,
            far,
            zoom,
        } = self;
        if ![fov, aspect, near, far, zoom].iter().all(|v| v.is_finite())
            || fov <= 0.
            || fov >= 180.
            || aspect <= 0.
            || near <= 0.
            || far <= near
            || zoom <= 0.
        {
            return Err(CameraError::InvalidProjection);
        }
        let f = zoom / (fov.to_radians() * 0.5).tan();
        let m = [
            [f / aspect, 0., 0., 0.],
            [0., f, 0., 0.],
            [0., 0., far / (near - far), -1.],
            [0., 0., far * near / (near - far), 0.],
        ];
        if m.iter().flatten().any(|v| !v.is_finite()) {
            return Err(CameraError::InvalidProjection);
        }
        Ok(m)
    }
    /// Nodes are root-to-camera TRS, nine floats each. No Document or Scene copies are needed.
    pub fn view_projection(self, nodes: &[f32]) -> Result<[[f32; 4]; 4], CameraError> {
        let projection = self.matrix()?;
        if nodes.is_empty()
            || !nodes.len().is_multiple_of(9)
            || nodes.iter().any(|v| !v.is_finite())
        {
            return Err(CameraError::InvalidTransform);
        }
        let mut world = identity();
        for n in nodes.as_chunks::<9>().0 {
            world = multiply(
                world,
                local(&Object {
                    position: n[0..3].try_into().unwrap(),
                    rotation: n[3..6].try_into().unwrap(),
                    scale: n[6..9].try_into().unwrap(),
                    ..Default::default()
                }),
            );
        }
        let view = inverse(world)?;
        let result = multiply(projection, view);
        if result.iter().flatten().any(|v| !v.is_finite()) {
            return Err(CameraError::InvalidTransform);
        }
        Ok(result)
    }
}
fn inverse(m: [[f32; 4]; 4]) -> Result<[[f32; 4]; 4], CameraError> {
    // f64 elimination avoids overflowing intermediate cofactors for otherwise valid f32 poses.
    let mut a = [[0f64; 8]; 4];
    for r in 0..4 {
        for c in 0..4 {
            a[r][c] = m[c][r] as f64;
        }
        a[r][r + 4] = 1.;
    }
    for col in 0..4 {
        let pivot = (col..4)
            .max_by(|&a_row, &b_row| a[a_row][col].abs().total_cmp(&a[b_row][col].abs()))
            .unwrap();
        if !a[pivot][col].is_finite() || a[pivot][col] == 0. {
            return Err(CameraError::SingularTransform);
        }
        a.swap(col, pivot);
        let divisor = a[col][col];
        for v in &mut a[col] {
            *v /= divisor;
        }
        let normalized = a[col];
        for (r, row) in a.iter_mut().enumerate() {
            if r != col {
                let factor = row[col];
                for (value, pivot) in row.iter_mut().zip(normalized) {
                    *value -= factor * pivot;
                }
            }
        }
    }
    let result = std::array::from_fn(|c| std::array::from_fn(|r| a[r][c + 4] as f32));
    if result.iter().flatten().any(|v| !v.is_finite()) {
        return Err(CameraError::InvalidTransform);
    }
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn camera() -> Perspective {
        Perspective {
            fov: 90.,
            aspect: 1.,
            near: 1.,
            far: 11.,
            zoom: 1.,
        }
    }
    fn project(m: [[f32; 4]; 4], p: [f32; 4]) -> [f32; 3] {
        let q: [f32; 4] = std::array::from_fn(|r| (0..4).map(|c| m[c][r] * p[c]).sum());
        [q[0] / q[3], q[1] / q[3], q[2] / q[3]]
    }
    fn close(a: [f32; 3], b: [f32; 3]) {
        for i in 0..3 {
            assert!((a[i] - b[i]).abs() < 1e-4, "{a:?} != {b:?}");
        }
    }
    #[test]
    fn clip_planes_aspect_zoom_and_translation() {
        let pose = [0., 0., 5., 0., 0., 0., 1., 1., 1.];
        let vp = camera().view_projection(&pose).unwrap();
        close(project(vp, [0., 0., 4., 1.]), [0., 0., 0.]);
        close(project(vp, [0., 0., -6., 1.]), [0., 0., 1.]);
        let vp = Perspective {
            aspect: 2.,
            zoom: 2.,
            ..camera()
        }
        .view_projection(&pose)
        .unwrap();
        close(project(vp, [1., 1., 4., 1.]), [1., 2., 0.]);
    }
    #[test]
    fn rotated_parent_and_nonuniform_scale_use_full_affine_inverse() {
        let parent = Object {
            position: [3., 2., 1.],
            rotation: [0.3, 0.8, -0.2],
            scale: [2., 3., 0.5],
            ..Default::default()
        };
        let child = Object {
            position: [1., 2., 4.],
            rotation: [-0.2, 0.4, 0.1],
            scale: [0.5, 2., 1.],
            ..Default::default()
        };
        let nodes: Vec<_> = [parent, child]
            .iter()
            .flat_map(|o| [o.position, o.rotation, o.scale].concat())
            .collect();
        let world = multiply(local(&parent), local(&child));
        let vp = camera().view_projection(&nodes).unwrap();
        let point = |z| std::array::from_fn(|r| world[2][r] * z + world[3][r]);
        close(project(vp, point(-1.)), [0., 0., 0.]);
        close(project(vp, point(-11.)), [0., 0., 1.]);
    }
    #[test]
    fn invalid_camera_inputs_and_singular_parents_are_rejected() {
        for p in [
            Perspective {
                near: 0.,
                ..camera()
            },
            Perspective {
                far: 1.,
                ..camera()
            },
            Perspective {
                zoom: 0.,
                ..camera()
            },
            Perspective {
                fov: f32::NAN,
                ..camera()
            },
            Perspective {
                aspect: f32::INFINITY,
                ..camera()
            },
        ] {
            assert_eq!(p.matrix(), Err(CameraError::InvalidProjection));
        }
        assert_eq!(
            camera().view_projection(&[]),
            Err(CameraError::InvalidTransform)
        );
        assert_eq!(
            camera().view_projection(&[0.; 8]),
            Err(CameraError::InvalidTransform)
        );
        assert_eq!(
            camera().view_projection(&[0.; 9]),
            Err(CameraError::SingularTransform)
        );
    }
}
