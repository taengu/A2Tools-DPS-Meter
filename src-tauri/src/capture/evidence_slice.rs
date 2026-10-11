//! The Evidence Slice — what a shared fight actually uploads.
//!
//! A `packets_*.txt` capture is the whole server-to-client game connection. We
//! checked what is in one (`tests/capture_contents.rs`) and it carries player
//! chat, GM broadcasts, names of people who were never in the party, and account
//! statistics. None of that may be uploaded, so a capture is not the artifact.
//!
//! A slice is *derived*: rebuilt from the packets the parser actually reads,
//! with every name replaced by an opaque token. Three transforms, in order:
//!
//! 1. **Time-scope** to the fight, plus a lead-in long enough to catch the party
//!    roster and identity records that name everyone.
//! 2. **Allowlist** — keep a packet only if its opcode is one the parser
//!    consumes. An allowlist and not a blocklist, because we cannot prove we
//!    stripped every chat message from a format we only partly understand, but
//!    we can prove we kept only what is on a published list.
//! 3. **Blind** — replace every known character name, in place, with a token of
//!    exactly the same byte length.
//!
//! Then a verifier re-reads the result and fails the build if any known name
//! survived. That check is the point: the allowlist and the blinder are both
//! best-effort pattern work, and neither is trusted on its own.
//!
//! **Why same-length tokens.** Names live inside variable-width records whose
//! tails the parser re-acquires by searching (see `parse_party_roster_at`).
//! Rewriting a name to a different length would shift every offset after it and
//! silently change the numbers. A same-length token leaves every length prefix,
//! varint and anchor exactly where it was, so the parser sees an identical
//! stream that happens to be full of strangers.
//!
//! The output is an ordinary framed packet stream — bundles decompressed, so the
//! blinder can reach inside them — which means the server replays it through the
//! same `consume_stream` as a live capture. No second parser, nothing to drift.

use std::collections::{HashMap, HashSet};

use sha2::{Digest, Sha256};

use super::framing::{self, FrameKind, MAX_BUNDLE_DEPTH};
use super::packet_accumulator::PacketAccumulator;

/// How far before the fight to keep packets. The party roster (`02 97`) and the
/// identity records that name each entity arrive on party changes and zone
/// entry, not on damage, so a slice that started at the first hit would upload a
/// fight full of unnamed `#id` rows.
pub const LEAD_IN_MS: i64 = 60_000;
/// How far past the last hit to keep. Covers the death packet and the final HP
/// updates that land just after.
pub const TAIL_MS: i64 = 15_000;
/// How far before the lead-in to keep *state* packets: spawns, identity, the
/// party roster, zone changes, summon ownership. Never damage or HP ticks.
///
/// Measured, not guessed: on real captures a boss's spawn record (which is the
/// only thing that says what the target IS, and so whether it is a boss at
/// all) and the roster that carries the dungeon id arrive when the party enters
/// the room, routinely minutes before the pull. A slice without them replays to
/// the right damage on an anonymous mob code 0, which the service cannot file
/// under any boss.
pub const PRELUDE_MS: i64 = 30 * 60_000;

/// What to keep from a stretch of capture.
#[derive(Clone, Copy, PartialEq)]
enum Keep {
    /// Every allowlisted opcode: the fight window.
    All,
    /// Allowlisted opcodes that establish who and what, not what happened.
    State,
}

/// Damage the parser recovers from inside OTHER packets.
///
/// `try_parse_embedded_damage_packet` scans any packet that is not itself a
/// `04 38` for a `04 38` record inside it, and on real captures that is a large
/// share of the damage: measured at 19% of a boss fight. The allowlist drops
/// those host packets, and it must, because a host can carry anything. So the
/// damage record alone is lifted out: from its `04 38` to at most this many
/// bytes, blinded and verified like everything else, and re-wrapped in a
/// neutral host (below) so the replay takes the same embedded path, with the
/// same trust gate and the same de-duplication, as the live meter did.
const EMBEDDED_KEEP: usize = 160;

/// The host a lifted record travels in: an opcode the parser has no handler
/// for, so the only thing it can do with the packet is the embedded scan.
pub const LIFTED_HOST: [u8; 2] = [0xE5, 0xA2];

/// The longest stretch kept after an embedded spawn whose own length cannot be
/// read. A spawn record runs to a few hundred bytes (a boss's, 230).
const EMBEDDED_SPAWN_KEEP: usize = 512;
/// Bounds on an embedded spawn's own length, when it can be read: the fixed
/// fields alone are longer than the first, and a field boss's record (1,037
/// bytes) is well inside the second.
const EMBEDDED_SPAWN_MIN: usize = 16;
const EMBEDDED_SPAWN_MAX: usize = 4096;

/// Records the parser recovers from inside other packets, lifted out.
///
/// Damage (`04 38`, see `EMBEDDED_KEEP`) in the fight window, and spawns (`40`
/// `41` `44` `45 36`) always. The live meter scans every packet's raw bytes for
/// spawns as well as damage, and some arrive only that way: the spawns that say
/// what a boss IS came inside a `00 36` container in one capture and a 13 KB
/// packet in another, so the slice replayed the right damage on a target with
/// no mob code, which is no boss, and derived nothing (2026-10-03: Guardian
/// Captain Raur, Glassvein, Decaying Durvati, Kernon of the West).
fn lift_embedded(packet: &[u8], keep: Keep) -> Vec<Vec<u8>> {
    let damage_at = |i: usize| packet[i] == 0x04 && packet[i + 1] == 0x38;
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for i in 2..packet.len().saturating_sub(1) {
        if damage_at(i) {
            if keep == Keep::All {
                spans.push((i, packet.len().min(i + EMBEDDED_KEEP)));
            }
        } else if let Some(end) = embedded_spawn_end(packet, i) {
            // A spawn's span stops short of any damage record after it, so in
            // the lead-in no damage rides along, and in the fight no record is
            // lifted twice.
            let end = (i + 2..end).find(|&j| j + 1 < packet.len() && damage_at(j)).unwrap_or(end);
            spans.push((i, end));
        }
    }

    // Records close together are lifted as ONE span, not one copy each. The
    // replay's embedded scan walks the whole lifted packet, so a per-record
    // copy that happened to contain the next record parsed it again, under a
    // de-duplication key (64 bytes from the `04 38`) cut short by the copy's
    // end, which the live parser never produced: a double count, measured at
    // +2.4% on a real fight. As spans, every record sees exactly the bytes it
    // saw live and no record appears twice.
    spans.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (from, to) in spans {
        match merged.last_mut() {
            Some((_, end)) if from < *end => *end = (*end).max(to),
            _ => merged.push((from, to)),
        }
    }

    merged
        .into_iter()
        .filter_map(|(from, to)| {
            let mut body = Vec::with_capacity(2 + to - from);
            body.extend_from_slice(&LIFTED_HOST);
            body.extend_from_slice(&packet[from..to]);
            frame_packet(&body)
        })
        .collect()
}

/// Where the spawn record whose opcode is at `i` ends, if one is there, by the
/// rules the live scan (`scan_for_embedded_40_36`) applies: a spawn opcode not
/// preceded by `00`, then an entity id in range. Its end is its own frame's,
/// when the length varint in front of it reads as one that fits; otherwise a
/// fixed stretch.
fn embedded_spawn_end(packet: &[u8], i: usize) -> Option<usize> {
    if packet[i + 1] != 0x36 || !matches!(packet[i], 0x40 | 0x41 | 0x44 | 0x45) || packet[i - 1] == 0x00 {
        return None;
    }
    let id = super::stream_processor::read_varint(packet, i + 2);
    if id.length <= 0 || !(100..=9_999_999).contains(&id.value) {
        return None;
    }
    // Longest length first: a two-byte length `8d 08` ends in a byte that
    // also reads as a one-byte length (8), which cut a field boss's 1,037-byte
    // spawn to four bytes. No spawn record is shorter than its fixed fields.
    let framed = (1..=3usize).rev().find_map(|n| {
        let at = i.checked_sub(n)?;
        let len = super::stream_processor::read_varint(packet, at);
        if len.length != n as i32 {
            return None;
        }
        let end = at + framing::frame_size(len.value, len.length)?;
        (end >= i + EMBEDDED_SPAWN_MIN && end <= packet.len() && end - i <= EMBEDDED_SPAWN_MAX)
            .then_some(end)
    });
    Some(framed.unwrap_or_else(|| packet.len().min(i + EMBEDDED_SPAWN_KEEP)))
}

/// The compact-skill context a packet declares, lifted out on its own.
///
/// Inside a bundle, one packet (a cast, not on the allowlist) carries the
/// marker `08 3B|3D 38 00 00` followed by an actor id and a skill code, and
/// `extract_pending_compact_skill_context` holds that for the rest of the
/// bundle so a later compact `04 38` can be split into the right skill and
/// damage. Dropping the cast silently changes those numbers. What is kept is
/// only marker..skill: an entity id and a skill code, nothing else from the
/// host. The walk mirrors the extractor's; whether the bytes are a known skill
/// is left to the replay, exactly as the live parser decides it.
fn lift_compact_context(packet: &[u8]) -> Option<Vec<u8>> {
    let li = super::stream_processor::read_varint(packet, 0);
    if li.length <= 0 || li.length as usize >= packet.len() {
        return None;
    }
    let body = &packet[li.length as usize..];
    let marker = (0..body.len().saturating_sub(4)).find(|&i| {
        body[i] == 0x08
            && (body[i + 1] == 0x3B || body[i + 1] == 0x3D)
            && body[i + 2] == 0x38
            && body[i + 3] == 0x00
            && body[i + 4] == 0x00
    })?;
    let opcode = (marker + 5..body.len()).find(|&i| body[i] == 0x38)?;
    if opcode + 2 >= body.len() {
        return None;
    }
    let actor = super::stream_processor::read_varint(body, opcode + 1);
    if actor.length <= 0 || actor.value < 100 {
        return None;
    }
    let skill_offset = opcode + 1 + actor.length as usize + 1;
    if skill_offset + 3 > body.len() {
        return None;
    }
    let end = (skill_offset + 4).min(body.len());
    let mut lifted = Vec::with_capacity(2 + end - marker);
    lifted.extend_from_slice(&LIFTED_HOST);
    lifted.extend_from_slice(&body[marker..end]);
    frame_packet(&lifted)
}

