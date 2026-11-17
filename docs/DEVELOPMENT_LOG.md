# Development Log

All times UTC. This log is append-only and records what was actually done, run, and observed.

---

## 2026-11-16 — Session 1: Reconnaissance + toolkit bootstrap

### Environment (observed)
- **OS:** Windows 11 Home 10.0.22631 (x64)
- **CPU:** AMD Ryzen 7 5700G (8C/16T)
- **RAM:** 47.6 GB
- **GPU:** NVIDIA GeForce RTX 5060 Ti (primary); a "Meta Virtual Monitor" virtual adapter also present
- **Disk:** D: has 321.7 GB free (project lives on D:); C: 127 GB free
- **Toolchains found:**
  - Rust `rustc 1.99.1`, host `x86_64-pc-windows-msvc`, via rustup. Build+link+run smoke test PASSED.
  - MSVC VC tools: VS 2026 Community (`18.9.12105.275`), toolset 14.16.27023 at
    `C:\Program Files\Microsoft Visual Studio\18\Community\VC\Tools\MSVC\14.16.27023` (also VS 2017 present).
  - `.NET 10.0.400` (`dotnet`)
  - `git`, `node`
  - **No usable Python** — the `python.exe` on PATH is the Microsoft Store App Execution Alias stub, not a real interpreter.
  - `cmake`, `ninja`, standalone `clang`, `dumpbin` not on PATH.
- **MCP reverse-engineering servers available:** `rea` and `ida`. These are the primary binary-analysis tools once the real executable is extracted.

### Actions
1. Inspected working directory: contained only `CLAUDE.md` (not a git repo).
2. Created input/analysis/docs/src layout.
3. Probed the supplied archive URL via HTTP HEAD:
   - Status 200, `Content-Length` = **27,385,950,541 bytes (25.51 GB)**, `Content-Type: application/zip`, `Accept-Ranges: bytes` (resumable).
4. Wrote `input/original/download_metadata.json`.
5. Started a resumable background download with `curl.exe -L -C - --retry 5`. **(in progress)**
6. Chose Rust for the binary toolkit (see ADR-0001).

### Status of milestones (CLAUDE.md §12)
- [~] 1. Archive downloaded and checksum recorded — download IN PROGRESS (25.5 GB).
- [ ] 2. Dump extracted and inventoried.
- Remaining milestones blocked on the download completing.

### Next priorities
- While download runs: build + test the PE/COFF parser against known Windows PEs (does not need the real binary).
- On completion: verify SHA-256, extract safely, inventory, identify main executable + modules, parse it, enumerate imports to find Xbox/GDK dependencies.

### Progress during the download (same session)
- **Built `tombox-pe` + `pe-inspect`** (zero-dep PE32+ parser). 8 tests pass incl.
  real `kernel32.dll` (1671 exports, `CreateFileW` present, AMD64/PE32+), a no-panic
  sweep over random / PE-shaped / truncated inputs, and a DIR64-relocation assertion.
  Verified `pe-inspect` on a real binary: full header/section/import/dir/TLS/reloc report + valid JSON.
- **Studied AnyPS5** (delegated Explore agent; `docs/ANYPS5_STUDY.md`). Confirmed a
  static-relink + reimplemented-syslib design. Drove **ADR-0002**: native x64 execution
  with import redirection, not emulation. Noted AnyPS5 avoids MSVC only due to SysV-ABI
  catch handlers — not a constraint for Xbox, so Rust/MSVC is the right fit.
- **Verified public Xbox facts** (web): GDK D3D12.x, MicrosoftGame.config/MSIXVC
  flat-file layout, DXIL+offline-precompiled shaders — specifics NDA-gated → will come
  from binary evidence. Wrote `docs/XBOX_RESEARCH.md` with confidence tags.
- **Built `tombox-archive` + `zip-inspect`** (zero-dep, ZIP64-aware central-directory
  reader + extraction safety validator). 9 tests pass: ZIP64 64-bit size resolution,
  path-traversal / absolute / drive-letter / UNC / symlink blocking, bomb-ratio warning.
  Verified on a real `Compress-Archive` zip. **This gates the 25 GB extraction.**
