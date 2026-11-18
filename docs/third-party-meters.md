# Uploading to a2tools.app from another meter

Meters other than the A2Tools DPS Meter can upload boss fights to
[a2tools.app](https://a2tools.app). Their logs are stored, get a link to share, and show
on the uploader's account like any other. To be counted on the leaderboards and in the class
statistics, a meter version has to pass the conformance check below. Ask in
**#a2tools-app** on the [Discord](https://discord.gg/Aion2Global) to get started.

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

### When an upload is refused

| Answer | Meaning | What to do |
|---|---|---|
| 422 `not_blinded` | A name was found in clear in the slice | Blind as `evidence_slice.rs` does. If you can't explain it, keep the slice and tell us: it can be ours (a packet misread as a name) |
| `open_world` | A field or world boss (`dungeonId` 0) | Not accepted; don't retry |
| `training_dummy` | A training dummy | Not accepted; don't retry |
| `implausible` | Numbers no real fight has | Check the slice; tell us if it was a real fight |
| `too_large` | Slice over 16 MiB gzipped | Trim the prelude; tell us |
| `derive_failed` | The log service could not derive a boss fight | Keep the slice and tell us |
| 429 `rate_limited` | Too many uploads this hour | Wait, then retry |
| 503 `unavailable` / `queue_failed` | Busy | Retry with backoff |
| 401 | Token revoked or wrong | Sign in again |

We keep refused uploads for 30 days, so we can look at them. A 400, 401, 429, or a 503 that was not
queued leaves no record on our side, so log those on yours.

## Player accounts and names

### How names are shown

- The slice carries no names (see above). What a log shows comes from the upload's `names`
  (actor id as a string, to the name your meter showed), for **the actors of that fight only**.
- The site shows the uploader, `uploaderActorId`, in full and masks everyone else: the first two
  characters, up to four stars, the last character (`Se****e`). This is done on the server, so a
  client can't skip it. Names you send already masked are fine; masking a masked name changes
  nothing.
- `uploaderActorId` must be the uploading player's actor id in that fight. A wrong one shows
  someone else's name in full.

### Signing a player in (their name in full)

A player's name shows in full when your meter uploads with **that player's own** A2Tools account
token. This is the device flow (RFC 8628), with no client secret:

1. `POST /api/device/code` with `{"client": "<your client id>", "device_label": "<your meter> on
   <OS>"}`. Don't put the PC's name in the label. The answer has `user_code`, `device_code`,
   `verification_uri`, `verification_uri_complete`, `expires_in` (600 s) and `interval` (5 s).
2. Open `verification_uri_complete` in the browser (check it is https on a2tools.app first), or show
   the player `user_code` to enter at `verification_uri`.
3. Poll `POST /api/device/token` with `{"device_code": "..."}` every `interval` seconds until
   `expires_in` runs out:
   - 428 or `authorization_pending`: keep polling.
   - `slow_down`: add 5 s to the interval.
   - `expired_token` or `access_denied`: stop, and offer to start again.
   - 200 with an `access_token`: done. A 200 with an empty `access_token` is not an approval.
4. Send `Authorization: Bearer <token>` on uploads. Tokens don't expire; a 401 means it was
   revoked, so drop it and offer sign-in again.

**One token per player, never shared.** A meter that signs in once and uploads everyone's fights
with that token puts them all on one account, under one name.

Store tokens in the OS's secure store (DPAPI, Keychain, Secret Service) or a file only the user can
read, never in plain settings, logs or crash reports.

### Uploading without an account (guests)

- `POST /api/guest` (optional body `{"deviceLabel": "..."}`) returns a guest token that owns the
  logs it uploads. At most two are made a day per network; expect 429.
- Guest logs are public and show the uploader as "Guest upload". **Mask every name before you
  send it, the uploader's too**, the same way the site masks (above). Masking on our side changes
  what is shown, not what was sent, so a full name must not leave the PC without an account.
- The guest token is the only proof the logs are the player's. Store it like an account token.
- `DELETE /api/guest/logs` with the guest token deletes everything that guest uploaded (a "stop
  sharing" button). Retry until 200 or 401.

### Claiming guest logs (and the uploader's name)

When the player signs in, their guest logs move to their account. To show their name on those
logs again, **keep the real uploader name for each guest upload locally**: the log id (from the
upload's answer), the uploader's actor id, and the name. Send them as
`"names": [{"log": "<log id>", "actorId": 5001, "name": "<real name>"}]` (at most 2,000). The site
takes a name only if it masks to what the log already shows: it can put the original name back,
never choose another. Never send a masked name as a real one.

Three ways to claim:

- **Signing in on the same PC:** send `"guestToken"` and `"names"` with `POST /api/device/token`
  (the answer then has `claimed`), or after sign-in, `POST /api/guest/merge` with the account's
  Bearer token and `{"guestToken": "...", "names": [...]}`.
- **From another PC:** `POST /api/guest/claim-code` with the guest's Bearer token, and
  `{"names": [...]}` as the body. The player types the code at a2tools.app/claim within 15
  minutes. The names are kept with the code only until it is used or runs out.
- Drop the guest token only after merge answers 2xx or 404; retry anything else, or the guest's
  logs are left behind.

A reinstall that wipes your app data, or a new PC, loses the guest token and the names. Nothing in
an uploaded fight says who uploaded it, so those logs can't be claimed after that.

### Party naming (opt-in)

A player can choose, on a2tools.app/logs/mine#party-names, to be shown in full and ranked in the
logs **their party members** upload. Your meter needs nothing extra for it, only what an upload
needs anyway:

- **The right `uploaderActorId`.** The site links a character to its account from that player's
  own signed-in party uploads, and only when the actor the log service detects as the slice's own
  player (the one the derive leaves unmasked) is the `uploaderActorId` you sent. A wrong one links
  nobody; it never names someone else.
- **The party roster and self-identity packets in the slice** (`02 97` and `33 36`, kept as
  `evidence_slice.rs` keeps them). The roster's per-character id is what the site matches; a solo
  fight has none, so it links nobody.
- Nothing in `names`: an opted-in party member's name comes from **their** account, never from
  the uploader's `names`. Keep masking everyone else as above.

Turning it off takes them off the boards at once and masks stored logs again within hours.

A meter can offer the setting itself, for a signed-in player (guest tokens can't use it):

- `GET /api/account/party-names` answers `{"enabled": true|false}`.
- `PUT /api/account/party-names` with `{"enabled": true|false}` sets it and answers the same; it's
  idempotent, and it's the same setting as the page.
- Send the player's account token (`Authorization: Bearer ...`, scope `profile`, which device
  tokens have). 401 `invalid_token`: revoked, sign in again. 403 `insufficient_scope`,
  `not_an_account` or `account_banned`: don't show the setting. 400 `invalid_request`: the body
  isn't `{"enabled": bool}`. 503: try later. Answers are `Cache-Control: no-store`.

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
