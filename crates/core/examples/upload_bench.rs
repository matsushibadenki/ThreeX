//! Revision delta upload preparation; no queue or GPU required.
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
        for mode in ["one_upload_prepare", "skip_8_prepare", "all_upload_prepare"] {
            let mut s = Scene::default();
            let mut initial = Vec::new();
            let mut updates = [Vec::new(), Vec::new()];
            for id in 1..=n as u32 {
                initial.extend(command(1, id, 0.));
                initial.extend(command(2, id, 0.));
                if mode == "all_upload_prepare" {
                    for (i, b) in updates.iter_mut().enumerate() {
                        b.extend(command(2, id, i as f32));
                    }
                }
            }
            s.apply(&initial).unwrap();
            let mut stamp = Some(s.stamp());
            let mut samples = Vec::new();
            for i in 0..140 {
                let start = Instant::now();
                if mode == "all_upload_prepare" {
                    s.apply(&updates[i % 2]).unwrap();
                } else {
                    let count = if mode == "skip_8_prepare" { 8 } else { 1 };
                    for j in 0..count {
                        s.apply(&command(2, (j + 1) as u32, (i + 1) as f32))
                            .unwrap();
                    }
                }
                let plan = s.instance_updates_since(stamp.as_ref(), false);
                std::hint::black_box(&plan.ranges);
                stamp = Some(plan.stamp);
                std::hint::black_box(&stamp);
                std::hint::black_box(s.instances());
                if i >= 12 {
                    samples.push(start.elapsed().as_secs_f64() * 1000.);
                }
            }
            samples.sort_by(f64::total_cmp);
            let q = |p: f64| samples[((samples.len() - 1) as f64 * p).ceil() as usize];
            println!(
                "{mode},{n},128,{:.6},{:.6},{:.6},{:.6}",
                q(0.5),
                q(0.95),
                q(0.99),
                q(1.)
            );
        }
    }
}
