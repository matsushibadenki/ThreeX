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
        "objects,group_size,workload,operation,samples,median_ms,p95_ms,p99_ms,max_ms,expected_fsyncs_per_sample"
    );
    for n in [100, 10000] {
        for size in [1usize, 8, 32] {
            for workload in ["create_each", "create_then_update"] {
                for mode in ["prepare_only", "journal_sync"] {
                    let p = std::env::temp_dir().join(format!(
                        "threex-prefix-{}-{n}-{size}-{workload}-{mode}",
                        std::process::id()
                    ));
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
                            .map(|j| {
                                if workload == "create_each" || j == 0 {
                                    command(n + (i * size + j + 1) as u32, 1)
                                } else {
                                    let mut c = command(n + (i * size + 1) as u32, 2);
                                    c[8..12].copy_from_slice(&(j as f32).to_le_bytes());
                                    for at in [32, 36, 40, 44, 48, 52] {
                                        c[at..at + 4].copy_from_slice(&1f32.to_le_bytes())
                                    }
                                    c[56..60].copy_from_slice(&1u32.to_le_bytes());
                                    c
                                }
                            })
                            .collect::<Vec<_>>();
                        let start = Instant::now();
                        let slices = b.iter().map(|b| b.as_slice()).collect::<Vec<_>>();
                        if mode == "prepare_only" {
                            std::hint::black_box(
                                a.document()
                                    .prepare_group(a.document().revision(), &slices)
                                    .unwrap(),
                            );
                        } else {
                            a.commit_group(a.document().revision(), &slices).unwrap();
                        }
                        if i >= 4 {
                            samples.push(start.elapsed().as_secs_f64() * 1000.)
                        }
                    }
                    samples.sort_by(f64::total_cmp);
                    let q = |p: f64| samples[((samples.len() - 1) as f64 * p).ceil() as usize];
                    let syncs = usize::from(mode == "journal_sync");
                    println!(
                        "{n},{size},{workload},{mode},24,{:.6},{:.6},{:.6},{:.6},{syncs}",
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
    }
}
