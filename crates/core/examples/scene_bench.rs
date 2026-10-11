use std::time::Instant;
use threex_core::Scene;
fn command(op: u32, id: u32) -> [u8; 64] {
    let mut b = [0; 64];
    b[..4].copy_from_slice(&op.to_le_bytes());
    b[4..8].copy_from_slice(&id.to_le_bytes());
    for n in [32, 36, 40, 44, 48, 52] {
        b[n..n + 4].copy_from_slice(&1f32.to_le_bytes());
    }
    b[56..60].copy_from_slice(&3u32.to_le_bytes());
    b
}
fn report(label: &str, n: usize, mut samples: Vec<f64>) {
    samples.sort_by(f64::total_cmp);
    let q = |p: f64| samples[((samples.len() - 1) as f64 * p).ceil() as usize];
    println!(
        "{label},{n},{:.6},{:.6},{:.6},{:.6}",
        q(0.5),
        q(0.95),
        q(0.99),
        q(1.)
    );
}
fn main() {
    println!("operation,objects,median_ms,p95_ms,p99_ms,max_ms");
    for n in [100, 1000, 10000, 100000] {
        let mut s = Scene::default();
        let mut batch = Vec::with_capacity(n * 128);
        for id in 1..=n as u32 {
            batch.extend(command(1, id));
            batch.extend(command(2, id));
        }
        s.apply(&batch).unwrap();
        for mode in ["idle_apply", "one_transform", "instances"] {
            let mut samples = vec![];
            for i in 0..40 {
                let start = Instant::now();
                match mode {
                    "idle_apply" => s.apply(&[]).unwrap(),
                    "one_transform" => {
                        let mut b = command(2, 1);
                        b[8..12].copy_from_slice(&(i as f32).to_le_bytes());
                        s.apply(&b).unwrap()
                    }
                    _ => {
                        std::hint::black_box(s.instances());
                    }
                }
                samples.push(start.elapsed().as_secs_f64() * 1000.);
            }
            report(mode, n, samples);
        }
    }
}
