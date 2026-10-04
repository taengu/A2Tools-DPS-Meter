// Decompresses the locked combat flow's LZ4 bundles and locates mob spawn
// packets by mob-type id and actor id, printing surrounding bytes so we can see
// the new spawn opcode/structure after a game update.
//
//   cargo run --bin inspect_mobs -- <dumpfile> <locked_src_port> <mobtype:actor> ...
// e.g.
//   cargo run --bin inspect_mobs -- dump.txt 63558 2100002:19870 2100009:26951

use a2tools_dps_meter_lib::capture::stream_processor::read_varint;

fn hexval(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
fn decode_hex(h: &str) -> Vec<u8> {
    let b = h.trim().as_bytes();
    let mut out = Vec::with_capacity(b.len() / 2);
    let mut i = 0;
    while i + 1 < b.len() {
        if let (Some(hi), Some(lo)) = (hexval(b[i]), hexval(b[i + 1])) {
            out.push((hi << 4) | lo);
        }
        i += 2;
    }
    out
}

/// Encode an i32 as the game's LE base-128 varint.
fn varint(mut v: i32) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let mut byte = (v & 0x7F) as u8;
        v = ((v as u32) >> 7) as i32;
        if v != 0 {
            byte |= 0x80;
            out.push(byte);
        } else {
            out.push(byte);
            break;
        }
    }
    out
}

fn find_all(hay: &[u8], needle: &[u8]) -> Vec<usize> {
    let mut v = Vec::new();
    if needle.is_empty() || needle.len() > hay.len() {
        return v;
    }
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if &hay[i..i + needle.len()] == needle {
            v.push(i);
        }
        i += 1;
    }
    v
}

fn hexline(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02X}", b)).collect::<Vec<_>>().join(" ")
}

fn decompress_bundle(payload: &[u8], out: &mut Vec<u8>) {
    if payload.len() < 7 {
        return;
    }
    let size = u32::from_le_bytes([payload[2], payload[3], payload[4], payload[5]]) as usize;
    if size == 0 || size > 1_000_000 {
        return;
    }
    if let Ok(d) = lz4_flex::decompress(&payload[6..], size) {
        walk(&d, out);
    }
}

