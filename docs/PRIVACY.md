# What leaves your machine

This document is written to be checkable. Everything in the "today" section can
be verified against the source in this repository, and the evidence for the
claims about packet captures is a test you can run yourself.

Status (meter 2.0.80): **a fight is uploaded when you press Upload on it, or
when a boss fight ends if you chose to share automatically** (signed in, or
anonymously without an account). Besides fights, the meter sends a few small
things on its own: what is in Settings → Privacy (party presence, field boss and
supply data) and the checks for updates. Each is listed below, with what it
contains and how to turn it off. This document says what the meter sends, what
it keeps, and what an upload contains.

---

## What the meter sends

Everything the meter sends goes to a2tools.app or its CDN, cdn.a2tools.app, over
HTTPS. Like any web request, each one shows the server your IP address.

**Version checks.** At start and every six hours the meter fetches
`https://cdn.a2tools.app/latest-v2.json` and its signature,
`latest-v2.json.sig` (`src-tauri/src/update_check.rs`). It sends no identifiers,
no telemetry and no combat data. The meter checks the signature against a key
it carries and offers only a newer version; if you accept, it downloads that
package from cdn.a2tools.app and installs it only if its SHA-256 matches the
manifest.

**The supporter list and the plugin catalog.** Downloaded from the CDN and used
on your machine (see below). Plugins run sandboxed with no network access; a
plugin that shows field boss timers gets them through the meter, which fetches
the server's published timers.

**Your account, if you connect one.** Signing in under *Settings → A2 Tools
Account* opens a2tools.app in your browser to approve the meter, which then
holds a token (Windows: encrypted with DPAPI in `credentials.dat`; Linux: in the
desktop's Secret Service). The meter names itself to the site by system only,
"A2Tools Meter on Windows", never by your PC's name. Signed in, it uses the token
to ask who you are, and to upload.

**Sharing boss fights anonymously (no account).** *Settings → Privacy → Share
boss fights anonymously.* Since 2.0.80 the meter asks before anything is shared:
a new install offers the choice once, and players who were already sharing see
a notice once. Nothing is uploaded without an account until then. With it on,
each boss fight is uploaded once it has ended, under a guest on a2tools.app that
has no sign-in of its own, and **every name in it is masked, yours too**
(`Na***a`, `src-tauri/src/share/mod.rs`): your full character name and your PC's
name are not sent. The fights are public and count on the leaderboards. If you
later sign in on the same PC, the fights become your account's and the meter
sends your real name for them so they show under it; fights claimed with a code
on another PC stay masked. **Stop sharing** (in the notice) turns it off and
deletes the fights this PC shared from a2tools.app, in the background
(`account/guest.rs`, `delete_uploads`). Turning it off in Settings only stops new
uploads.

**Every boss fight, signed in.** *Settings → A2 Tools Account → Upload boss
fights automatically.* With it on and an account connected, each boss fight is
uploaded once it has ended (never while it is still being fought), under your
name, with the visibility you chose on a2tools.app.

Training dummies are never uploaded. Turning automatic uploads off stops them
immediately; it does not remove logs already uploaded, which you manage from
your account.

**Party presence.** *Settings → Privacy → Show A2Tools users in my party*, off
until you enable it in Settings or the unchecked one-time prompt. Older versions
enabled this even when their prompt was dismissed; those installs require a new
choice before reporting resumes. While you are in a party, the meter sends a
hash of your character's name and server, and of each party member's, so that
members who also use A2Tools see a small A2 tag beside each other's names: on
each party change and every 10 minutes (`src-tauri/src/party_presence.rs`).
Solo, it sends nothing. **A hash of a name is not anonymous:** anyone who knows a
name and a server can compute it, so the site can tell who plays with whom among
A2Tools users, and party members using A2Tools learn that you use a meter.

**Field boss and supply data.** *Settings → Privacy → Sync useful data to
A2Tools*, off until you enable it. An existing explicit choice is preserved;
the older separate boss and supply switches migrate only when both were
explicitly enabled. To keep A2Tools' boss timers and Supply
Request boards current for everyone, the meter reports game state it sees on
your server: the field bosses on a map you open, the ones you see killed and
their health (every few seconds while one is being fought), who holds the Abyss
artifacts, the Abyss's Dimensional Cores (up, killed, their health, and which side
killed them), and the Supply Request boards after each reset
(`src-tauri/src/boss_report.rs`, `src-tauri/src/supply_report.rs`). **Nothing
about your character** is in these reports.

