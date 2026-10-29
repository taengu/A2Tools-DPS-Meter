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
//!    exactly the same byte length. Stable roster identities are replaced with
//!    per-slice identities, preserving only the server bits replay needs.
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

use aho_corasick::AhoCorasick;
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

/// Buff changes (`2b 38`), kept from the lead-in on but not in the prelude.
/// A change renews an instance the add (`2a 38`, kept) began, so the lead-in's
/// changes still give each buff on at the pull with its start. In a crowd the
/// prelude's changes were most of a slice: at a field boss (343 players, 13.5
/// minutes before the pull) 510,125 changes, 24 of its 32 MiB, past what the
/// log service reads (2026-10-21). On 14 fights of two captures the Buffs
/// timeline came out as before; dropping adds and removals as well lost buffs
/// put on before the lead-in.
const PRELUDE_SKIPPED: &[[u8; 2]] = &[[0x2B, 0x38]];

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
    /// Name tokens declared by the slice. Values are zero in new slices;
    /// the field remains readable for compatibility with older containers.
    pub blind_map: HashMap<String, u64>,
    pub stats: SliceStats,
}

#[derive(Debug)]
pub enum SliceError {
    /// A name survived blinding. The slice is discarded rather than uploaded.
    NameLeaked(usize),
    /// Nothing was left after filtering — there is no evidence to send.
    Empty,
    /// A retained roster could not be completely anonymized.
    RosterIdentity,
    ExpansionLimit,
    EmbeddedIdentity,
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
            SliceError::RosterIdentity => write!(f, "a roster identity could not be anonymized; slice discarded"),
            SliceError::ExpansionLimit => write!(f, "the slice exceeded its decompression budget; slice discarded"),
            SliceError::EmbeddedIdentity => write!(f, "a retained packet contains embedded compressed data that cannot be anonymized; slice discarded"),
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
    /// Every name in `known`, found in one pass (pattern i is `known[i]`).
    /// Looking for each name in turn cost a pass over every packet per name:
    /// with a few hundred names seen in a session, a slice took over a
    /// minute to cut, and a save before a zone change held the parser that long.
    finder: Option<AhoCorasick>,
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
    /// Original stable identities -> per-slice identities. Their server bits
    /// stay intact because replay uses them to locate the roster's tail.
    roster_ids: HashMap<u64, u64>,
    invalid_roster: bool,
    unsupported_embedded: bool,
}

