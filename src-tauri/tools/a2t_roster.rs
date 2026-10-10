//! `a2t-roster` — build the supporter roster the meter downloads.
//!
//! ```text
//! a2t-roster names.txt -o patrons-v1.bin        one character name per line
//! a2t-roster names.txt --dbid -o patrons-v1.bin ids instead of names
//! a2t-roster chars.txt --server -o patrons-v2.bin   server:name per line
//! a2t-roster --check patrons-v1.bin --find Misti [--on 1304]
//! ```
//!
//! Until accounts exist this list is curated by hand from payment messages, so
//! the input is character names. Once characters can be verified the server will
//! know each supporter's roster id and should switch to `--dbid`, which survives
//! renames and cannot collide across worlds — the file format carries which kind
//! it holds, so the meter needs no change when that happens.
//!
//! Publishing, per the release process, is an upload to the same R2 bucket the
//! installer lives in:
//!
//! ```text
//! aws s3api put-object --bucket aion2-dps-meter --key patrons-v1.bin \
//!   --body patrons-v1.bin --content-type application/octet-stream \
//!   --endpoint-url $R2_ENDPOINT
//! ```

use std::process::ExitCode;

use a2tools_dps_meter_lib::supporters::{self, KeyKind, Roster};

/// Ships in the meter, so it is not a secret and is not trying to be. Hashing
/// keeps the published file from being read as a donor list; it does not stop
/// anyone testing a name they already have. Changing it invalidates every
/// entry, so it changes only alongside a format version.
const SALT: &[u8] = b"a2tools-supporters-v1";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }

    let flag = |name: &str| args.iter().position(|a| a == name);
    let value_of = |name: &str| flag(name).and_then(|i| args.get(i + 1)).cloned();

    // --check reads a roster back, which is the only way to confirm the file you
    // are about to publish says what you think it says.
    if let Some(path) = value_of("--check") {
        let Ok(bytes) = std::fs::read(&path) else {
            eprintln!("cannot read {path}");
            return ExitCode::FAILURE;
        };
        let Some(roster) = Roster::parse(&bytes) else {
            eprintln!("{path} is not a supporter roster");
            return ExitCode::FAILURE;
        };
        println!("{path}: {} entries, {} bytes", roster.len(), bytes.len());
        if let Some(name) = value_of("--find") {
            let server = value_of("--on").and_then(|s| s.parse::<u16>().ok()).unwrap_or(0);
            let hit = roster.contains(&name, name.parse::<u64>().unwrap_or(0), server);
            println!("{name:?}: {}", if hit { "supporter" } else { "not found" });
            return if hit { ExitCode::SUCCESS } else { ExitCode::FAILURE };
        }
        return ExitCode::SUCCESS;
    }

    let input = &args[0];
    let out = value_of("-o").unwrap_or_else(|| "patrons-v1.bin".to_string());
    let kind = if args.iter().any(|a| a == "--dbid") {
        KeyKind::Dbid
    } else if args.iter().any(|a| a == "--server") {
        KeyKind::NameServer
    } else {
        KeyKind::Name
    };

    let Ok(text) = std::fs::read_to_string(input) else {
        eprintln!("cannot read {input}");
        return ExitCode::FAILURE;
    };
    let entries: Vec<String> = text
        .lines()
        .map(|l| l.trim())
        // '#' starts a comment so the list can say who each entry is without
        // that ending up in the published file.
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.to_string())
        .collect();

    if entries.is_empty() {
        eprintln!("{input} has no entries");
        return ExitCode::FAILURE;
    }

    let bytes = supporters::build(kind, SALT, &entries);
    if let Err(e) = std::fs::write(&out, &bytes) {
        eprintln!("cannot write {out}: {e}");
        return ExitCode::FAILURE;
    }

    let roster = Roster::parse(&bytes).expect("what we just built must parse");
    println!(
        "{out}: {} entries from {} lines, {} bytes ({:?}-keyed)",
        roster.len(),
        entries.len(),
        bytes.len(),
        kind
    );
    if roster.len() != entries.len() {
        println!(
            "  {} duplicate(s) collapsed",
            entries.len() - roster.len()
        );
    }
    println!();
    println!("The file contains hashes, not names. Nothing in it identifies anyone");
    println!("to someone who does not already have a name to test — but anyone can");
    println!("test a guess, so only list supporters who chose to be visible.");
    ExitCode::SUCCESS
}

const USAGE: &str = "\
a2t-roster — build the supporter roster the meter downloads

  a2t-roster names.txt [-o patrons-v1.bin]   one character name per line
  a2t-roster ids.txt --dbid [-o out.bin]     roster ids instead of names
  a2t-roster chars.txt --server [-o out.bin] server:name per line (1304:Misti)
  a2t-roster --check <file> [--find NAME] [--on SERVER]   read a roster back

Blank lines and lines starting with # are ignored, so the source list can carry
notes about who each entry is without publishing them.";