**Logs for the developer, when you send them.** *Settings → Advanced → Send Logs
to Dev* sends your debug log and your most recent packet logs (see below),
listing each file and its size before it asks. Only the developer can open them,
and they are deleted after 30 days. Nothing else sends them.

## What it captures locally

The meter reads game network traffic with Npcap in order to compute damage. Two
different things get written to disk, and they are **not** equally sensitive.

### Fight history — `%APPDATA%\com.a2tools.dps-meter\history\*.json`

One JSON file per boss fight, saved automatically. It holds damage, skills,
timings, and per-player summaries.

Party members' names are **masked at the moment the record is created**, not when
it is displayed: `obscure_nickname` in `src-tauri/src/entity/fight_record.rs`
keeps the first and last character and masks the middle, so a saved file contains
`Ta****x`, not the real name. Your own character's name is not masked.

### Recent traffic, in memory only

To make a fight uploadable without packet logging having been on, the meter
keeps the game traffic it has just parsed in memory for about an hour
(`src-tauri/src/share/ring.rs`). This is the same data it is already reading to
draw the meter. **It is never written to disk** and is gone when the meter
closes.

### Evidence Slices — `%APPDATA%\com.a2tools.dps-meter\slices\`

When a boss fight is saved, the meter cuts that fight's Evidence Slice from
memory and saves it beside the history: `<fight>.a2es.gz`, typically tens of
kilobytes, plus a small `<fight>.json` noting which actor was you and, once
uploaded, the link. A slice is what an upload sends. It is *not* a packet
capture: it holds only the packet types the damage parser reads, with every
character name replaced by an opaque token. Deleting a fight deletes its slice,
and slices for fights that have aged out of history are removed.

### Packet capture — `%APPDATA%\com.a2tools.dps-meter\packets_*.txt`

**Off by default.** Written only while *Settings → Diagnostics → Enable packet
logging* is on. This is a debugging tool, and you should understand what it
records before turning it on.

It is **the whole server-to-client game connection**, not a combat feed. AION 2
multiplexes everything down one connection, so the file contains far more than
damage numbers.

We checked rather than assumed. Running

```
A2_REPLAY_CAPTURE=<your capture> cargo test --test capture_contents -- --ignored --nocapture
```

over one ordinary five-player dungeon run surfaced, among other things:

- **Player chat**, including recruitment and trade messages from public channels
- **A GM system broadcast** about account suspension policy
- **Names of players who were not in the party** — bystanders and legion names
- **Account statistics** such as `{"kill_count":…,"death_count":…,"assist_count":…}`
- Session GUIDs, server IP addresses, and TLS certificate fragments

Run that test on your own capture and read the output before you share one with
anybody, including us.

These files are **never uploaded with a fight**: an upload sends a slice, not a
capture. They leave your machine only if you press *Send Logs to Dev*, which
lists them first. They grow for as long as the setting is on. Delete them when you are done; the
meter does not need them.

### The other capture tool

`src-tauri/diagnostics/packet_dump.rs` is a developer tool that is **not compiled
into the released app**. It captures on every network adapter with no filter, so
it records traffic from other applications on your machine. It writes
`rawpackets_*.txt`.

If you built it yourself to help debug something: those files are far more
sensitive than the ones above. The upload feature described below will refuse
them by format, not merely by policy.

---

## What an upload contains

Pressing Upload sends two things to a2tools.app: the fight's Evidence Slice, and
the names to show on the log. **It sends no numbers.** The service runs this
repository's parser over the slice (`log-service/`, the same code compiled to
WebAssembly) and publishes what it derives. That is what makes a log worth
trusting with an open-source client: a modified meter cannot upload damage it
did not do, because nobody asks it what the damage was.

