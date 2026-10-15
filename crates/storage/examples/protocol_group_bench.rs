use std::{fs, time::Instant};
use threex_storage::{
    AutosaveDocument, DocumentStore, ProtocolAction, ProtocolRequest, new_session_id,
};
fn command(id: u32) -> Vec<u8> {
    let mut b = vec![0; 64];
    b[..4].copy_from_slice(&1u32.to_le_bytes());
    b[4..8].copy_from_slice(&id.to_le_bytes());
    b
}
fn main() {
    println!(
        "objects,group_size,samples,median_ms,p95_ms,p99_ms,max_ms,expected_fsyncs_per_sample"
    );
    for n in [100, 10000] {
        for size in [1usize, 8, 32] {
            let p = std::env::temp_dir().join(format!(
                "threex-protocol-bench-{}-{n}-{size}",
                std::process::id()
            ));
            let mut a = AutosaveDocument::open(DocumentStore::new(&p)).unwrap();
            for chunk in (1..=n).collect::<Vec<u32>>().chunks(512) {
                let b = chunk.iter().flat_map(|&id| command(id)).collect::<Vec<_>>();
                a.commit(a.document().revision(), &b).unwrap();
            }
            a.save().unwrap();
            let session = new_session_id().unwrap();
            let mut samples = Vec::new();
            for i in 0..28 {
                let base = a.document().revision();
                let requests = (0..size)
                    .map(|j| ProtocolRequest {
                        protocol_version: 1,
                        document_id: a.document().document_id().unwrap(),
                        session_id: session,
                        sequence: (i * size + j + 1) as u64,
                        action: ProtocolAction::Commit {
                            expected_revision: base + j as u64,
                            commands: command(n + (i * size + j + 1) as u32),
                        },
                    })
                    .collect::<Vec<_>>();
                let start = Instant::now();
                #[cfg(feature = "protocol_group")]
                a.request_group(&requests.iter().collect::<Vec<_>>())
                    .unwrap();
                #[cfg(not(feature = "protocol_group"))]
                for r in &requests {
                    a.request(r).unwrap();
                }
                if i >= 4 {
                    samples.push(start.elapsed().as_secs_f64() * 1000.);
                }
            }
            samples.sort_by(f64::total_cmp);
            let q = |p: f64| samples[((samples.len() - 1) as f64 * p).ceil() as usize];
            let syncs = if cfg!(feature = "protocol_group") {
                1
            } else {
                size
            };
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
