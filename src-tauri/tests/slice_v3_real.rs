//! Slice version 3 on real uploaded fights (A2_V2_SLICES: a folder of gzipped
//! version 2 slices, as the site stores them). Each is read, written as
//! version 3, compressed as the meter does (zstd 19), decompressed as the log
//! service does (ruzstd), and read back: the records must match exactly, and
//! derived as the service reads it, the fight must match the version 2 one.
//! Skips when the folder is not given.

use std::io::Read;

use a2tools_dps_meter_lib::capture::evidence_slice::{self, EvidenceSlice};
use a2tools_dps_meter_lib::rederive;

const NPCS: &str = include_str!("../../src/data/i18n/npcs/en.json");
const SKILLS: &str = include_str!("../../src/data/i18n/skills/en.json");
const DOTS: &str = include_str!("../../src/data/dot_skill_ids.json");

#[test]
fn real_slices_survive_version_3_and_zstd() {
    let Ok(dir) = std::env::var("A2_V2_SLICES") else { return };
    let (mut before, mut after, mut n) = (0usize, 0usize, 0);
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("gz") {
            continue;
        }
        let gz = std::fs::read(&path).unwrap();
        let mut v2 = Vec::new();
        flate2::read::GzDecoder::new(&gz[..]).read_to_end(&mut v2).unwrap();
        let (records, blind_map) = evidence_slice::decode(&v2).expect("a version 2 slice");
        let v3 = evidence_slice::encode(&EvidenceSlice { records: records.clone(), blind_map, stats: Default::default() });
        let zst = zstd::bulk::compress(&v3, 19).unwrap();
        let mut back = Vec::new();
        ruzstd::decoding::StreamingDecoder::new(&zst[..]).unwrap().read_to_end(&mut back).unwrap();
        assert_eq!(back, v3, "ruzstd reads what zstd wrote");
        let (again, _) = evidence_slice::decode(&back).expect("a version 3 slice");
        assert_eq!(again, records, "{}", path.display());
        // As the log service reads each: streamed, limited by the records.
        let streamed = evidence_slice::decode_from(ruzstd::decoding::StreamingDecoder::new(&zst[..]).unwrap(), 24 << 20)
            .expect("streams within the limit");
        let fight = |r: Result<_, _>| r.map(|f| serde_json::to_value(f).unwrap()).map_err(|e| format!("{e:?}"));
        assert_eq!(
            fight(rederive::derive_decoded(&streamed, NPCS, SKILLS, DOTS)),
            fight(rederive::derive_fight(&v2, NPCS, SKILLS, DOTS)),
            "derived alike: {}",
            path.display()
        );
        before += gz.len();
        after += zst.len();
        n += 1;
    }
    println!("{n} slices: gzip v2 {} KB -> zstd v3 {} KB ({:.1}%)", before / 1024, after / 1024, 100.0 * after as f64 / before as f64);
    assert!(n > 0);
}
