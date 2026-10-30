# Uploading to a2tools.app from another meter

Meters other than the A2Tools DPS Meter can upload boss fights to
[a2tools.app](https://a2tools.app). Their logs are stored, get a link to share, and show
on the uploader's account like any other. To be counted on the leaderboards and in the class
statistics, a meter version has to pass the conformance check below. Ask in
**#aion2-dpsmeter** on the [Discord](https://discord.gg/Aion2Global) to get started.

## Identify your meter

Send these with every fight upload (`POST /api/logs`) and dev-log registration:

| Field | Value |
|---|---|
| `client` | your meter's name, lowercase, e.g. `"daevalog"` |
| `clientVersion` | your version, e.g. `"1.0 r444"` |
| `appVersion` | your version too; the site keeps it as a label |

Use your own User-Agent as well. Uploads without `client` are taken to come from the A2Tools
meter. Nothing on the site checks a version number to accept or parse a log.

## What the server does with an upload

The site never trusts the numbers a meter shows. The log service re-derives every fight from
the **Evidence Slice** you upload, with the A2Tools parser compiled to WebAssembly, so what
must stay compatible is the slice: the `A2ES` container, the packet allowlist and the name
blinding in `src-tauri/src/capture/evidence_slice.rs`. If you change what goes into a slice,
tell us first.

Since A2Tools 2.0.56 the allowlist also keeps the buff and debuff records (`2A 38`, `2B 38`,
`2C 38`) for a log's Buffs timeline. **Please include them.** Keep every one in the fight's
window, the same as damage records: the site builds each player's buff and debuff uptime from
them, and a log without them shows no Buffs timeline at all. They carry entity, effect and skill
ids, timings and a position, never a name, so they need no blinding.

A slice without them is still accepted and derived exactly as before (its log says it has no
buff data), so a meter that cannot add them yet keeps uploading.

Every slice is checked, whichever meter cut it:

- **Names.** The service runs the blinder's second pass over every packet and counts any
  name-shaped run that is not one of the slice's declared tokens. It also looks for each name
  the upload says it showed (`names` in the payload). An upload with either is refused.
- **Structure.** A capture that loses the game's framing (for example, by taking Ethernet
  padding for payload) has its packets read as blobs, and nearly everything kept from it is
  lifted out of other packets while compressed bundles all but vanish. On the captures checked,
  clean slices are 0 to 7% lifted records; a corrupted one was 83 to 98%. More than 25% marks
  the log `garbled`.
- **Kills against max HP.** When the boss's death is in the slice, the damage on it should be
  at least its max HP: 100 to 107% on every kill checked, more when the boss healed. Below 97%
  marks the log `short`.
- **Other uploads of the same pull.** When another account uploads the same pull (same boss,
  server and duration, starts within two minutes), the totals should agree to within 3%. A log
  short of the best by more is `disputed`; logs that agree corroborate each other.

A `garbled`, `short` or `disputed` log stays viewable and shareable but is not ranked or
counted. Neither is a log from a meter version that has not passed the conformance check.

## Compatibility history