/// Prefix a body with the game's length varint (see `framing`).
fn frame_packet(body: &[u8]) -> Option<Vec<u8>> {
    let mut out = encode_varint(framing::length_value(body.len()));
    out.extend_from_slice(body);
    Some(out)
}

/// Opcodes that report what happened rather than who is there. Kept only in
/// the fight window, so the prelude cannot carry another fight's numbers.
const EVENT_OPCODES: &[[u8; 2]] = &[[0x04, 0x38], [0x05, 0x38], [0x1B, 0x92]];

/// The opcodes the parser reads, and nothing else.
///
/// Each entry is a leading opcode pair as it appears immediately after a
/// packet's length varint. Keeping the citation next to the bytes is deliberate:
/// this list has to track `stream_processor.rs`, and the acceptance test
/// (`evidence_slice_replays_to_the_same_numbers`) is what proves it still does —
/// if an opcode the parser needs is missing here, the replayed slice produces
/// different damage and the test fails.
pub const ALLOWED_OPCODES: &[(&[u8; 2], &str)] = &[
    (&[0x04, 0x38], "damage"),
    (&[0x05, 0x38], "damage over time"),
    (&[0x1B, 0x92], "hp/mp update"),
    (&[0x04, 0x8D], "summon ownership"),
    (&[0x23, 0x36], "zone change"),
    (&[0x21, 0x36], "map load"),
    (&[0x41, 0x36], "death / spawn"),
    (&[0x42, 0x36], "death (post 2026-06 opcode shift)"),
    (&[0x40, 0x36], "summon spawn"),
    (&[0x44, 0x36], "player spawn"),
    (&[0x45, 0x36], "player spawn"),
    (&[0x33, 0x36], "self identity"),
    (&[0x02, 0x97], "party roster"),
    // Buffs and debuffs, for the log's Buffs timeline: entity ids, abnormal
    // and skill ids, timings and a position, no names (capture/abnormal.rs).
    (&[0x2A, 0x38], "buff/debuff added"),
    (&[0x2B, 0x38], "buff/debuff changed"),
    (&[0x2C, 0x38], "buff/debuff removed"),
];

/// One captured buffer, as the packet logger recorded it.
#[derive(Debug, Clone)]
pub struct CapturedPacket {
    pub captured_at_ms: i64,
    /// The TCP stream this arrived on — the packet logger writes
    /// `Client:<client port>:<server port>` (older logs `Client:<server port>`).
    ///
    /// Required, and it is not bookkeeping: a captured buffer is a TCP segment,
    /// not a packet. Packets straddle segments, so framing a segment on its own
    /// starts mid-packet and the resync walk *invents* packets out of the middle
    /// of a real one's payload. Doing that produced damage rows at 250% of the
    /// truth before this field existed. Bytes have to be reassembled per stream
    /// first, exactly as the live meter does.
    pub stream: String,
    pub bytes: Vec<u8>,
}

/// Who to blind. Maps a character name to the roster id it belongs to, so the
/// token is stable across every packet the name appears in — the parser joins
/// the roster to in-world entities *by name*, and that join has to keep working
/// after blinding or the slice derives nothing.
pub type NameMap = HashMap<String, u64>;

#[derive(Debug, Default, Clone)]
pub struct SliceStats {
    pub packets_seen: usize,
    pub packets_kept: usize,
    pub bytes_seen: usize,
    pub bytes_kept: usize,
    pub names_blinded: usize,
    pub bundles_expanded: usize,
}

#[derive(Debug)]
pub struct EvidenceSlice {
    /// Framed packets, blinded, in capture order, each with its offset in
    /// milliseconds from the start of the fight. Relative so the slice carries
    /// no wall-clock and no session length.
    pub records: Vec<(i32, Vec<u8>)>,
    /// token -> roster id, so the server can map a blinded name back to a
    /// participant without ever having been told the name.
    pub blind_map: HashMap<String, u64>,
    pub stats: SliceStats,
}

#[derive(Debug)]
pub enum SliceError {
    /// A name survived blinding. The slice is discarded rather than uploaded.
    NameLeaked(usize),
    /// Nothing was left after filtering — there is no evidence to send.
    Empty,
}

impl std::fmt::Display for SliceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Deliberately reports a length, not the name: this string ends up
            // in logs and error toasts, and a privacy failure should not be
            // reported by printing the thing that leaked.
            SliceError::NameLeaked(len) => write!(
                f,
                "a character name ({len} bytes) survived blinding; slice discarded"
            ),
            SliceError::Empty => write!(f, "no allowlisted packets in the fight window"),
        }
    }
}

/// A random key, one per slice: mixed into every token, then dropped. Never
/// stored or sent, so nobody can hash a list of names or roster ids and look
/// them up in a slice, or match one person's tokens across slices.
pub type SliceKey = [u8; 32];

/// A token of exactly `len` bytes, derived from the roster id when we have one
/// and from the name otherwise, under the slice's key.
///
/// Hex, so it is always ASCII and always valid UTF-8 at any truncation — a name
/// is `<u8 len><utf8>`, and emitting a token that split a multi-byte character
/// would leave the parser reading invalid UTF-8 where it used to read a name.
fn token_for(key: &SliceKey, name: &str, dbid: u64, len: usize) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key);
    if dbid != 0 {
        hasher.update(b"a2es-dbid\x00");
        hasher.update(dbid.to_le_bytes());
    } else {
        // No roster entry — a bystander, or someone who joined late. Hash the
        // name so the token is still stable everywhere it appears.
        hasher.update(b"a2es-name\x00");
        hasher.update(name.as_bytes());
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for b in digest.iter() {
        hex.push_str(&format!("{b:02x}"));
    }
    while hex.len() < len {
        hex.push('0');
    }
    hex.truncate(len);
    hex
}

/// Shortest and longest byte length treated as a name. The roster parse rejects
/// anything outside 1..=40, so this matches what the game itself will accept.
const MIN_NAME_BYTES: usize = 2;
const MAX_NAME_BYTES: usize = 40;

/// Replaces names with same-length tokens.
///
/// Two passes, and the second one is why this is a struct rather than a
/// function. Blinding only the names the meter resolved is not enough: a capture
/// also carries the names of players who merely stood nearby, and legion names,
/// and those are never in any map the meter builds. `a2t-inspect --strings` on a
/// slice built that way showed a bystander's name and a recruitment message
/// still in the clear.
///
/// So pass one replaces every known name wherever its bytes appear, and pass two
/// walks the packet for anything *shaped* like a name — a length byte followed
/// by that many bytes of plausible text — and blinds that too. Pass two is what
/// makes the guarantee "no character names", rather than "none of the names we
/// happened to recognise".
///
/// Before both, the name fields: every place a record the parser reads holds
/// a name (`stream_processor::name_fields`), blinded whether or not the name
/// is known. That is the only pass for a name of one byte: searched for, its
/// byte is everywhere. A known name "A" blinded every `41` byte in a slice,
/// spawn opcodes too, and the slice derived nothing (Daevalog, 2026-10-06).
/// Pass one leaves one-byte names to the name fields, and looks for a name
/// under `SHORT_NAME_BYTES` only after its length byte, as the verifier does.
struct Blinder {
    /// The slice's key (see `SliceKey`).
    key: SliceKey,
    /// Pass one: name bytes -> token bytes, longest first, for names of
    /// `MIN_NAME_BYTES` or more.
    known: Vec<(Vec<u8>, Vec<u8>)>,
    /// Every known name, of any length -> its token: what a name field gets.
    tokens: HashMap<Vec<u8>, Vec<u8>>,
    /// Every token written, so no pass blinds one again.
    known_tokens: HashSet<Vec<u8>>,
    /// Tokens minted for names nobody resolved, found in a name field or by
    /// shape, for the blind map. These have no roster id — we never learned
    /// who they were, which is the point.
    discovered: HashMap<String, u64>,
    /// One-byte names and the one-byte tokens given (see `one_byte_token`).
    one_byte_taken: HashSet<u8>,
    one_byte_given: HashMap<(String, u64), String>,
}

impl Blinder {
    fn new(key: SliceKey, ordered: &[(&String, &u64)]) -> Self {
        let mut blinder = Self::exempting(HashSet::new());
        blinder.key = key;
        blinder.one_byte_taken = ordered.iter().filter(|(n, _)| n.len() == 1).map(|(n, _)| n.as_bytes()[0]).collect();
        for (name, dbid) in ordered {
            let raw = name.as_bytes();
            if raw.is_empty() {
                continue;
            }
            let token = blinder.token(name, **dbid, 0);
            debug_assert_eq!(token.len(), raw.len(), "token must not change byte length");
            blinder.known_tokens.insert(token.as_bytes().to_vec());
            blinder.tokens.insert(raw.to_vec(), token.clone().into_bytes());
            if raw.len() >= MIN_NAME_BYTES {
                blinder.known.push((raw.to_vec(), token.into_bytes()));
            }
        }
        blinder
    }

    /// A blinder that knows no names and leaves `tokens` alone.
    fn exempting(tokens: HashSet<Vec<u8>>) -> Self {
        Self {
            // Only counts: the tokens it would mint are never written.
            key: [0; 32],
            known: Vec::new(),
            tokens: HashMap::new(),
            known_tokens: tokens,
            discovered: HashMap::new(),
            one_byte_taken: HashSet::new(),
            one_byte_given: HashMap::new(),
        }
    }

    /// The token a known name got.
    fn token_of(&self, name: &str) -> Option<&[u8]> {
        self.tokens.get(name.as_bytes()).map(Vec::as_slice)
    }

    /// The token for `name`. `salt` above 0 asks for another one (see `mint`).
    fn token(&mut self, name: &str, dbid: u64, salt: u32) -> String {
        let len = name.len();
        if len == 1 {
            let key = (name.to_string(), dbid);
            if salt == 0
                && let Some(token) = self.one_byte_given.get(&key)
            {
                return token.clone();
            }
            let token = one_byte_token(&self.key, name, dbid, salt, &self.one_byte_taken);
            self.one_byte_taken.insert(token.as_bytes()[0]);
            if salt == 0 {
                self.one_byte_given.insert(key, token.clone());
            }
            return token;
        }
        match salt {
            0 => token_for(&self.key, name, dbid, len),
            _ => token_for(&self.key, &format!("{name}\0{salt}"), 0, len),
        }
    }

