//! CPU render-preparation benchmark; no GPU or JS/WASM overhead is measured.
use std::time::Instant;
use threex_core::Scene;
fn command(op: u32, id: u32, parent: u32, x: f32, color: f32, visible: bool) -> [u8; 64] {
    let mut b = [0; 64];
    for (offset, value) in [(0, op), (4, id), (56, 2 | u32::from(visible)), (60, parent)] {
        b[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    for (offset, value) in [
        (8, x),
        (32, 1.),
        (36, 1.),
        (40, 1.),
        (44, color),
        (48, 1.),
        (52, 1.),
    ] {
        b[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    b
}
fn sample(scene: &mut Scene, mode: &str, n: usize, iteration: usize) {
    let last = n as u32;
    let x = (iteration % 2) as f32;
    match mode {
        "idle_prepare" => scene.apply(&[]).unwrap(),
        "leaf_transform_prepare" => scene.apply(&command(2, last, 0, x, 1., true)).unwrap(),
        "parent_10_children_prepare" => scene.apply(&command(2, 1, 0, x, 1., true)).unwrap(),
        "material_prepare" => scene.apply(&command(2, last, 0, 0., x, true)).unwrap(),
        "visibility_prepare" => scene
            .apply(&command(2, last, 0, 0., 1., iteration.is_multiple_of(2)))
            .unwrap(),
        _ => unreachable!(),
    }
    std::hint::black_box(scene.instances());
}
fn main() {
    println!("operation,objects,samples,median_ms,p95_ms,p99_ms,max_ms");
    for n in [100, 1000, 10000, 100000] {
        for mode in [
            "idle_prepare",
            "leaf_transform_prepare",
            "parent_10_children_prepare",
            "material_prepare",
            "visibility_prepare",
        ] {
            let mut scene = Scene::default();
            let mut bytes = Vec::with_capacity(n * 128);
            for id in 1..=n as u32 {
                let parent = if (2..=11).contains(&id) { 1 } else { 0 };
                bytes.extend(command(1, id, 0, 0., 1., true));
                bytes.extend(command(2, id, parent, 0., 1., true));
            }
            scene.apply(&bytes).unwrap();
            for i in 0..8 {
                sample(&mut scene, mode, n, i);
            }
            let mut samples = Vec::with_capacity(80);
            for i in 0..80 {
                let start = Instant::now();
                sample(&mut scene, mode, n, i);
                samples.push(start.elapsed().as_secs_f64() * 1000.);
            }
            samples.sort_by(f64::total_cmp);
            let q = |p: f64| samples[((samples.len() - 1) as f64 * p).ceil() as usize];
            println!(
                "{mode},{n},80,{:.6},{:.6},{:.6},{:.6}",
                q(0.5),
                q(0.95),
                q(0.99),
                q(1.)
            );
        }
    }
}
