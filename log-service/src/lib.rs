//! The log service: turn an uploaded Evidence Slice into the fight it records.
//!
//! One route, `POST /derive`, reachable only through a service binding from the
//! a2tools.app Worker (this Worker has no public route; see wrangler.toml). The
//! site handles accounts, storage and access; this does the one thing the site
//! cannot: run the meter's own parser, compiled to wasm32, over the slice, so a
//! log shows what the packets say rather than what a client claimed.
//!
//! Stateless and deterministic: the same slice and the same build give the
//! same record, byte for byte. That is what lets a log be re-derived later, by
//! anyone with the slice and the published source.

use a2tools_dps_meter_lib::capture::evidence_slice;
use a2tools_dps_meter_lib::rederive::{derive_decoded_with, Decoded, DeriveError, Tables};
use flate2::read::GzDecoder;
use futures_util::{Stream, StreamExt};
use ruzstd::decoding::StreamingDecoder;
use worker::*;

/// The meter's own data tables, compiled in. Without the NPC table no target
/// counts as a boss and nothing is derived; without the DoT list ticks are
/// filed as direct hits. English, because the stored record is language
/// neutral apart from names, and the site translates by code.
const NPCS: &str = include_str!("../../src/data/i18n/npcs/en.json");
const SKILLS: &str = include_str!("../../src/data/i18n/skills/en.json");
const DOTS: &str = include_str!("../../src/data/dot_skill_ids.json");

/// The tables above, parsed once per isolate rather than on every request
/// (~15 ms each, twice when the first pass found nothing). Read only while
/// deriving, so every request gets the same answer as with its own copy.
static TABLES: std::sync::LazyLock<Tables> = std::sync::LazyLock::new(|| Tables::load(NPCS, SKILLS, DOTS));

/// Upper bounds, generous against real slices (a party boss fight compresses to
/// tens or hundreds of KB) and tight against anything built to make this Worker
/// work. The second is on the fight's packets as version 2 stores them (see
/// `read_slice`), so it takes or refuses a fight alike in every version.
// 16 MiB, as the site takes (was 4): a slice carries 30 minutes of the zone's
// state before the pull, which in a crowded place passed 4 MiB for a short
// fight (2026-10-17).
const MAX_GZIPPED: usize = 16 * 1024 * 1024;
// 36 MiB (was 24): a fight at a field boss with hundreds of players came to
// 30-47 MiB from meters before 2.0.74, which kept every buff change from the
// half hour before the pull. Memory, not this number, is the real bound: the
// Worker has 128 MB, and reading and deriving a 32 MiB fight (343 players)
// peaked at 77 MiB of heap, a 47 MiB one (577 players, 6 minutes) at 191 MiB
// (2026-10-21). Above this a slice is refused, not run out of memory.
const MAX_RECORD_BYTES: usize = 36 * 1024 * 1024;
// A zstd frame's window is allocated as declared, before any output: no
// bigger than the records it could hold (#131).
const MAX_ZSTD_WINDOW: u64 = MAX_RECORD_BYTES as u64;

/// The slice inside an upload: zstd from meters 2.0.67 on, gzip before, told
/// apart by their magic bytes, so every stored slice still derives. Read as it
/// is unpacked, never whole: version 3 stores bundles unpacked and is larger
/// so, but its records read back as version 2's, and only those are counted.
/// A bomb stops at the limit. None for anything that is not a slice.
fn read_slice(body: &[u8]) -> Option<Decoded> {
    if body.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        evidence_slice::decode_from(StreamingDecoder::new_with_max_window_size(body, MAX_ZSTD_WINDOW).ok()?, MAX_RECORD_BYTES)
    } else {
        evidence_slice::decode_from(GzDecoder::new(body), MAX_RECORD_BYTES)
    }
}

/// Whether what `read_slice` refused begins as a slice does: then it was
/// refused for its size, not for what it is.
fn looks_like_slice(body: &[u8]) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 4];
    let read = if body.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        StreamingDecoder::new_with_max_window_size(body, MAX_ZSTD_WINDOW).ok().is_some_and(|mut r| r.read_exact(&mut magic).is_ok())
    } else {
        GzDecoder::new(body).read_exact(&mut magic).is_ok()
    };
    read && &magic == evidence_slice::MAGIC
}