    fn blind(&mut self, buf: &mut [u8]) -> usize {
        let mut replaced = 0;
        if !is_event(buf) {
            let fields = super::stream_processor::name_fields(buf);
            replaced += self.blind_fields(buf, &fields);
        }
        replaced += self.blind_known(buf);
        replaced += self.blind_name_shaped(buf);
        replaced
    }

    /// Pass zero: every name field, at any length. A known name gets its
    /// token, any other name one of its own. Nothing else in the packet
    /// changes.
    fn blind_fields(&mut self, buf: &mut [u8], fields: &[super::stream_processor::NameField]) -> usize {
        let mut replaced = 0;
        let mut done_to = 0;
        for field in fields {
            let range = field.range.clone();
            if range.is_empty() || range.start < done_to || range.end > buf.len() {
                continue;
            }
            let name = &buf[range.clone()];
            if self.known_tokens.contains(name) {
                continue;
            }
            let token = match self.tokens.get(name) {
                Some(token) => token.clone(),
                None => {
                    let Ok(text) = std::str::from_utf8(name).map(str::to_string) else { continue };
                    let token = self.mint(buf, range.clone(), &text);
                    self.known_tokens.insert(token.clone().into_bytes());
                    self.discovered.insert(token.clone(), 0);
                    token.into_bytes()
                }
            };
            buf[range.clone()].copy_from_slice(&token);
            done_to = range.end;
            replaced += 1;
        }
        replaced
    }

    /// Write a token for `text`, a name nobody resolved, over `buf[range]`,
    /// and return it. A token must not spell a known name with the bytes
    /// around it: "13b87050fe08" before a `44` byte spelled the player name
    /// "8D".
    fn mint(&mut self, buf: &mut [u8], range: std::ops::Range<usize>, text: &str) -> String {
        let mut salt = 0u32;
        loop {
            let token = self.token(text, 0, salt);
            buf[range.clone()].copy_from_slice(token.as_bytes());
            if salt == 15 || !self.spells_known_name(buf, range.start, range.end) {
                return token;
            }
            salt += 1;
        }
    }

    /// Pass one: every known name of `SHORT_NAME_BYTES` or more wherever it
    /// appears, length-prefixed or not; a shorter one only after its length
    /// byte, where the game puts a name (see `name_in`). Two bytes turn up in
    /// ordinary data by chance, and rewriting them there changed that data.
    /// A name of one byte is left to the name fields.
    fn blind_known(&self, buf: &mut [u8]) -> usize {
        let mut replaced = 0;
        for (needle, token) in &self.known {
            if needle.len() > buf.len() {
                continue;
            }
            let short = needle.len() < SHORT_NAME_BYTES;
            let mut i = 0;
            while i + needle.len() <= buf.len() {
                if &buf[i..i + needle.len()] == needle.as_slice()
                    && (!short || (i > 0 && buf[i - 1] as usize == needle.len()))
                {
                    buf[i..i + needle.len()].copy_from_slice(token);
                    replaced += 1;
                    i += needle.len();
                } else {
                    i += 1;
                }
            }
        }
        replaced
    }

    /// Pass two: anything shaped like `<u8 len><len bytes of text>`.
    ///
    /// Not in a packet's header, nor in the header of a packet embedded in it.
    /// A spawn opcode is two printable bytes (`40 36` is "@6") and a length of
    /// 256 to 383 bytes ends in `02`, so `<len> 40 36` reads as a two-letter
    /// name: blinding it turned a boss's spawn into nothing the parser knows,
    /// and its fight into one on an unknown mob, which no slice could file
    /// (Decaying Durvati, 2026-02 capture).
    fn blind_name_shaped(&mut self, buf: &mut [u8]) -> usize {
        let mut replaced = 0;
        // Every buffer handed to the blinder is one framed packet: skip its
        // length and opcode.
        let header = super::stream_processor::read_varint(buf, 0);
        if is_event(buf) {
            return 0;
        }
        let mut i = if header.length > 0 { header.length as usize + 2 } else { 0 };
        while i < buf.len() {
            let len = buf[i] as usize;
            if !(MIN_NAME_BYTES..=MAX_NAME_BYTES).contains(&len) || i + 1 + len > buf.len() {
                i += 1;
                continue;
            }
            let span = &buf[i + 1..i + 1 + len];
            if !looks_like_text(span) || self.known_tokens.contains(span) || embedded_header(buf, i) {
                i += 1;
                continue;
            }
            // Safe: `looks_like_text` already required valid UTF-8.
            let text = std::str::from_utf8(span).unwrap().to_string();
            let token = self.mint(buf, i + 1..i + 1 + len, &text);
            self.discovered.insert(token, 0);
            replaced += 1;
            i += 1 + len;
        }
        replaced
    }

    /// Does any known name overlap `buf[from..to]`?
    fn spells_known_name(&self, buf: &[u8], from: usize, to: usize) -> bool {
        self.known.iter().any(|(name, _)| {
            let n = name.len();
            let start = (from + 1).saturating_sub(n);
            let end = (to + n - 1).min(buf.len());
            end >= start + n && buf[start..end].windows(n).any(|w| w == name.as_slice())
        })
    }
}

/// Damage, damage over time and HP updates are ids and numbers only: no
/// pass looks in them for names. Scanning them blinded skill ids that read as
/// text: `02 | 50 77 f6 00` (Water Spirit: Ice Chain) is "Pw".
fn is_event(packet: &[u8]) -> bool {
    let header = super::stream_processor::read_varint(packet, 0);
    let o = header.length.max(0) as usize;
    header.length > 0 && packet.len() >= o + 2 && EVENT_OPCODES.contains(&[packet[o], packet[o + 1]])
}

/// A one-byte token: a letter, so the parser still reads it as a name and
/// joins the player's records by it (a hex digit is no name to it). Never
/// the name itself, another one-byte name, or a token already given, so two
/// players never share one.
fn one_byte_token(key: &SliceKey, name: &str, dbid: u64, salt: u32, taken: &HashSet<u8>) -> String {
    const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut hasher = Sha256::new();
    hasher.update(key);
    hasher.update(b"a2es-one\x00");
    match dbid {
        0 => hasher.update(name.as_bytes()),
        _ => hasher.update(dbid.to_le_bytes()),
    }
    hasher.update(salt.to_le_bytes());
    let digest = hasher.finalize();
    let free = |b: &u8| !taken.contains(b) && name.as_bytes() != [*b];
    let pick = digest.iter().map(|d| LETTERS[*d as usize % LETTERS.len()]).find(free);
    let pick = pick.or_else(|| LETTERS.iter().copied().find(free)).unwrap_or(b'q');
    (pick as char).to_string()
}

