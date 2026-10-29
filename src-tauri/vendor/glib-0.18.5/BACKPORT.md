# GLib iterator soundness backport

This directory contains the published `glib 0.18.5` crate with the upstream
fix for [RUSTSEC-2024-0429](https://rustsec.org/advisories/RUSTSEC-2024-0429.html)
and a focused iterator regression test. The GTK3 dependency family requires
`glib 0.18`; the registry release containing the fix is `glib 0.20.0`.

Source archive:

- URL: <https://static.crates.io/crates/glib/glib-0.18.5.crate>
- SHA-256: `233daaf6e83ae6a12a52055f568f9d7cf4671dabb78ff9560ab6da230ce00ee5`
- Upstream fix: [gtk-rs-core commit b5a4071e439bef2b5eea76c3aa25e5ae84839e34](https://github.com/gtk-rs/gtk-rs-core/commit/b5a4071e439bef2b5eea76c3aa25e5ae84839e34)
- Upstream review: [gtk-rs-core PR 1343](https://github.com/gtk-rs/gtk-rs-core/pull/1343)

The production source change is exactly the two-line upstream fix in
`src/variant_iter.rs`: make `p` mutable and pass `&mut p` to the C function
that writes the out-pointer. This repairs `VariantStrIter::next`, `next_back`,
`nth`, `nth_back`, and `last`, which all use `impl_get`. The original version
number is retained. Version-only advisory scanners can still report this
backported package; review the patched source and its provenance.

The archive's `LICENSE` and `COPYRIGHT` are retained. Apart from this note and
the regression test in `src/variant_iter.rs`, the remaining published files
are unchanged.

On Linux with the GLib development libraries installed, run the iterator tests
with optimizations, where the original bug was observed:

```sh
cargo test --release --manifest-path vendor/glib-0.18.5/Cargo.toml --lib test_variant_str_iter
```

The regression exercises both directions, skipping, the last element, exhausted
iterators, and a UTF-8 value. The tests need a native GLib installation and are
not executed by Windows-only validation.