#[event(fetch)]
async fn fetch(mut req: Request, env: Env, _ctx: Context) -> Result<Response> {
    if req.method() != Method::Post || req.path() != "/derive" {
        return Response::error("not found", 404);
    }
    // Defence in depth: the binding is the real boundary, but a Worker that
    // does expensive work on request should not trust that nobody ever adds a
    // route to it by mistake.
    // Two keys may be accepted, so the key can change without a gap:
    // LOG_SERVICE_KEY and, while the callers move to a new one,
    // LOG_SERVICE_KEY_PREVIOUS. One unset or empty accepts nothing.
    let presented = req.headers().get("x-a2-service-key")?.unwrap_or_default();
    let accepted = ["LOG_SERVICE_KEY", "LOG_SERVICE_KEY_PREVIOUS"].iter().any(|name| {
        let key = env.secret(name).map(|s| s.to_string()).unwrap_or_default();
        !key.is_empty() && constant_time_eq(key.as_bytes(), presented.as_bytes())
    });
    if !accepted {
        return Response::error("forbidden", 403);
    }

    // Count actual chunks, including when Content-Length is absent or wrong.
    // A bodyless request still reaches the existing not_a_slice response.
    let body = if req.inner().body().is_none() {
        Some(Vec::new())
    } else {
        read_limited_body(req.stream()?, MAX_GZIPPED).await?
    };
    let Some(body) = body else {
        return error("too_large", "The slice is larger than any real fight.", 413);
    };
    let Some(decoded) = read_slice(&body) else {
        if looks_like_slice(&body) {
            return error("too_large", "The fight's records are larger than this service reads.", 413);
        }
        return error(
            "not_a_slice",
            "Not a gzipped or zstd-compressed Evidence Slice this build reads, or larger than any real fight.",
            422,
        );
    };

    // The names the upload says it showed, base64 of a JSON array, optional:
    // the client's own leak check, run again here, where a client cannot skip it.
    let names: Vec<String> = req
        .headers()
        .get("x-a2-names")?
        .and_then(|h| base64_decode(&h))
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .unwrap_or_default();

    match derive_decoded_with(&decoded, &TABLES) {
        Ok(fight) => {
            let leaked = evidence_slice::leaked_names(&decoded.0, &names);
            let mut json = serde_json::to_value(&fight)?;
            json["checks"]["leakedNames"] = serde_json::json!(leaked);
            Response::from_json(&json)
        }
        Err(DeriveError::NotASlice) => error("not_a_slice", "Not an Evidence Slice this build reads.", 422),
        Err(DeriveError::NothingDerived) => error("no_fight", "The slice contains no boss fight.", 422),
    }
}

async fn read_limited_body<S: Stream<Item = Result<Vec<u8>>>>(stream: S, max: usize) -> Result<Option<Vec<u8>>> {
    futures_util::pin_mut!(stream);
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if chunk.len() > max - body.len() {
            return Ok(None);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Some(body))
}

fn error(code: &str, message: &str, status: u16) -> Result<Response> {
    Ok(Response::from_json(&serde_json::json!({ "error": code, "message": message }))?
        .with_status(status))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Standard base64, padded or not. Small enough not to need a crate.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut buf, mut bits) = (0u32, 0u32);
    for c in text.trim().bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        } as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{stream, FutureExt};
    use std::cell::Cell;

    #[test]
    fn streamed_body_accepts_empty_and_exact_limit() {
        let empty = read_limited_body(stream::empty(), 4).now_or_never().unwrap().unwrap();
        assert_eq!(empty, Some(Vec::new()));
        let exact = stream::iter([Ok(vec![1, 2]), Ok(vec![]), Ok(vec![3, 4])]);
        let body = read_limited_body(exact, 4).now_or_never().unwrap().unwrap();
        assert_eq!(body, Some(vec![1, 2, 3, 4]));
    }

    #[test]
    fn streamed_body_stops_at_the_first_excess_chunk() {
        let polls = Cell::new(0);
        let chunks = stream::iter([Ok(vec![1, 2]), Ok(vec![3, 4, 5]), Ok(vec![6])])
            .inspect(|_| polls.set(polls.get() + 1));
        assert!(read_limited_body(chunks, 4).now_or_never().unwrap().unwrap().is_none());
        assert_eq!(polls.get(), 2);
        let single = stream::iter([Ok(vec![0; 5])]);
        assert!(read_limited_body(single, 4).now_or_never().unwrap().unwrap().is_none());
    }

    #[test]
    fn streamed_body_propagates_read_errors() {
        let chunks = stream::iter([Ok(vec![1]), Err(Error::RustError("read failed".into()))]);
        assert!(matches!(read_limited_body(chunks, 4).now_or_never().unwrap(), Err(Error::RustError(_))));
    }
}