- **Wrote docs**: README, ARCHITECTURE, COMPATIBILITY, TECHNICAL_DEBT, FORTNITE_ANALYSIS,
  DECISIONS (ADR-0001/0002), ANYPS5_STUDY. Initialized local git (game data/refs/target excluded).
- **17 tests total, all passing.** Toolchain: full Rust build/link/run confirmed; MSVC
  toolset located (VS 2026, 14.16).

---

## 2026-11-16 — Session 1 (cont.): download complete + real binary analysis

### Download & verification
- Archive downloaded in full: 27,385,950,541 bytes (size matches server exactly).
- **SHA-256 (archive):** `433E7B3B65C0C51A4F20001721F7CC9FDA9AE97A0022A7F7B5AA94914BEFCA95`
- `zip-inspect` on the real archive: ZIP64, 834 entries, **safe** (0 blocking findings).

### Extraction (selective, safe)
- Vetted first, then extracted **code only** (Binaries + System32 DLLs/EXEs + manifests +
  symbols = ~390 MB) via .NET ZipFile with a path-traversal guard; skipped 24 GB of assets.
- Structure: `EmbeddedXvd/` (Xbox GameOS system image, 276 System32 DLLs) + `Mount/`
  (game package). **Loose decrypted files — not an encrypted XVC** (premise confirmed).

### Main executable (parsed with pe-inspect)
- `FortniteClient-XSX-Shipping.exe`, 150 MiB, **PE32+/AMD64**, WINDOWS_GUI, MSVC linker 14.24,
  entry RVA `0x229a298`, image base `0x140000000`, ASLR/DEP, **2,045,522 DIR64 relocations**,
  TLS present, 8 sections incl. Xbox-specific **`.xbld`**.
- **SHA-256 (EXE):** `0B3D2E5968B0913EB01F52612793BFCC1984FF9764BD15D1C5AABF4830FCC3A0`
- **67 import modules / 670 functions.** ~52 are standard Windows API-Sets/UCRT (BindOS
  candidates). Reimplementation list is short: `d3d12_xs`, `xmem`, `xg_xs`, `GameInput`,
  `XAudio2_9`, `AcpHal`, `XCRDAPI`(DirectStorage), `dxcompiler_xs`, + dynamic `xgameruntime`.
- Analyzed each Xbox system DLL's exports/imports (`analysis/syslib/*.json`): `d3d12_xs` is
  a 33 KB **thin shim** exporting standard D3D12 COM + Xbox ext; `xgameruntime` exports only
  `QueryApiImpl` dispatch and is **dynamically loaded** (not a static import).

### Shaders
- **DXBC/DXIL confirmed** (not alien microcode): `dxcompiler_x/xs.dll` (DXC) + `dxbc2dxil_xs.dll`
  present; EXE imports `DxcCreateInstance`. Top graphics risk (R1) downgraded.

### Symbols
- `FortniteClient-XSX-Shipping-Symbols.bin` = UE console symbol DB, decompresses to 118 MB of
  demangled C++ names. Startup chain: `GuardedMain`→`FEngineLoop`→`RHIInit`→`FD3D12DynamicRHI`.

### Milestones reached (CLAUDE.md §12)
- [x] 1 Archive downloaded + checksum. [x] 2 Extracted + inventoried. [x] 3 Main exe + modules
  identified. [x] 4 Executable parsed/validated. [x] 5 Dependencies/imports mapped.
- [~] 6 Xbox loader requirements: substantially understood (dynamic xgameruntime, `.xbld`,
  import redirection). [~] 7 Architecture: ADR-0002 confirmed by evidence.

### Next priorities
1. Decode `-Symbols.bin` address↔name map; inspect `.xbld`.
2. Enumerate GDK `QueryApiImpl` surface via symbol-labelled call sites (REA/IDA).
3. Begin the loader PoC: map + DIR64 reloc + import table build with redirection + fail-loud
   unsupported-feature table (now evidence-justified).
