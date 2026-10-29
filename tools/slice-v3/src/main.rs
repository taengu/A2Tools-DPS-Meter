//! Converts stored Evidence Slices to version 3 with zstd, as meters write
//! them from 2.0.67.
//!
//!   slice-v3 <in_dir> <out_dir>
//!
//! Each file in `in_dir` is one stored slice, named by its log id. A gzipped
//! slice is read, written as version 3, compressed with zstd 19, then read back
//! the way the log service reads it (ruzstd) and compared record by record and
//! blind map entry by entry with the original. Only a slice that matches is
//! written to `out_dir`. One line per file goes to stdout:
//!
//!   <name> converted <bytes_in> <bytes_out> <sha256 of out>
//!   <name> skipped already-zstd
//!   <name> failed <reason>

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use a2tools_dps_meter_lib::capture::evidence_slice::{self, EvidenceSlice};
use sha2::{Digest, Sha256};

const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];
const GZIP_MAGIC: [u8; 2] = [0x1F, 0x8B];

fn convert(data: &[u8]) -> Result<Option<Vec<u8>>, String> {
    if data.starts_with(&ZSTD_MAGIC) {
        return Ok(None);
    }
    if !data.starts_with(&GZIP_MAGIC) {
        return Err("not gzip".into());
    }
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(data)
        .read_to_end(&mut raw)
        .map_err(|e| format!("gunzip: {e}"))?;
    let (records, blind_map) = evidence_slice::decode(&raw).ok_or("not a slice")?;
    let v3 = evidence_slice::encode(&EvidenceSlice {
        records: records.clone(),
        blind_map: blind_map.clone(),
        stats: Default::default(),
    });
    let zst = zstd::bulk::compress(&v3, 19).map_err(|e| format!("zstd: {e}"))?;

    let mut back = Vec::new();
    ruzstd::decoding::StreamingDecoder::new(&zst[..])
        .map_err(|e| format!("ruzstd: {e}"))?
        .read_to_end(&mut back)
        .map_err(|e| format!("ruzstd: {e}"))?;
    if back != v3 {
        return Err("ruzstd read different bytes".into());
    }
    let (again, again_map) = evidence_slice::decode(&back).ok_or("v3 does not decode")?;
    if again.len() != records.len() {
        return Err(format!("{} records, not {}", again.len(), records.len()));
    }
    if let Some(i) = again.iter().zip(&records).position(|(a, b)| a != b) {
        return Err(format!("record {i} differs"));
    }
    if again_map != blind_map {
        return Err("blind map differs".into());
    }
    Ok(Some(zst))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, in_dir, out_dir] = &args[..] else {
        eprintln!("usage: slice-v3 <in_dir> <out_dir>");
        std::process::exit(2);
    };
    std::fs::create_dir_all(out_dir).expect("out_dir");
    let mut files: Vec<PathBuf> = std::fs::read_dir(in_dir)
        .expect("in_dir")
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file())
        .collect();
    files.sort();

    let next = AtomicUsize::new(0);
    let out = Mutex::new(std::io::stdout());
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(path) = files.get(i) else { break };
                    let line = one(path, Path::new(out_dir));
                    use std::io::Write;
                    let mut o = out.lock().unwrap();
                    let _ = writeln!(o, "{line}");
                }
            });
        }
    });
}

fn one(path: &Path, out_dir: &Path) -> String {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => return format!("{name} failed read: {e}"),
    };
    // A panic in the codec fails this slice, not the batch.
    match std::panic::catch_unwind(|| convert(&data)) {
        Ok(Ok(Some(zst))) => {
            if let Err(e) = std::fs::write(out_dir.join(&name), &zst) {
                return format!("{name} failed write: {e}");
            }
            let sha: String = Sha256::digest(&zst).iter().map(|b| format!("{b:02x}")).collect();
            format!("{name} converted {} {} {sha}", data.len(), zst.len())
        }
        Ok(Ok(None)) => format!("{name} skipped already-zstd"),
        Ok(Err(e)) => format!("{name} failed {e}"),
        Err(_) => format!("{name} failed panic"),
    }
}