**Raw captures are never uploaded.** Given what is demonstrably inside them, an
upload is an *Evidence Slice*: rebuilt from an allowlist of the packet types the
parser reads, with names replaced by opaque tokens. An allowlist, not a
blocklist: we cannot prove we stripped every chat message from a format we only
partly understand, but we can prove what we kept. The list is `ALLOWED_OPCODES`
in `src-tauri/src/capture/evidence_slice.rs`, sixteen entries, each named.
Three of them (meter 2.0.56 on) are the buff and debuff records behind a log's
Buffs timeline: which effect an entity gained, changed or lost, the skill and
entity that applied it, its timings and a position. They hold no names.

Two additions to the allowlist, both measured as necessary on real fights and
both narrower than keeping more packet types:

- **State from before the pull.** A boss's spawn record, which is the only thing
  that says what the target is, and the party roster arrive when you enter the
  room, often minutes before the first hit. The slice keeps allowlisted *state*
  packets (spawns, identity, roster, zone changes, summon ownership) from up to
  30 minutes before the fight. Damage and health updates from that period are
  never kept, so it cannot carry an earlier fight.
- **Damage embedded in other packets.** The parser recovers damage records that
  sit inside packet types no allowlist would keep; on a real boss fight that was
  19% of the damage. Those host packets are still dropped. The damage record
  alone is lifted out: from its `04 38` marker to at most 160 bytes after it, or
  to the end of a run of records that sit closer together than that. **This is a
  real exception to "only allowlisted packets":** up to 160 bytes following a
  damage record come from a packet we do not name. They pass through the same
  name blinder and the same leak check as everything else.

`a2t-derive` holds the result to account: it replays a whole capture, cuts the
slice, runs the service's derivation over the slice alone and compares. On the
fights it has been run against, the slice derives to the same boss, the same
total and the same skill table, row for row, as the whole capture. It has been
run against few fights so far, all solo; treat "identical" as measured on those
and expected elsewhere, not proven for every fight. A log can also differ by a
fraction of a percent from what your meter showed live, because the live meter
and a replay do not always agree with each other; that gap exists with no slice
involved.

Two tests guard what must not be in a slice.
`no_real_name_survives_in_a_slice_built_from_a_real_capture` decompresses the
finished artifact and asserts that not one character name the meter resolved
appears in it. `no_readable_text_from_the_capture_survives_into_the_slice`
asserts the same for every length-prefixed string in the capture, which is what
catches chat and bystanders.

**Names.** The slice contains no names. Each token is made with a random key
that belongs to that one slice and is thrown away once it is cut, so a token
cannot be worked out from a name or a roster id, and the same name gets a
different token in every slice. Roster identities are also replaced for each
slice, so their original stable values cannot join uploads.
Separately, the upload sends the names the saved fight already holds: signed
in, **yours in full, everyone else masked**; shared anonymously without an
account, **everyone masked, you too**; masked the way
the meter shows them (`Ta****x`: first two characters, last character, at most
four stars). The service applies that masking again itself, so a modified client
cannot publish another player's full name. An earlier version of this document
promised that no name at all would be uploaded and that others would appear as
"Sorcerer #2"; logs now look like the meter instead, and this paragraph is the
correction.

**Roster ids.** The party roster packet's leader and member identities are
replaced with values derived from the slice's random key. Repeated references
keep the same value within that slice; the server bits stay intact because
replay needs them to read the roster. Recognized identities copied elsewhere in
retained packets are replaced too. The separate name-token table carries zero
ids. Roster packets are rebuilt from the fields replay understands, leaving out
opaque tails. A retained roster whose member layout cannot be completely read
is refused. A plain packet with an embedded compressed identity block is also
refused until its surrounding layout has a privacy rule; the meter reports why
the slice could not be shared instead of sending unchecked compressed data.
Older local cached slices are re-anonymized before upload. Slices already sent
to the service can still contain original ids. Packet fields whose meaning is
unknown remain a limit of the format and its allowlist.

**Who can see a log.** You choose whether your logs are public or private, as a
default and per log. A private log is not listed anywhere, but anyone you give
its link to can open it. Class statistics count every log, public or private,
as numbers with no names.

**Diagnostic captures are separate, opt-in, and per-incident.** Debugging a parser
regression sometimes does need the packets an allowlist would strip. *Send Logs
to Dev* is that action: it is its own button, lists the files it will send and
their sizes, says what packet logs contain, asks every time, and is never implied
by any other setting. It sends the files as they are, not blinded.

