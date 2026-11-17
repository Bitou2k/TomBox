# TomBox — Xbox Series X|S Compatibility Runtime (research)

An investigation into loading and running user-provided, already-dumped/decrypted
**Xbox Series X|S** game builds on conventional Windows x64 PC hardware.

> **Status: early reconnaissance + tooling.** No game runs yet. This repository
> currently contains a working, tested PE/COFF analysis toolkit and the research
> record. Nothing here claims Fortnite boots. Claims are labelled by evidence
> level (see `docs/COMPATIBILITY.md`).

First test target: **Fortnite Xbox Series X|S — version 2.0.1461.7919**.

## Why this might be more tractable than classic emulation

The Xbox Series X|S CPU is **AMD Zen 2, x86-64** — the same instruction set as the
host PC. Game code is native x64; there is **no CPU to emulate**. The real work is
compatibility at three boundaries:

1. **OS / platform APIs** — the game imports Xbox GameOS + GDK runtime DLLs
   (e.g. `xgameruntime`) that do not exist on desktop Windows.
2. **Graphics** — the game targets **Direct3D 12.x** (the Xbox console variant of
   D3D12), not PC D3D12, and its shaders are not necessarily plain DXBC/DXIL.
3. **Packaging / filesystem** — content layout and path assumptions differ.

This is the same class of problem the PS5 project *AnyPS5* solves (PS5 is also x64);
we study its architecture for transferable engineering, not PlayStation internals.

## Current verified capabilities

- `tombox-pe`: a zero-dependency, bounds-checked PE32+ parser. Parses COFF/optional
  headers, sections, data directories, imports, delay-imports, exports, base
  relocations, and TLS. **Never panics on malformed input** (fuzz/truncation tested).
- `pe-inspect`: a CLI that emits a human report or JSON, and heuristically flags
  likely Xbox/GDK modules in a binary's dependency list.
- Verified against real Windows PE32+ binaries (kernel32.dll: 1671 exports incl.
  `CreateFileW`; our own build: 10 import modules / 91 functions).

## Build

Requires a Rust toolchain (tested: `rustc 1.99.1`, `x86_64-pc-windows-msvc`).

```
cargo build --release
cargo test
./target/release/pe-inspect <path-to-pe> [--json] [--imports] [--max N]
```

## Supported host platforms

- Windows x64 (primary, developed here).
- Core parser is portable Rust; host-specific runtime code (later) will sit behind
  explicit interfaces so Linux can be evaluated.

## Current limitations

- No loader, no runtime, no graphics layer yet.
- The supplied 25.5 GB build is still downloading at the time of writing; analysis
  of the *actual* executable has not yet begun.
- Deep Xbox-internal specifics (D3D12.x differences, shader microcode, GameOS
  syscalls) are NDA-gated and will be established from binary evidence, not guessed.

## Layout

```
input/original/    # the original downloaded archive, preserved unchanged
input/extracted/   # extracted build (kept out of any future VCS)
analysis/          # generated inspection output (JSON, reports)
crates/            # Rust workspace (tombox-pe, tombox-cli)
docs/              # ARCHITECTURE, XBOX_RESEARCH, FORTNITE_ANALYSIS, COMPATIBILITY,
                   # TECHNICAL_DEBT, DEVELOPMENT_LOG, DECISIONS
refs/              # third-party reference source (AnyPS5) — study only
```

## Legal / scope

Authorized interoperability research. Works only with binaries the user is
authorized to analyze. No console-security circumvention, no connection to Fortnite
production services, no anti-cheat/entitlement bypass. Game data is kept out of
source control and is not redistributed.
