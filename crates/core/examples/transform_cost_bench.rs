//! Initial construction and all-dynamic updates, to expose retained-cache costs.
use std::time::Instant;
use threex_core::Scene;
fn command(op: u32, id: u32, x: f32) -> [u8; 64] {
    let mut b = [0; 64];
    for (at, v) in [(0, op), (4, id), (56, 3)] {
        b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
    for (at, v) in [
        (8, x),
        (32, 1.),
        (36, 1.),
        (40, 1.),
        (44, 1.),
        (48, 1.),
        (52, 1.),
    ] {
        b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
    b
}
fn main() {
    println!("operation,objects,samples,median_ms,p95_ms,p99_ms,max_ms");
    for n in [100, 1000, 10000, 100000] {
        let mut initial = Vec::with_capacity(n * 128);
        let mut updates = [Vec::with_capacity(n * 64), Vec::with_capacity(n * 64)];
        for id in 1..=n as u32 {
            initial.extend(command(1, id, 0.));
            initial.extend(command(2, id, 0.));
            for (i, bytes) in updates.iter_mut().enumerate() {
                bytes.extend(command(2, id, i as f32));
            }
        }
        for mode in ["initial_build_prepare", "all_transform_prepare"] {
            let mut scene = Scene::default();
            scene.apply(&initial).unwrap();
            let mut samples = Vec::with_capacity(20);
            for i in 0..24 {
                if mode == "initial_build_prepare" {
                    scene = Scene::default();
                }
                let bytes = if mode == "initial_build_prepare" {
                    &initial
                } else {
                    &updates[i % 2]
                };
                let start = Instant::now();
                scene.apply(bytes).unwrap();
                std::hint::black_box(scene.instances());
                let elapsed = start.elapsed().as_secs_f64() * 1000.;
                if i >= 4 {
                    samples.push(elapsed);
                }
            }
            samples.sort_by(f64::total_cmp);
            let q = |p: f64| samples[((samples.len() - 1) as f64 * p).ceil() as usize];
            println!(
                "{mode},{n},20,{:.6},{:.6},{:.6},{:.6}",
                q(0.5),
                q(0.95),
                q(0.99),
                q(1.)
            );
        }
    }
}
