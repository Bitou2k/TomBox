# Architecture Decision Records

Each record: problem, evidence, options, decision, rejected alternatives, risks, tests.

---

## ADR-0001 — Implementation language for the binary toolkit and loader

**Date:** 2026-11-16

**Problem.** The project needs (a) a safe parser for untrusted Xbox PE binaries and
(b) eventually a loader/runtime that maps and executes that code on Windows x64 with
precise control over memory layout, page protections, and native OS integration.

**Evidence.**
- Rust 1.99.1 (`x86_64-pc-windows-msvc`) is installed and its full build→link→run chain
  works here (verified with a smoke-test binary).
- MSVC toolset 14.16 is present, so the MSVC linker Rust needs is available.
- Real Python is not installed (only the Store alias stub); `.NET 10` is present.
- CLAUDE.md §10 mandates memory safety, bounds checking, and safe parsing of untrusted input.

**Options.**
1. Rust — memory-safe by default, zero-cost abstractions, precise control over layout and
   raw pointers via `unsafe` where needed, first-class Windows FFI (`windows`/`winapi`),
   no runtime/GC.
2. C++ (MSVC) — maximum control and the native ABI, but manual memory safety; higher defect
   risk in untrusted-input parsers.
3. C# / .NET 10 — safe and productive, but GC and runtime make a from-scratch executable
   loader / custom memory manager awkward; native interop is heavier.
4. Python — ruled out (not actually installed; also unsuitable for the loader).

**Decision.** Rust for both the analysis toolkit and the runtime core. Safety for the parser
of untrusted input is non-negotiable; Rust also keeps a clean path to the low-level loader
without a second language.

**Rejected.** C++ (safety burden on untrusted parsers), C# (GC/runtime vs. a custom loader),
Python (unavailable + unsuitable).

**Risks / unknowns.**
- The eventual loader will need substantial `unsafe` and Windows API use; that code must be
  carefully isolated and tested.
- If an Xbox-specific structure needs a layout Rust expresses awkwardly, revisit per-module.

**Tests.** Parser correctness is validated against known-good Windows PEs (system DLLs, our own
built binaries) and against malformed/truncated inputs that must be rejected without panicking.

**Status.** Adopted. Core parser has zero external dependencies to keep it fully auditable and
network-independent.

---

## ADR-0002 — Execution strategy: static/dynamic relink + import redirection, not emulation

**Date:** 2026-11-16

**Problem.** How should the Xbox Series executable actually run on the host?

**Evidence.**
- Xbox Series CPU is x86-64 (Zen 2); the game is a native PE32+/x64 image using the
  Windows x64 ABI (`XBOX_RESEARCH.md §1`). No instruction set to emulate.
- The AnyPS5 study (`ANYPS5_STUDY.md`) shows a proven design for the analogous PS5
  case: a static relinker that rewrites the guest image to bind its imports to
  reimplemented host system libraries, run by the native OS loader — **not** an
  emulator. AnyPS5's single hardest step is ELF→PE *format conversion*.
- Xbox executables are **already** PE32+/x64 and import by ordinary DLL name/ordinal,
  so the format-conversion and NID-rename steps are unnecessary for us.
- Microsoft's own PC D3D12 / XAudio2 / XInput exist on the host, giving a correctness
  oracle for large parts of the compat surface.

**Decision.** Pursue **native execution of the guest's own x64 code** inside a host
Windows process, with:
1. a loader that maps the image, applies relocations, initializes TLS/exceptions, and
2. **import redirection**: the guest's imports of Xbox-only modules (e.g.
   `xgameruntime.dll`, a console `d3d12_x.dll`/`d3d12_xs.dll`, `GameInput`, console
   `xaudio2`) are resolved to our reimplemented compatibility DLLs; standard imports
   that behave identically may bind to real Windows libraries (to be verified per symbol).

One reimplemented module per Xbox component, each independently built and tested,
with a fail-loud `[[noreturn]]`-style "not implemented" discipline (no faked success).

**Rejected.**
- *Full CPU emulation* — unnecessary (native x64) and enormously slower.
- *Pure native load with real Windows DLLs* — fails because Xbox-only modules and
  console API/GPU semantics are absent on desktop Windows.
- *Copying AnyPS5's ELF→PE/NID/SPIR-V machinery* — PS5-specific; does not apply.

**Risks / unknowns.**
- The exact divergence of Xbox variants of "familiar" system DLLs from desktop
  Windows is `[NDA]`; which imports can safely bind to real Windows libs is unverified
  (tracked as assumption A3). Must be decided per symbol, with tests.
- Graphics (D3D12.x semantics) and shader format are the dominant risks (R1, R2).
- Whether a first frame can be reached fully offline (no service/anti-cheat) — R5.

**Tests.** Validated incrementally: loader against the real EXE; each reimplemented
module against Microsoft's PC equivalent where one exists; import-redirection verified
by observing the guest reach successive startup milestones without faked returns.

**Status.** Adopted as the working strategy, pending confirmation from the actual
binary's import surface (download in progress).
