use std::{fs, time::Instant};
use threex_storage::{AutosaveDocument, DocumentStore};
fn command(id: u32, op: u32) -> [u8; 64] {
    let mut b = [0; 64];
    b[..4].copy_from_slice(&op.to_le_bytes());
    b[4..8].copy_from_slice(&id.to_le_bytes());
    b
}
fn main() {
    println!(
        "objects,group_size,samples,median_ms,p95_ms,p99_ms,max_ms,expected_fsyncs_per_sample"
    );
    for n in [100, 10000] {
        for size in [1usize, 8, 32] {
            let p = std::env::temp_dir()
                .join(format!("threex-group-{}-{n}-{size}", std::process::id()));
            let mut a = AutosaveDocument::open(DocumentStore::new(&p)).unwrap();
            for chunk in (1..=n).collect::<Vec<u32>>().chunks(512) {
                let b = chunk
                    .iter()
                    .flat_map(|&id| command(id, 1))
                    .collect::<Vec<_>>();
                a.commit(a.document().revision(), &b).unwrap();
            }
            a.save().unwrap();
            let mut samples = Vec::new();
            for i in 0..28 {
                let b = (0..size)
                    .map(|j| command(n + (i * size + j + 1) as u32, 1))
                    .collect::<Vec<_>>();
                let start = Instant::now();
                #[cfg(feature = "group")]
                {
                    let slices = b.iter().map(|b| b.as_slice()).collect::<Vec<_>>();
                    a.commit_group(a.document().revision(), &slices).unwrap();
                }
                #[cfg(not(feature = "group"))]
                {
                    for b in &b {
                        a.commit(a.document().revision(), b).unwrap();
                    }
                }
                if i >= 4 {
                    samples.push(start.elapsed().as_secs_f64() * 1000.)
                }
            }
            samples.sort_by(f64::total_cmp);
            let q = |p: f64| samples[((samples.len() - 1) as f64 * p).ceil() as usize];
            let syncs = if cfg!(feature = "group") { 1 } else { size };
            println!(
                "{n},{size},24,{:.6},{:.6},{:.6},{:.6},{syncs}",
                q(0.5),
                q(0.95),
                q(0.99),
                q(1.)
            );
            drop(a);
            fs::remove_dir_all(p).unwrap();
        }
    }
}
