# Dependency License Audit

This audit covers the resolved Cargo dependency graph for:

```sh
cargo metadata --format-version 1 --all-features --locked
```

The repository also includes a CI-friendly guard:

```sh
python3 scripts/check_licenses.py
```

Last checked: 2026-06-19.

## Recommendation

Use `MIT OR Apache-2.0` for this crate.

That is the standard Rust library choice, matches most of the dependency graph,
is permissive for commercial and open-source users, and gives downstream users
the option of Apache-2.0 patent terms or MIT simplicity. The manifest already
declares:

```toml
license = "MIT OR Apache-2.0"
```

The repository includes `LICENSE`, `LICENSE-MIT`, and `LICENSE-APACHE`.

## Current Result

- No resolved dependency is missing license metadata.
- No GPL or AGPL dependency is present in the current all-feature graph.
- The previously unused `subparse` dependency was removed; that also removed
  its `LGPL-3.0` transitive dependency (`chardet`) from the resolved graph.
- The previously unused `mp4parse` dependency was removed; this also reduced
  unnecessary MPL-2.0 surface.
- `audio-io` still uses `symphonia`, whose crates are `MPL-2.0`. This is
  acceptable for a permissively licensed Rust library when treated as an
  external dependency, but modifications to Symphonia itself must follow MPL-2.0
  terms.
- `r-efi` appears in the all-feature transitive graph with
  `MIT OR Apache-2.0 OR LGPL-2.1-or-later`; the permissive branch is available.
- `libfuzzer-sys` appears with `(MIT OR Apache-2.0) AND NCSA`; this is
  compatible with the recommended dual license.

This is not legal advice; re-run the audit before publishing a release.

## Direct Dependencies

| Crate | Version | License |
| --- | ---: | --- |
| `ab_glyph` | 0.2.32 | Apache-2.0 |
| `bytes` | 1.11.1 | MIT |
| `clap` | 4.6.1 | MIT OR Apache-2.0 |
| `fast_image_resize` | 5.5.0 | MIT OR Apache-2.0 |
| `futures` | 0.3.32 | MIT OR Apache-2.0 |
| `gif` | 0.14.2 | MIT OR Apache-2.0 |
| `h264-reader` | 0.8.0 | MIT/Apache-2.0 |
| `hound` | 3.5.1 | Apache-2.0 |
| `image` | 0.25.10 | MIT OR Apache-2.0 |
| `imageproc` | 0.25.1 | MIT |
| `m3u8-rs` | 6.0.0 | MIT |
| `matroska-demuxer` | 0.7.0 | Zlib OR MIT OR Apache-2.0 |
| `mp4e` | 1.0.5 | MIT |
| `muxide` | 0.2.5 | MIT OR Apache-2.0 |
| `object_store` | 0.12.5 | MIT/Apache-2.0 |
| `png` | 0.18.1 | MIT OR Apache-2.0 |
| `rav1e` | 0.8.1 | BSD-2-Clause |
| `ravif` | 0.13.0 | BSD-3-Clause |
| `re_mp4` | 0.5.0 | MIT |
| `resvg` | 0.47.0 | Apache-2.0 OR MIT |
| `rubato` | 2.0.0 | MIT |
| `rust_h264` | 0.4.0 | MIT OR Apache-2.0 |
| `rust_h265` | 0.1.0 | MIT OR Apache-2.0 |
| `symphonia` | 0.5.5 | MPL-2.0 |
| `thiserror` | 2.0.18 | MIT OR Apache-2.0 |
| `v_frame` | 0.3.9 | BSD-2-Clause |
| `yuv` | 0.8.16 | BSD-3-Clause OR Apache-2.0 |

## All-Feature License Expression Summary

| Count | License expression |
| ---: | --- |
| 1 | `(Apache-2.0 OR MIT) AND BSD-3-Clause` |
| 1 | `(MIT OR Apache-2.0) AND NCSA` |
| 1 | `(MIT OR Apache-2.0) AND Unicode-3.0` |
| 1 | `0BSD OR MIT OR Apache-2.0` |
| 7 | `Apache-2.0` |
| 1 | `Apache-2.0 / MIT` |
| 1 | `Apache-2.0 AND ISC` |
| 1 | `Apache-2.0 OR BSL-1.0` |
| 4 | `Apache-2.0 OR ISC OR MIT` |
| 18 | `Apache-2.0 OR MIT` |
| 3 | `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` |
| 4 | `BSD-2-Clause` |
| 2 | `BSD-2-Clause OR Apache-2.0 OR MIT` |
| 6 | `BSD-3-Clause` |
| 3 | `BSD-3-Clause OR Apache-2.0` |
| 1 | `CC0-1.0 OR Apache-2.0` |
| 2 | `ISC` |
| 66 | `MIT` |
| 172 | `MIT OR Apache-2.0` |
| 1 | `MIT OR Apache-2.0 OR LGPL-2.1-or-later` |
| 4 | `MIT OR Apache-2.0 OR Zlib` |
| 1 | `MIT OR Zlib OR Apache-2.0` |
| 21 | `MIT/Apache-2.0` |
| 14 | `MPL-2.0` |
| 18 | `Unicode-3.0` |
| 4 | `Unlicense OR MIT` |
| 2 | `Unlicense/MIT` |
| 1 | `Zlib` |
| 4 | `Zlib OR Apache-2.0 OR MIT` |
| 2 | `Zlib OR MIT OR Apache-2.0` |

## Notable Transitive Licenses

| Crate | Version | License | Notes |
| --- | ---: | --- | --- |
| `symphonia` and component crates | 0.5.5 | MPL-2.0 | Optional via `audio-io`; file-level weak copyleft for that dependency. |
| `r-efi` | 5.3.0 | MIT OR Apache-2.0 OR LGPL-2.1-or-later | Permissive branch is available. |
| `libfuzzer-sys` | 0.4.12 | (MIT OR Apache-2.0) AND NCSA | Compatible; transitive tooling/fuzzing surface. |