This records changes relevant to slice producers and readers. The proposed entry below
describes source under review; it is not a claim that the public service has deployed it
or that stored logs have been re-derived. The notification channel, notice period,
staging arrangements and optional metadata endpoint remain under discussion in
[#40](https://github.com/taengu/A2Tools-DPS-Meter/issues/40).

| Date / status | Version or proposal | Compatibility and action |
|---|---|---|
| 2026-10-11 | 2.0.56 | The documented upload contract includes the optional buff/debuff records described above. Include `2A 38`, `2B 38` and `2C 38` for the fight's Buffs timeline. Their absence does not by itself prevent deriving a fight. |
| 2026-10-16 | 2.0.67 | Evidence Slice version 3 stores a record kind and unpacked bundle streams, and the meter compresses the container with Zstandard. Readers of these newer slices need version 3 and Zstandard support. The reference decoder still recognizes versions 1 and 2, and the log service recognizes gzip as well as Zstandard, subject to its validation and size limits. |
| Proposed / unreleased | [Security changes #131](https://github.com/taengu/A2Tools-DPS-Meter/pull/131) | The container stays at version 3, but the producer's roster anonymization and failure conditions change, and decoder resource checks become stricter. Review the migration notes below and rerun conformance checks after changing a producer. No new minimum `clientVersion` or public compatibility endpoint is introduced by this patch. |

The historical dates are the repository's release commits for
[2.0.56](https://github.com/taengu/A2Tools-DPS-Meter/commit/d8924ce) and
[2.0.67](https://github.com/taengu/A2Tools-DPS-Meter/commit/6304b6b).
The version 3 encoding change is documented in
[its source commit](https://github.com/taengu/A2Tools-DPS-Meter/commit/51150a9).

### Proposed / unreleased: security changes in #131

These notes describe the security changes proposed in [#131](https://github.com/taengu/A2Tools-DPS-Meter/pull/131),
checked against source revision `2b826a7a4572f97cef372aa2d56fa34afe146436`.
They do not announce an upstream release. The complete rationale and limits are in
[the proposal's security changelog](https://github.com/taengu/A2Tools-DPS-Meter/blob/2b826a7a4572f97cef372aa2d56fa34afe146436/docs/SECURITY-CHANGES.md),
particularly its third-party-meter and re-derivation section. Merging these documentation
notes alone does not implement the proposed producer or decoder changes.

**Producer privacy.** Clearing the numeric IDs from the blind map alone does not remove
stable roster IDs embedded in retained packet bytes. The proposed builder replaces
recognized leader/member IDs and other retained copies with per-slice identities, keeps
their in-slice relationships, and writes zero ID values in the blind map. It preserves
the identifier's server-prefix bits needed by replay; those bits are not evidence of a
transferred character's current home server. Name tokens retain their byte lengths.

The builder reconstructs supported roster fields rather than copying opaque tails.
Malformed or incomplete roster layouts, unsafe embedded compressed identity data and
exhausted traversal budgets refuse sharing instead of producing a partial slice.
Other producers should check their final serialized bytes and decoded records, with
nested bundles expanded, to confirm that original roster IDs and names are absent while
replay relationships survive. Searching compressed upload bytes alone cannot prove this.
The reference implementation and container layout are in
[`capture/evidence_slice.rs` in the proposal](https://github.com/taengu/A2Tools-DPS-Meter/blob/2b826a7a4572f97cef372aa2d56fa34afe146436/src-tauri/src/capture/evidence_slice.rs).

**Cached slices.** The proposed A2Tools client sanitizes supported raw, gzip and
Zstandard saved slices immediately before upload and emits a Zstandard upload. It keeps
their existing name tokens while replacing retained roster identities. A third-party
client reusing its own cached slices needs an equivalent migration; updating only its
new-slice builder leaves the old-cache path unchanged. See
[`sanitize_compressed_slice` in the proposal](https://github.com/taengu/A2Tools-DPS-Meter/blob/2b826a7a4572f97cef372aa2d56fa34afe146436/src-tauri/src/share/mod.rs) and
`anonymize_stored_slice` in the reference builder. This client migration does not rewrite
already-uploaded slices, stored logs or another meter's cache.

**Decoder limits.** The reference decoder adds a record-count cap and an input-read
budget covering metadata too, and rejects zero-length records. It permits at most
1,048,576 records and reads at most the smaller of 128 MiB or the caller's record-byte
budget plus 32 MiB. The log service's existing limits remain 16 MiB for compressed
request bytes and 36 MiB for reconstructed record bytes; the proposal checks the
request limit while reading and also caps the declared Zstandard history window at
36 MiB before allocation. Compressing a file more tightly does not increase these
allowances.

Record-count, zero-length-record and input-read limits also apply to versions 1 and 2.
The record-byte budget counts reconstructed wrapped records across supported versions.
The Zstandard window cap applies to Zstandard input regardless of its container version;
gzip input has no Zstandard window. Separate bundle traversal budgets also bound parsing
and privacy verification. Details are in
[`decode_from` in the proposal](https://github.com/taengu/A2Tools-DPS-Meter/blob/2b826a7a4572f97cef372aa2d56fa34afe146436/src-tauri/src/capture/evidence_slice.rs) and
[`log-service/src/lib.rs` in the proposal](https://github.com/taengu/A2Tools-DPS-Meter/blob/2b826a7a4572f97cef372aa2d56fa34afe146436/log-service/src/lib.rs).

Container recognition and server acceptance do not prove roster-ID anonymization: the
decoder cannot infer whether arbitrary numeric IDs were derived from a private slice
key. Producers remain responsible for applying and testing the privacy transformation.
The proposal does not make a server-side deployment or historical re-derivation claim.

Before adopting these changes, run the conformance check below against the same parser
revision used to validate the producer. Also test supported cached formats, malformed
rosters and resource-limit refusals. A format-version match alone does not establish
privacy or replay conformance. No production uploads are needed for those local checks.

## The conformance check

`a2t-derive` (in this repository, `src-tauri/tools/a2t_derive.rs`) replays a packet capture
the way the A2Tools meter reads it, and derives every boss fight in it. With `--partner`, it
also takes the slices your meter cut from the same capture and holds each to the same
standard: the fight the whole capture shows, skill row for skill row, with nothing left
unblinded.

1. Record a packet capture of a few boss fights in the A2Tools format
   (`TIMESTAMP|STREAMKEY|HEX`, one TCP segment per line, server to client).
2. Have your meter cut the slice for each boss fight in it, as it would upload them, and save
   them to a folder (`.a2es` or `.a2es.gz`).
3. Run, from `src-tauri`:

   ```
   cargo run --release --features desktop --bin a2t-derive -- --capture <capture.txt> --partner <folder>
   ```

   Each fight prints a `partner` line ending in `conforms` or `DOES NOT CONFORM`, with the
   totals, the number of differing skill rows, unblinded names and the slice's structure. The
   tool exits 0 only when every fight conforms.

Everything runs on your machine; the capture and the slices go nowhere. When your build
passes on captures covering the content you support (solo and party, a dungeon and a field
boss at least), send us the output. We add your meter and version to the approved list
(`approved_client` on the site, per version or `*` for every version). A later version that
changes slice building needs the check again.
