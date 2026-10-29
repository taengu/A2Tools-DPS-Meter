//! Deriving the same slice gives the same fight every time (A2_STABLE_SLICES:
//! a folder of stored slices, gzip or zstd). A HashMap's order changes from
//! one map to the next, and in the log service from one request to the next,
//! so nothing derived may depend on it: it flipped a player's class between
//! derives of the same bytes (2026-10-17). Skips when the folder is not given.

use std::io::Read;

use a2tools_dps_meter_lib::rederive;

const NPCS: &str = include_str!("../../src/data/i18n/npcs/en.json");
const SKILLS: &str = include_str!("../../src/data/i18n/skills/en.json");
const DOTS: &str = include_str!("../../src/data/dot_skill_ids.json");

fn unpack(body: &[u8]) -> Vec<u8> {
    let mut raw = Vec::new();
    if body.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        ruzstd::decoding::StreamingDecoder::new(body).unwrap().read_to_end(&mut raw).unwrap();
    } else {
        flate2::read::GzDecoder::new(body).read_to_end(&mut raw).unwrap();
    }
    raw
}

#[test]
fn a_slice_derives_the_same_every_time() {
    let Ok(dir) = std::env::var("A2_STABLE_SLICES") else { return };
    let mut unstable = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let slice = unpack(&std::fs::read(&path).unwrap());
        let first = serde_json::to_string(&rederive::derive_fight(&slice, NPCS, SKILLS, DOTS).ok()).unwrap();
        if std::env::var_os("A2_STABLE_SHOW").is_some() {
            let v: serde_json::Value = serde_json::from_str(&first).unwrap();
            let ids: Vec<_> = v["record"]["actors"].as_array().into_iter().flatten().map(|a| a["jobId"].clone()).collect();
            println!("{} {ids:?}", path.file_name().unwrap().to_string_lossy());
        }
        for _ in 0..20 {
            let again = serde_json::to_string(&rederive::derive_fight(&slice, NPCS, SKILLS, DOTS).ok()).unwrap();
            if again != first {
                let (a, b): (serde_json::Value, serde_json::Value) =
                    (serde_json::from_str(&first).unwrap(), serde_json::from_str(&again).unwrap());
                let mut paths = Vec::new();
                diff("", &a, &b, &mut paths);
                paths.truncate(8);
                unstable.push(format!("{}: {paths:?}", path.file_name().unwrap().to_string_lossy()));
                break;
            }
        }
    }
    assert!(unstable.is_empty(), "derived differently between runs: {unstable:?}");
}

/// Where two derived fights differ, as JSON paths with both values.
fn diff(at: &str, a: &serde_json::Value, b: &serde_json::Value, out: &mut Vec<String>) {
    use serde_json::Value::{Array, Object};
    match (a, b) {
        (Object(x), Object(y)) => {
            for (k, v) in x {
                diff(&format!("{at}.{k}"), v, y.get(k).unwrap_or(&serde_json::Value::Null), out);
            }
        }
        (Array(x), Array(y)) if x.len() == y.len() => {
            for (i, (v, w)) in x.iter().zip(y).enumerate() {
                diff(&format!("{at}[{i}]"), v, w, out);
            }
        }
        _ if a != b => out.push(format!("{at}: {a} vs {b}")),
        _ => {}
    }
}
