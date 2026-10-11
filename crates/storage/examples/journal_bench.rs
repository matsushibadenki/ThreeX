use std::{fs, time::Instant};
use threex_core::{Document, Object};
use threex_storage::{AutosaveDocument, DocumentStore};
fn crc(b: &[u8]) -> u32 {
    let mut c = !0u32;
    for &v in b {
        c ^= v as u32;
        for _ in 0..8 {
            c = (c >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(c & 1));
        }
    }
    !c
}
fn command(id: u32, x: f32) -> [u8; 64] {
    let mut b = [0; 64];
    b[..4].copy_from_slice(&2u32.to_le_bytes());
    b[4..8].copy_from_slice(&id.to_le_bytes());
    b[8..12].copy_from_slice(&x.to_le_bytes());
    for at in [32, 36, 40, 44, 48, 52] {
        b[at..at + 4].copy_from_slice(&1f32.to_le_bytes())
    }
    b[56..60].copy_from_slice(&3u32.to_le_bytes());
    b
}
fn seed(n: u64) -> Document {
    let mut b = b"THREEXD\0".to_vec();
    b.extend(1u32.to_le_bytes());
    for v in [1, 8 * 1024 * 1024, 256 * 1024, n, 0, 0] {
        b.extend(v.to_le_bytes())
    }
    for id in 1..=n {
        b.extend(command(id as u32, 0.))
    }
    b.extend(crc(&b).to_le_bytes());
    Document::from_checkpoint(&b).unwrap()
}
fn main() {
    let _ = Object::default();
    println!("operation,objects,samples,median_ms,p95_ms,p99_ms,max_ms");
    for n in [100, 1000, 10000, 100000] {
        for mode in ["stage_apply", "journal_sync"] {
            let mut d = seed(n);
            let path = std::env::temp_dir()
                .join(format!("threex-jbench-{}-{n}-{mode}", std::process::id()));
            fs::create_dir(&path).unwrap();
            let store = DocumentStore::new(&path);
            store.save(&d).unwrap();
            let mut owner = AutosaveDocument::open(store).unwrap();
            let mut samples = Vec::new();
            for i in 0..28 {
                let c = command(n as u32, (i + 1) as f32);
                let start = Instant::now();
                if mode == "journal_sync" {
                    owner.commit(owner.document().revision(), &c).unwrap();
                } else {
                    #[cfg(feature = "prepared")]
                    {
                        let edit = d.prepare_commit(d.revision(), &c).unwrap();
                        d.apply_prepared(edit).unwrap();
                    }
                    #[cfg(not(feature = "prepared"))]
                    {
                        let mut copy = Document::from_checkpoint(&d.checkpoint().unwrap()).unwrap();
                        copy.commit(copy.revision(), &c).unwrap();
                        d.commit(d.revision(), &c).unwrap();
                    }
                }
                std::hint::black_box(d.scene().instances());
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
            drop(owner);
            fs::remove_dir_all(path).unwrap();
        }
    }
}