### Supporter names are resolved on your machine

Supporters' names render gold on everyone's meter. The obvious way to build that
is for the meter to ask a server "is this player a supporter?", and it would mean
sending us a list of who you play with, every fight, in exchange for a colour.

So it works the other way round: a small file listing supporters is published to
the CDN, your meter downloads it every few hours, and the matching happens
locally. Nothing about your party is transmitted, and it works offline.

The entries in that file are hashed. That is anti-scraping, not secrecy — the
salt travels with the file, so anyone can test a name they already have. What it
prevents is downloading the list and reading off who has given money.

Gold is cosmetic and only cosmetic. It never changes ordering, bar colour, or any
number, and there is a test asserting the damage figures are identical with the
roster on and off. You can turn the colour off entirely in Settings.

### How to check any of this yourself

Two things exist so you do not have to take the section above on trust.

**A dry run.** In Battle History, the eye icon on any fight writes the exact two
files an upload would send — `<fight>.a2es` and `<fight>.upload.json` — into
`%APPDATA%\com.a2tools.dps-meter\share-preview\`, and opens the folder. It makes
no network call. It builds from a packet capture covering that fight, so packet
logging has to have been on at the time; the slice the meter saved on its own,
in `slices\`, is the same artifact and can be read the same way after
decompressing it.

**A reader.** `a2t-inspect` prints what is inside a slice:

```
a2t-inspect fight.a2es              summary, opcode histogram, blinded tokens
a2t-inspect fight.a2es --strings    every readable run in the content
a2t-inspect fight.a2es --find NAME  search it; exits non-zero if found
```

It decompresses before searching, which matters: a slice keeps the game's LZ4
bundles, so searching the raw file would find nothing no matter what was in it
and look reassuring while meaning nothing.

**This is not decorative — it caught a real leak.** The first slice builder
blinded only the names the meter had resolved, which is the party.
`a2t-inspect --strings` on its output showed a player who was never in the party
and a public chat message still in the clear. Names of passers-by are never in
any list the meter builds, so nothing was looking for them. The blinder now makes
a second pass over anything *shaped* like a name — a length byte followed by that
many bytes of text — rather than only names it recognises, and a test derived
from the capture itself asserts that none of its 449 length-prefixed strings
survive. That fix cost nothing in accuracy: the damage figures above are
unchanged to the digit.

**Names where a record holds them, at any length.** Before both passes, the
blinder finds every place where a record the parser reads holds a character
name: your own record and other players' records and spawns, spawns that name
their caster, the party roster, summon and loot owners, and the parser's other
name patterns. It replaces the name there, known or not. That is the only way
to blind a one-letter name, which the game allows: searched for, its one byte
turns up all through the packets. A known name "A", replaced wherever its
byte appeared, turned every spawn record (`41 36`) into something the parser
does not know, and the slice derived nothing. A name of two or three bytes is
looked for only after its length byte, where the game puts a name, since two
bytes also turn up in ordinary data by chance; longer names are looked for
everywhere. The leak check looks for every name in the name fields too.

### What these rules do not protect against

Stated plainly, because a privacy document that only lists strengths is
marketing.

- **A masked name is not anonymous.** `Ta****x` hides most of a name, not all of
  it. Someone who knows who you play with can often tell who it is.
- **The operator sees more than the public does.** Signed in, your log is tied
  to your account and your own name is on it in full. Every upload, signed in or
  not, comes from an IP address. The slice keeps server identifiers and
  per-slice roster relationships. None of that is published, but it is not
  hidden from the service.
- **Party presence links people.** The hashes it sends can be matched to any
  name the site already knows, so the site can see who plays together among
  A2Tools users. Turn it off in Settings → Privacy if that matters to you.
- **The slice is derived, not audited line by line.** The allowlist and the name
  checks are tests of what we know to look for. The lifted damage spans carry a
  bounded number of bytes we cannot name.
- **People in your party already saw your name.** Masking protects you from the
  public, not from the seven people you played with.
- **A diagnostic capture you send us contains what the section above describes**,
  including third-party chat and names, and an administrator can read it.
