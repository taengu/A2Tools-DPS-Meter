# Security changes — 2026-10-29

Unreleased changes based on `be1f1f6765da31148cb74e1d8771309a286bfae8`
(meter 2.0.86). This changelog covers the security patch as a whole, including
dependency repairs, privacy changes, resource limits and compatibility effects.
The complete package/advisory inventory is in
[Dependency security review](dependency-security.md).

## Candidate packaging, saved settings and focus visibility

The follow-up candidate restores saved settings before the meter and plugin
settings initialize. New WebViews wait for the native settings snapshot rather
than applying defaults from an empty or stale local cache. Regression coverage
includes delayed settings and plugin initialization across a fresh start.

The master focus-hiding setting covers the meter, plugin panels, dock and
widgets. Focus-owned hiding is released when the setting is turned off, while
manual hiding and the game/party/combat rules retain their ownership. Cursor
style updates are deferred while a native window is hidden or minimized so
they cannot undo a focus hide through the window toolkit's cached styles.
The exact reported Alt-Tab reversal still requires a live Windows retest.

The plugin data/network-risk warning appears once per app launch, at the first
installation, update installation or new developer-folder addition. Existing
plugins open without this warning. Declining blocks installations until the
next launch; catalog signatures, manifest validation and native permissions
continue to apply.

The proposed source retains the product/title **A2Tools DPS Meter** and reports
the plain application version `2.0.86`. Cargo, npm, Tauri and all three Rust
lockfiles agree on that version, which is also returned by `get_app_version`.
The Windows MSI follows that plain package version; no separate installer
version is pinned in the proposed source. The executable's numeric Windows
file version is `2.0.86.0`. The source commit and artifact hashes identify this
candidate; this does not declare a new official upstream release.

Update checks use the normal signed upstream manifest and offer only newer
application versions. The normal release-notes links, publisher metadata and
Tauri-derived MSI UpgradeCode are restored. The existing application identifier
`com.a2tools.dps-meter` remains unchanged. The earlier locally named preview
used a different MSI upgrade family, so replacing its installation requires an
explicit one-time replacement of the old installed application files.

Contribution credit for this security review and follow-up work:
**Twine aka Roswell**. Original project authorship and licenses are preserved.

## Exposure and scope

The findings have different prerequisites. This distinction matters when
deciding whether a particular installation was exposed:

| Area | What an attacker would need |
| --- | --- |
| Vulnerable TLS dependencies | An affected TLS interaction and the ability to intercept or redirect an outbound connection, or control its peer. Individual dependency advisories have their own additional prerequisites. |
| Capture/parser resource exhaustion | Traffic reaching a captured interface, passing classification and reaching the relevant parser path. Connection identification is stronger after this patch, but captured TCP is still unauthenticated input. |
| Plugin behavior | An installed, enabled plugin and the relevant capability or data access. A signed catalog authenticates the distributor; it does not prove plugin behavior is harmless. |
| Installer/DLL placement and handoff | Local file-placement or replacement rights, or substitution of an installer before authenticity checks. Installer execution also requires the user's installation/elevation decision. |
| Uploaded identifiers | A fight upload containing the affected evidence, and a recipient able to inspect it. This was a linkability/privacy defect. |
| OBS overlay | The overlay enabled, a reachable network path and, for its data, a valid key or observation of its unencrypted traffic. |
| Log-service decoding | An uploaded or local slice reaching the decoder. The worker's existing service-key boundary remains relevant; the decoder is not described here as a public unauthenticated endpoint. |

Knowing a public IP address alone does not establish these prerequisites. No
new application CVE, remote code-execution exploit or compromise is claimed by
this changelog. Existing upstream advisory identifiers are listed with their
actual dependency repairs.

## Dependency repairs

- Update the bundled Rustls trust roots and certificate verifier to the distrust
  fixes for **CVE-2026-111943**: `webpki-roots 1.0.10` and
  `rustls-webpki 0.103.16`. The older `webpki-roots 0.26.11` compatibility
  package now resolves to the repaired 1.x root set.
- Update Rustls itself to `0.23.46`, along with the verified compatible repairs
  for Tokio, XML/build tooling, serialization and the other Rust advisory
  matches recorded in the dependency inventory.
- Update compression dependencies in all three Rust lock contexts: the desktop
  application, log service and slice conversion tool. The selected versions
  include `zstd 0.14.3`, `zstd-safe 8.2.0`, `zstd-sys 2.1.2` and
  `ruzstd 0.9.1`.