/// Each packet of a record, bundles opened: what the parser reads one at a
/// time.
fn packets_in(record: &[u8]) -> Vec<Vec<u8>> {
    fn walk_into(buffer: &[u8], out: &mut Vec<Vec<u8>>, depth: usize, top: bool) {
        if depth > MAX_BUNDLE_DEPTH {
            return;
        }
        let frames = if top { framing::walk(buffer).frames } else { framing::walk_inner(buffer).frames };
        for frame in frames {
            match frame.kind {
                FrameKind::Packet => out.push(frame.bytes(buffer).to_vec()),
                FrameKind::Bundle => {
                    if let Some(inner) = framing::decompress_bundle(frame.payload(buffer)) {
                        walk_into(&inner, out, depth + 1, false);
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    walk_into(record, &mut out, 0, true);
    out
}

/// What every name field of the records holds (event packets aside): where
/// the leak checks look for names of any length, a one-byte name too. Where a
/// record holds a name, a chance match is no concern.
fn name_field_contents(records: &[(i32, Vec<u8>)]) -> HashSet<Vec<u8>> {
    let mut out = HashSet::new();
    for (_, record) in records {
        for packet in packets_in(record) {
            if is_event(&packet) {
                continue;
            }
            for field in super::stream_processor::name_fields(&packet) {
                out.insert(packet[field.range].to_vec());
            }
        }
    }
    out
}

/// Is `buf[i]` the last byte of an embedded packet's length, followed by a
/// spawn or identity opcode and an entity id, rather than a two-byte name?
/// All three together: a length of two or more bytes (the byte before has its
/// continuation bit), one of the `36` opcodes the parser scans for inside
/// other packets, and an id in the range the parser accepts.
fn embedded_header(buf: &[u8], i: usize) -> bool {
    if buf[i] != 2 || i == 0 || buf[i - 1] & 0x80 == 0 || i + 3 >= buf.len() {
        return false;
    }
    if buf[i + 2] != 0x36 || !matches!(buf[i + 1], 0x23 | 0x33 | 0x40 | 0x41 | 0x42 | 0x44 | 0x45) {
        return false;
    }
    let id = super::stream_processor::read_varint(buf, i + 3);
    id.length > 0 && (0..=9_999_999).contains(&id.value)
}

/// Is this run plausibly a name or other human-readable string?
///
/// Deliberately broad. A false positive costs a field of binary data getting
/// overwritten with hex of the same length, which the acceptance test would
/// catch as drifting damage. A false negative costs someone's name being
/// uploaded, which nothing would catch.
fn looks_like_text(span: &[u8]) -> bool {
    let Ok(s) = std::str::from_utf8(span) else {
        return false;
    };
    let mut letters = 0;
    for c in s.chars() {
        if c.is_control() {
            return false;
        }
        if c.is_alphanumeric() {
            letters += 1;
        }
    }
    // At least half the characters being letters or digits rules out runs of
    // punctuation that happen to decode. One character can be a name ("é",
    // "あ"), so it is blinded; the skill ids that read as one letter sit in
    // damage records, which are not scanned at all.
    // A short run must be letters or digits only: Water Bomb (16001105,
    // `51 28 f4 00`) after a `02` byte reads "Q(", half a letter.
    let chars = s.chars().count();
    letters * 2 >= chars && (chars > 3 || letters == chars)
}

/// Every byte of a record in the clear, bundles decompressed.
///
/// This is what a reader actually has to inspect: the verifier below, and
/// `a2t-inspect`, both need to see through the compression rather than take a
/// compressed buffer's silence as evidence of anything.
pub fn expand(record: &[u8]) -> Vec<u8> {
    fn walk_into(buffer: &[u8], out: &mut Vec<u8>, depth: usize, top: bool) {
        if depth > MAX_BUNDLE_DEPTH {
            return;
        }
        let frames = if top {
            framing::walk(buffer).frames
        } else {
            framing::walk_inner(buffer).frames
        };
        for frame in frames {
            match frame.kind {
                FrameKind::Packet => out.extend_from_slice(frame.bytes(buffer)),
                FrameKind::Bundle => {
                    if let Some(inner) = framing::decompress_bundle(frame.payload(buffer)) {
                        walk_into(&inner, out, depth + 1, false);
                    }
                }
            }
        }
    }
    let mut out = Vec::with_capacity(record.len() * 2);
    walk_into(record, &mut out, 0, true);
    out
}

/// Name-shaped runs a slice still carries that are not among the tokens it
/// declares: what pass two of the blinder would replace in it now.
///
/// The server's check on what a client sent. The client's own verifier only
/// catches the names that client knew, and only if that client runs it: a
/// fork or a broken build could upload a slice with names in the clear, and
/// the service would store it. Every packet is put through the same pass two
/// the meter applies before uploading, with every declared token exempt, so a
/// slice blinded by this meter's rules (or by any older, broader ones) counts
/// zero. Nothing is changed; the count is the answer.
pub fn unblinded_names(records: &[(i32, Vec<u8>)], blind_map: &HashMap<String, u64>) -> usize {
    fn walk(buffer: &[u8], top: bool, depth: usize, blinder: &mut Blinder, found: &mut usize) {
        if depth > MAX_BUNDLE_DEPTH {
            return;
        }
        let frames = if top { framing::walk(buffer).frames } else { framing::walk_inner(buffer).frames };
        for frame in frames {
            match frame.kind {
                FrameKind::Packet => {
                    let mut packet = frame.bytes(buffer).to_vec();
                    *found += blinder.blind_name_shaped(&mut packet);
                }
                FrameKind::Bundle => {
                    if let Some(inner) = framing::decompress_bundle(frame.payload(buffer)) {
                        walk(&inner, false, depth + 1, blinder, found);
                    }
                }
            }
        }
    }
    let mut blinder = Blinder::exempting(blind_map.keys().map(|t| t.as_bytes().to_vec()).collect());
    let mut found = 0;
    for (_, record) in records {
        walk(record, true, 0, &mut blinder, &mut found);
    }
    found
}

/// How many of `names` appear anywhere in the slice, bundles decompressed:
/// the client's own verifier, run again on the server with the names the
/// upload says it showed.
pub fn leaked_names(records: &[(i32, Vec<u8>)], names: &[String]) -> usize {
    let plaintext: Vec<Vec<u8>> = records.iter().map(|(_, p)| expand(p)).collect();
    let fields = name_field_contents(records);
    names
        .iter()
        .map(|n| n.as_bytes())
        .filter(|n| !n.is_empty())
        .filter(|n| fields.contains(*n) || plaintext.iter().any(|buf| name_in(buf, n)))
        .count()
}

/// Names this short are only looked for as the game sends a name, after its
/// length byte. Two bytes match ordinary data by chance: "Jo" or "Mo" turns up
/// 8-23 times in 3.8 MB of a party's traffic, so one player named that, even
/// a bystander, failed every slice after as a leak (2026-10-07).
const SHORT_NAME_BYTES: usize = 4;

/// Whether `name` is in `buf`: anywhere, or for a short name, length-prefixed.
/// Never for a name of one byte: `01 41` is "A" after its length byte, and
/// also the end of a frame length `8f 01` before a spawn `41 36`. A one-byte
/// name is looked for in the name fields only (`name_field_contents`).
fn name_in(buf: &[u8], name: &[u8]) -> bool {
    if name.len() < MIN_NAME_BYTES {
        return false;
    }
    if name.len() >= SHORT_NAME_BYTES {
        return buf.len() >= name.len() && buf.windows(name.len()).any(|w| w == name);
    }
    let needle: Vec<u8> = std::iter::once(name.len() as u8).chain(name.iter().copied()).collect();
    buf.len() >= needle.len() && buf.windows(needle.len()).any(|w| w == needle.as_slice())
}

/// Is this packet one the parser reads (and, in the prelude, a state packet)?
fn is_allowed(packet: &[u8], keep: Keep) -> bool {
    let li = super::stream_processor::read_varint(packet, 0);
    if li.length <= 0 {
        return false;
    }
    let o = li.length as usize;
    if o + 1 >= packet.len() {
        return false;
    }
    let op = [packet[o], packet[o + 1]];
    if keep == Keep::State && EVENT_OPCODES.contains(&op) {
        return false;
    }
    ALLOWED_OPCODES.iter().any(|(allowed, _)| **allowed == op)
}

/// Encode a length varint the way `read_varint` decodes one: seven bits per
/// byte, low group first, high bit set on every byte but the last.
fn encode_varint(mut value: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(3);
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

/// Re-wrap a filtered inner stream as an `FF FF` bundle:
/// `<varint len> FF FF <u32 decompressed_size> <lz4 block>`.
fn rewrap_bundle(inner: &[u8]) -> Option<Vec<u8>> {
    let mut payload = vec![0xFF, 0xFF];
    payload.extend_from_slice(&(inner.len() as u32).to_le_bytes());
    payload.extend_from_slice(&lz4_flex::compress(inner));
    frame_packet(&payload)
}

/// Filter and blind the packets inside a decompressed bundle, returning the
/// inner stream to re-compress. Nested bundles are inlined into their parent.
fn filter_bundle_inner(
    buffer: &[u8],
    blinder: &mut Blinder,
    stats: &mut SliceStats,
    depth: usize,
    keep: Keep,
) -> Vec<u8> {
    let mut out = Vec::new();
    if depth > MAX_BUNDLE_DEPTH {
        return out;
    }
    for frame in framing::walk_inner(buffer).frames {
        match frame.kind {
            FrameKind::Packet => {
                stats.packets_seen += 1;
                stats.bytes_seen += frame.len();
                let mut packet = frame.bytes(buffer).to_vec();
                if !is_allowed(&packet, keep) {
                    // Context first: the live parser extracts it from a packet
                    // before parsing that same packet. It only matters to damage.
                    let context = if keep == Keep::All { lift_compact_context(&packet) } else { None };
                    for mut lifted in context.into_iter().chain(lift_embedded(&packet, keep)) {
                        stats.names_blinded += blinder.blind(&mut lifted);
                        stats.bytes_kept += lifted.len();
                        out.extend_from_slice(&lifted);
                    }
                    continue;
                }
                stats.names_blinded += blinder.blind(&mut packet);
                stats.packets_kept += 1;
                stats.bytes_kept += packet.len();
                out.extend_from_slice(&packet);
            }
            FrameKind::Bundle => {
                if let Some(nested) = framing::decompress_bundle(frame.payload(buffer)) {
                    stats.bundles_expanded += 1;
                    out.extend_from_slice(&filter_bundle_inner(&nested, blinder, stats, depth + 1, keep));
                }
            }
        }
    }
    out
}

/// Frame a reassembled stream buffer and emit the records worth keeping.
///
/// Bundles are kept **as bundles**, not flattened. That is not a size decision:
/// `pending_compact_skill_context` is set by one packet inside a bundle and read
/// by a later one in the same bundle (`stream_processor.rs:178` and `:1940`), and
/// it exists only for the duration of one `unwrap_bundle` call. Flattening a
/// bundle into standalone packets silently drops that context and loses every
/// compact-form damage record that depended on it.
///
/// Returns how many bytes were consumed, so the caller retains the trailing
/// fragment for the next segment.
fn filter_stream(
    buffer: &[u8],
    blinder: &mut Blinder,
    out: &mut Vec<Vec<u8>>,
    stats: &mut SliceStats,
    keep: Keep,
) -> usize {
    let walk = framing::walk(buffer);
    for frame in &walk.frames {
        match frame.kind {
            FrameKind::Packet => {
                stats.packets_seen += 1;
                stats.bytes_seen += frame.len();
                let mut packet = frame.bytes(buffer).to_vec();
                if !is_allowed(&packet, keep) {
                    for mut lifted in lift_embedded(&packet, keep) {
                        stats.names_blinded += blinder.blind(&mut lifted);
                        stats.bytes_kept += lifted.len();
                        out.push(lifted);
                    }
                    continue;
                }
                stats.names_blinded += blinder.blind(&mut packet);
                stats.packets_kept += 1;
                stats.bytes_kept += packet.len();
                out.push(packet);
            }
            FrameKind::Bundle => {
                let Some(decompressed) = framing::decompress_bundle(frame.payload(buffer)) else {
                    continue;
                };
                stats.bundles_expanded += 1;
                let inner = filter_bundle_inner(&decompressed, blinder, stats, 1, keep);
                if inner.is_empty() {
                    continue;
                }
                if let Some(bundle) = rewrap_bundle(&inner) {
                    out.push(bundle);
                }
            }
        }
    }
    walk.consumed
}

/// Build a slice for one fight.
///
/// `names` should carry every character name the meter resolved during the
/// fight, roster members and otherwise — anything absent from it is a name the
/// blinder cannot see, and the verifier cannot catch either.
///
/// `key` must be fresh random bytes for each slice (see `SliceKey`).
pub fn build(
    packets: &[CapturedPacket],
    fight_start_ms: i64,
    fight_end_ms: i64,
    names: &NameMap,
    key: SliceKey,
) -> Result<EvidenceSlice, SliceError> {
    let from = fight_start_ms - LEAD_IN_MS;
    let to = fight_end_ms + TAIL_MS;

    // Longest first: a name that contains another ("Misti" inside "Mistifix2")
    // must be replaced before its substring, or the shorter match corrupts the
    // longer name and leaves half of it in the clear.
    let mut ordered: Vec<(&String, &u64)> = names.iter().collect();
    ordered.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(b.0)));

    let mut blinder = Blinder::new(key, &ordered);
    let mut blind_map: HashMap<String, u64> = HashMap::new();
    for (name, dbid) in ordered.iter().copied() {
        if let Some(token) = blinder.token_of(name) {
            blind_map.insert(String::from_utf8_lossy(token).into_owned(), *dbid);
        }
    }

    let mut stats = SliceStats::default();
    let mut records: Vec<(i32, Vec<u8>)> = Vec::new();

    // Reassemble per stream before framing. Every segment is appended even when
    // it falls outside the fight window, because dropping one would leave the
    // next segment starting mid-packet — the window selects what is *kept*, not
    // what is parsed.
    let mut streams: HashMap<&str, PacketAccumulator> = HashMap::new();

    for cap in packets {
        let acc = streams
            .entry(cap.stream.as_str())
            .or_insert_with(PacketAccumulator::new);
        acc.append(&cap.bytes);

        // Before the lead-in, state only; a segment is still framed either way
        // so the stream stays in step.
        let keep = if cap.captured_at_ms < from { Keep::State } else { Keep::All };
        let mut kept = Vec::new();
        let consumed = filter_stream(acc.snapshot(), &mut blinder, &mut kept, &mut stats, keep);
        acc.discard_bytes(consumed);

        if cap.captured_at_ms < from - PRELUDE_MS || cap.captured_at_ms > to {
            continue;
        }

        let dt = (cap.captured_at_ms - fight_start_ms).clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        for packet in kept {
            records.push((dt, packet));
        }
    }

    if records.is_empty() {
        return Err(SliceError::Empty);
    }

    // The verifier. Everything above is pattern work; this is what we actually
    // rely on. A name that reaches here means the slice is not safe to send, and
    // the answer is to discard it, not to send it anyway with a warning.
    //
    // Scans the *decompressed* content: records may be re-compressed bundles,
    // and searching a compressed buffer for a plaintext name finds nothing no
    // matter what is inside it. A verifier that passes vacuously is worse than
    // no verifier, because it is believed.
    let plaintext: Vec<Vec<u8>> = records.iter().map(|(_, p)| expand(p)).collect();
    for (name, _) in &ordered {
        if name.is_empty() {
            continue;
        }
        if plaintext.iter().any(|buf| name_in(buf, name.as_bytes())) {
            return Err(SliceError::NameLeaked(name.len()));
        }
    }
    // Every name, a one-byte name too, where a record holds one.
    let fields = name_field_contents(&records);
    if let Some((name, _)) = ordered.iter().find(|(name, _)| !name.is_empty() && fields.contains(name.as_bytes())) {
        return Err(SliceError::NameLeaked(name.len()));
    }

    // Names discovered structurally have no roster id: we blinded them without
    // ever learning who they were.
    for (token, dbid) in blinder.discovered {
        blind_map.entry(token).or_insert(dbid);
    }

    Ok(EvidenceSlice {
        records,
        blind_map,
        stats,
    })
}

// ===== container =====

pub const MAGIC: &[u8; 4] = b"A2ES";
/// 2: lengths follow the corrected framing rule. Version 1 slices are read by
/// re-framing their records (see `upgrade_v1_record`).
pub const VERSION: u16 = 2;

/// Serialise to the `.a2es` container.
///
/// ```text
/// "A2ES" | u16 version | u16 opcode_count | [u16 opcode]*
///        | u16 blind_count | [u8 token_len | token bytes | u64 dbid]*
///        | u32 record_count
///        | [ i32 dt_ms | u16 len | bytes ]*
/// ```
///
/// Deliberately not the game's framing: a reader must not be able to mistake a
/// slice for a capture, and `a2t-inspect` needs a magic number to refuse one.
pub fn encode(slice: &EvidenceSlice) -> Vec<u8> {
    let mut out = Vec::with_capacity(slice.stats.bytes_kept + 256);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());

    out.extend_from_slice(&(ALLOWED_OPCODES.len() as u16).to_le_bytes());
    for (op, _) in ALLOWED_OPCODES {
        out.extend_from_slice(*op);
    }

    out.extend_from_slice(&(slice.blind_map.len() as u16).to_le_bytes());
    let mut entries: Vec<_> = slice.blind_map.iter().collect();
    entries.sort(); // deterministic output: the same fight encodes byte-identically
    for (token, dbid) in entries {
        out.push(token.len() as u8);
        out.extend_from_slice(token.as_bytes());
        out.extend_from_slice(&dbid.to_le_bytes());
    }

    out.extend_from_slice(&(slice.records.len() as u32).to_le_bytes());
    for (dt, packet) in &slice.records {
        out.extend_from_slice(&dt.to_le_bytes());
        out.extend_from_slice(&(packet.len() as u16).to_le_bytes());
        out.extend_from_slice(packet);
    }
    out
}

/// Read a `.a2es` back. Used by the server, and by `a2t-inspect` so a user can
/// see exactly what an upload would contain.
pub fn decode(data: &[u8]) -> Option<(Vec<(i32, Vec<u8>)>, HashMap<String, u64>)> {
    let mut o = 0usize;
    fn take<'a>(data: &'a [u8], o: &mut usize, n: usize) -> Option<&'a [u8]> {
        if *o + n > data.len() {
            return None;
        }
        let s = &data[*o..*o + n];
        *o += n;
        Some(s)
    }
    if take(data, &mut o, 4)? != MAGIC {
        return None;
    }
    let version = u16::from_le_bytes(take(data, &mut o, 2)?.try_into().ok()?);
    if version != VERSION && version != 1 {
        return None;
    }
    let op_count = u16::from_le_bytes(take(data, &mut o, 2)?.try_into().ok()?) as usize;
    take(data, &mut o, op_count * 2)?;

    let blind_count = u16::from_le_bytes(take(data, &mut o, 2)?.try_into().ok()?) as usize;
    let mut blind_map = HashMap::with_capacity(blind_count);
    for _ in 0..blind_count {
        let len = take(data, &mut o, 1)?[0] as usize;
        let token = std::str::from_utf8(take(data, &mut o, len)?)
            .ok()?
            .to_string();
        let dbid = u64::from_le_bytes(take(data, &mut o, 8)?.try_into().ok()?);
        blind_map.insert(token, dbid);
    }

    let rec_count = u32::from_le_bytes(take(data, &mut o, 4)?.try_into().ok()?) as usize;
    let mut records = Vec::with_capacity(rec_count.min(1 << 20));
    for _ in 0..rec_count {
        let dt = i32::from_le_bytes(take(data, &mut o, 4)?.try_into().ok()?);
        let len = u16::from_le_bytes(take(data, &mut o, 2)?.try_into().ok()?) as usize;
        let record = take(data, &mut o, len)?;
        let record = if version == 1 { upgrade_v1_record(record) } else { record.to_vec() };
        records.push((dt, record));
    }
    Some((records, blind_map))
}

/// Re-frame a version 1 record for the current walk.
///
/// Version 1 was cut with the old rule: a frame spans `len - 3` bytes, plus one
/// for a top-level bundle. Each frame keeps exactly the bytes it had then and
/// gets a length the current walk reads as that span, so the parser sees what
/// it saw when the slice was made.
fn upgrade_v1_record(record: &[u8]) -> Vec<u8> {
    fn reframe(buf: &[u8], top: bool, depth: usize) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(buf.len() + 8);
        let mut o = 0;
        while o < buf.len() {
            if buf[o] == 0x00 {
                out.push(0x00);
                o += 1;
                continue;
            }
            let len = super::stream_processor::read_varint(buf, o);
            if len.length <= 0 || len.value <= 3 {
                break;
            }
            let n = len.length as usize;
            let bundle = buf.len() > o + n + 1 && buf[o + n] == 0xFF && buf[o + n + 1] == 0xFF;
            let size = len.value as usize - 3 + usize::from(bundle && top);
            if size < n || o + size > buf.len() {
                break;
            }
            let body = &buf[o + n..o + size];
            let rewritten = match bundle {
                true if depth < MAX_BUNDLE_DEPTH => framing::decompress_bundle(body)
                    .and_then(|inner| reframe(&inner, false, depth + 1))
                    .and_then(|inner| rewrap_bundle(&inner)),
                _ => None,
            };
            match rewritten {
                Some(b) => out.extend_from_slice(&b),
                None => out.extend_from_slice(&frame_packet(body)?),
            }
            o += size;
        }
        out.extend_from_slice(&buf[o..]);
        Some(out)
    }
    reframe(record, true, 0).unwrap_or_else(|| record.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A fixed key, so tests see the same tokens every run.
    const KEY: SliceKey = [7; 32];

    fn host_with(records_at: &[usize], len: usize) -> Vec<u8> {
        // A non-allowlisted host: <len> 0x99 0x36 ... with `04 38` at the given offsets.
        let mut body = vec![0x99, 0x36];
        body.resize(len, 0x11);
        for &at in records_at {
            body[at] = 0x04;
            body[at + 1] = 0x38;
        }
        frame_packet(&body).unwrap()
    }

    #[test]
    fn nearby_embedded_records_lift_as_one_span() {
        // Two records 40 bytes apart: one lifted packet, or the replay would
        // parse the second twice under a truncated de-duplication key.
        let host = host_with(&[20, 60], 400);
        let lifted = lift_embedded(&host, Keep::All);
        assert_eq!(lifted.len(), 1);
        let clear = &lifted[0];
        let hits = clear.windows(2).filter(|w| *w == [0x04, 0x38]).count();
        assert_eq!(hits, 2);
    }

    #[test]
    fn distant_embedded_records_lift_separately_and_bounded() {
        let host = host_with(&[20, 300], 600);
        let lifted = lift_embedded(&host, Keep::All);
        assert_eq!(lifted.len(), 2);
        for l in &lifted {
            // varint + host opcode + at most EMBEDDED_KEEP bytes of the host
            assert!(l.len() <= 3 + 2 + EMBEDDED_KEEP);
            assert_eq!(l.windows(2).filter(|w| *w == [0x04, 0x38]).count(), 1);
        }
    }

    #[test]
    fn a_lifted_packet_frames_like_a_real_one() {
        let host = host_with(&[20], 100);
        let lifted = &lift_embedded(&host, Keep::All)[0];
        let walk = framing::walk(lifted);
        assert_eq!(walk.frames.len(), 1);
        assert_eq!(walk.consumed, lifted.len());
        assert!(!is_allowed(lifted, Keep::All), "the host opcode must not collide with the allowlist");
    }

    /// A non-allowlisted host carrying one framed spawn record, as a field
    /// boss's arrived: `<len 8d 08> 41 36 <id b6 f5 01> …`, length 1,037, so
    /// 1,035 bytes with its two-byte length.
    fn host_with_spawn() -> (Vec<u8>, usize) {
        let mut body = vec![0x99, 0x36, 0x11, 0x11];
        let at = body.len() + 2;
        body.extend_from_slice(&[0x8d, 0x08, 0x41, 0x36, 0xb6, 0xf5, 0x01]);
        body.resize(4 + 1035, 0x22);
        body.resize(body.len() + 40, 0x11);
        (frame_packet(&body).unwrap(), at)
    }

    #[test]
    fn an_embedded_spawn_is_lifted_whole_even_in_the_lead_in() {
        let (host, _) = host_with_spawn();
        for keep in [Keep::State, Keep::All] {
            let lifted = lift_embedded(&host, keep);
            assert_eq!(lifted.len(), 1);
            let l = &lifted[0];
            let body = &l[l.len() - 1033..];
            assert_eq!(&body[..5], &[0x41, 0x36, 0xb6, 0xf5, 0x01], "starts at the spawn");
            // Its own length (two bytes, `8d 08`), not the one-byte `08` inside it.
            assert!(l.len() > 1000, "cut short: {}", l.len());
        }
    }

    #[test]
    fn a_spawn_lifted_in_the_lead_in_carries_no_damage() {
        let (mut host, at) = host_with_spawn();
        // A damage record inside the spawn's span.
        host[at + 200] = 0x04;
        host[at + 201] = 0x38;
        let lifted = lift_embedded(&host, Keep::State);
        assert!(lifted.iter().all(|l| !l.windows(2).any(|w| w == [0x04, 0x38])));
    }

    #[test]
    fn blinding_leaves_packet_headers_alone() {
        // A spawn whose length (256..=383) ends in 02: `<b3 02> 40 36` reads
        // as a two-letter name "@6" unless headers are left alone.
        let mut body = vec![0x40, 0x36, 0xc9, 0x8f, 0x07, 0x0c];
        body.resize(304, 0x00);
        let mut packet = frame_packet(&body).unwrap();
        assert_eq!(packet[1], 0x02, "a length ending in 02");
        let mut blinder = Blinder::new(KEY, &[]);
        blinder.blind(&mut packet);
        assert_eq!(&packet[2..7], &[0x40, 0x36, 0xc9, 0x8f, 0x07]);

        // The same record embedded in another packet.
        let mut host = vec![0x99, 0x36, 0x00, 0xb3, 0x02, 0x40, 0x36, 0xc9, 0x8f, 0x07, 0x0c];
        host.resize(64, 0x00);
        let mut host = frame_packet(&host).unwrap();
        blinder.blind(&mut host);
        assert!(host.windows(5).any(|w| w == [0x40, 0x36, 0xc9, 0x8f, 0x07]));
    }

    #[test]
    fn a_two_letter_name_is_still_blinded() {
        let mut body = vec![0x45, 0x36, 0x05, 0x00, 0x02, b'M', b'7', 0x00];
        body.resize(32, 0x00);
        let mut packet = frame_packet(&body).unwrap();
        Blinder::new(KEY, &[]).blind(&mut packet);
        assert!(!packet.windows(2).any(|w| w == b"M7"));
    }

    #[test]
    fn a_skill_id_that_decodes_as_one_letter_is_left_alone() {
        // A spirit's damage record (2026-10-04): `.. 02 | d3 86 01 00` is a
        // byte 02 then skill 100051, and d3 86 is valid UTF-8 for one letter.
        let mut body = vec![0x04, 0x38, 0xfe, 0x9e, 0x02, 0x04, 0x00, 0x9e, 0x9b, 0x01, 0x02, 0xd3, 0x86, 0x01, 0x00];
        body.resize(40, 0x00);
        let mut packet = frame_packet(&body).unwrap();
        Blinder::new(KEY, &[]).blind(&mut packet);
        assert!(packet.windows(4).any(|w| w == [0xd3, 0x86, 0x01, 0x00]));
    }

    #[test]
    fn a_one_character_name_is_blinded() {
        // A player spawn carrying the one-letter name "あ" (`e3 81 82`).
        let mut body = vec![0x44, 0x36, 0x9e, 0x9b, 0x01, 0x03, 0xe3, 0x81, 0x82];
        body.resize(40, 0x00);
        let mut packet = frame_packet(&body).unwrap();
        Blinder::new(KEY, &[]).blind(&mut packet);
        assert!(!packet.windows(3).any(|w| w == [0xe3, 0x81, 0x82]));
    }

    #[test]
    fn damage_records_are_not_scanned_for_names() {
        // Water Spirit: Ice Chain, 2026-10-04: actor 48405 ends in 02, and the
        // skill id 16152400 starts `50 77`, "Pw".
        let mut body = vec![0x04, 0x38, 0xfe, 0x9e, 0x02, 0x04, 0x00, 0x95, 0xfa, 0x02, 0x50, 0x77, 0xf6, 0x00];
        body.resize(40, 0x00);
        let mut packet = frame_packet(&body).unwrap();
        Blinder::new(KEY, &[]).blind(&mut packet);
        assert!(packet.windows(4).any(|w| w == [0x50, 0x77, 0xf6, 0x00]));
    }

    #[test]
    fn a_skill_id_that_reads_as_a_letter_and_a_bracket_is_left_alone() {
        // Water Bomb, 16001105: `02 | 51 28 f4 00` reads "Q(".
        let mut body = vec![0x04, 0x38, 0xfe, 0x9e, 0x02, 0x04, 0x00, 0x9e, 0x9b, 0x01, 0x02, 0x51, 0x28, 0xf4, 0x00];
        body.resize(40, 0x00);
        let mut packet = frame_packet(&body).unwrap();
        Blinder::new(KEY, &[]).blind(&mut packet);
        assert!(packet.windows(4).any(|w| w == [0x51, 0x28, 0xf4, 0x00]));
    }

    #[test]
    fn a_token_never_spells_a_known_name_with_its_neighbours() {
        // Capture 2026-10-04 01:45, a `45 36` player spawn: the run
        // "d\dddd:^ddZl" became "13b87050fe08", and with the `44` after it
        // spelled "8D", the name of another player in the capture.
        let mut body = vec![0x45, 0x36, 0x3c, 0x03, 0x00, 0x00, 0xde, 0x02, 0x0c, 0x64, 0x5c, 0x64, 0x64, 0x64, 0x64, 0x3a, 0x5e, 0x64, 0x64, 0x5a, 0x6c, 0x44, 0xb2, 0x64, 0x6a, 0x64, 0x64, 0x58, 0x64, 0x48, 0x7c, 0x8a];
        body.resize(64, 0x00);
        let mut packet = frame_packet(&body).unwrap();
        let before = packet.clone();
        let name = "8D".to_string();
        Blinder::new(KEY, &[(&name, &0)]).blind(&mut packet);
        assert!(!packet.windows(2).any(|w| w == b"8D"));
        // Still blinded, and nothing but the run changed.
        let at = before.windows(3).position(|w| w == [0xde, 0x02, 0x0c]).unwrap() + 3;
        assert_ne!(packet[at..at + 12], before[at..at + 12]);
        assert_eq!(packet[..at], before[..at]);
        assert_eq!(packet[at + 12..], before[at + 12..]);
    }

    #[test]
    fn the_prelude_never_carries_damage() {
        let damage = frame_packet(&[0x04, 0x38, 0x01, 0x02, 0x03]).unwrap();
        let spawn = frame_packet(&[0x41, 0x36, 0x01, 0x02, 0x03]).unwrap();
        assert!(!is_allowed(&damage, Keep::State));
        assert!(is_allowed(&damage, Keep::All));
        assert!(is_allowed(&spawn, Keep::State));
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// A mob spawn from a capture (2026-10-06 18:12:55): its length is
    /// `8f 01`, so its frame starts `8f 01 41 36`, the bytes a one-letter
    /// name "A" has after its length byte.
    fn spawn_after_8f_01() -> Vec<u8> {
        hex(concat!(
            "8f014136efb4031c000064902c0000026b519247339aa5c700540a4600d78b3fc7000107076400000064",
            "000000000000000000000000000000000000006400000064000000010000000000000000000000000000",
            "00000000000601110181969800ffffffffffffffff8075d52abb030000efb40301026b519247339aa5c7",
            "00540a46063ed40000002900000000",
        ))
    }

    /// Frame a record whose body is under 124 bytes: one length byte.
    fn small_frame(body: &str) -> Vec<u8> {
        let packet = frame_packet(&hex(body)).unwrap();
        assert!(packet.len() < 128);
        packet
    }

    /// Each record, and where its names are. The bodies are records the
    /// parser reads, with made-up names: a self record ("Xy"), a player
    /// spawn ("A"), a spawn naming its caster ("B"), a summon owner record
    /// ("Cd"), a party roster ("Ef"), and the spawn above.
    fn records_with_names() -> Vec<(Vec<u8>, Vec<std::ops::Range<usize>>)> {
        vec![
            (small_frame("3336ed745e91c1283702587918051e000000011c000000"), vec![11..13]),
            (small_frame("4536d74800000000070141000000000000000000"), vec![11..12]),
            (small_frame("4136c391031c0001014264902c0000026b5192473300"), vec![10..11]),
            (small_frame("048dece6027228e900ae0b19050243640001000000"), vec![15..17]),
            (
                small_frame(concat!(
                    "029701000000012e0600000000000000000000000000000000000101010100000000001905",
                    "0245661e0000002d0000006400000019050000000102030000000000000000",
                )),
                vec![39..41],
            ),
            (spawn_after_8f_01(), vec![]),
        ]
    }

    fn slice_of(packets: &[Vec<u8>], names: &NameMap) -> EvidenceSlice {
        let at = CapturedPacket { captured_at_ms: 0, stream: "Client:1".into(), bytes: packets.concat() };
        build(&[at], 0, 10, names, KEY).unwrap()
    }

    /// `blinded` is `source` but for its names: the name fields given, and
    /// the runs a blinder that knows no names changes (text shaped like a
    /// name, which the log service also counts as one).
    fn only_names_changed(source: &[u8], blinded: &[u8], fields: &[std::ops::Range<usize>]) {
        assert_eq!(blinded.len(), source.len());
        let mut shaped = source.to_vec();
        Blinder::new(KEY, &[]).blind_name_shaped(&mut shaped);
        for (i, (b, s)) in blinded.iter().zip(source).enumerate() {
            if !fields.iter().any(|f| f.contains(&i)) && shaped[i] == *s {
                assert_eq!(b, s, "byte {i} of {source:02x?}");
            }
        }
        for field in fields {
            assert_ne!(blinded[field.clone()], source[field.clone()], "a name was left in {source:02x?}");
        }
    }

    #[test]
    fn a_one_letter_name_is_blinded_where_its_record_holds_it() {
        // A player named "A" (a one-letter name is allowed), then the spawn
        // whose frame starts `8f 01 41 36`.
        let player = small_frame("4536d74800000000070141000000000000000000");
        let spawn = spawn_after_8f_01();
        let mut names = NameMap::new();
        names.insert("A".into(), 0);
        let slice = slice_of(&[player.clone(), spawn.clone()], &names);
        assert_eq!(slice.records.len(), 2);
        only_names_changed(&player, &slice.records[0].1, &[11..12]);
        only_names_changed(&spawn, &slice.records[1].1, &[]);
        let token = slice.records[0].1[11];
        assert!(token.is_ascii_alphabetic(), "{token:02x}");
        assert!(slice.blind_map.contains_key(&(token as char).to_string()));

        // The slice still names the player and still spawns the mob.
        let storage = Arc::new(crate::combat::data_storage::DataStorage::new());
        let mut processor = crate::capture::stream_processor::StreamProcessor::new(
            storage.clone(),
            Arc::new(crate::i18n::lookup::SkillLookup::new()),
            Arc::new(crate::i18n::lookup::NpcLookup::new()),
        );
        for (_, record) in &slice.records {
            processor.consume_stream(record);
        }
        assert_eq!(storage.get_nickname(9303), Some((token as char).to_string()));
        assert_eq!(storage.mob_code(55919), Some(2_920_548));

        // A one-letter name is a leak in a name field, and nowhere else: the
        // spawn's `01 41` is no leak.
        assert_eq!(leaked_names(&[(0, player)], &["A".to_string()]), 1);
        assert_eq!(leaked_names(&[(0, spawn)], &["A".to_string()]), 0);
    }

    #[test]
    fn blinding_changes_nothing_but_the_names() {
        let records = records_with_names();
        let mut names = NameMap::new();
        names.insert("Xy".into(), 7);
        names.insert("A".into(), 0);
        names.insert("Ef".into(), 0x0519_0000_0000_0001);
        // "B" and "Cd" are names the meter never resolved.
        let packets: Vec<Vec<u8>> = records.iter().map(|(p, _)| p.clone()).collect();
        let slice = slice_of(&packets, &names);
        assert_eq!(slice.records.len(), records.len());
        for ((_, blinded), (source, fields)) in slice.records.iter().zip(&records) {
            only_names_changed(source, blinded, fields);
        }
    }

    #[test]
    fn a_known_two_letter_name_is_only_blinded_where_the_game_puts_a_name() {
        // "Jo" by chance in a record's data stays; after its length byte it goes.
        let name = "Jo".to_string();
        let mut chance = frame_packet(&[0x41, 0x36, 0x9e, 0x9b, 0x01, 0x10, b'J', b'o', 0x22, 0x00, 0x00]).unwrap();
        let before = chance.clone();
        Blinder::new(KEY, &[(&name, &0)]).blind_known(&mut chance);
        assert_eq!(chance, before);
        let mut named = frame_packet(&[0x41, 0x36, 0x9e, 0x9b, 0x01, 0x02, b'J', b'o', 0x22, 0x00, 0x00]).unwrap();
        Blinder::new(KEY, &[(&name, &0)]).blind_known(&mut named);
        assert!(!named.windows(2).any(|w| w == b"Jo"));
    }

    /// A reconnect from the same server port. The old connection's last
    /// packet never finished; the new connection's bytes, appended to it in one
    /// stream, were read as its tail, and every slice after came out as lifted
    /// fragments or not at all (2026-10-07: 98 % lifted from 08:05, then no
    /// slices). One stream per connection keeps the new one whole.
    #[test]
    fn a_short_name_is_only_a_leak_where_the_game_puts_a_name() {
        // "Jo" by chance inside other data is not a leak; after its length byte it is.
        assert!(!name_in(&[0x10, b'J', b'o', 0x22, 0x01], b"Jo"));
        assert!(name_in(&[0x10, 0x02, b'J', b'o', 0x22], b"Jo"));
        // A longer name counts anywhere.
        assert!(name_in(&[0x00, b'M', b'i', b's', b't', b'i', 0x00], b"Misti"));
        assert!(!name_in(&[0x00, b'M', b'i', b's', b't', 0x00], b"Misti"));
        let records = vec![(0, vec![0x05, 0x10, b'J', b'o', 0x22, 0x01])];
        assert_eq!(leaked_names(&records, &["Jo".to_string()]), 0);
    }

    #[test]
    fn a_reconnect_on_the_same_server_port_starts_a_clean_stream() {
        use crate::capture::captured_payload::stream_key;
        let world = hex("34213601000000f2030000dd7f3c00000000006868d047d0c62c470098da46fa63284300000000000000000000004f0000");
        let at = |ms, stream: String, bytes: Vec<u8>| CapturedPacket { captured_at_ms: ms, stream, bytes };
        let cut = |old: String, new: String| {
            let packets = vec![
                at(100_000, old.clone(), world.clone()),
                at(100_000, old, world[..10].to_vec()),
                at(101_000, new.clone(), world.clone()),
                at(101_000, new, world.clone()),
            ];
            build(&packets, 100_000, 110_000, &HashMap::new(), KEY).map(|s| s.records.len()).unwrap_or(0)
        };
        assert_eq!(cut(stream_key(7777, 50000), stream_key(7777, 50001)), 3, "every whole packet kept");
        assert!(cut("Client:7777".into(), "Client:7777".into()) < 3, "one stream per server port loses the new connection");
    }

    #[test]
    fn map_loads_are_kept_without_their_text_and_party_scope_is_not() {
        // Captured 2026-10-04: a party-scope record, a load into World_L_A
        // (1010), and a load naming a cutscene.
        let scope = hex("0f0638eab601b26c18000c00");
        let world = hex("34213601000000f2030000dd7f3c00000000006868d047d0c62c470098da46fa63284300000000000000000000004f0000");
        let cutscene = hex("492136030000009b8a01001d8a01000000000006bfcd47aaa3904700482c46069fb6c2020000000000000000000039154375747363656e655f4c5f415f5365715f3131353100");
        let at = |ms, bytes: &Vec<u8>| CapturedPacket { captured_at_ms: ms, stream: "Client:1".into(), bytes: bytes.clone() };
        // The first two in the prelude, the rest in the fight window.
        let packets = vec![at(0, &scope), at(0, &world), at(100_000, &cutscene), at(100_000, &scope)];
        let slice = build(&packets, 100_000, 110_000, &HashMap::new(), KEY).unwrap();
        // Party scope only feeds the loot owner, which a derivation never
        // reads, and it made slices 30-50 % larger (2026-10-07, three bosses).
        assert_eq!(slice.records.len(), 2);
        let kept: Vec<u8> = slice.records.iter().flat_map(|(_, r)| r.clone()).collect();
        assert!(kept.windows(4).any(|w| w == [0x9b, 0x8a, 0x01, 0x00]), "the map id survives");
        assert!(!kept.windows(8).any(|w| w == b"Cutscene"), "the text does not");

        let storage = Arc::new(crate::combat::data_storage::DataStorage::new());
        let mut processor = crate::capture::stream_processor::StreamProcessor::new(
            storage.clone(),
            Arc::new(crate::i18n::lookup::SkillLookup::new()),
            Arc::new(crate::i18n::lookup::NpcLookup::new()),
        );
        storage.set_current_dungeon(600021);
        for (_, record) in &slice.records[..2] {
            processor.consume_stream(record);
        }
        assert_eq!(storage.current_dungeon_id(), 0, "the replay saw the load into the open world");
    }

    fn framed(payload: &[u8]) -> Vec<u8> {
        let total = payload.len() + 1;
        let mut v = vec![(total + 3) as u8];
        v.extend_from_slice(payload);
        v
    }

    fn cap(ms: i64, payload: &[u8]) -> CapturedPacket {
        CapturedPacket {
            captured_at_ms: ms,
            stream: "Client:1".into(),
            bytes: framed(payload),
        }
    }

    /// A player spawn naming "Grandine", as `45 36 <id> ... <len><name>`.
    fn spawn_named(name: &str) -> Vec<u8> {
        let mut body = vec![0x45, 0x36, 0xb5, 0x02, 0x00, 0x00];
        body.push(name.len() as u8);
        body.extend_from_slice(name.as_bytes());
        body.extend_from_slice(&[0x00; 8]);
        framed(&body)
    }

    #[test]
    fn the_server_finds_names_a_client_left_in_the_clear() {
        // A slice cut by this meter: nothing left to blind.
        let slice = build(&[cap(500, &spawn_named("Grandine")[1..])], 0, 1_000, &HashMap::new(), KEY).unwrap();
        assert_eq!(unblinded_names(&slice.records, &slice.blind_map), 0);
        assert_eq!(leaked_names(&slice.records, &["Grandine".to_string()]), 0);

        // The same record from a client that did not blind it.
        let raw = vec![(0, spawn_named("Grandine"))];
        assert_eq!(unblinded_names(&raw, &HashMap::new()), 1);
        assert_eq!(leaked_names(&raw, &["Grandine".to_string(), "Misti".to_string()]), 1);
    }

    #[test]
    fn token_is_always_the_same_byte_length() {
        for name in ["Misti", "丨Mamepoko丨", "九州依然在", "a", "Grandine"] {
            let t = token_for(&KEY, name, 0x03f6_0000_0001_4b85, name.len());
            assert_eq!(t.len(), name.len(), "{name}");
            assert!(t.is_ascii());
        }
    }

    #[test]
    fn token_is_stable_per_roster_id_not_per_name() {
        let a = token_for(&KEY, "Misti", 7, 5);
        let b = token_for(&KEY, "Other", 7, 5);
        assert_eq!(
            a, b,
            "same dbid must blind to the same token regardless of name"
        );
        assert_ne!(a, token_for(&KEY, "Misti", 8, 5));
    }

    #[test]
    fn another_key_gives_other_tokens() {
        let other: SliceKey = [8; 32];
        assert_ne!(token_for(&KEY, "Velkora", 0, 7), token_for(&other, "Velkora", 0, 7));
        assert_ne!(token_for(&KEY, "Velkora", 7, 7), token_for(&other, "Velkora", 7, 7));

        let mut payload = vec![0x45, 0x36, 0x07];
        payload.extend_from_slice(b"Velkora");
        let mut names = NameMap::new();
        names.insert("Velkora".into(), 42);
        let a = build(&[cap(0, &payload)], 0, 10, &names, KEY).unwrap();
        let b = build(&[cap(0, &payload)], 0, 10, &names, other).unwrap();
        assert_ne!(a.blind_map.keys().collect::<Vec<_>>(), b.blind_map.keys().collect::<Vec<_>>());
        assert_ne!(encode(&a), encode(&b));
    }

    #[test]
    fn a_name_gets_one_token_throughout_a_slice() {
        let mut payload = vec![0x45, 0x36, 0x07];
        payload.extend_from_slice(b"Velkora");
        let mut names = NameMap::new();
        names.insert("Velkora".into(), 42);
        let slice = build(&[cap(0, &payload), cap(5, &payload)], 0, 10, &names, KEY).unwrap();

        assert_eq!(slice.blind_map.len(), 1);
        let token = slice.blind_map.keys().next().unwrap().as_bytes();
        assert_eq!(slice.records.len(), 2);
        for (_, record) in &slice.records {
            assert!(record.windows(token.len()).any(|w| w == token));
        }
    }

    #[test]
    fn drops_packets_that_are_not_on_the_allowlist() {
        let packets = vec![
            cap(1_000, &[0x04, 0x38, 0x01, 0x02]),
            cap(1_000, &[0xAB, 0xCD, b'c', b'h', b'a', b't']),
        ];
        let slice = build(&packets, 1_000, 2_000, &HashMap::new(), KEY).unwrap();
        assert_eq!(slice.records.len(), 1);
        assert_eq!(slice.stats.packets_seen, 2);
        assert_eq!(slice.stats.packets_kept, 1);
    }

    #[test]
    fn blinds_a_name_and_keeps_every_length_identical() {
        let mut payload = vec![0x45, 0x36, 0x05];
        payload.extend_from_slice(b"Misti");
        let before = framed(&payload).len();

        let mut names = NameMap::new();
        names.insert("Misti".into(), 42);
        let slice = build(&[cap(0, &payload)], 0, 10, &names, KEY).unwrap();

        assert_eq!(slice.records.len(), 1);
        assert_eq!(
            slice.records[0].1.len(),
            before,
            "blinding must not resize the packet"
        );
        assert!(
            !slice.records[0].1.windows(5).any(|w| w == b"Misti"),
            "the name is still in the slice"
        );
    }

    #[test]
    fn a_name_that_contains_another_is_fully_blinded() {
        // "Misti" is a substring of "Mistifix2". Replacing the short one first
        // would leave "fix2" in the clear.
        let mut payload = vec![0x45, 0x36];
        payload.extend_from_slice(b"Mistifix2");
        let mut names = NameMap::new();
        names.insert("Misti".into(), 1);
        names.insert("Mistifix2".into(), 2);

        let slice = build(&[cap(0, &payload)], 0, 10, &names, KEY).unwrap();
        let out = &slice.records[0].1;
        assert!(!out.windows(9).any(|w| w == b"Mistifix2"));
        assert!(!out.windows(5).any(|w| w == b"Misti"));
    }

    #[test]
    fn cjk_names_survive_blinding_as_valid_utf8() {
        let mut payload = vec![0x02, 0x97];
        payload.extend_from_slice("九州依然在".as_bytes());
        let mut names = NameMap::new();
        names.insert("九州依然在".into(), 3);

        let slice = build(&[cap(0, &payload)], 0, 10, &names, KEY).unwrap();
        let out = &slice.records[0].1;
        assert!(!out
            .windows("九州依然在".len())
            .any(|w| w == "九州依然在".as_bytes()));
        // The token replaced 15 bytes with 15 ASCII bytes; the packet must still
        // decode as UTF-8 wherever it did before.
        assert_eq!(out.len(), framed(&payload).len());
    }

    #[test]
    fn packets_outside_the_fight_window_are_dropped() {
        let payload = [0x04, 0x38, 0x01];
        let packets = vec![
            cap(0, &payload),                        // before the lead-in
            cap(100_000 - LEAD_IN_MS + 1, &payload), // inside the lead-in
            cap(100_000, &payload),                  // during
            cap(200_000 + TAIL_MS + 1, &payload),    // past the tail
        ];
        let slice = build(&packets, 100_000, 200_000, &HashMap::new(), KEY).unwrap();
        assert_eq!(slice.records.len(), 2);
    }

    #[test]
    fn relative_timestamps_carry_no_wall_clock() {
        let payload = [0x04, 0x38, 0x01];
        let slice = build(
            &[cap(1_700_000_005_000, &payload)],
            1_700_000_000_000,
            1_700_000_010_000,
            &HashMap::new(),
            KEY,
        )
        .unwrap();
        assert_eq!(slice.records[0].0, 5_000);
    }

    #[test]
    fn round_trips_through_the_container() {
        let mut payload = vec![0x02, 0x97, 0x01, 0x02, 0x03];
        payload.extend_from_slice(b"Grandine");
        let mut names = NameMap::new();
        names.insert("Grandine".into(), 0x03f5_0000_0001_b9c0);

        let slice = build(&[cap(500, &payload)], 0, 1_000, &names, KEY).unwrap();
        let bytes = encode(&slice);
        let (records, blind_map) = decode(&bytes).expect("decodes");

        assert_eq!(records, slice.records);
        assert_eq!(blind_map, slice.blind_map);
        // Present, not first: the map also holds tokens for name-shaped strings
        // discovered structurally, and HashMap order is not a thing to assert on.
        assert!(
            blind_map.values().any(|&d| d == 0x03f5_0000_0001_b9c0),
            "the roster id survived the round trip"
        );
    }

    #[test]
    fn encoding_is_deterministic() {
        let mut payload = vec![0x04, 0x38];
        payload.extend_from_slice(b"Misti");
        let mut names = NameMap::new();
        names.insert("Misti".into(), 1);
        names.insert("Grandine".into(), 2);
        let a = encode(&build(&[cap(0, &payload)], 0, 10, &names, KEY).unwrap());
        let b = encode(&build(&[cap(0, &payload)], 0, 10, &names, KEY).unwrap());
        assert_eq!(a, b);
    }

    #[test]
    fn a_capture_with_nothing_in_the_window_is_an_error() {
        let packets = vec![cap(0, &[0x04, 0x38, 0x01])];
        assert!(matches!(
            build(&packets, 10_000_000, 10_001_000, &HashMap::new(), KEY),
            Err(SliceError::Empty)
        ));
    }

    #[test]
    fn a_version_1_slice_is_reframed_for_the_current_walk() {
        // Version 1 lengths: a frame spans `len - 3`, a top-level bundle one more.
        fn old(body: &[u8], bundle: bool) -> Vec<u8> {
            for n in 1u32..=3 {
                let value = n + body.len() as u32 + 3 - u32::from(bundle);
                let mut v = encode_varint(value);
                if v.len() == n as usize {
                    v.extend_from_slice(body);
                    return v;
                }
            }
            unreachable!()
        }
        let long: Vec<u8> = [0x04, 0x38].into_iter().chain((0..200).map(|i| i as u8 | 1)).collect();
        let short = vec![0x41, 0x36, 0x05];
        let mut inner = old(&short, false);
        inner.extend(old(&long, false));
        let mut payload = vec![0xFF, 0xFF];
        payload.extend_from_slice(&(inner.len() as u32).to_le_bytes());
        payload.extend_from_slice(&lz4_flex::compress(&inner));
        let slice = EvidenceSlice {
            records: vec![(0, old(&long, false)), (1, old(&payload, true)), (2, old(&short, false))],
            blind_map: HashMap::new(),
            stats: SliceStats::default(),
        };
        let mut bytes = encode(&slice);
        bytes[4..6].copy_from_slice(&1u16.to_le_bytes());

        let (records, _) = decode(&bytes).expect("a version 1 slice still decodes");
        let bodies: Vec<Vec<u8>> = records
            .iter()
            .map(|(_, r)| {
                let w = framing::walk(r);
                assert_eq!(w.consumed, r.len());
                assert_eq!(w.frames.len(), 1);
                w.frames[0].payload(r).to_vec()
            })
            .collect();
        assert_eq!(bodies[0], long);
        assert_eq!(bodies[2], short);
        let unpacked = framing::decompress_bundle(&bodies[1]).expect("still a bundle");
        let w = framing::walk_inner(&unpacked);
        assert_eq!(w.consumed, unpacked.len());
        let got: Vec<_> = w.frames.iter().map(|f| f.payload(&unpacked).to_vec()).collect();
        assert_eq!(got, vec![short.clone(), long.clone()]);
    }

    #[test]
    fn decode_refuses_something_that_is_not_a_slice() {
        assert!(decode(b"not a slice at all").is_none());
        assert!(decode(&[]).is_none());
        // A raw capture must not be mistaken for a slice.
        assert!(decode(&framed(&[0x04, 0x38, 0x01])).is_none());
    }
}