impl Blinder {
    fn new(key: SliceKey, ordered: &[(&String, &u64)]) -> Self {
        let mut blinder = Self::exempting(HashSet::new());
        blinder.key = key;
        blinder.one_byte_taken = ordered.iter().filter(|(n, _)| n.len() == 1).map(|(n, _)| n.as_bytes()[0]).collect();
        for (name, dbid) in ordered {
            if **dbid >> 48 != 0 { blinder.roster_id(**dbid); }
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
        if !blinder.known.is_empty() {
            blinder.finder = AhoCorasick::new(blinder.known.iter().map(|(name, _)| name)).ok();
        }
        blinder
    }

    /// Which of `known` appear in `buf`, by index, in `known`'s order.
    fn present(&self, buf: &[u8]) -> Vec<usize> {
        // Without the finder (it could not be built), every name, as before.
        let Some(finder) = &self.finder else { return (0..self.known.len()).collect() };
        let mut present = vec![false; self.known.len()];
        let mut count = 0;
        for m in finder.find_overlapping_iter(buf) {
            let slot = &mut present[m.pattern().as_usize()];
            if !*slot { *slot = true; count += 1; }
            if count == present.len() { break; }
        }
        present.into_iter().enumerate().filter_map(|(i, found)| found.then_some(i)).collect()
    }

    /// A blinder that knows no names and leaves `tokens` alone.
    fn exempting(tokens: HashSet<Vec<u8>>) -> Self {
        Self {
            // Only counts: the tokens it would mint are never written.
            key: [0; 32],
            known: Vec::new(),
            finder: None,
            tokens: HashMap::new(),
            known_tokens: tokens,
            discovered: HashMap::new(),
            one_byte_taken: HashSet::new(),
            one_byte_given: HashMap::new(),
            roster_ids: HashMap::new(),
            invalid_roster: false,
            unsupported_embedded: false,
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
        self.unsupported_embedded |= contains_embedded_bundle(buf);
        let identities = match super::stream_processor::roster_identity_fields(buf) {
            Ok(fields) => fields.into_iter().filter_map(|range| {
                let raw = u64::from_le_bytes(buf.get(range.clone())?.try_into().ok()?);
                Some((range, self.roster_id(raw)))
            }).collect::<Vec<_>>(),
            Err(()) => { self.invalid_roster = true; Vec::new() },
        };
        if !is_event(buf) {
            let fields = super::stream_processor::name_fields(buf);
            replaced += self.blind_fields(buf, &fields);
        }
        replaced += self.blind_known(buf);
        replaced += self.blind_name_shaped(buf);
        // Restore the whole identity field after name blinding, which must not
        // accidentally alter a binary identity that happens to contain text.
        for (range, identity) in identities {
            buf[range].copy_from_slice(&identity.to_le_bytes());
        }
        replaced
    }

    fn canonical_roster(&mut self, packet: Vec<u8>) -> Vec<u8> {
        match super::stream_processor::roster_body_for_sharing(&packet) {
            Ok(Some(body)) => frame_packet(&body).unwrap(),
            Ok(None) => packet,
            Err(()) => { self.invalid_roster = true; packet },
        }
    }

    fn roster_id(&mut self, raw: u64) -> u64 {
        if raw == 0 { return 0; }
        if let Some(&id) = self.roster_ids.get(&raw) { return id; }
        if self.roster_ids.values().any(|&id| id == raw) { self.invalid_roster = true; }
        const LOW: u64 = (1u64 << 48) - 1;
        for salt in 0..64u32 {
            let mut hash = Sha256::new();
            hash.update(self.key);
            hash.update(b"a2es-roster-id\0");
            hash.update(raw.to_le_bytes());
            hash.update(salt.to_le_bytes());
            let digest = hash.finalize();
            let low = u64::from_le_bytes(digest[..8].try_into().unwrap()) & LOW;
            let id = (raw & !LOW) | low;
            if low != 0 && id != raw && !self.roster_ids.contains_key(&id)
                && !self.roster_ids.values().any(|&taken| taken == id)
            {
                self.roster_ids.insert(raw, id);
                return id;
            }
        }
        self.invalid_roster = true;
        0
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
    ///
    /// Names are still replaced one at a time, longest first; only the names
    /// in the packet are visited. A replacement writes a token, which could
    /// spell a later name with the bytes around it, so the packet is searched
    /// again after one.
    fn blind_known(&self, buf: &mut [u8]) -> usize {
        let mut replaced = 0;
        let mut todo = self.present(buf);
        let mut next = 0;
        while next < todo.len() {
            let index = todo[next];
            next += 1;
            let (needle, token) = &self.known[index];
            let before = replaced;
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
            if replaced > before {
                todo.truncate(next);
                todo.extend(self.present(buf).into_iter().filter(|&later| later > index));
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
        let identities = super::stream_processor::roster_identity_fields(buf).unwrap_or_default();
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
            if identities.iter().any(|r| r.start < i + 1 + len && r.end > i)
                || !looks_like_text(span) || self.known_tokens.contains(span) || embedded_header(buf, i) {
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
        let Some(finder) = &self.finder else {
            return self.known.iter().any(|(name, _)| {
                let n = name.len();
                let start = (from + 1).saturating_sub(n);
                let end = (to + n - 1).min(buf.len());
                end >= start + n && buf[start..end].windows(n).any(|w| w == name.as_slice())
            });
        };
        let longest = self.known.iter().map(|(name, _)| name.len()).max().unwrap_or(0);
        let lo = (from + 1).saturating_sub(longest);
        let hi = (to + longest).saturating_sub(1).min(buf.len());
        if hi <= lo {
            return false;
        }
        // A name of n bytes counts where it lies inside
        // [from + 1 - n, to + n - 1): it shares a byte with buf[from..to].
        finder.find_overlapping_iter(&buf[lo..hi]).any(|m| {
            let (start, end, n) = (lo + m.start(), lo + m.end(), m.end() - m.start());
            start >= (from + 1).saturating_sub(n) && end <= (to + n - 1).min(buf.len())
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

/// The live parser can discover identity bundles inside another plain packet.
/// Their surrounding layout is not understood well enough to rewrite in
/// place. Refuse to certify such a retained packet until it has a typed rule.
fn contains_embedded_bundle(packet: &[u8]) -> bool {
    for i in 1..packet.len().saturating_sub(6) {
        if packet[i..i + 2] != [0xff, 0xff] { continue; }
        let size = u32::from_le_bytes(packet[i + 2..i + 6].try_into().unwrap());
        if !(1..=1_000_000).contains(&size) { continue; }
        if (1..=3usize).any(|n| {
            let Some(at) = i.checked_sub(n) else { return false };
            let len = super::stream_processor::read_varint(packet, at);
            len.length == n as i32 && framing::frame_size(len.value, len.length)
                .is_some_and(|span| at + span <= packet.len() && at + span > i + 6)
        }) { return true; }
    }
    false
}

/// Match the live parser's expansion grouping before filtering can split one
/// captured input into several records. Even discarded hosts can consume its
/// embedded-identity budget and change what later bundles the live parser saw.
fn input_expansion_fits(buffer: &[u8], total: &mut framing::DecompressionBudget) -> bool {
    fn decompress(payload: &[u8], call: &mut framing::DecompressionBudget, total: &mut framing::DecompressionBudget) -> Option<Vec<u8>> {
        total.charge(payload)?;
        call.decompress(payload)
    }
    fn visit(buffer: &[u8], top: bool, depth: usize, call: &mut framing::DecompressionBudget,
        total: &mut framing::DecompressionBudget)
    {
        let frames = if top { framing::walk(buffer).frames } else { framing::walk_inner(buffer).frames };
        for frame in frames {
            match frame.kind {
                FrameKind::Bundle if depth < MAX_BUNDLE_DEPTH => {
                    if let Some(inner) = decompress(frame.payload(buffer), call, total) { visit(&inner, false, depth + 1, call, total); }
                },
                FrameKind::Packet if top => {
                    let packet = frame.bytes(buffer);
                    let mut i = 1;
                    while i + 8 < packet.len() {
                        if packet[i..i + 2] != [0xff, 0xff] { i += 1; continue; }
                        let end = (1..=3usize).rev().find_map(|n| {
                            let at = i.checked_sub(n)?;
                            let len = super::stream_processor::read_varint(packet, at);
                            if len.length != n as i32 { return None; }
                            let end = at + framing::frame_size(len.value, len.length)?;
                            decompress(packet.get(i..end)?, call, total)?;
                            Some(end)
                        });
                        i = end.unwrap_or(i + 1);
                    }
                },
                _ => {},
            }
            if call.is_exhausted() || total.is_exhausted() { break; }
        }
    }
    let mut call = framing::DecompressionBudget::default();
    visit(buffer, true, 0, &mut call, total);
    !call.is_exhausted() && !total.is_exhausted()
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
fn packets_in(record: &[u8], budget: &mut framing::DecompressionBudget) -> Option<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    for_each_packet(record, &mut |packet| out.push(packet.to_vec()), budget)?;
    Some(out)
}

/// What every name field of the records holds (event packets aside): where
/// the leak checks look for names of any length, a one-byte name too. Where a
/// record holds a name, a chance match is no concern.
fn name_field_contents(records: &[(i32, Vec<u8>)]) -> Option<HashSet<Vec<u8>>> {
    let mut out = HashSet::new();
    let mut budget = framing::DecompressionBudget::new(128 * 1024 * 1024, 65_536);
    for (_, record) in records {
        for packet in packets_in(record, &mut budget)? {
            if is_event(&packet) {
                continue;
            }
            for field in super::stream_processor::name_fields(&packet) {
                out.insert(packet[field.range].to_vec());
            }
        }
    }
    Some(out)
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
    expand_checked(record, &mut framing::DecompressionBudget::default()).unwrap_or_default()
}

/// Expand only if every nested bundle is readable within the shared budget.
/// Privacy verification must use this result rather than treating a diagnostic
/// `expand` that returned no bytes as proof that no name exists inside it.
pub fn expand_checked(record: &[u8], budget: &mut framing::DecompressionBudget) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for_each_packet(record, &mut |packet| out.extend_from_slice(packet), budget)?;
    Some(out)
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
    let mut blinder = Blinder::exempting(blind_map.keys().map(|t| t.as_bytes().to_vec()).collect());
    let mut found = 0;
    let mut budget = framing::DecompressionBudget::new(128 * 1024 * 1024, 65_536);
    for (_, record) in records {
        let complete = for_each_packet(record, &mut |packet| {
            found += blinder.blind_name_shaped(&mut packet.to_vec());
        }, &mut budget);
        // An unread bundle cannot be certified safe. Preserve this count API's
        // zero-means-safe contract even when the verifier cannot finish.
        if complete.is_none() { return found.saturating_add(1); }
    }
    found
}

/// How many of `names` appear anywhere in the slice, bundles decompressed:
/// the client's own verifier, run again on the server with the names the
/// upload says it showed.
///
/// One walk over the slice and one pass over each record for every name at
/// once: looking for each name in turn over every record took 8-31 seconds
/// for 200 names on a 10-18 MB fight, past the service's CPU limit
/// (2026-10-28). Each record is expanded as `expand` does it (its packets,
/// bundles opened, end to end), the name fields read from the same packets
/// as `name_field_contents` reads them, so the count is the one looking for
/// each name in turn gave (the tests keep that as the reference).
pub fn leaked_names(records: &[(i32, Vec<u8>)], names: &[String]) -> usize {
    let names: Vec<&[u8]> = names.iter().map(|n| n.as_bytes()).filter(|n| !n.is_empty()).collect();
    if names.is_empty() {
        return 0;
    }
    let needles: Vec<(usize, Vec<u8>)> = names.iter().enumerate()
        .filter_map(|(i, n)| name_needle(n).map(|needle| (i, needle)))
        .collect();
    let finder = match needles.is_empty() {
        true => None,
        false => AhoCorasick::new(needles.iter().map(|(_, needle)| needle)).ok(),
    };
    let mut found = vec![false; names.len()];
    let mut fields: HashSet<Vec<u8>> = HashSet::new();
    let mut plain = Vec::new();
    let mut budget = framing::DecompressionBudget::new(128 * 1024 * 1024, 65_536);
    for (_, record) in records {
        plain.clear();
        let complete = for_each_packet(record, &mut |packet| {
            if !is_event(packet) {
                for field in super::stream_processor::name_fields(packet) {
                    fields.insert(packet[field.range].to_vec());
                }
            }
            plain.extend_from_slice(packet);
        }, &mut budget);
        if complete.is_none() { return names.len().max(1); }
        match &finder {
            Some(finder) => {
                for m in finder.find_overlapping_iter(&plain) {
                    found[needles[m.pattern().as_usize()].0] = true;
                }
            }
            // Never pass unchecked: one name at a time, as before.
            None => {
                for (i, name) in names.iter().enumerate() {
                    found[i] = found[i] || name_in(&plain, name);
                }
            }
        }
    }
    names.iter().zip(&found).filter(|(n, found)| **found || fields.contains(**n)).count()
}

/// Each packet of a record, bundles opened, in order: what `packets_in`
/// lists and `expand` joins, without a copy of each.
fn for_each_packet(record: &[u8], visit: &mut dyn FnMut(&[u8]), budget: &mut framing::DecompressionBudget) -> Option<()> {
    fn walk_into(buffer: &[u8], visit: &mut dyn FnMut(&[u8]), depth: usize, top: bool,
        budget: &mut framing::DecompressionBudget) -> Option<()> {
        if depth > MAX_BUNDLE_DEPTH {
            return None;
        }
        let frames = if top { framing::walk(buffer).frames } else { framing::walk_inner(buffer).frames };
        for frame in frames {
            match frame.kind {
                FrameKind::Packet => visit(frame.bytes(buffer)),
                FrameKind::Bundle => {
                    if depth >= MAX_BUNDLE_DEPTH { return None; }
                    let inner = budget.decompress(frame.payload(buffer))?;
                    walk_into(&inner, visit, depth + 1, false, budget)?;
                }
            }
        }
        Some(())
    }
    walk_into(record, visit, 0, true, budget)
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
/// What `name_in` looks for: the name, or under `SHORT_NAME_BYTES` the name
/// after its length byte. None for a name too short to look for.
fn name_needle(name: &[u8]) -> Option<Vec<u8>> {
    if name.len() < MIN_NAME_BYTES {
        return None;
    }
    if name.len() >= SHORT_NAME_BYTES {
        return Some(name.to_vec());
    }
    Some(std::iter::once(name.len() as u8).chain(name.iter().copied()).collect())
}

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
    if keep == Keep::State && (EVENT_OPCODES.contains(&op) || PRELUDE_SKIPPED.contains(&op)) {
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
    budget: &mut framing::DecompressionBudget,
    call_budget: &mut framing::DecompressionBudget,
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
                packet = blinder.canonical_roster(packet);
                stats.names_blinded += blinder.blind(&mut packet);
                stats.packets_kept += 1;
                stats.bytes_kept += packet.len();
                out.extend_from_slice(&packet);
            }
            FrameKind::Bundle => {
                let payload = frame.payload(buffer);
                if budget.charge(payload).is_none() { continue; }
                if let Some(nested) = call_budget.decompress(payload) {
                    stats.bundles_expanded += 1;
                    out.extend_from_slice(&filter_bundle_inner(&nested, blinder, stats, depth + 1, keep, budget, call_budget));
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
    budget: &mut framing::DecompressionBudget,
    call_budget: &mut framing::DecompressionBudget,
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
                packet = blinder.canonical_roster(packet);
                stats.names_blinded += blinder.blind(&mut packet);
                stats.packets_kept += 1;
                stats.bytes_kept += packet.len();
                out.push(packet);
            }
            FrameKind::Bundle => {
                let payload = frame.payload(buffer);
                if budget.charge(payload).is_none() { continue; }
                let Some(decompressed) = call_budget.decompress(payload) else {
                    continue;
                };
                stats.bundles_expanded += 1;
                let inner = filter_bundle_inner(&decompressed, blinder, stats, 1, keep, budget, call_budget);
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

/// Also replace copies of a recognized roster identity outside its primary
/// field. The mapping is complete before this pass, so a copy that preceded
/// its roster is covered too. Bundles retain their packet/context boundaries.
fn rewrite_roster_ids(records: &mut [(i32, Vec<u8>)], ids: &HashMap<u64, u64>) -> Result<(), SliceError> {
    if ids.is_empty() { return Ok(()); }
    let mut entries: Vec<_> = ids.iter().map(|(&raw, &id)| (raw.to_le_bytes(), id.to_le_bytes())).collect();
    entries.sort_by_key(|(raw, _)| *raw);
    let finder = AhoCorasick::new(entries.iter().map(|(raw, _)| raw)).map_err(|_| SliceError::RosterIdentity)?;
    fn rewrite(buffer: &[u8], top: bool, depth: usize, finder: &AhoCorasick,
        entries: &[([u8; 8], [u8; 8])], budget: &mut framing::DecompressionBudget) -> Option<Vec<u8>>
    {
        if depth > MAX_BUNDLE_DEPTH { return None; }
        let frames = if top { framing::walk(buffer).frames } else { framing::walk_inner(buffer).frames };
        let mut out = Vec::new();
        for frame in frames {
            match frame.kind {
                FrameKind::Packet => {
                    let packet = frame.bytes(buffer).to_vec();
                    let mut packet = match super::stream_processor::roster_body_for_sharing(&packet).ok()? {
                        Some(body) => frame_packet(&body)?,
                        None => packet,
                    };
                    let matches: Vec<_> = finder.find_iter(&packet).map(|m| (m.start(), m.pattern().as_usize())).collect();
                    for (at, index) in matches { packet[at..at + 8].copy_from_slice(&entries[index].1); }
                    if finder.is_match(&packet) { return None; }
                    out.extend_from_slice(&packet);
                },
                FrameKind::Bundle => {
                    let inner = budget.decompress(frame.payload(buffer))?;
                    let inner = rewrite(&inner, false, depth + 1, finder, entries, budget)?;
                    out.extend_from_slice(&rewrap_bundle(&inner)?);
                },
            }
        }
        Some(out)
    }
    let mut budget = framing::DecompressionBudget::new(128 * 1024 * 1024, 65_536);
    for (_, record) in records {
        *record = rewrite(record, true, 0, &finder, &entries, &mut budget).ok_or(SliceError::RosterIdentity)?;
    }
    Ok(())
}

/// Upgrade the privacy of an already stored slice before it leaves the PC.
/// Old cached slices bypass `build`, so their payload identities and metadata
/// must be treated just like a newly captured roster at the upload boundary.
pub fn anonymize_stored_slice(reader: impl std::io::Read, key: SliceKey) -> Result<Vec<u8>, SliceError> {
    let (mut records, mut blind_map) = decode_from(reader, 32 * 1024 * 1024).ok_or(SliceError::RosterIdentity)?;
    let ordered: Vec<_> = blind_map.iter().collect();
    let mut blinder = Blinder::new(key, &ordered);
    let mut budget = framing::DecompressionBudget::new(128 * 1024 * 1024, 65_536);
    for (_, record) in &records {
        for_each_packet(record, &mut |packet| {
            let canonical = match super::stream_processor::roster_body_for_sharing(packet) {
                Ok(body) => body.and_then(|body| frame_packet(&body)),
                Err(()) => { blinder.invalid_roster = true; None },
            };
            let packet = canonical.as_deref().unwrap_or(packet);
            blinder.unsupported_embedded |= contains_embedded_bundle(packet);
            match super::stream_processor::roster_identity_fields(packet) {
                Ok(fields) => for field in fields {
                    let raw = u64::from_le_bytes(packet[field].try_into().unwrap());
                    blinder.roster_id(raw);
                },
                Err(()) => blinder.invalid_roster = true,
            }
        }, &mut budget).ok_or(SliceError::ExpansionLimit)?;
    }
    if blinder.invalid_roster { return Err(SliceError::RosterIdentity); }
    if blinder.unsupported_embedded { return Err(SliceError::EmbeddedIdentity); }
    rewrite_roster_ids(&mut records, &blinder.roster_ids)?;
    for id in blind_map.values_mut() { *id = 0; }
    Ok(encode(&EvidenceSlice { records, blind_map, stats: SliceStats::default() }))
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
    for (name, _dbid) in ordered.iter().copied() {
        if let Some(token) = blinder.token_of(name) {
            blind_map.insert(String::from_utf8_lossy(token).into_owned(), 0);
        }
    }

    let mut stats = SliceStats::default();
    let mut records: Vec<(i32, Vec<u8>)> = Vec::new();

    // Reassemble per stream before framing. Every segment is appended even when
    // it falls outside the fight window, because dropping one would leave the
    // next segment starting mid-packet — the window selects what is *kept*, not
    // what is parsed.
    let mut streams: HashMap<&str, PacketAccumulator> = HashMap::new();
    let mut budget = framing::DecompressionBudget::new(128 * 1024 * 1024, 65_536);
    let mut preflight_budget = framing::DecompressionBudget::new(128 * 1024 * 1024, 65_536);

    for cap in packets {
        let acc = streams
            .entry(cap.stream.as_str())
            .or_insert_with(PacketAccumulator::new);
        acc.append(&cap.bytes);

        // Before the lead-in, state only; a segment is still framed either way
        // so the stream stays in step.
        let keep = if cap.captured_at_ms < from { Keep::State } else { Keep::All };
        let mut kept = Vec::new();
        if !input_expansion_fits(acc.snapshot(), &mut preflight_budget) { return Err(SliceError::ExpansionLimit); }
        let mut call_budget = framing::DecompressionBudget::default();
        let consumed = filter_stream(acc.snapshot(), &mut blinder, &mut kept, &mut stats, keep, &mut budget, &mut call_budget);
        acc.discard_bytes(consumed);
        if budget.is_exhausted() || call_budget.is_exhausted() { return Err(SliceError::ExpansionLimit); }

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
    if blinder.invalid_roster { return Err(SliceError::RosterIdentity); }
    if blinder.unsupported_embedded { return Err(SliceError::EmbeddedIdentity); }
    rewrite_roster_ids(&mut records, &blinder.roster_ids)?;

    // The verifier. Everything above is pattern work; this is what we actually
    // rely on. A name that reaches here means the slice is not safe to send, and
    // the answer is to discard it, not to send it anyway with a warning.
    //
    // Scans the *decompressed* content: records may be re-compressed bundles,
    // and searching a compressed buffer for a plaintext name finds nothing no
    // matter what is inside it. A verifier that passes vacuously is worse than
    // no verifier, because it is believed.
    //
    // Every name in one pass over each record (see `Blinder::finder`); the
    // name reported is still the first in `ordered` that was found.
    let needles: Vec<(usize, Vec<u8>)> = ordered.iter().enumerate()
        .filter_map(|(i, (name, _))| name_needle(name.as_bytes()).map(|needle| (i, needle)))
        .collect();
    let mut verification_budget = framing::DecompressionBudget::new(128 * 1024 * 1024, 65_536);
    let plaintext: Vec<Vec<u8>> = records.iter()
        .map(|(_, packet)| expand_checked(packet, &mut verification_budget))
        .collect::<Option<_>>().ok_or(SliceError::ExpansionLimit)?;
    let leaked = match AhoCorasick::new(needles.iter().map(|(_, needle)| needle)) {
        Ok(finder) => plaintext.iter()
            .filter_map(|packet| {
                finder.find_overlapping_iter(packet).map(|m| needles[m.pattern().as_usize()].0).min()
            })
            .min(),
        // Never pass unchecked: one name at a time, as before.
        Err(_) => {
            ordered.iter().position(|(name, _)| plaintext.iter().any(|buf| name_in(buf, name.as_bytes())))
        }
    };
    if let Some(first) = leaked {
        return Err(SliceError::NameLeaked(ordered[first].0.len()));
    }
    // Every name, a one-byte name too, where a record holds one.
    let fields = name_field_contents(&records).ok_or(SliceError::ExpansionLimit)?;
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
/// 3: a bundle record is stored unpacked (its inner stream, not LZ4), so the
/// slice's own compression can reach it: most of a slice is bundles, and LZ4
/// output does not compress again (37% smaller than gzipped v2 with zstd, on
/// real fights, 2026-10-16). Read back, it is wrapped as before.
pub const VERSION: u16 = 3;

/// A version 3 record: the packet as it is, or a bundle's inner stream.
const RECORD_PACKET: u8 = 0;
const RECORD_BUNDLE: u8 = 1;

/// The inner stream of a bundle record, or None for any other packet.
fn bundle_inner(packet: &[u8]) -> Option<Vec<u8>> {
    let len = super::stream_processor::read_varint(packet, 0);
    let o = usize::try_from(len.length).ok().filter(|&n| n > 0)?;
    let payload = packet.get(o..)?;
    if payload.get(..2)? != [0xFF, 0xFF] {
        return None;
    }
    let inner = framing::decompress_bundle(payload)?;
    // Only when it wraps back to the same bytes: the record then reads back
    // exactly as it went in.
    (rewrap_bundle(&inner).as_deref() == Some(packet)).then_some(inner)
}

/// Serialise to the `.a2es` container.
///
/// ```text
/// "A2ES" | u16 version | u16 opcode_count | [u16 opcode]*
///        | u16 blind_count | [u8 token_len | token bytes | u64 dbid]*
///        | u32 record_count
///        | [ i32 dt_ms | u8 kind | u32 len | bytes ]*      (version 3)
/// ```
///
/// `kind` 0 is the packet as it is, 1 a bundle's inner stream (see VERSION).
/// Versions 1 and 2 had `u16 len | bytes` and no kind.
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
        let (kind, bytes) = match bundle_inner(packet) {
            Some(inner) => (RECORD_BUNDLE, std::borrow::Cow::Owned(inner)),
            None => (RECORD_PACKET, std::borrow::Cow::Borrowed(packet.as_slice())),
        };
        out.push(kind);
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(&bytes);
    }
    out
}

/// Read a `.a2es` back. Used by the server, and by `a2t-inspect` so a user can
/// see exactly what an upload would contain.
pub fn decode(data: &[u8]) -> Option<(Vec<(i32, Vec<u8>)>, HashMap<String, u64>)> {
    decode_from(data, usize::MAX)
}

/// `decode` from a stream, so a compressed slice is read as it is unpacked,
/// never whole. None once the records come to more than `max_record_bytes` as
/// they read back: a version 3 bundle is counted wrapped, as version 2 stored
/// it, so one fight has one size in every version and a limit on it takes or
/// refuses the fight whichever version carries it.
/// Containers also have independent hard limits on wire bytes and record count;
/// empty records are refused before they can bypass the payload-byte limit.
pub fn decode_from(mut r: impl std::io::Read, max_record_bytes: usize) -> Option<(Vec<(i32, Vec<u8>)>, HashMap<String, u64>)> {
    use std::io::Read;
    // Payload bytes alone do not bound the container: an empty v3 record
    // occupies nine wire bytes and still reserves an entry in `records`.
    // Keep both the record count and all bytes read finite, including tables.
    const MAX_RECORDS: usize = 1 << 20;
    const MAX_CONTAINER_BYTES: usize = 128 * 1024 * 1024;
    const MAX_METADATA_BYTES: usize = 32 * 1024 * 1024;
    let max_record_bytes = max_record_bytes.min(MAX_CONTAINER_BYTES);
    let max_container_bytes = max_record_bytes.saturating_add(MAX_METADATA_BYTES).min(MAX_CONTAINER_BYTES);
    let mut r = r.by_ref().take(max_container_bytes as u64);
    fn array<const N: usize>(r: &mut impl Read) -> Option<[u8; N]> {
        let mut b = [0u8; N];
        r.read_exact(&mut b).ok()?;
        Some(b)
    }
    // Read as far as the data goes, so a length that lies allocates nothing.
    fn bytes<R: Read>(r: &mut R, n: usize) -> Option<Vec<u8>> {
        let mut v = Vec::new();
        r.by_ref().take(n as u64).read_to_end(&mut v).ok()?;
        (v.len() == n).then_some(v)
    }
    if array::<4>(&mut r)? != *MAGIC {
        return None;
    }
    let version = u16::from_le_bytes(array(&mut r)?);
    if !(1..=VERSION).contains(&version) {
        return None;
    }
    let op_count = u16::from_le_bytes(array(&mut r)?) as usize;
    bytes(&mut r, op_count * 2)?;

    let blind_count = u16::from_le_bytes(array(&mut r)?) as usize;
    let mut blind_map = HashMap::with_capacity(blind_count.min(1024));
    for _ in 0..blind_count {
        let len = array::<1>(&mut r)?[0] as usize;
        let token = String::from_utf8(bytes(&mut r, len)?).ok()?;
        let dbid = u64::from_le_bytes(array(&mut r)?);
        blind_map.insert(token, dbid);
    }

    let rec_count = u32::from_le_bytes(array(&mut r)?) as usize;
    if rec_count > MAX_RECORDS { return None; }
    let mut records = Vec::with_capacity(rec_count.min(4096));
    let mut total = 0usize;
    let mut budget = framing::DecompressionBudget::new(128 * 1024 * 1024, 65_536);
    for _ in 0..rec_count {
        let dt = i32::from_le_bytes(array(&mut r)?);
        let record = if version >= 3 {
            let kind = array::<1>(&mut r)?[0];
            let len = u32::from_le_bytes(array(&mut r)?) as usize;
            if len == 0 || len > max_record_bytes
                || (kind == RECORD_PACKET && len > max_record_bytes.saturating_sub(total)) {
                return None;
            }
            let stored = bytes(&mut r, len)?;
            match kind {
                RECORD_PACKET => stored,
                RECORD_BUNDLE => rewrap_bundle(&stored)?,
                _ => return None,
            }
        } else {
            let len = u16::from_le_bytes(array(&mut r)?) as usize;
            if len == 0 || len > max_record_bytes.saturating_sub(total) { return None; }
            let record = bytes(&mut r, len)?;
            if version == 1 { upgrade_v1_record(&record, &mut budget)? } else { record }
        };
        total = total.checked_add(record.len())?;
        if total > max_record_bytes {
            return None;
        }
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
fn upgrade_v1_record(record: &[u8], budget: &mut framing::DecompressionBudget) -> Option<Vec<u8>> {
    fn reframe(buf: &[u8], top: bool, depth: usize, budget: &mut framing::DecompressionBudget) -> Option<Vec<u8>> {
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
            if bundle {
                if depth >= MAX_BUNDLE_DEPTH { return None; }
                let inner = budget.decompress(body)?;
                let inner = reframe(&inner, false, depth + 1, budget)?;
                out.extend_from_slice(&rewrap_bundle(&inner)?);
            } else {
                out.extend_from_slice(&frame_packet(body)?);
            }
            o += size;
        }
        out.extend_from_slice(&buf[o..]);
        Some(out)
    }
    reframe(record, true, 0, budget)
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

    #[test]
    fn the_prelude_carries_no_buff_changes() {
        for op in PRELUDE_SKIPPED {
            let buff = frame_packet(&[op[0], op[1], 0x01, 0x02, 0x03]).unwrap();
            assert!(!is_allowed(&buff, Keep::State));
            assert!(is_allowed(&buff, Keep::All));
        }
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

    fn identity_roster(ids: [u64; 2]) -> Vec<u8> {
        let mut body = vec![0x02, 0x97];
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(&[1, b'.', 2]);
        body.extend_from_slice(&600091u32.to_le_bytes());
        body.extend_from_slice(&[0; 2]);
        body.extend_from_slice(&ids[0].to_le_bytes());
        body.extend_from_slice(&[0, 0, 0, 2]);
        for (i, (id, name)) in ids.into_iter().zip([b"Alice", b"Bobby"]).enumerate() {
            body.extend_from_slice(&[0x0c, i as u8 + 1]);
            body.extend_from_slice(&id.to_le_bytes());
            body.push(name.len() as u8);
            body.extend_from_slice(name);
            body.extend_from_slice(&1u32.to_le_bytes());
            body.extend_from_slice(&30u32.to_le_bytes());
            body.extend_from_slice(&100u32.to_le_bytes());
            body.extend_from_slice(&((id >> 48) as u16).to_le_bytes());
            body.extend_from_slice(&[0, 0, 1]);
            body.extend_from_slice(&1000u64.to_le_bytes());
        }
        frame_packet(&body).unwrap()
    }

    #[test]
    fn serialized_slices_replace_roster_ids_and_earlier_copies_with_per_slice_values() {
        let ids = [0x0519_12b3_56d7_89efu64, 0x051a_9087_6543_210fu64];
        let roster = identity_roster(ids);
        let copy = frame_packet(&[&[0x04, 0x38][..], &ids[0].to_le_bytes()].concat()).unwrap();
        let bundle = rewrap_bundle(&roster).unwrap();
        let packets = [CapturedPacket { captured_at_ms: 0, stream: "Client:1".into(), bytes: [copy, bundle].concat() }];
        let slice = build(&packets, 0, 10, &NameMap::new(), KEY).unwrap();
        let encoded = encode(&slice);
        for id in ids {
            assert!(!encoded.windows(8).any(|b| b == id.to_le_bytes()), "stable identity survived final serialization");
        }
        let (records, map) = decode(&encoded).unwrap();
        assert!(map.values().all(|&id| id == 0));
        let plain = expand(&records[1].1);
        let fields = super::super::stream_processor::roster_identity_fields(&plain).unwrap();
        let anonymized: Vec<_> = fields.iter().map(|r| u64::from_le_bytes(plain[r.clone()].try_into().unwrap())).collect();
        assert_eq!(anonymized.len(), 3);
        assert_eq!(anonymized[0], anonymized[1], "the leader/member relationship survives replay");
        assert_ne!(anonymized[1], anonymized[2]);
        assert_eq!(anonymized[1] >> 48, ids[0] >> 48);
        assert_eq!(anonymized[2] >> 48, ids[1] >> 48);
        assert!(expand(&records[0].1).windows(8).any(|b| b == anonymized[1].to_le_bytes()));
        let other = build(&packets, 0, 10, &NameMap::new(), [8; 32]).unwrap();
        assert_ne!(expand(&other.records[1].1), plain, "identities cannot be joined across slices");
        assert_eq!(unblinded_names(&records, &map), 0);
    }

    #[test]
    fn an_incomplete_retained_roster_is_refused() {
        assert!(matches!(build(&[cap(0, &[0x02, 0x97, 1, 2, 3])], 0, 10, &NameMap::new(), KEY), Err(SliceError::RosterIdentity)));
    }

    #[test]
    fn an_early_last_slot_cannot_conceal_another_members_identity() {
        let ids = [0x0519_12b3_56d7_89efu64, 0x051a_9087_6543_210fu64];
        for slot in [2, 255] {
            let mut roster = identity_roster(ids);
            let fields = super::super::stream_processor::roster_identity_fields(&roster).unwrap();
            roster[fields[1].start - 1] = slot;
            let packets = [CapturedPacket { captured_at_ms: 0, stream: "Client:1".into(), bytes: roster }];
            assert!(matches!(build(&packets, 0, 10, &NameMap::new(), KEY), Err(SliceError::RosterIdentity)));
        }
    }

    #[test]
    fn unknown_roster_tails_are_excluded_from_the_serialized_slice() {
        let ids = [0x0519_12b3_56d7_89efu64, 0x051a_9087_6543_210fu64];
        let third = 0x051b_9087_6543_216fu64;
        let roster = identity_roster(ids);
        let length = super::super::stream_processor::read_varint(&roster, 0).length as usize;
        let mut body = roster[length..].to_vec();
        body.extend_from_slice(&third.to_le_bytes());
        let slice = slice_of(&[frame_packet(&body).unwrap()], &NameMap::new());
        assert!(!encode(&slice).windows(8).any(|b| b == third.to_le_bytes()));
    }

    #[test]
    fn splitting_one_capture_into_records_cannot_bypass_its_live_expansion_limit() {
        let mut inner = frame_packet(&[0x04, 0x38, 0]).unwrap();
        inner.resize(1_000_000, 0);
        let bundle = rewrap_bundle(&inner).unwrap();
        let cap = CapturedPacket { captured_at_ms: 0, stream: "Client:1".into(), bytes: bundle.repeat(9) };
        assert!(matches!(build(&[cap], 0, 10, &NameMap::new(), KEY), Err(SliceError::ExpansionLimit)));
        // A discarded host's embedded identity blocks also consume the live
        // input budget before the kept damage bundle that follows it.
        let mut host = vec![0x99, 0x36];
        host.extend_from_slice(&bundle.repeat(9));
        let bytes = [frame_packet(&host).unwrap(), bundle].concat();
        let cap = CapturedPacket { captured_at_ms: 0, stream: "Client:1".into(), bytes };
        assert!(matches!(build(&[cap], 0, 10, &NameMap::new(), KEY), Err(SliceError::ExpansionLimit)));
    }

    #[test]
    fn a_retained_packet_with_embedded_compressed_identity_is_refused() {
        let roster = identity_roster([0x0519_12b3_56d7_89ef, 0x051a_9087_6543_210f]);
        let mut host = vec![0x04, 0x38, 0];
        host.extend_from_slice(&rewrap_bundle(&roster).unwrap());
        let packets = [CapturedPacket { captured_at_ms: 0, stream: "Client:1".into(), bytes: frame_packet(&host).unwrap() }];
        assert!(matches!(build(&packets, 0, 10, &NameMap::new(), KEY), Err(SliceError::EmbeddedIdentity)));
    }

    #[test]
    fn overlapping_known_names_are_collected_once_per_pattern() {
        let names: NameMap = (2..=16).map(|n| ("A".repeat(n), 0)).collect();
        let ordered: Vec<_> = names.iter().collect();
        let blinder = Blinder::new(KEY, &ordered);
        assert_eq!(blinder.present(&vec![b'A'; 1024 * 1024]), (0..ordered.len()).collect::<Vec<_>>());
    }

    #[test]
    fn a_cached_slice_is_reanonymized_without_changing_its_name_tokens() {
        let ids = [0x0519_12b3_56d7_89efu64, 0x051a_9087_6543_210fu64];
        let mut roster = identity_roster(ids);
        Blinder::new(KEY, &[]).blind(&mut roster);
        let fields = super::super::stream_processor::roster_identity_fields(&roster).unwrap();
        for (field, raw) in fields.into_iter().zip([ids[0], ids[0], ids[1]]) {
            roster[field].copy_from_slice(&raw.to_le_bytes());
        }
        let records = vec![(0, rewrap_bundle(&roster).unwrap())];
        let before = name_field_contents(&records).unwrap();
        let old = encode(&EvidenceSlice { records, blind_map: HashMap::from([("token".into(), ids[0])]), stats: Default::default() });
        let migrated = anonymize_stored_slice(&old[..], [9; 32]).unwrap();
        for id in ids { assert!(!migrated.windows(8).any(|b| b == id.to_le_bytes())); }
        let (records, map) = decode(&migrated).unwrap();
        assert_eq!(name_field_contents(&records).unwrap(), before);
        assert!(map.values().all(|&id| id == 0));
    }

    /// `blinded` is `source` but for its names: the name fields given, and
    /// the runs a blinder that knows no names changes (text shaped like a
    /// name, which the log service also counts as one).
    fn only_names_changed(source: &[u8], blinded: &[u8], fields: &[std::ops::Range<usize>]) {
        let canonical = super::super::stream_processor::roster_body_for_sharing(source).unwrap()
            .and_then(|body| frame_packet(&body));
        let source = canonical.as_deref().unwrap_or(source);
        let canonical_fields: Vec<_> = super::super::stream_processor::name_fields(source).into_iter().map(|f| f.range).collect();
        let fields = if canonical.is_some() { &canonical_fields[..] } else { fields };
        assert_eq!(blinded.len(), source.len());
        let mut shaped = source.to_vec();
        Blinder::new(KEY, &[]).blind_name_shaped(&mut shaped);
        let identities = super::super::stream_processor::roster_identity_fields(source).unwrap_or_default();
        for (i, (b, s)) in blinded.iter().zip(source).enumerate() {
            if !fields.iter().chain(&identities).any(|f| f.contains(&i)) && shaped[i] == *s {
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
    /// The names found in one pass give what looking for each in turn gave:
    /// the same bytes blinded, and the same answer to "does a known name
    /// overlap here?". Names that contain each other, and a small alphabet so
    /// they turn up often and next to each other.
    #[test]
    fn one_pass_over_the_names_blinds_as_one_name_at_a_time_did() {
        fn one_at_a_time(known: &[(Vec<u8>, Vec<u8>)], buf: &mut [u8]) -> usize {
            let mut replaced = 0;
            for (needle, token) in known {
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
        fn overlaps(known: &[(Vec<u8>, Vec<u8>)], buf: &[u8], from: usize, to: usize) -> bool {
            known.iter().any(|(name, _)| {
                let n = name.len();
                let start = (from + 1).saturating_sub(n);
                let end = (to + n - 1).min(buf.len());
                end >= start + n && buf[start..end].windows(n).any(|w| w == name.as_slice())
            })
        }
        let names: Vec<String> = ["Misti", "Mistifix", "ab", "abc", "bca", "cab", "abcab", "ba", "c2", "Nyx"]
            .iter().map(|n| n.to_string()).collect();
        let ids: Vec<u64> = (0..names.len() as u64).collect();
        let mut ordered: Vec<(&String, &u64)> = names.iter().zip(ids.iter()).collect();
        ordered.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(b.0)));
        let blinder = Blinder::new(KEY, &ordered);
        assert!(blinder.finder.is_some());

        // Names, parts of names and length bytes, run together.
        let pieces: [&[u8]; 14] = [b"Misti", b"fix", b"ab", b"c", b"a", b"b", b"Mist", b"Nyx", b"2", b"\x02", b"\x03", b"\x05", b"\x08", b"z"];
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let (mut blinded, mut overlapping) = (0, 0);
        for _ in 0..3000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let buf: Vec<u8> = (0..1 + seed % 10).flat_map(|i| pieces[(seed >> (i * 5)) as usize % pieces.len()].iter().copied()).collect();
            let len = buf.len();

            let mut expected = buf.clone();
            let mut got = buf.clone();
            let replaced = blinder.blind_known(&mut got);
            assert_eq!(replaced, one_at_a_time(&blinder.known, &mut expected), "{buf:?}");
            blinded += replaced;
            assert_eq!(got, expected, "{buf:?}");

            let from = (seed >> 8) as usize % len;
            let to = (from + 1 + (seed >> 16) as usize % 6).min(len);
            let spells = blinder.spells_known_name(&buf, from, to);
            assert_eq!(spells, overlaps(&blinder.known, &buf, from, to), "{buf:?} {from}..{to}");
            overlapping += spells as usize;
        }
        assert!(blinded > 300 && overlapping > 300, "too few names to compare: {blinded} blinded, {overlapping} overlapping");
    }

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

    /// `leaked_names` as it was: each name looked for in turn.
    fn leaked_names_one_at_a_time(records: &[(i32, Vec<u8>)], names: &[String]) -> usize {
        let plaintext: Vec<Vec<u8>> = records.iter().map(|(_, p)| expand(p)).collect();
        let fields = name_field_contents(records).expect("complete traversal");
        names
            .iter()
            .map(|n| n.as_bytes())
            .filter(|n| !n.is_empty())
            .filter(|n| fields.contains(*n) || plaintext.iter().any(|buf| name_in(buf, n)))
            .count()
    }

    #[test]
    fn one_pass_over_the_names_counts_leaks_as_one_name_at_a_time_did() {
        let named: Vec<Vec<u8>> = records_with_names().into_iter().map(|(r, _)| r).collect();
        let mut records: Vec<(i32, Vec<u8>)> = named.iter().cloned().map(|r| (0, r)).collect();
        records.push((5, spawn_named("Grandine")));
        records.push((6, spawn_after_8f_01()));
        records.push((7, vec![0x05, 0x10, b'J', b'o', 0x22, 0x01]));
        // The named records again inside a bundle, a name across two packets too.
        let inner: Vec<u8> = named.concat().into_iter().chain(spawn_named("Misti")).collect();
        records.push((8, rewrap_bundle(&inner).unwrap()));
        let lists: Vec<Vec<&str>> = vec![
            vec![],
            vec![""],
            vec!["Nobody", "Q", "ab"],
            vec!["Xy", "A", "B", "Cd", "Ef", "Grandine", "Misti", "Jo", "", "Grandine", "Q", "Zz", "Nobody"],
            vec!["Misti", "Misti", "Mist", "isti", "A"],
        ];
        for list in lists {
            let names: Vec<String> = list.iter().map(|s| s.to_string()).collect();
            for upto in [1, 6, records.len()] {
                let records = &records[..upto];
                assert_eq!(leaked_names(records, &names), leaked_names_one_at_a_time(records, &names), "{list:?} {upto}");
            }
        }
        let all = ["Xy", "A", "B", "Cd", "Ef", "Grandine", "Misti"].map(String::from);
        assert!(leaked_names(&records, &all) > 0);
    }

    /// The same, over real slices: `A2ES_SLICES=<dir> cargo test -- --ignored`.
    /// Names are each slice's own tokens, which are in it, and some that are not.
    #[test]
    #[ignore]
    fn one_pass_over_the_names_counts_leaks_as_one_name_at_a_time_did_on_real_slices() {
        use std::io::Read;
        let Ok(dir) = std::env::var("A2ES_SLICES") else { return };
        let mut paths: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
        paths.sort();
        let limit = std::env::var("A2ES_LIMIT").ok().and_then(|s| s.parse().ok()).unwrap_or(25);
        let (mut checked, mut found) = (0, 0);
        for path in paths.into_iter().take(limit) {
            let body = std::fs::read(&path).unwrap();
            let mut raw = Vec::new();
            let read = if body.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
                zstd::stream::read::Decoder::new(&body[..]).and_then(|mut d| d.read_to_end(&mut raw))
            } else {
                flate2::read::GzDecoder::new(&body[..]).read_to_end(&mut raw)
            };
            let Some((records, blind_map)) = read.ok().and_then(|_| decode(&raw)) else { continue };
            let mut names: Vec<String> = blind_map.keys().take(40).cloned().collect();
            names.extend(["Nobody", "ab", "Q", "Plyr0042"].map(String::from));
            let count = leaked_names(&records, &names);
            assert_eq!(count, leaked_names_one_at_a_time(&records, &names), "{path:?}");
            (checked, found) = (checked + 1, found + count);
        }
        eprintln!("{checked} slices, {found} names found in them");
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
        let mut payload = vec![0x04, 0x8D];
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
        let mut payload = vec![0x04, 0x8D, 0x01, 0x02, 0x03];
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
        assert!(blind_map.values().all(|&d| d == 0), "the metadata carries no stable roster ids");
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

    /// A slice in the version 1/2 layout (`u16 len`, no kind), as older
    /// meters wrote it.
    fn encode_legacy(slice: &EvidenceSlice, version: u16) -> Vec<u8> {
        let current = encode(&EvidenceSlice { records: Vec::new(), blind_map: slice.blind_map.clone(), stats: SliceStats::default() });
        let mut out = current[..current.len() - 4].to_vec();
        out[4..6].copy_from_slice(&version.to_le_bytes());
        out.extend_from_slice(&(slice.records.len() as u32).to_le_bytes());
        for (dt, packet) in &slice.records {
            out.extend_from_slice(&dt.to_le_bytes());
            out.extend_from_slice(&(packet.len() as u16).to_le_bytes());
            out.extend_from_slice(packet);
        }
        out
    }

    /// Version 3 stores a bundle unpacked and wraps it again on reading: the
    /// records come back byte for byte, and a version 2 slice of the same
    /// fight reads the same. The unpacked bundle is in the clear in the
    /// slice, where the outer compression can reach it.
    #[test]
    fn a_streamed_slice_is_limited_by_its_records_alike_in_every_version() {
        let bundle = rewrap_bundle(&[0x07, 0x00, 0x2A, 0x38, 1, 2, 3, 4]).expect("a bundle");
        let records = vec![(0, vec![0x07, 0x00, 0x2B, 0x38, 9, 9, 9]), (5, bundle)];
        let slice = EvidenceSlice { records: records.clone(), blind_map: HashMap::new(), stats: Default::default() };
        let size: usize = records.iter().map(|(_, r)| r.len()).sum();
        for bytes in [encode(&slice), encode_legacy(&slice, 2)] {
            assert_eq!(decode_from(&bytes[..], size).map(|(r, _)| r), Some(records.clone()));
            assert_eq!(decode_from(&bytes[..], size - 1), None);
        }
        // Cut short: no records invented, nothing read past the end.
        let v3 = encode(&slice);
        assert_eq!(decode_from(&v3[..v3.len() - 1], usize::MAX), None);
    }

    #[test]
    fn a_version_3_slice_reads_back_the_records_it_was_given() {
        let packet = frame_packet(&[0x04, 0x38, 0x01, 0x02, 0x03]).unwrap();
        let inner: Vec<u8> = [frame_packet(&[0x41, 0x36, 0x05]).unwrap(), frame_packet(&[0x2B, 0x38, 9, 9, 9, 9]).unwrap()].concat();
        let bundle = rewrap_bundle(&inner).unwrap();
        let slice = EvidenceSlice {
            records: vec![(0, packet.clone()), (40, bundle.clone()), (90, packet.clone())],
            blind_map: HashMap::from([("@a".to_string(), 7u64)]),
            stats: SliceStats::default(),
        };
        let bytes = encode(&slice);
        assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 3);
        assert!(bytes.windows(inner.len()).any(|w| w == inner.as_slice()), "the bundle is stored unpacked");
        let (records, blind) = decode(&bytes).expect("decodes");
        assert_eq!(records, slice.records);
        assert_eq!(blind.get("@a"), Some(&7));
        let (old, _) = decode(&encode_legacy(&slice, 2)).expect("a version 2 slice still decodes");
        assert_eq!(old, records);
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
        let bytes = encode_legacy(&slice, 1);

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
    fn expansion_shares_one_budget_across_records_and_nested_siblings() {
        let packet = frame_packet(&[0x04, 0x38, 1]).unwrap();
        let bundle = rewrap_bundle(&packet).unwrap();
        let mut budget = framing::DecompressionBudget::new(packet.len(), 10);
        assert_eq!(expand_checked(&bundle, &mut budget), Some(packet.clone()));
        assert!(expand_checked(&bundle, &mut budget).is_none());

        let siblings = [bundle.clone(), bundle].concat();
        let nested = rewrap_bundle(&siblings).unwrap();
        let mut budget = framing::DecompressionBudget::new(siblings.len() + packet.len(), 10);
        assert!(expand_checked(&nested, &mut budget).is_none(), "partial expansion must not certify a record");
    }

    #[test]
    fn name_verification_refuses_unreadable_or_over_nested_bundles() {
        let malformed = frame_packet(&[0xff, 0xff, 40, 0, 0, 0, 0xff]).unwrap();
        let records = vec![(0, malformed)];
        assert!(unblinded_names(&records, &HashMap::new()) > 0);
        assert!(leaked_names(&records, &["Alice".into()]) > 0);
        assert!(name_field_contents(&records).is_none());

        let mut nested = frame_packet(&[0x04, 0x38, 1]).unwrap();
        for _ in 0..=MAX_BUNDLE_DEPTH { nested = rewrap_bundle(&nested).unwrap(); }
        assert!(expand_checked(&nested, &mut framing::DecompressionBudget::default()).is_none());
        assert!(unblinded_names(&[(0, nested)], &HashMap::new()) > 0);
    }

    #[test]
    fn legacy_upgrade_cannot_retain_a_bundle_it_could_not_expand() {
        // The old one-byte inner frame spans len - 3 bytes; a top-level
        // bundle spans len - 2. No fallback may return its compressed bytes.
        let inner = [7, 0x04, 0x38, 1];
        let mut payload = vec![0xff, 0xff];
        payload.extend_from_slice(&(inner.len() as u32).to_le_bytes());
        payload.extend_from_slice(&lz4_flex::compress(&inner));
        let mut old = vec![(payload.len() + 3) as u8];
        old.extend_from_slice(&payload);
        let mut budget = framing::DecompressionBudget::new(0, 10);
        assert!(upgrade_v1_record(&old, &mut budget).is_none());
    }

    #[test]
    fn decode_refuses_unbounded_record_counts_before_reading_records() {
        struct HeaderOnly(std::io::Cursor<Vec<u8>>);
        impl std::io::Read for HeaderOnly {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                assert!(self.0.position() < self.0.get_ref().len() as u64,
                    "an impossible record count must be refused before reading a record");
                std::io::Read::read(&mut self.0, out)
            }
        }
        let mut header = MAGIC.to_vec();
        header.extend_from_slice(&VERSION.to_le_bytes());
        header.extend_from_slice(&[0, 0, 0, 0]); // no opcode or name table
        header.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode_from(HeaderOnly(std::io::Cursor::new(header)), 32 * 1024 * 1024).is_none());
    }

    #[test]
    fn zero_length_records_cannot_evade_the_decoded_size_limit() {
        let slice = EvidenceSlice {
            records: vec![(0, Vec::new()); 16],
            blind_map: HashMap::new(),
            stats: SliceStats::default(),
        };
        let bytes = encode(&slice);
        assert!(decode_from(&bytes[..], 1024).is_none());
        assert!(decode_from(&encode_legacy(&slice, 2)[..], 1024).is_none());
    }

    #[test]
    fn decode_refuses_something_that_is_not_a_slice() {
        assert!(decode(b"not a slice at all").is_none());
        assert!(decode(&[]).is_none());
        // A raw capture must not be mistaken for a slice.
        assert!(decode(&framed(&[0x04, 0x38, 0x01])).is_none());
    }
}