- Backport the exact upstream GLib iterator out-pointer fix into the GTK3-
  compatible `glib 0.18.5` source. Keep its version, license, provenance and
  regression visible in [the backport note](../src-tauri/vendor/glib-0.18.5/BACKPORT.md).
  This preserves the GTK3/Tauri dependency family rather than attempting an
  unrelated GTK migration in the security patch.
- Raise the Vite manifest floor to `^6.4.5` and update the locked Vite, PostCSS,
  nanoid and source-map-js packages. The reviewed npm graph now has zero known
  audit findings. These are development/build tools; packaged assets do not
  start the Vite development server.
- Record target/feature applicability, the interpreted `block-buffer 0.10.4`
  false positive, and outstanding Linux dependency maintenance warnings.
  Maintenance warnings are not represented as repaired CVEs, and no global
  advisory ignore is added.

## Capture identification and bounded state

### Keep separate connections separate

- Identify a flow by both IP/port endpoints and the capture device. Directional
  stream state additionally retains the direction of the TCP bytes.
- Pin the selected full flow, so traffic from a different host or device cannot
  replace it merely by using the same ports.
- Use process-local opaque tags for addressed live streams in packet logs and
  the evidence ring. The tag separates endpoints without putting their IP
  addresses in an Evidence Slice. The final stream-name component remains the
  server port for existing readers.
- Preserve legacy names and ordering behavior for saved text captures that
  lack endpoint/sequence metadata. A real live sequence number of zero still
  participates in TCP ordering.

### Bound discovery, handoff and accumulation

- Limit active parser flows to **16**, remembered connection classifications to
  **128**, and port-detection candidates to **64**. Expire idle discovery state
  after **30 seconds** rather than retaining every candidate for the session.
- Apply an actual sliding signature window: the required **12** hits must fit
  inside **three seconds**. Previously, a succession of short gaps could let
  hits accumulate over a much longer period.
- Limit quiet-connection handoff to **six flows**, each with at most **4 MiB**
  and **4,096 packets**. Retain the first packets of the selected replacement
  flow so a legitimate server/route switch can still recover its initial data.
- Enforce the accumulator's **2 MiB** limit before extending its buffer,
  including allocation growth and a single oversized input. Tell the stream
  assembler when bytes were dropped so it resynchronizes.
- Count packet objects and stream-name allocations in the evidence ring's
  **96 MiB** accounting, and add a **100,000-packet** ceiling. Tiny payloads can
  no longer bypass a quota that counted only payload bytes.

These limits bound the affected transport/discovery buffers. They are not a
claim that every combat aggregation structure or all possible CPU work has a
single global memory budget. Sustained traffic can still consume bandwidth and
processing time.

### Validate captured headers

- Reject undersized IPv4 and TCP headers, inconsistent lengths and fragmented
  IPv4 input that cannot be interpreted as a complete TCP segment.
- Preserve supported Ethernet, VLAN, loopback, raw-IP and Linux cooked capture
  formats, along with the legitimate segmentation-offload cases exercised by
  the tests.
- Keep Ethernet padding outside the TCP payload and refuse header-only or
  invalid payload ranges.

Header validation and flow selection do not authenticate a game server, verify
socket ownership or turn passive capture into a trusted transport.

## Decompression and slice decoding

- Charge an LZ4 bundle's declared output before allocating it. Nested and
  embedded bundles share the parser call's budget instead of each receiving a
  fresh allowance.
- Retain the per-bundle **1,000,000-byte** expansion ceiling and nesting-depth
  limit of **four**, and add a combined **8 MiB / 256-attempt** parser-call
  budget. Failed attempts consume budget too.
- Mark an insufficient budget as exhausted, so callers can refuse an incomplete
  result rather than quietly proceeding with a partial slice.
- Apply cumulative traversal budgets to slice verification, legacy conversion
  and identity rewriting. Slice traversal phases have a **128 MiB /
  65,536-attempt** budget. Slice construction also checks the original parser
  call's budget, preventing flattening from giving replay more decompression
  work than live parsing received. That preflight includes embedded probes in
  discarded host packets; dropping a host does not refund work that affected
  what the live parser could read next.
- Bound decoded container bytes and record metadata independently of record
  payload bytes. Refuse zero-length records and excessive advertised record
  counts before allocating from that count; initial reservation is limited to
  **4,096 entries**. The hard record ceiling is **1,048,576**, and the container
  ceiling is **128 MiB**, with the caller's smaller payload limit still applied.
- Stream log-service request bodies under the existing **16 MiB** compressed
  request limit. Bound the decoder's declared Zstandard history window before
  allocation, using the service's **36 MiB** record budget.

An input that exceeds a privacy or expansion contract is refused for sharing.
Rejecting it is preferable to producing a verified-looking partial replay.

