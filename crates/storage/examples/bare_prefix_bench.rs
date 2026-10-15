use std::{
    fs,
    time::{Duration, Instant},
};
use threex_storage::{
    AutosaveDocument, DocumentStore, JournalWorker, WorkerAction, WorkerConfig, WorkerRequest,
};
fn record(id: u32, op: u32, x: f32) -> Vec<u8> {
    let mut b = vec![0; 64];
    b[..4].copy_from_slice(&op.to_le_bytes());
    b[4..8].copy_from_slice(&id.to_le_bytes());
    b[8..12].copy_from_slice(&x.to_le_bytes());
    for at in [32, 36, 40, 44, 48, 52] {
        b[at..at + 4].copy_from_slice(&1f32.to_le_bytes());
    }
    b[56..60].copy_from_slice(&1u32.to_le_bytes());
    b
}
fn main() {
    println!("objects,group_size,workload,mode,samples,median_us,p95_us,p99_us");
    for n in [100u32, 10000] {
        for size in [1usize, 8, 32] {
            for workload in ["create_each", "create_update"] {
                for mode in ["candidate_cpu", "worker_sync"] {
                    let path = std::env::temp_dir().join(format!(
                        "threex-bare-prefix-{}-{n}-{size}-{workload}-{mode}",
                        std::process::id()
                    ));
                    let mut a = AutosaveDocument::open(DocumentStore::new(&path)).unwrap();
                    for chunk in (1..=n).collect::<Vec<_>>().chunks(512) {
                        let bytes = chunk
                            .iter()
                            .flat_map(|&id| record(id, 1, 0.))
                            .collect::<Vec<_>>();
                        a.commit(a.document().revision(), &bytes).unwrap();
                    }
                    a.save().unwrap();
                    let base = a.document().revision();
                    let worker = if mode == "worker_sync" {
                        drop(a);
                        Some(
                            JournalWorker::start(
                                DocumentStore::new(&path),
                                WorkerConfig {
                                    batch_window: Duration::from_millis(2),
                                    ..Default::default()
                                },
                            )
                            .unwrap(),
                        )
                    } else {
                        None
                    };
                    let document = if worker.is_none() {
                        Some(DocumentStore::new(&path).load().unwrap().document)
                    } else {
                        None
                    };
                    let mut samples = Vec::new();
                    for i in 0..28 {
                        let base = base
                            + if worker.is_some() {
                                (i * size) as u64
                            } else {
                                0
                            };
                        let requests = (0..size)
                            .map(|j| WorkerAction::Commit {
                                expected_revision: base + j as u64,
                                commands: if workload == "create_each" || j == 0 {
                                    record(n + (i * size + j + 1) as u32, 1, 0.)
                                } else {
                                    record(n + (i * size + 1) as u32, 2, j as f32)
                                },
                            })
                            .collect::<Vec<_>>();
                        let start = Instant::now();
                        if let Some(d) = &document {
                            let input = requests
                                .iter()
                                .map(|r| {
                                    let WorkerAction::Commit {
                                        expected_revision,
                                        commands,
                                    } = r
                                    else {
                                        unreachable!()
                                    };
                                    (*expected_revision, commands.as_slice())
                                })
                                .collect::<Vec<_>>();
                            for _ in 0..32 {
                                #[cfg(not(feature = "bare_prefix"))]
                                {
                                    for end in 1..=input.len() {
                                        let bytes =
                                            input[..end].iter().map(|r| r.1).collect::<Vec<_>>();
                                        std::hint::black_box(
                                            d.prepare_group(base, &bytes).unwrap(),
                                        );
                                    }
                                    std::hint::black_box(
                                        d.prepare_group(
                                            base,
                                            &input.iter().map(|r| r.1).collect::<Vec<_>>(),
                                        )
                                        .unwrap(),
                                    );
                                }
                                #[cfg(feature = "bare_prefix")]
                                {
                                    let mut builder = d.commit_group_builder(base).unwrap();
                                    for &(expected, commands) in &input {
                                        builder.push(expected, commands).unwrap();
                                    }
                                    std::hint::black_box(builder.finish());
                                }
                            }
                        } else {
                            let worker = worker.as_ref().unwrap();
                            let client = worker.client();
                            let replies = requests
                                .into_iter()
                                .enumerate()
                                .map(|(j, r)| {
                                    client
                                        .try_submit(WorkerRequest {
                                            id: (i * size + j + 1) as u64,
                                            action: r,
                                        })
                                        .unwrap()
                                })
                                .collect::<Vec<_>>();
                            for reply in replies {
                                reply
                                    .recv_timeout(Duration::from_secs(5))
                                    .unwrap()
                                    .result
                                    .unwrap();
                            }
                        }
                        if i >= 4 {
                            samples.push(
                                start.elapsed().as_secs_f64() * 1e6
                                    / if mode == "candidate_cpu" { 32. } else { 1. },
                            );
                        }
                    }
                    samples.sort_by(f64::total_cmp);
                    let q = |p: f64| samples[((samples.len() - 1) as f64 * p).ceil() as usize];
                    println!(
                        "{n},{size},{workload},{mode},24,{:.3},{:.3},{:.3}",
                        q(0.5),
                        q(0.95),
                        q(0.99)
                    );
                    if let Some(w) = worker {
                        w.shutdown().unwrap();
                    }
                    fs::remove_dir_all(path).unwrap();
                }
            }
        }
    }
}
