//! CPU packing and upload-plan benchmark; no GPU required.
use std::time::Instant;
use threex_core::Scene;
fn command(op: u32, id: u32, flags: u32) -> [u8; 64] {
    let mut b = [0; 64];
    for (at, v) in [(0, op), (4, id), (56, flags)] {
        b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
    for at in [32, 36, 40, 44, 48, 52] {
        b[at..at + 4].copy_from_slice(&1f32.to_le_bytes());
    }
    b
}
fn main() {
    println!(
        "operation,objects,samples,median_ms,p95_ms,p99_ms,max_ms,mean_upload_bytes,mean_instance_writes"
    );
    for n in [100u32, 1000, 10000, 100000] {
        for mode in [
            "visibility",
            "mesh_membership",
            "entity_lifecycle",
            "tail_visibility",
            "skip_8_visibility",
            "all_visibility",
        ] {
            let mut scene = Scene::default();
            let mut initial = Vec::new();
            for id in 1..=n {
                initial.extend(command(1, id, 3));
                initial.extend(command(2, id, 3));
            }
            scene.apply(&initial).unwrap();
            let mut stamp = scene.stamp();
            let mut samples = Vec::new();
            let mut bytes = 0usize;
            let mut writes = 0usize;
            for i in 0..140 {
                let mut batch = Vec::new();
                let count = match mode {
                    "all_visibility" => n,
                    "skip_8_visibility" => 8,
                    _ => 1,
                };
                for id in 1..=count {
                    let id = if mode == "tail_visibility" { n } else { id };
                    if mode == "entity_lifecycle" {
                        if i % 2 == 0 {
                            batch.extend(command(3, id, 0));
                        } else {
                            batch.extend(command(1, id, 3));
                            batch.extend(command(2, id, 3));
                        }
                    } else {
                        let flags = if i % 2 == 1 {
                            3
                        } else if mode == "mesh_membership" {
                            1
                        } else {
                            2
                        };
                        batch.extend(command(2, id, flags));
                    }
                }
                let start = Instant::now();
                if mode == "skip_8_visibility" {
                    for c in batch.as_chunks::<64>().0 {
                        scene.apply(c).unwrap();
                    }
                } else {
                    scene.apply(&batch).unwrap();
                }
                let plan = scene.instance_updates_since(Some(&stamp), false);
                std::hint::black_box(scene.instances());
                if i >= 12 {
                    samples.push(start.elapsed().as_secs_f64() * 1000.);
                    bytes += plan.ranges.iter().map(|r| r.len() * 80).sum::<usize>();
                    writes += scene.update_stats().instance_writes;
                }
                stamp = plan.stamp;
            }
            samples.sort_by(f64::total_cmp);
            let q = |p: f64| samples[((samples.len() - 1) as f64 * p).ceil() as usize];
            println!(
                "{mode},{n},128,{:.6},{:.6},{:.6},{:.6},{:.1},{:.1}",
                q(0.5),
                q(0.95),
                q(0.99),
                q(1.),
                bytes as f64 / 128.,
                writes as f64 / 128.
            );
        }
    }
}