## Upload privacy and consent

### Replace retained roster identifiers

Previously, clearing the slice's name-to-ID table left raw stable roster IDs
inside allowed packet bytes. The receiving service could still link those
identifiers across uploads.

- Extract roster leader/member identity fields structurally and replace them
  with identities derived from a fresh slice key.
- Preserve within-slice leader/member relationships and the server bits needed
  by replay; ensure a replacement differs from its original identifier.
- Rewrite other retained copies of recognized roster IDs before serialization,
  and write zero stable IDs in the blind map.
- Rebuild retained rosters from understood fields. Refuse incomplete or
  malformed roster layouts and discard opaque roster tails rather than copying
  unknown identity-bearing bytes into the upload.
- Refuse retained packets containing unsupported embedded compressed data that
  cannot be anonymized safely. This can make an unusual fight unshareable while
  leaving local metering available.
- Sanitize existing cached slices immediately before upload, not only slices
  made by the new builder. The migration handles the supported saved compression
  formats and preserves their already-generated name tokens.
- Track name-pattern presence once per pattern instead of materializing every
  overlapping match before deduplication. Repeated short names can no longer
  turn a bounded packet into a very large temporary vector of match indexes.
- Mask a one-character name instead of returning it unchanged in guest-upload
  metadata and saved masked-name paths.

The changes address retained roster identifiers; uploads still carry their
documented game/server data and account or guest metadata. A per-slice token is
not a promise that all other information is anonymous. Signed-in uploads still
use their documented naming behavior, and these changes do not retroactively
remove information already sent by an earlier version.

### Require explicit reporting choices

- Party presence requires both an explicit enabled value and a new consent
  marker. The prompt starts unchecked, and dismissing it does not enable
  reporting. An old prompt-shown flag cannot prove consent because earlier
  versions saved an implicit yes on dismissal.
- Field-boss, artifact and Supply Request reporting require explicit opt-in.
  Preserve an explicit current choice; migrate the older separate switches only
  when both were explicitly enabled.
- Recheck sharing state while processing queued work and responses, and clear
  pending reporting state when sharing is turned off.
- Keep these choices distinct from account auto-upload and guest fight-sharing
  consent. Presence hashes remain deterministic name/server pseudonyms when a
  user chooses the feature; they are not described as anonymous.

## Plugin execution, permissions and lifecycle

### Put the trust decision in the host

- In this follow-up candidate, show the native risk warning at the first plugin
  installation, update installation or new developer-folder addition in each
  app launch. One decision covers all installations during that launch;
  **No** is the default, and declining blocks installations until the next
  launch. Existing plugins can still load and open according to their saved
  enable/disable choices without this warning.
- Describe decoder access as selected game fields, which may include other
  players, rather than promising that arbitrary numeric fields are own-player
  data. Numeric lists can encode text and are not an anonymization boundary.
- Keep the warning decision in process memory. It replaces the earlier
  persistent per-plugin execution/decoder-fingerprint approvals. Decoder
  layout and permission changes still clear old queued/replay events; they do
  not create another risk warning after this launch's decision.
- Expand the absolute refusal list for identity, roster, movement and
  summon-owner packet types. Keep meaningful optional self filters for layouts
  that actually contain a local-player entity; forcing such a field into every
  legitimate decoder would break unrelated layouts.
- Add field-name, duplicate-name, varint and decoded-value limits. Nested lists
  share an **8,192-value** output budget instead of multiplying independent
  per-list limits into millions of allocations.

### Confirm browser destinations natively

- Remove the generic external-URL command from plugin-window permissions and
  add a plugin-bound command checked against its actual host window/dock.
- Show the exact HTTPS destination, including path and query, in a native
  default-No question before passing it to the browser. A forged iframe
  message or SDK click flag cannot stand in for that decision.
- Reject credentials, control characters, invalid hosts and addresses longer
  than **2,048 bytes**. Limit prompts globally and space each plugin's link
  requests by **10 seconds**.
- Recheck the plugin generation after the question, so a response to an old
  enabled instance cannot authorize a new one.

### Stop collection when disabled

- Disable stops decoder collection and clears queued and replayed events.
- Stamp work and delivery with a lifecycle generation. Re-enabling cannot
  revive stale decoder events or delayed unnumbered character/map/boss/widget
  messages from the previous enabled period.
- Keep collection, queued delivery and host data methods behind the same
  enabled-state boundary, rather than relying on closing the visible page.

### Bound plugin resources

- Cap pending delivery at **1,024 events / 8 MiB**, alongside the existing
  **20,000-event / 2 MiB** replay log limits.
