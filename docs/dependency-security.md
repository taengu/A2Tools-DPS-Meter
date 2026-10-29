# Dependency security review — 2026-10-29

This review covers `src-tauri/Cargo.lock`, `log-service/Cargo.lock`,
`tools/slice-v3/Cargo.lock`, and the root npm lockfile. The baseline is commit
`be1f1f6765da31148cb74e1d8771309a286bfae8`. Registry advisories are a dated
snapshot; they do not establish the contents of a previously distributed MSI.

The Rust lockfiles were queried by package name and version against the public
[OSV batch API](https://google.github.io/osv.dev/post-v1-querybatch/). Matches were
checked against RustSec or the maintainer advisory, including affected APIs and
target/feature graphs. `cargo tree --locked --target ... --edges normal,build`
was used for Windows online, Windows desktop without online features, Linux
desktop, and the parser-only WebAssembly build. A lockfile includes optional
packages that a particular build does not link. npm's complete locked dependency
graph was checked with `npm audit --json`.

## Rust fixes

The versions below are the selected lockfile versions, not claims that every
listed dependency flaw was reachable from the application's inputs.

| Package: baseline → patched | Advisory | Applicability and repair |
| --- | --- | --- |
| `webpki-roots 1.0.6 → 1.0.10`; `rustls-webpki 0.103.10 → 0.103.16` | [RUSTSEC-2026-0381](https://rustsec.org/advisories/RUSTSEC-2026-0381.html), [RUSTSEC-2026-0382](https://rustsec.org/advisories/RUSTSEC-2026-0382.html); CVE-2026-111943 | The online client uses bundled Rustls roots. Both distrust fixes are resolved; the `webpki-roots 0.26.11` compatibility wrapper now uses the repaired 1.x root set. An attack requires interception/redirection or a malicious peer presenting an accepted affected chain. |
| `rustls 0.23.37 → 0.23.46` | [RUSTSEC-2026-0285](https://rustsec.org/advisories/RUSTSEC-2026-0285.html), [2026-0346](https://rustsec.org/advisories/RUSTSEC-2026-0346.html), [2026-0348](https://rustsec.org/advisories/RUSTSEC-2026-0348.html), [2026-0349](https://rustsec.org/advisories/RUSTSEC-2026-0349.html) | Repairs handshake encryption-level checks, session debug disclosure, certificate-decompression allocation, and key-exchange cache poisoning. Rustls is in the online graph; individual flaws have additional API/feature prerequisites. |
| `anyhow 1.0.102 → 1.0.104` | [RUSTSEC-2026-0190](https://rustsec.org/advisories/RUSTSEC-2026-0190.html) | Repairs `Error::downcast_mut` soundness in the desktop/build dependency graph. |
| `ctutils 0.4.2 → 0.4.7` | [RUSTSEC-2026-0385](https://rustsec.org/advisories/RUSTSEC-2026-0385.html) | Repairs constant-time operations; the affected baseline package was in the Linux graph. |
| `event-listener 5.4.1 → 5.4.2` | [RUSTSEC-2026-0221](https://rustsec.org/advisories/RUSTSEC-2026-0221.html) | Repairs tag/thread soundness in the Linux dependency graph. |
| `quick-xml 0.38.4 → 0.41.0` through `plist 1.8.0 → 1.10.0` | [RUSTSEC-2026-0194](https://rustsec.org/advisories/RUSTSEC-2026-0194.html), [2026-0195](https://rustsec.org/advisories/RUSTSEC-2026-0195.html) | Repairs duplicate-attribute work and namespace-allocation bounds. This dependency belongs to desktop build tooling, rather than captured game-packet parsing. |
| `quinn-proto 0.11.14 → 0.11.19` | [RUSTSEC-2026-0185](https://rustsec.org/advisories/RUSTSEC-2026-0185.html) / CVE-2026-25800, [2026-0338](https://rustsec.org/advisories/RUSTSEC-2026-0338.html), [2026-0339](https://rustsec.org/advisories/RUSTSEC-2026-0339.html), [2026-0340](https://rustsec.org/advisories/RUSTSEC-2026-0340.html), [2026-0342](https://rustsec.org/advisories/RUSTSEC-2026-0342.html) | Updates the optional QUIC implementation. The baseline default desktop graphs did not enable it; a lockfile match alone was not an active HTTP/3 exposure. |
| `rand 0.9.2 → 0.10.3` | [RUSTSEC-2026-0097](https://rustsec.org/advisories/RUSTSEC-2026-0097.html) | Updated with the compatible parent dependency resolution. The affected baseline 0.9.2 package was optional in the inspected desktop graphs. The flaw requires a reentrant custom logger. |
| `serde_with 3.18.0 → 3.24.0` | [GHSA-7gcf-g7xr-8hxj](https://github.com/jonasbb/serde_with/security/advisories/GHSA-7gcf-g7xr-8hxj) | Repairs `KeyValueMap` serialization panic handling; used in the desktop/build dependency graph. |
| `tokio 1.50.0 → 1.54.1`; server `1.53.1 → 1.54.1` | [RUSTSEC-2026-0386](https://rustsec.org/advisories/RUSTSEC-2026-0386.html) | Repairs the Unix `AsyncFd` API soundness issue. Windows does not compile the affected Unix API; both lock contexts receive the patched release. |
| `ruzstd 0.7.3 → 0.9.1` in all three contexts | [RUSTSEC-2026-0344](https://rustsec.org/advisories/RUSTSEC-2026-0344.html) | Repairs unbounded output from a compressed block. This is a production uploaded-input decoder in the log service, a desktop test dependency, and a conversion-tool dependency. The server also caps the request while streaming and refuses a declared history window above its record budget before allocation. |
| `zstd 0.13.3 → 0.14.3` | [RUSTSEC-2026-0391](https://rustsec.org/advisories/RUSTSEC-2026-0391.html) | Repairs stale data carried between streams sharing a context. The desktop currently uses bulk compression, but the library is upgraded rather than relying on that API distinction. |
| `zstd-safe 7.3.0 → 8.2.0` | [RUSTSEC-2026-0395](https://rustsec.org/advisories/RUSTSEC-2026-0395.html), [0396](https://rustsec.org/advisories/RUSTSEC-2026-0396.html), [0397](https://rustsec.org/advisories/RUSTSEC-2026-0397.html), [0398](https://rustsec.org/advisories/RUSTSEC-2026-0398.html), [0399](https://rustsec.org/advisories/RUSTSEC-2026-0399.html), [0400](https://rustsec.org/advisories/RUSTSEC-2026-0400.html), [0401](https://rustsec.org/advisories/RUSTSEC-2026-0401.html), [0402](https://rustsec.org/advisories/RUSTSEC-2026-0402.html), [0403](https://rustsec.org/advisories/RUSTSEC-2026-0403.html), [0404](https://rustsec.org/advisories/RUSTSEC-2026-0404.html), [0405](https://rustsec.org/advisories/RUSTSEC-2026-0405.html), [0406](https://rustsec.org/advisories/RUSTSEC-2026-0406.html) | Updates the desktop/conversion compressor's safety wrapper. These advisories cover distinct context, dictionary, seekable, and buffer APIs; their presence does not show every affected function was called by the application. |
| `zstd-sys 2.1.1 → 2.1.2` | [RUSTSEC-2026-0410](https://rustsec.org/advisories/RUSTSEC-2026-0410.html) | Repairs the WebAssembly allocation shim. The parser-only WASM build excludes this C-backed compressor, and the log service uses `ruzstd`; the locked native dependency is still updated. |
| `glib 0.18.5`, patched local source | [RUSTSEC-2024-0429](https://rustsec.org/advisories/RUSTSEC-2024-0429.html) | GTK3/Tauri requires the 0.18 ABI; upgrading to the fixed 0.20 registry family would break that dependency family. The local copy carries the exact upstream mutable out-pointer fix and a focused optimized-mode iterator regression. See [backport provenance](../src-tauri/vendor/glib-0.18.5/BACKPORT.md). The original version is retained honestly. |

## Matches that require interpretation

`block-buffer 0.10.4` is returned by OSV's normalized
[GHSA-qwgh-2vcv-g2f7](https://github.com/RustCrypto/utils/security/advisories/GHSA-qwgh-2vcv-g2f7)
record, whose range is broader than the maintainer's current advisory. The
maintainer specifies `>=0.11.0,<0.12.1`. Version 0.10.4 keeps its cursor in a
separate field, rather than the inline byte overwritten in the affected
implementation. The other locked branch is already 0.12.1. No vulnerable
inline-cursor implementation is retained by accepting this 0.10.4 match.

The local GLib backport may still trigger a version-only scanner. Its production
change is the two-line fix from upstream commit
`b5a4071e439bef2b5eea76c3aa25e5ae84839e34`, and the patched `src/variant_iter.rs`
(including the regression) has SHA-256
`ff4a1723ae0733e0901b14ac13b1dcf7d219a3ca996a41478123395a91ecc3ee`.
Review the source and run the optimized Linux iterator regression when updating
this copy; the lockfile uses a local path rather than the vulnerable registry
source. No global advisory ignore is introduced.

The following records are maintenance warnings, without a reported vulnerability
or patched version: [proc-macro-error / RUSTSEC-2024-0370](https://rustsec.org/advisories/RUSTSEC-2024-0370.html),
[zvariant / RUSTSEC-2026-0370](https://rustsec.org/advisories/RUSTSEC-2026-0370.html),
[zbus_names / RUSTSEC-2026-0371](https://rustsec.org/advisories/RUSTSEC-2026-0371.html),
and [zvariant_derive / RUSTSEC-2026-0372](https://rustsec.org/advisories/RUSTSEC-2026-0372.html).
They are confined to the inspected Linux graph and remain visible. Replacing
the GTK/D-Bus dependency family is separate maintenance work; these warnings
must not be represented as fixed CVEs.

## npm fixes

The baseline lockfile matched 17 advisory records across four development
packages. The patched locked graph returned zero findings on the review date.
These packages run during development/builds; the production bundle does not
start Vite's development server. Individual advisories require their stated
server exposure or attacker-controlled build input.

| Package | Selected version | Baseline advisory records addressed |
| --- | --- | --- |
| Vite | `6.4.5` (manifest floor `^6.4.5`) | [GHSA-4w7w-66w2-5vf9](https://github.com/advisories/GHSA-4w7w-66w2-5vf9), [GHSA-p9ff-h696-f583](https://github.com/vitejs/vite/security/advisories/GHSA-p9ff-h696-f583), [GHSA-v6wh-96g9-6wx3](https://github.com/advisories/GHSA-v6wh-96g9-6wx3), [GHSA-698v-4r3x-3vcv](https://github.com/advisories/GHSA-698v-4r3x-3vcv), [GHSA-r825-w8mm-5pxw](https://github.com/advisories/GHSA-r825-w8mm-5pxw), [GHSA-587h-7wmj-h5pm](https://github.com/advisories/GHSA-587h-7wmj-h5pm), [GHSA-9jrq-w75r-8gcw](https://github.com/advisories/GHSA-9jrq-w75r-8gcw), [GHSA-rq7h-c2jc-7f22](https://github.com/advisories/GHSA-rq7h-c2jc-7f22), [GHSA-fx2h-pf6j-xcff](https://github.com/vitejs/vite/security/advisories/GHSA-fx2h-pf6j-xcff) |
| PostCSS | `8.5.32` | [GHSA-qx2v-qp2m-jg93](https://github.com/advisories/GHSA-qx2v-qp2m-jg93), [GHSA-6g55-p6wh-862q](https://github.com/advisories/GHSA-6g55-p6wh-862q), [GHSA-fxqj-rqcc-2cmp](https://github.com/advisories/GHSA-fxqj-rqcc-2cmp), [GHSA-r28c-9q8g-f849](https://github.com/advisories/GHSA-r28c-9q8g-f849) |
| nanoid | `3.3.20` | [GHSA-28wg-ghj8-5hjv](https://github.com/advisories/GHSA-28wg-ghj8-5hjv), [GHSA-2v37-7h3g-55p8](https://github.com/advisories/GHSA-2v37-7h3g-55p8), [GHSA-xwg4-73v4-xw9w](https://github.com/advisories/GHSA-xwg4-73v4-xw9w) |
| source-map-js | `1.2.2` | [GHSA-68fv-2mgg-jv7q](https://github.com/advisories/GHSA-68fv-2mgg-jv7q) |

## Validation to retain

Run `npm ci`, `npm audit`, the frontend build and tests, the desktop test matrix,
the parser-only WASM check, and the separate log-service and conversion-tool
checks after resolving dependencies. Exercise the vendored GLib iterator tests
with optimizations on Linux as described in its backport note. A successful
dependency audit is evidence about known database entries at that time, not a
guarantee that the application contains no undiscovered vulnerabilities.
