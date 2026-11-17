# AnyPS5 — Engineering Study (reference)

Study of the AnyPS5 source (`refs/AnyPS5`, shallow clone) for **transferable
engineering**, not PlayStation internals. AnyPS5 runs PS5 games on PC. Findings
below are file-cited against that tree. PS5 is also x64, so its *architecture*
(not its ABI/GPU specifics) is the closest existing analog to this project.

## What AnyPS5 is
A **static relinker + reimplemented system libraries**, explicitly *not* an
emulator (`AGENTS.md:3`, `docs/dev/ARCHITECTURE.md:3`). A PS5 ELF is rewritten into a
native host executable (ELF/PE/Mach-O) that runs as an ordinary process and
dynamically links against host-built replacement libraries. C++20, CMake+Ninja.

## Component map (real dirs)
- `core/relinker/` — offline binary translator (parse, map, relocate, rewrite imports).
  Clean ports-and-adapters DI: `core/relinker/relinker/include/relinker/domain/I*.hpp`.
- `core/libs/prx/` — ~413 reimplemented system libs, each a host shared library.
- `core/libs/nid/` — post-build patcher renaming exports to PS5 NID hashes so the
  host loader binds guest imports.
- `core/shader/recompiler/` — RDNA2/GCN ISA → SPIR-V.
- `tools/` — oracles (hw-oracle, library-oracle), replay harnesses, ~90 py scripts.
- `3rdparty/` — ~45 from-source submodules.

## Transferable to an Xbox (PE32+/x64, D3D12.x, GDK) runtime
1. **Static relinking, not emulation.** Xbox already ships PE32+/x64 → we *skip
   AnyPS5's hardest step* (ELF→PE conversion). Keep the section-map + relocation +
   import-rewrite core, and copy the DI interface design (makes it unit-testable &
   fuzzable offline).
2. **Bind imports via the host loader.** AnyPS5 renames replacement-lib exports to
   match guest import symbols, then lets the OS loader bind them (no per-call
   trampoline). For Xbox the import *names already match* desktop conventions
   (`d3d12.dll`, `xaudio2.dll`, `xgameruntime.dll`), so we may need **no rename at
   all** — just point the guest's import of an Xbox-only module at our DLL.
3. **One reimplemented DLL per component**, each independently built + tested + with
   its own gap ledger (`core/libs/prx/<lib>/tests/`, `docs/dev/debt/<lib>.md`).
4. **Fail loud, never fake success.** `NotImplemented_nid_no_patch(__func__)` is
   `[[noreturn]]` (`libc/include/General.hpp:15`). Implement for *all* inputs, never
   special-case one title's trace (`AGENTS.md:6`).
5. **Differential oracle testing.** `tools/hw-oracle` (run ISA on real GPU),
   `tools/library-oracle` (diff vs reference lib). For Xbox we can diff our
   `d3d12`/`xaudio2` against **Microsoft's PC D3D12/XAudio2, which exist on PC** —
   a strong correctness oracle AnyPS5 doesn't even have for most of its surface.
6. **Capture/replay at the API boundary** (`agc_frame_replay`, `agc_pm4_replay`).
   Xbox analog: capture D3D12 command lists/PSOs at `ExecuteCommandLists`, replay in CI.
7. **Content-hashed, device-fingerprinted PSO cache + startup journal replay**
   (`ShaderDiskCache`, `PipelineJournal.cpp`) — general fix for PSO-compile stutter;
   D3D12 has the same problem.
8. **Runtime driver-quirk probing at device creation** (`FloatFlushProbe.cpp`):
   compile two variants, compare bit-for-bit, pick the trustworthy path per GPU.
9. **Granular dependency-free sub-builds + impacted-test selection**
   (`-DANYPS5_RELINKER_ONLY`, `tools/select_tests.py`).
10. **Zero-copy guest memory into GPU where the driver is fast, tracked copy fallback**
    (`VK_EXT_external_memory_host` + write-watch). D3D12 analog:
    `OpenExistingHeapFromAddress`/upload heaps, decided per-device.

## Does NOT transfer (do not copy)
- **NID hashing** (`core/libs/nid/NidCompute.cpp`) — Xbox imports by name/ordinal.
- **SELF/SCE ELF handling + ELF→PE conversion** (`elfpatcher/`) — Xbox is already PE.
- **RDNA→SPIR-V recompiler + PM4/AGC decoder** — Xbox emits DXBC/DXIL + D3D12 command
  lists; our path is DXIL→backend and D3D12.x→native D3D12. *Staging/caching transfers;
  ISA/packet specifics do not.*
- **PS5 libkernel ABI** (FreeBSD syscalls, SysV AMD64, red zone, raw `syscall` trapping,
  `codegen/x86/RedZoneFramer.cpp`). Xbox = Windows x64 ABI + SEH + GDK. **Note:** AnyPS5
  uses MinGW-w64 on Windows *because* it needs SysV-ABI catch handlers — a constraint
  that does **not** apply to us, so MSVC-ABI (Rust `-msvc`, or MSVC C++) is the right fit.
- The ~413 `libSce*` catalog — we reimplement the GDK/D3D12/GameInput/XAudio2 surface.

## Loader fail-loud model worth copying verbatim
- Relocation support is an explicit **allowlist** (`analysis/ValidationPolicy.cpp`);
  unexpected types are rejected, not skipped.
- Format gate rejects the wrong container outright (`ElfReader::ValidateMagic`).
- Uncaught exceptions print `what()` and end the process — no silent degradation.