- Enforce **16 MiB / 128 keys** of aggregate storage per plugin, in addition to
  the **1 MiB** per-value limit. Replacing an existing value remains possible
  at the key limit.
- Limit feed replies to **1 MiB**, the feed cache to **32 entries**, and concurrent
  feed fetches to **eight**.
- Validate screenshot rectangles against the actual window dimensions and
  scale. Refuse non-finite/overflowing geometry, dimensions above **8,192 pixels**
  and captures above **16,777,216 pixels**; allow one capture job at a time and
  space each plugin's requests by **two seconds**. Native screen arithmetic and
  GDI allocations have corresponding checked bounds.

### Explain the network boundary accurately

Plugin responses retain restrictive CSP and add
`Connection-Allowlist: (response-origin)` plus DNS-prefetch hardening. A local
synthetic test on Chromium 156 observed a WebRTC STUN packet with CSP alone and
none with the allowlist header. That result verifies the tested engine, not
every supported WebView2/WebKitGTK release.

The host installation question discloses possible outbound connections and
disclosure of granted data. In this follow-up candidate it is shown once per app launch when
an installation is attempted, rather than when existing plugins execute. The
header strengthens supporting engines; it is not used to make an unconditional
cross-platform no-network promise. Plugins remain a trust decision even when
their distribution signature is valid.

## HTTP and live-feed limits

- Read small HTTP responses as checked chunks, rejecting an excessive declared
  length early and enforcing the same cap when length is absent or misleading.
  The cap is checked before extending the buffer.
- Apply an absolute **30-second** body deadline and strict UTF-8 decoding for
  text responses.
- Cover update manifests (**64 KiB**), signatures (**1 KiB**), release notes and
  plugin catalogs (**1 MiB**), plugin files under the remaining **8 MiB** package
  allowance, account replies (**256 KiB**), upload/developer-log/reporting
  replies (**64 KiB**), and the supporter roster (**8 MiB**).
- Bound update packages at **256 MiB** before creating the file and during the
  streamed write. Create a new download exclusively and remove a partial file
  after a failed transfer.
- Bound boss WebSockets to **eight servers per plugin**, **32 servers globally**,
  **256 subscription entries** and **200 distinct bosses per connection**.
  Acquire a connection-task permit before spawning, including connecting and
  closing tasks. Limit messages to **256 KiB**, frames to **64 KiB**, write
  buffers to **256 KiB**, and sends/closes to **10 seconds**.

These are availability limits, not additional authentication of a compromised
trusted service. Existing manifest/catalog signatures and file hashes remain
separate integrity boundaries.

## Windows library and installer integrity

- Resolve the Windows system directory through the OS rather than trusting a
  `SystemRoot` environment value. Load Npcap from absolute system installation
  paths and constrain DLL dependency lookup to the DLL's directory and System32.
  The application directory, current directory and PATH are not searched for
  `wpcap.dll` by this path.
- Bound the Npcap page and installer download (**1 MiB / 32 MiB**), use an
  unpredictable exclusively created temporary file, and clean up after the
  selected installation flow.
- Verify Windows Authenticode trust and the expected **Nmap Software LLC**
  publisher before running the Npcap download. A plausible filename or `MZ`
  header alone is no longer the authenticity check.
- Protect installer files/ancestor paths against replacement while inspected,
  and use a separate elevated handoff that copies and verifies the stored
  package in an Administrators/SYSTEM-only directory before execution.
- Launch Windows PowerShell and Windows Installer by trusted absolute system
  paths. The fixed helper script receives quoted data, uses a protected
  compilation/temp directory, creates its package leaf exclusively, verifies
  the copied bytes against the expected SHA-256, waits for installation and
  cleans its own package/directory.

The protected-copy handoff is intended to preserve integrity after the meter
exits, rather than relying on a random filename or a second hash followed by
another mutable-path launch. Installation still follows Windows' elevation and
installer UI. The Npcap prompt explains that the Microsoft PowerShell helper may
be the process named by UAC before it runs the verified installer copy.

The online updater still depends on its authenticated signed manifest and
constrained package URL. This patch does not claim a signature or matching hash
establishes source-to-binary reproducibility.

## OBS overlay handling

- Acquire the global **32-connection** and per-peer **16-connection** permits
  before creating a handler. Reject excess sockets without spawning an
  unbounded set of response tasks.
- Enforce the **8 KiB** header cap including the terminating bytes, a
  **five-second** header deadline, and **two-second** response/SSE write
  deadlines.
- Cancel the whole handler on shutdown or key rotation, including a blocked
  write. Synchronizing unchanged settings does not disconnect valid streams.
