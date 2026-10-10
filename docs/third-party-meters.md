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
