use std::time::Instant;
use threex_core::Scene;
fn command(op: u32, id: u32, parent: u32) -> [u8; 64] {
    let mut b = [0; 64];
    for (at, v) in [(0, op), (4, id), (56, 3), (60, parent)] {
        b[at..at + 4].copy_from_slice(&v.to_le_bytes())
    }
    for at in [32, 36, 40, 44, 48, 52] {
        b[at..at + 4].copy_from_slice(&1f32.to_le_bytes())
    }
    b
}
fn main() {
    println!("operation,objects,samples,median_ms,p95_ms,p99_ms,max_ms");
    for n in [100u32, 10000, 100000] {
        for mode in ["reparent_leaf", "delete_recreate_leaf"] {
            let mut s = Scene::default();
            let initial = (1..=n)
                .flat_map(|id| [command(1, id, 0), command(2, id, 0)].concat())
                .collect::<Vec<_>>();
            s.apply(&initial).unwrap();
            let mut samples = Vec::new();
            for i in 0..28 {
                let b = if mode == "reparent_leaf" {
                    command(2, n, if i % 2 == 0 { 1 } else { 2 }).to_vec()
                } else if i % 2 == 0 {
                    command(3, n, 0).to_vec()
                } else {
                    [command(1, n, 0), command(2, n, 0)].concat()
                };
                let start = Instant::now();
                s.apply(&b).unwrap();
                std::hint::black_box(s.instances());
                if i >= 4 {
                    samples.push(start.elapsed().as_secs_f64() * 1000.)
                }
            }
            samples.sort_by(f64::total_cmp);
            let q = |p: f64| samples[((samples.len() - 1) as f64 * p).ceil() as usize];
            println!(
                "{mode},{n},24,{:.6},{:.6},{:.6},{:.6}",
                q(0.5),
                q(0.95),
                q(0.99),
                q(1.)
            );
        }
    }
}