- Require the OS random source for a new 128-bit overlay key. Report failure
  and fail closed instead of falling back to a clock-derived key; reuse an
  existing valid key without unnecessary regeneration.

The overlay remains an opt-in plain-HTTP LAN feature. An observer of that
traffic can see its key. Application connection limits cannot prevent all
network or bandwidth exhaustion, and the feature should be exposed only to its
intended network.

## Compatibility and migration summary

- Existing privacy opt-outs remain off. Party-presence installs without the new
  consent marker need a deliberate choice before reporting resumes.
- Installed plugins keep their enable/disable choices. This follow-up candidate no longer
  reads or writes the earlier per-plugin trust approvals; installation risks
  are acknowledged once per app launch when installing or updating a plugin
  or adding a developer folder. No warning appears merely because an existing
  plugin loads or opens.
- Existing text captures remain readable; addressed live stream names gain an
  opaque connection component while retaining their final server-port field.
- Supported cached slices are sanitized before upload. Malformed, unsupported
  or over-budget evidence is refused instead of being sent incompletely or
  with unexamined identity bytes.
- Windows installations must provide Npcap in a trusted system location. A
  DLL copied beside a portable executable is deliberately no longer selected.
- Oversized network replies, plugin data or capture inputs may now produce a
  bounded error/drop instead of allocating indefinitely.
- The parser core remains independent of desktop/HTTP/OS dependencies, so its
  WebAssembly build can continue to serve the log-service re-derivation path.

### Third-party meters and re-derivation

The slice container version remains unchanged, and supported existing saved
formats are still decoded. The accepted privacy/validation contract is stricter:
rosters are rebuilt from typed fields, stable identifiers are replaced,
unsupported embedded compressed identity data is refused, and resource budgets
can reject evidence that earlier builds accepted.

Third-party producers and the log-service parser should run the existing
[conformance checks](third-party-meters.md) against these changes, including
roster/server attribution, damage totals, legacy formats and malformed evidence.
Client-side sanitization covers this client's cached uploads; it does not
rewrite evidence already stored by the service or automatically repair another
meter's upload builder. Re-deriving a stored log is a separate service action.
This changelog records the proposed compatibility effects; it does not establish
a maintainer-selected notice period, staging endpoint or notification policy.

## Validation record

The regression work covers cross-host/device flow collisions, signature-window
timing, sequence-zero replay, malformed headers, buffer/packet quotas, nested
expansion, serialized identifier absence and cached-slice migration, consent
defaults, plugin lifecycle generations and resource limits, streamed/chunked
HTTP limits, WebSocket limits, OBS stalled clients/key rotation, installer
publisher/path handling and protected-copy checks.

| Check | Release status |
| --- | --- |
| Focused privacy/parser regressions | 53 passed, no failures; one pre-existing real-capture test ignored because its external fixture was unavailable. Full desktop integration remains a separate check. |
| Frontend production build and full JavaScript tests | Production build passed; all 200 JavaScript tests passed, including settings hydration, plugin-settings startup, plain-version compatibility, consent and plugin generation regressions. |
| Windows desktop Rust tests and binary build | The final `cargo test --locked --offline --all-targets` run passed 592 tests across 24 test groups; 10 existing external-capture/diagnostic tests were ignored. This includes the delayed focus-observation and queued click-through lock regressions. The earlier security checkpoint's optimized Windows binary built successfully; the final plain-version binary remains a separate build check. |
| Desktop build without online features | `cargo check --locked --no-default-features --features desktop --lib` passed. |
| Parser-only WebAssembly check | `cargo check --locked --lib --no-default-features --target wasm32-unknown-unknown` passed. |
| Separate log-service tests/WASM check and conversion-tool check | Both server security tests passed, its WebAssembly build check passed, and the separately locked conversion tool passed `cargo check`. |
| Optimized Linux GLib backport regression and Linux desktop checks | Added the optimized iterator regression to Linux CI. Native Linux results remain pending; Windows tests do not establish Linux results. |
| MSI creation, installed startup and game-capture smoke check | The earlier locally named 2.0.86.1 preview installed and captured live game traffic successfully with Npcap administrator-only access enabled. The final normal-name 2.0.86 MSI, its packaged executable hashes, saved-settings relaunch and focus/Alt-Tab retest remain pending. The earlier smoke check does not validate the final follow-up candidate. |

Focused synthetic tests use controlled local inputs. Tests requiring an
external real capture may remain skipped/ignored when their fixture is not
available. No claim of live exploitation or exhaustive safety follows from a
passing test suite. The released artifact's version, SHA-256 and final validation
results should accompany distribution of the candidate build.
