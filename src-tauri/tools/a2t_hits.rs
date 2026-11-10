//! Check `rederive::derive_hits_with` against stored slices.
//!
//!     a2t-hits <slice.a2es.gz|zst> [...]
//!
//! For each slice: the record derived as the log service derives it, and again
//! with the hits collected. The two records must be identical (collecting
//! changes nothing), and the hits must add up to every row (`matches`).
//! Prints the hit count and the size of the per-hit JSON, raw and gzipped.

use std::io::{Read, Write};

use a2tools_dps_meter_lib::capture::evidence_slice;
use a2tools_dps_meter_lib::rederive::{derive_decoded_with, derive_hits_with, Tables};

const NPCS: &str = include_str!("../../src/data/i18n/npcs/en.json");
const SKILLS: &str = include_str!("../../src/data/i18n/skills/en.json");
const DOTS: &str = include_str!("../../src/data/dot_skill_ids.json");

fn main() {
    let tables = Tables::load(NPCS, SKILLS, DOTS);
    let mut failed = 0;
    for path in std::env::args().skip(1) {
        let body = std::fs::read(&path).expect("read");
        let raw = if body.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
            zstd::decode_all(&body[..]).expect("zstd")
        } else {
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(&body[..]).read_to_end(&mut out).expect("gzip");
            out
        };
        let decoded = evidence_slice::decode(&raw).expect("a slice");
        let plain = derive_decoded_with(&decoded, &tables);
        let with = derive_hits_with(&decoded, &tables);
        let name = std::path::Path::new(&path).file_name().unwrap().to_string_lossy().to_string();
        match (plain, with) {
            (Ok(plain), Ok((fight, hits))) => {
                let same = serde_json::to_string(&plain).unwrap() == serde_json::to_string(&fight).unwrap();
                let json = serde_json::to_vec(&hits).unwrap();
                let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
                gz.write_all(&json).unwrap();
                let gz = gz.finish().unwrap();
                let rows_hits: i64 = fight.record.details.skills.iter().map(|r| r.time as i64).sum();
                println!(
                    "{name}: hits {} (rows count {rows_hits}), rows {}, matches {}, record unchanged {same}, json {} KB, gz {} KB",
                    hits.t.len(), hits.rows.len(), hits.matches, json.len() / 1024, gz.len() / 1024
                );
                if !same || !hits.matches {
                    failed += 1;
                }
            }
            (Err(a), Err(b)) => println!("{name}: no fight ({a:?} / {b:?})"),
            (a, b) => {
                println!("{name}: the two derives disagree ({} / {})", a.is_ok(), b.is_ok());
                failed += 1;
            }
        }
    }
    std::process::exit(if failed > 0 { 1 } else { 0 });
}