/// Walk varint-framed packets, appending non-bundle packet bytes to `out` and
/// recursively decompressing nested bundles.
fn walk(buf: &[u8], out: &mut Vec<u8>) {
    let mut offset = 0;
    while offset < buf.len() {
        if buf[offset] == 0 {
            offset += 1;
            continue;
        }
        let li = read_varint(buf, offset);
        if li.length <= 0 || li.value <= 0 {
            offset += 1;
            continue;
        }
        // The length counts the payload plus 4.
        let total = li.value as i64 - 4 + li.length as i64;
        if total <= 0 || total > 65535 {
            offset += 1;
            continue;
        }
        let total = total as usize;
        if offset + total > buf.len() {
            break;
        }
        let ps = li.length as usize;
        let is_bundle = ps + 1 < total && buf[offset + ps] == 0xFF && buf[offset + ps + 1] == 0xFF;
        if is_bundle {
            decompress_bundle(&buf[offset + ps..offset + total], out);
            offset += total;
        } else {
            out.extend_from_slice(&buf[offset..offset + total]);
            offset += total;
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dump = &args[1];
    let port: u16 = args[2].parse().unwrap();
    let targets: Vec<(i32, i32)> = args[3..]
        .iter()
        .filter_map(|s| {
            let (a, b) = s.split_once(':')?;
            Some((a.parse().ok()?, b.parse().ok()?))
        })
        .collect();

    // Reassemble the locked flow (server->client, src == port).
    let content = std::fs::read_to_string(dump).unwrap();
    let mut stream = Vec::new();
    for line in content.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let p: Vec<&str> = line.splitn(6, '|').collect();
        if p.len() < 6 {
            continue;
        }
        let (src, _dst) = match p[1].split_once("->") {
            Some(x) => x,
            None => continue,
        };
        let src_port: u16 = src.rsplit(':').next().and_then(|s| s.parse().ok()).unwrap_or(0);
        if src_port != port {
            continue;
        }
        stream.extend(decode_hex(p[5]));
    }
    println!("Reassembled {} raw bytes for src port {}", stream.len(), port);

    // Decompress into the plaintext packet stream.
    let mut plain = Vec::new();
    walk(&stream, &mut plain);
    println!("Decompressed plaintext stream: {} bytes\n", plain.len());

    // Histogram of every `XX 36` two-byte opcode in the decompressed stream.
    println!("--- `XX 36` opcode histogram (decompressed) ---");
    let mut hist: std::collections::HashMap<u8, usize> = std::collections::HashMap::new();
    for w in plain.windows(2) {
        if w[1] == 0x36 {
            *hist.entry(w[0]).or_default() += 1;
        }
    }
    let mut rows: Vec<_> = hist.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    for (b, c) in rows.iter().take(16) {
        println!("  {:02X} 36 : {}", b, c);
    }
    println!();

    // Death packets: structure <op> <entity_varint> 00 03 (combat death flag 3).
    // Find each killed mob's entity id followed by `00 03` and show the opcode.
    println!("--- death-packet opcode probe (looking for <actor> 00 03) ---");
    for (_mt, actor) in &targets {
        let mut needle = varint(*actor);
        needle.push(0x00);
        needle.push(0x03);
        let hits = find_all(&plain, &needle);
        if hits.is_empty() {
            println!("  actor {}: no `<id> 00 03` death pattern found", actor);
        }
        for &idx in hits.iter().take(2) {
            let start = idx.saturating_sub(4);
            println!("  actor {}: ...{} [ID..0003]", actor, hexline(&plain[start..(idx + needle.len()).min(plain.len())]));
        }
    }
    println!();

    // Player-spawn probe: dump context for candidate opcodes and flag ASCII runs
    // (player spawn carries the character name as `07 <len> <utf8>`).
    for op in [0x44u8, 0x45, 0x46] {
        let hits = find_all(&plain, &[op, 0x36]);
        println!("--- {:02X} 36 : {} hits (player-spawn candidate) ---", op, hits.len());
        for &idx in hits.iter().take(3) {
            let end = (idx + 48).min(plain.len());
            let raw = &plain[idx..end];
            let ascii: String = raw.iter().map(|&b| if (0x20..=0x7E).contains(&b) { b as char } else { '.' }).collect();
            println!("  @{} {}", idx, hexline(raw));
            println!("       ascii: {}", ascii);
        }
    }
    println!();

    for (mobtype, actor) in &targets {
        println!("==== mob_type {} , actor {} ====", mobtype, actor);
        // mob type is stored little-endian 3-byte in the spawn packet
        let mt = (*mobtype as u32).to_le_bytes();
        let mt3 = [mt[0], mt[1], mt[2]];
        let av = varint(*actor);
        println!("  mob_type LE3 = {}   actor varint = {}", hexline(&mt3), hexline(&av));

        let mt_hits = find_all(&plain, &mt3);
        let av_hits = find_all(&plain, &av);
        println!("  mob_type bytes found {} times; actor varint found {} times", mt_hits.len(), av_hits.len());

        // Show context around the first few mob_type occurrences (the spawn record).
        for (n, &idx) in mt_hits.iter().take(3).enumerate() {
            let start = idx.saturating_sub(24);
            let end = (idx + 40).min(plain.len());
            println!("  [mt hit {}] @{}  ...{}  [MT]{}  {}...",
                n, idx,
                hexline(&plain[start..idx]),
                hexline(&plain[idx..idx + 3]),
                hexline(&plain[idx + 3..end]));
        }
        // Also show context around first actor-varint occurrence preceded by an opcode.
        for (n, &idx) in av_hits.iter().take(2).enumerate() {
            let start = idx.saturating_sub(8);
            let end = (idx + 48).min(plain.len());
            println!("  [actor hit {}] @{}  pre={}  [ID]{}  post={}",
                n, idx,
                hexline(&plain[start..idx]),
                hexline(&plain[idx..idx + av.len()]),
                hexline(&plain[idx + av.len()..end]));
        }
        println!();
    }
}
