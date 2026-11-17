# Technical Debt / Known Gaps / Unverified Assumptions

Honest ledger. Updated continuously.

## Incomplete functionality
- Loader, compat libraries, host services, graphics backend: **not started**
  (intentionally — these follow the real binary's evidence per ADR-0002).
- ~~`pe-inspect --json` omits exports, relocations, TLS~~ **RESOLVED**: JSON now
  includes exports (named+ordinals), relocation type histogram, and TLS summary.

## Independent validation done
- `pe-inspect` cross-checked against **Microsoft `dumpbin`** on the same binary:
  machine/sections/magic/entry-point/image-base/subsystem and the full import
  module list all agree. (CLAUDE.md §3A — compare against independent tools.)

## Tests that should exist but don't yet
- Base-relocation parser: a fixture asserting exact block/entry/type counts
  (currently only exercised, not value-asserted).
- TLS parser: a fixture asserting callback count on a binary with TLS callbacks.
- A hand-built minimal PE32+ fixture for fully deterministic unit tests that do not
  depend on `C:\Windows\System32`.
- `cargo fuzz` / sanitizer builds not yet configured (MSVC target; consider a
  property-test loop as an interim — the random/truncation tests partially cover this).

## Unverified assumptions (must confirm from binary)
- **A1** The extracted EXE is a readable PE32+, not an encrypted blob. _[premise from user; verify first]_
- **A2** Game code is native x64 needing no instruction translation. _[strong inference; confirm machine + .pdata]_
- **A3** Imports reference console D3D12.x + GDK modules distinct from PC DLLs. _[expected; confirm]_
- **A4** Host Zen 3 ISA is a superset of whatever the binary needs. _[likely; confirm via instruction scan]_
- **A5** Fortnite = Unreal Engine; UE narrows the GDK surface at startup. _[expected; confirm engine version]_

## Architectural risks
- **R1 (high)** Shader format: if shaders ship as Xbox microcode rather than DXIL,
  the graphics path is dramatically harder. Unknown until the shader container is found.
- **R2 (high)** D3D12.x semantic differences from PC D3D12 are NDA-gated; must be
  reverse-engineered from usage. A shader translator alone is not enough.
- **R3 (med)** GameOS/NT API behavior differences hidden behind familiar names.
- **R4 (med)** No MSVC-CMake/clang on PATH; C++ interop (e.g. with host D3D12 headers)
  will go through Rust's `windows` crate or a located MSVC toolchain, not system CMake.
- **R5 (med)** Anti-cheat / online dependencies at startup could block a pure offline
  boot; must map whether first-frame requires any network/service call (we will not
  connect to production services).
- **R6 (low/now)** 25.5 GB input: disk and iteration cost; keep derived analysis small.

## Environment constraints
- No real Python interpreter (Store alias stub only).
- `cmake`/`ninja`/standalone `clang`/`dumpbin` not on PATH (MSVC toolset present under
  VS 2026 but not in the default shell environment).
