//! CPU reference for column-major WebGPU clip coordinates (-w..w XY, 0..w Z).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundsVisibility {
    Outside,
    Inside,
    Intersecting,
    Uncertain,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoundsAssessment {
    pub visibility: BoundsVisibility,
    pub error_pixels: f64,
}
impl BoundsAssessment {
    pub fn uncertain() -> Self {
        Self {
            visibility: BoundsVisibility::Uncertain,
            error_pixels: f64::INFINITY,
        }
    }
}
pub struct BoundsView {
    vp: [[f64; 4]; 4],
    width: f64,
    height: f64,
}
fn norm(v: [f64; 3]) -> f64 {
    v[0].hypot(v[1]).hypot(v[2])
}
fn interval(row: [f64; 4], bounds: [f64; 6]) -> (f64, f64, f64) {
    let center: f64 = (0..3)
        .map(|i| row[i] * (bounds[i] + bounds[i + 3]) * 0.5)
        .sum::<f64>()
        + row[3];
    let radius: f64 = (0..3)
        .map(|i| row[i].abs() * (bounds[i + 3] - bounds[i]) * 0.5)
        .sum();
    // Guard CPU/GPU f32 rounding at boundaries; planes need not be normalized.
    let magnitude = row[3].abs()
        + (0..3)
            .map(|i| row[i].abs() * bounds[i].abs().max(bounds[i + 3].abs()))
            .sum::<f64>();
    let tolerance = 64. * f64::from(f32::EPSILON) * magnitude.max(1.);
    (center - radius, center + radius, tolerance)
}
impl BoundsView {
    pub fn new(vp: &[f32; 16], width: u32, height: u32) -> Option<Self> {
        if width == 0 || height == 0 || vp.iter().any(|v| !v.is_finite()) {
            return None;
        }
        let vp: [[f64; 4]; 4] =
            std::array::from_fn(|c| std::array::from_fn(|r| f64::from(vp[c * 4 + r])));
        // Degenerate cameras cannot prove visibility; do not accidentally cull everything.
        let mut rows: [[f64; 4]; 4] = std::array::from_fn(|r| std::array::from_fn(|c| vp[c][r]));
        for c in 0..4 {
            let pivot = (c..4).max_by(|&a, &b| rows[a][c].abs().total_cmp(&rows[b][c].abs()))?;
            if rows[pivot][c] == 0. || !rows[pivot][c].is_finite() {
                return None;
            }
            rows.swap(c, pivot);
            let row = rows[c];
            for other in rows.iter_mut().skip(c + 1) {
                let scale = other[c] / row[c];
                for i in c..4 {
                    other[i] -= scale * row[i];
                }
            }
        }
        Some(Self {
            vp,
            width: f64::from(width),
            height: f64::from(height),
        })
    }
    pub fn assess(&self, bounds: [f32; 6], model: &[[f32; 4]; 4], error: f32) -> BoundsAssessment {
        if bounds.iter().any(|v| !v.is_finite())
            || (0..3).any(|i| bounds[i] > bounds[i + 3])
            || model.iter().flatten().any(|v| !v.is_finite())
            || !error.is_finite()
            || error < 0.
        {
            return BoundsAssessment::uncertain();
        }
        let bounds = bounds.map(f64::from);
        let c: [[f64; 4]; 4] = std::array::from_fn(|r| {
            std::array::from_fn(|col| {
                (0..4)
                    .map(|k| self.vp[k][r] * f64::from(model[col][k]))
                    .sum()
            })
        });
        let add = |a: usize, b: usize, sign: f64| std::array::from_fn(|i| c[a][i] + sign * c[b][i]);
        let planes = [
            add(3, 0, 1.),
            add(3, 0, -1.),
            add(3, 1, 1.),
            add(3, 1, -1.),
            c[2],
            add(3, 2, -1.),
        ];
        let mut visibility = BoundsVisibility::Inside;
        let mut depth_crossing = false;
        for (i, plane) in planes.into_iter().enumerate() {
            let (min, max, tolerance) = interval(plane, bounds);
            if max < -tolerance {
                visibility = BoundsVisibility::Outside;
            } else if min <= tolerance && visibility != BoundsVisibility::Outside {
                visibility = BoundsVisibility::Intersecting;
            }
            if i >= 4 && min <= tolerance {
                depth_crossing = true;
            }
        }
        let (w_min, _, w_tolerance) = interval(c[3], bounds);
        let w_norm = norm([c[3][0], c[3][1], c[3][2]]);
        let error = f64::from(error);
        let lower = w_min - error * w_norm;
        let pixels = if depth_crossing || lower <= w_tolerance {
            f64::INFINITY
        } else {
            let axis = |row: usize, extent: f64| {
                let (min, max, _) = interval(c[row], bounds);
                let n = norm([c[row][0], c[row][1], c[row][2]]);
                extent
                    * 0.5
                    * error
                    * (n / lower + min.abs().max(max.abs()) * w_norm / (w_min * lower))
            };
            axis(0, self.width).hypot(axis(1, self.height))
        };
        BoundsAssessment {
            visibility,
            error_pixels: if pixels.is_finite() {
                pixels
            } else {
                f64::INFINITY
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: [[f32; 4]; 4] = [
        [1., 0., 0., 0.],
        [0., 1., 0., 0.],
        [0., 0., 1., 0.],
        [0., 0., 0., 1.],
    ];
    fn view() -> BoundsView {
        BoundsView::new(&ID.concat().try_into().unwrap(), 1000, 1000).unwrap()
    }
    #[test]
    fn six_planes_boundary_and_invalid_inputs() {
        let v = view();
        for (axis, value) in [(0, -2.), (0, 2.), (1, -2.), (1, 2.), (2, -1.), (2, 2.)] {
            let mut m = ID;
            m[3][axis] = value;
            assert_eq!(
                v.assess([0.; 6], &m, 0.).visibility,
                BoundsVisibility::Outside
            );
        }
        assert_eq!(
            v.assess([-1., -1., 0., 1., 1., 1.], &ID, 0.).visibility,
            BoundsVisibility::Intersecting
        );
        assert!(
            v.assess([-1., -1., 0., 1., 1., 1.], &ID, 1.)
                .error_pixels
                .is_infinite()
        );
        assert_eq!(
            v.assess([f32::NAN; 6], &ID, 0.).visibility,
            BoundsVisibility::Uncertain
        );
        assert!(BoundsView::new(&[0.; 16], 100, 100).is_none());
        assert!(BoundsView::new(&ID.concat().try_into().unwrap(), 0, 100).is_none());
    }
    #[test]
    fn orthographic_error_scales_with_viewport_and_reflected_shear() {
        let b = [-0.1, -0.1, 0.2, 0.1, 0.1, 0.8];
        let e = view().assess(b, &ID, 0.01).error_pixels;
        assert!((e - 50f64.sqrt()).abs() < 1e-5);
        let mut m = ID;
        m[0][0] = -2.;
        m[1][0] = 1.;
        m[1][1] = 3.;
        let actual = view().assess(b, &m, 0.01).error_pixels;
        assert!(actual > e * 2.);
        let small = BoundsView::new(&ID.concat().try_into().unwrap(), 500, 500).unwrap();
        assert!((small.assess(b, &m, 0.01).error_pixels * 2. - actual).abs() < 1e-8);
    }
    #[test]
    fn perspective_error_bounds_independent_projected_samples() {
        let vp = [
            1., 0., 0., 0., 0., 1., 0., 0., 0., 0., -1.001, -1., 0., 0., -0.1001, 0.,
        ];
        let v = BoundsView::new(&vp, 1200, 800).unwrap();
        let mut m = ID;
        m[0][0] = -2.;
        m[1][0] = 0.3;
        m[1][1] = 0.7;
        m[3][2] = -10.;
        let b = [-0.5, -0.5, -0.5, 0.5, 0.5, 0.5];
        let near = v.assess(b, &m, 0.1);
        assert_eq!(near.visibility, BoundsVisibility::Inside);
        let project = |p: [f64; 4]| {
            let world: [f64; 4] =
                std::array::from_fn(|r| (0..4).map(|c| f64::from(m[c][r]) * p[c]).sum());
            let clip: [f64; 4] =
                std::array::from_fn(|r| (0..4).map(|c| f64::from(vp[c * 4 + r]) * world[c]).sum());
            [clip[0] / clip[3] * 600., clip[1] / clip[3] * 400.]
        };
        for i in 0..1000 {
            let t = i as f64 * 0.731;
            let p = [
                t.sin() * 0.5,
                (t * 1.7).cos() * 0.5,
                (t * 2.3).sin() * 0.5,
                1.,
            ];
            let delta = [t.cos() * 0.05, t.sin() * 0.05, (t * 3.).cos() * 0.05];
            let q = [p[0] + delta[0], p[1] + delta[1], p[2] + delta[2], 1.];
            let a = project(p);
            let z = project(q);
            assert!((a[0] - z[0]).hypot(a[1] - z[1]) <= near.error_pixels);
        }
        m[3][2] = -50.;
        assert!(v.assess(b, &m, 0.1).error_pixels < near.error_pixels);
        m[3][2] = 10.;
        assert_eq!(v.assess(b, &m, 0.1).visibility, BoundsVisibility::Outside);
    }
    #[test]
    fn culling_matches_corner_oracle_for_parent_rotation_shear_and_large_bounds() {
        let v = view();
        let bounds = [-0.3, -0.2, -0.1, 0.3, 0.2, 0.1];
        let mut rejected = 0;
        for i in 0..1200 {
            let t = i as f32 * 0.137;
            let (sn, cs) = t.sin_cos();
            let parent = [
                [cs, sn, 0., 0.],
                [-sn, cs, 0., 0.],
                [0., 0., 1., 0.],
                [t.sin() * 3., (t * 1.7).cos() * 3., 0.5, 1.],
            ];
            let child = [
                [-2., 0., 0., 0.],
                [0.7, 0.3, 0., 0.],
                [0., 0., 1., 0.],
                [0.; 4],
            ];
            let mut child = child;
            child[3][3] = 1.;
            let model = crate::multiply(parent, child);
            let a = v.assess(bounds, &model, 0.01);
            if a.visibility != BoundsVisibility::Outside {
                continue;
            }
            rejected += 1;
            let corners: Vec<[f64; 4]> = (0..8)
                .map(|bits| {
                    let p: [f64; 4] = std::array::from_fn(|j| {
                        if j == 3 {
                            1.
                        } else {
                            f64::from(bounds[j + if bits & (1 << j) == 0 { 0 } else { 3 }])
                        }
                    });
                    std::array::from_fn(|r| (0..4).map(|c| f64::from(model[c][r]) * p[c]).sum())
                })
                .collect();
            assert!((0..6).any(|plane| corners.iter().all(|p| match plane {
                0 => p[3] + p[0] < 0.,
                1 => p[3] - p[0] < 0.,
                2 => p[3] + p[1] < 0.,
                3 => p[3] - p[1] < 0.,
                4 => p[2] < 0.,
                _ => p[3] - p[2] < 0.,
            })));
        }
        assert!(rejected > 100);
        assert_eq!(
            v.assess([0.; 6], &ID, 0.).visibility,
            BoundsVisibility::Intersecting
        );
        assert_eq!(
            v.assess(
                [
                    -f32::MAX,
                    -f32::MAX,
                    -f32::MAX,
                    f32::MAX,
                    f32::MAX,
                    f32::MAX
                ],
                &ID,
                1.
            )
            .visibility,
            BoundsVisibility::Intersecting
        );
    }
}
