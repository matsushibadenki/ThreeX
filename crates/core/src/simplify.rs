//! Independent, endpoint-only edge contraction with locked patch boundaries.
//! Error bounds a simplicial correspondence to the original surface, not samples.
use std::collections::{BTreeMap, BTreeSet};
type Edge = (u32, u32);
fn edge(a: u32, b: u32) -> Edge {
    if a < b { (a, b) } else { (b, a) }
}
#[derive(Clone, Copy, Debug)]
pub struct SimplifyBudget {
    pub target_triangles: usize,
    pub max_error: f32,
    pub max_attempts: usize,
    pub max_input_triangles: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimplifyError {
    Input,
    Limit,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimplifyStop {
    Target,
    Constrained,
    AttemptLimit,
    UnsupportedTopology,
}
#[derive(Clone, Debug)]
pub struct SimplifiedMesh {
    /// Indices into the unchanged source vertex buffer.
    pub triangles: Vec<[u32; 3]>,
    /// Conservative bidirectional surface-distance bound in asset units.
    pub error: f32,
    /// Original referenced vertex -> final endpoint. Useful for verifying provenance.
    pub vertex_remap: BTreeMap<u32, u32>,
    pub attempts: usize,
    pub stop: SimplifyStop,
}
struct Topology {
    edges: BTreeMap<Edge, Vec<u32>>,
    boundary: BTreeSet<Edge>,
    adjacent: BTreeMap<u32, BTreeSet<u32>>,
}
fn normal(vertices: &[[f32; 3]], t: [u32; 3]) -> [f64; 3] {
    let p = t.map(|i| vertices[i as usize].map(f64::from));
    let a: [f64; 3] = std::array::from_fn(|i| p[1][i] - p[0][i]);
    let b: [f64; 3] = std::array::from_fn(|i| p[2][i] - p[0][i]);
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm(v: [f64; 3]) -> f64 {
    v[0].hypot(v[1]).hypot(v[2])
}
fn topology(vertices: &[[f32; 3]], triangles: &[[u32; 3]]) -> Option<Topology> {
    let mut edges: BTreeMap<Edge, Vec<u32>> = BTreeMap::new();
    let mut directions = BTreeMap::<Edge, i32>::new();
    let mut faces = BTreeSet::new();
    let mut adjacent = BTreeMap::<u32, BTreeSet<u32>>::new();
    let mut links = BTreeMap::<u32, BTreeMap<u32, BTreeSet<u32>>>::new();
    for &t in triangles {
        let mut sorted = t;
        sorted.sort_unstable();
        if sorted[0] == sorted[1]
            || sorted[1] == sorted[2]
            || !faces.insert(sorted)
            || norm(normal(vertices, t)) == 0.
        {
            return None;
        }
        for (a, b, c) in [(t[0], t[1], t[2]), (t[1], t[2], t[0]), (t[2], t[0], t[1])] {
            let faces = edges.entry(edge(a, b)).or_default();
            if faces.len() == 2 {
                return None;
            }
            faces.push(c);
            let link = links.entry(a).or_default();
            link.entry(b).or_default().insert(c);
            link.entry(c).or_default().insert(b);
            *directions.entry(edge(a, b)).or_default() += if a < b { 1 } else { -1 };
            adjacent.entry(a).or_default().insert(b);
            adjacent.entry(b).or_default().insert(a);
        }
    }
    if edges
        .iter()
        .any(|(e, faces)| faces.len() > 2 || (faces.len() == 2 && directions[e] != 0))
    {
        return None;
    }
    // A vertex link must be one cycle (interior) or one path (boundary).
    // Reject pinched/bow-tie vertices rather than changing their components.
    for link in links.values() {
        let ends = link.values().filter(|n| n.len() == 1).count();
        if ![0, 2].contains(&ends) || link.values().any(|n| n.is_empty() || n.len() > 2) {
            return None;
        }
        let mut visited = BTreeSet::new();
        let mut stack = vec![*link.keys().next()?];
        while let Some(v) = stack.pop() {
            if visited.insert(v) {
                stack.extend(link[&v].iter().copied());
            }
        }
        if visited.len() != link.len() {
            return None;
        }
    }
    let boundary = edges
        .iter()
        .filter(|(_, f)| f.len() == 1)
        .map(|(&e, _)| e)
        .collect();
    Some(Topology {
        edges,
        boundary,
        adjacent,
    })
}
fn distance_bound(a: [f32; 3], b: [f32; 3]) -> f64 {
    if a == b {
        return 0.;
    }
    let d = norm(std::array::from_fn(|i| f64::from(a[i]) - f64::from(b[i])));
    // Cover subtraction/hypot rounding even at highly unequal coordinate scales.
    let magnitude = (0..3)
        .map(|i| f64::from(a[i]).abs() + f64::from(b[i]).abs())
        .sum::<f64>();
    (d + 64. * f64::EPSILON * (d + magnitude)).next_up()
}
fn error_bound(vertices: &[[f32; 3]], remap: &BTreeMap<u32, u32>) -> f32 {
    let e = remap
        .iter()
        .map(|(&a, &b)| distance_bound(vertices[a as usize], vertices[b as usize]))
        .fold(0., f64::max);
    if e == 0. { 0. } else { (e as f32).next_up() }
}
/// Every boundary edge of the input patch is preserved, including cluster seams.
/// Extra locks can mark creases/attribute seams; this position-only API never welds IDs.
/// Unsupported topology is returned unchanged with zero error. Invalid data/budgets fail.
pub fn simplify(
    vertices: &[[f32; 3]],
    triangles: &[[u32; 3]],
    locks: &[u32],
    budget: SimplifyBudget,
) -> Result<SimplifiedMesh, SimplifyError> {
    if triangles.len() > budget.max_input_triangles
        || locks.len() > budget.max_input_triangles.saturating_mul(3)
    {
        return Err(SimplifyError::Limit);
    }
    if triangles.is_empty()
        || budget.target_triangles == 0
        || !budget.max_error.is_finite()
        || budget.max_error < 0.
        || locks.iter().any(|&v| v as usize >= vertices.len())
        || triangles.iter().flatten().any(|&v| {
            vertices
                .get(v as usize)
                .is_none_or(|p| p.iter().any(|x| !x.is_finite()))
        })
    {
        return Err(SimplifyError::Input);
    }
    let mut remap: BTreeMap<u32, u32> = triangles.iter().flatten().map(|&v| (v, v)).collect();
    let mut current = triangles.to_vec();
    let mut attempts = 0;
    let mut error = 0.;
    let Some(initial) = topology(vertices, &current) else {
        return Ok(SimplifiedMesh {
            triangles: current,
            error,
            vertex_remap: remap,
            attempts,
            stop: SimplifyStop::UnsupportedTopology,
        });
    };
    let locked: BTreeSet<_> = initial
        .boundary
        .iter()
        .flat_map(|&(a, b)| [a, b])
        .chain(locks.iter().copied())
        .collect();
    while current.len() > budget.target_triangles && attempts < budget.max_attempts {
        let topo = topology(vertices, &current).unwrap();
        let mut candidates = Vec::new();
        for (&(a, b), opposite) in &topo.edges {
            if opposite.len() != 2 {
                continue;
            }
            for (from, to) in [(a, b), (b, a)] {
                if !locked.contains(&from) {
                    candidates.push((
                        distance_bound(vertices[from as usize], vertices[to as usize]),
                        from,
                        to,
                    ));
                }
            }
        }
        candidates
            .sort_unstable_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        let mut accepted = false;
        for (_, from, to) in candidates {
            if attempts == budget.max_attempts {
                break;
            }
            attempts += 1;
            let link: BTreeSet<_> = topo.adjacent[&from]
                .intersection(&topo.adjacent[&to])
                .copied()
                .collect();
            if link != topo.edges[&edge(from, to)].iter().copied().collect() {
                continue;
            }
            let mut next = Vec::with_capacity(current.len());
            let mut collapsed = Vec::new();
            let mut valid = true;
            for &t in &current {
                let mapped = t.map(|v| if v == from { to } else { v });
                if mapped[0] == mapped[1] || mapped[1] == mapped[2] || mapped[2] == mapped[0] {
                    collapsed.push(mapped);
                    continue;
                }
                let a = normal(vertices, t);
                let b = normal(vertices, mapped);
                if (0..3).map(|i| a[i] * b[i]).sum::<f64>() <= 1e-12 * norm(a) * norm(b) {
                    valid = false;
                    break;
                }
                next.push(mapped);
            }
            if !valid || next.is_empty() {
                continue;
            }
            let Some(next_topo) = topology(vertices, &next) else {
                continue;
            };
            if next_topo.boundary != initial.boundary {
                continue;
            }
            // Removed faces map to edges/points of the retained complex. This is
            // what makes vertex correspondence bound *all* surface points.
            if collapsed.iter().any(|t| {
                let ids: BTreeSet<_> = t.iter().copied().collect();
                if ids.len() == 1 {
                    !next_topo.adjacent.contains_key(ids.first().unwrap())
                } else {
                    let mut ids = ids.into_iter();
                    !next_topo
                        .edges
                        .contains_key(&edge(ids.next().unwrap(), ids.next().unwrap()))
                }
            }) {
                continue;
            }
            let candidate: BTreeMap<_, _> = remap
                .iter()
                .map(|(&a, &b)| (a, if b == from { to } else { b }))
                .collect();
            if triangles.iter().any(|&original| {
                let mapped = original.map(|v| candidate[&v]);
                if mapped[0] == mapped[1] || mapped[1] == mapped[2] || mapped[2] == mapped[0] {
                    return false;
                }
                let a = normal(vertices, original);
                let b = normal(vertices, mapped);
                (0..3).map(|i| a[i] * b[i]).sum::<f64>() <= 1e-12 * norm(a) * norm(b)
            }) {
                continue;
            }
            let bound = error_bound(vertices, &candidate);
            if !bound.is_finite() || bound > budget.max_error {
                continue;
            }
            current = next;
            remap = candidate;
            error = bound;
            accepted = true;
            break;
        }
        if !accepted {
            break;
        }
    }
    let stop = if current.len() <= budget.target_triangles {
        SimplifyStop::Target
    } else if attempts >= budget.max_attempts {
        SimplifyStop::AttemptLimit
    } else {
        SimplifyStop::Constrained
    };
    Ok(SimplifiedMesh {
        triangles: current,
        error,
        vertex_remap: remap,
        attempts,
        stop,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) fn grid(size: usize, curved: bool) -> (Vec<[f32; 3]>, Vec<[u32; 3]>) {
        let vertices = (0..size * size)
            .map(|i| {
                let x = (i % size) as f32 / (size - 1) as f32;
                let y = (i / size) as f32 / (size - 1) as f32;
                [
                    x,
                    y,
                    if curved {
                        (x * std::f32::consts::PI).sin() * (y * std::f32::consts::PI).sin() * 0.1
                    } else {
                        0.
                    },
                ]
            })
            .collect();
        let mut triangles = Vec::new();
        for y in 0..size - 1 {
            for x in 0..size - 1 {
                let a = (y * size + x) as u32;
                let b = a + 1;
                let c = a + size as u32;
                let d = c + 1;
                triangles.extend([[a, b, d], [a, d, c]]);
            }
        }
        (vertices, triangles)
    }
    fn budget() -> SimplifyBudget {
        SimplifyBudget {
            target_triangles: 12,
            max_error: 2.,
            max_attempts: 2000,
            max_input_triangles: 128,
        }
    }
    #[test]
    fn planar_and_curved_patches_reduce_with_identical_boundaries_and_locks() {
        for curved in [false, true] {
            let (vertices, triangles) = grid(5, curved);
            let original = topology(&vertices, &triangles).unwrap();
            let mesh = simplify(&vertices, &triangles, &[12], budget()).unwrap();
            assert!(mesh.triangles.len() < triangles.len(), "{:?}", mesh.stop);
            assert!(mesh.error > 0. && mesh.error <= 2.);
            assert_eq!(mesh.vertex_remap[&12], 12);
            assert_eq!(
                topology(&vertices, &mesh.triangles).unwrap().boundary,
                original.boundary
            );
            for &(a, b) in &original.boundary {
                assert_eq!(mesh.vertex_remap[&a], a);
                assert_eq!(mesh.vertex_remap[&b], b);
            }
            let again = simplify(&vertices, &triangles, &[12], budget()).unwrap();
            assert_eq!(mesh.triangles, again.triangles);
            assert_eq!(mesh.error.to_bits(), again.error.to_bits());
        }
    }
    #[test]
    fn zero_error_budget_attempt_limits_and_unsupported_topology_keep_exact_fallback() {
        let (v, t) = grid(4, false);
        for options in [
            SimplifyBudget {
                max_error: 0.,
                ..budget()
            },
            SimplifyBudget {
                max_attempts: 0,
                ..budget()
            },
        ] {
            let m = simplify(&v, &t, &[], options).unwrap();
            assert_eq!(m.triangles, t);
            assert_eq!(m.error, 0.);
        }
        for faces in [
            vec![[0, 1, 5], [0, 1, 5]],
            vec![[0, 0, 1]],
            vec![[0, 1, 5], [0, 8, 9]],
            vec![[0, 1, 5], [0, 1, 4]],
            vec![[0, 1, 5], [1, 0, 4], [0, 1, 6]],
        ] {
            let m = simplify(&v, &faces, &[], budget()).unwrap();
            assert_eq!(m.stop, SimplifyStop::UnsupportedTopology);
            assert_eq!(m.triangles, faces);
            assert_eq!(m.error, 0.);
        }
        assert_eq!(
            simplify(
                &v,
                &t,
                &[],
                SimplifyBudget {
                    max_input_triangles: 1,
                    ..budget()
                }
            )
            .unwrap_err(),
            SimplifyError::Limit
        );
        assert_eq!(
            simplify(&v, &[[0, 1, 99]], &[], budget()).unwrap_err(),
            SimplifyError::Input
        );
        let mut nan = v.clone();
        nan[0][0] = f32::NAN;
        assert!(simplify(&nan, &t, &[], budget()).is_err());
        assert!(
            simplify(
                &v,
                &t,
                &[],
                SimplifyBudget {
                    max_error: f32::INFINITY,
                    ..budget()
                }
            )
            .is_err()
        );
    }
    // Independent test oracle: closest point to a triangle by plane barycentrics
    // plus the three segments. This does not use the simplifier's correspondence.
    fn point_distance(p: [f64; 3], t: [[f64; 3]; 3]) -> f64 {
        let sub = |a: [f64; 3], b: [f64; 3]| std::array::from_fn::<_, 3, _>(|i| a[i] - b[i]);
        let dot = |a: [f64; 3], b: [f64; 3]| (0..3).map(|i| a[i] * b[i]).sum::<f64>();
        let a = sub(t[1], t[0]);
        let b = sub(t[2], t[0]);
        let d = sub(p, t[0]);
        let aa = dot(a, a);
        let ab = dot(a, b);
        let bb = dot(b, b);
        let ad = dot(a, d);
        let bd = dot(b, d);
        let denom = aa * bb - ab * ab;
        let u = (ad * bb - bd * ab) / denom;
        let v = (bd * aa - ad * ab) / denom;
        let mut best = f64::INFINITY;
        if denom > 0. && u >= 0. && v >= 0. && u + v <= 1. {
            best = norm(std::array::from_fn(|i| d[i] - u * a[i] - v * b[i]));
        }
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let e = sub(b, a);
            let fraction = (dot(sub(p, a), e) / dot(e, e)).clamp(0., 1.);
            best = best.min(norm(std::array::from_fn(|i| p[i] - a[i] - fraction * e[i])));
        }
        best
    }
    #[test]
    fn conservative_error_bounds_both_surfaces_including_face_interiors() {
        let (v, t) = grid(5, true);
        let mesh = simplify(&v, &t, &[], budget()).unwrap();
        assert!(mesh.triangles.len() < t.len());
        for (source, target) in [(&t, &mesh.triangles), (&mesh.triangles, &t)] {
            for tri in source {
                for i in 0..=8 {
                    for j in 0..=8 - i {
                        let u = i as f64 / 8.;
                        let w = j as f64 / 8.;
                        let p = std::array::from_fn(|a| {
                            f64::from(v[tri[0] as usize][a]) * (1. - u - w)
                                + f64::from(v[tri[1] as usize][a]) * u
                                + f64::from(v[tri[2] as usize][a]) * w
                        });
                        let distance = target
                            .iter()
                            .map(|t| point_distance(p, t.map(|id| v[id as usize].map(f64::from))))
                            .fold(f64::INFINITY, f64::min);
                        assert!(
                            distance <= f64::from(mesh.error),
                            "{distance} > {}",
                            mesh.error
                        );
                    }
                }
            }
        }
        for (&a, &b) in &mesh.vertex_remap {
            assert!(
                norm(std::array::from_fn(
                    |i| f64::from(v[a as usize][i]) - f64::from(v[b as usize][i])
                )) <= f64::from(mesh.error)
            );
        }
    }
    #[test]
    fn holes_closed_components_and_extreme_coordinates_do_not_lose_coverage() {
        let (v, t) = grid(7, false);
        let hole: Vec<_> = t
            .into_iter()
            .enumerate()
            .filter(|(i, _)| i / 2 != 14)
            .map(|(_, t)| t)
            .collect();
        let result = simplify(&v, &hole, &[], budget()).unwrap();
        assert_eq!(
            topology(&v, &hole).unwrap().boundary,
            topology(&v, &result.triangles).unwrap().boundary
        );
        let tetra = [[0., 0., 1.], [1., 0., 0.], [0., 1., 0.], [-1., -1., 0.]];
        let faces = [[0, 1, 2], [0, 2, 3], [0, 3, 1], [1, 3, 2]];
        let exact = simplify(
            &tetra,
            &faces,
            &[],
            SimplifyBudget {
                target_triangles: 1,
                ..budget()
            },
        )
        .unwrap();
        assert_eq!(exact.triangles, faces);
        assert_eq!(exact.error, 0.);
        let octa = [
            [0., 0., 1.],
            [0., 0., -1.],
            [1., 0., 0.],
            [0., 1., 0.],
            [-1., 0., 0.],
            [0., -1., 0.],
        ];
        let faces = [
            [0, 2, 3],
            [0, 3, 4],
            [0, 4, 5],
            [0, 5, 2],
            [1, 3, 2],
            [1, 4, 3],
            [1, 5, 4],
            [1, 2, 5],
        ];
        let reduced = simplify(
            &octa,
            &faces,
            &[],
            SimplifyBudget {
                target_triangles: 4,
                ..budget()
            },
        )
        .unwrap();
        assert!(reduced.triangles.len() < faces.len());
        assert!(reduced.triangles.len() >= 4);
        assert!(
            topology(&octa, &reduced.triangles)
                .unwrap()
                .boundary
                .is_empty()
        );
        let huge = octa.map(|v| v.map(|x| x * f32::MAX));
        let fallback = simplify(
            &huge,
            &faces,
            &[],
            SimplifyBudget {
                target_triangles: 4,
                max_error: f32::MAX,
                ..budget()
            },
        )
        .unwrap();
        assert_eq!(fallback.triangles, faces);
        assert_eq!(fallback.error, 0.);
        let tiny = octa.map(|v| v.map(|x| x * 1e-30));
        let tiny = simplify(
            &tiny,
            &faces,
            &[],
            SimplifyBudget {
                target_triangles: 4,
                max_error: 2e-30,
                ..budget()
            },
        )
        .unwrap();
        assert!(tiny.triangles.len() < faces.len());
        assert!(tiny.error > 0. && tiny.error.is_finite());
    }
}
