# Architecture

> This is a **proposal under construction**, not a finished design. Per CLAUDE.md,
> the architecture follows the evidence; sections marked *(provisional)* will be
> revised once the actual binary's requirements are mapped.

## Execution model (provisional)

Because Xbox Series code is native x86-64 (see `XBOX_RESEARCH.md §1`), the intended
model is **native execution of the game's own x64 code** inside a host Windows
process, with a compatibility layer that satisfies the game's imports and a
graphics translation layer — **not** CPU emulation.

```
          ┌─────────────────────────────────────────────────────────┐
          │                 host process (Windows x64)               │
          │                                                          │
          │   ┌───────────┐   maps & relocates   ┌───────────────┐   │
          │   │  loader   │─────────────────────▶│  game image   │   │
          │   │ /relinker │   resolves imports    │ (native x64)  │   │
          │   └─────┬─────┘        │              └──────┬────────┘   │
          │         │              ▼                     │ calls      │
          │         │     ┌──────────────────┐           ▼            │
          │         │     │ compat libraries │◀── imports resolved to │
          │         │     │ (xgameruntime,   │    our implementations │
          │         │     │  xtaskqueue, …)  │                        │
          │         │     └───────┬──────────┘                        │
          │         │             ▼                                   │
          │         │     ┌──────────────────┐   ┌─────────────────┐  │
          │         └────▶│ host services    │   │ graphics backend│  │
          │               │ (mem, threads,   │   │ D3D12.x → host  │  │
          │               │  fs, input, …)   │   │ D3D12 / Vulkan  │  │
          │               └──────────────────┘   └─────────────────┘  │
          └─────────────────────────────────────────────────────────┘
```

## Subsystem boundaries (planned)

| Subsystem | Responsibility | Status |
|---|---|---|
| `tombox-pe` | Safe PE/COFF parsing & fact reporting | **implemented, tested** |
| `pe-inspect` | CLI/JSON inspection, dependency classification | **implemented, tested** |
| loader/relinker | Map sections, apply relocations, resolve imports, set up TLS/exceptions | not started |
| compat libraries | Replacement implementations of Xbox/GDK import modules | not started |
| host services | Memory, threads, timing, filesystem, input, audio behind host-abstraction interfaces | not started |
| graphics backend | D3D12.x command/resource/descriptor/shader translation to host D3D12 or Vulkan | not started |
| diagnostics | Structured logging, import-miss reporting, crash capture | not started |

## Design rules

1. **Fail loud.** Unsupported relocations, imports, or features must produce an
   explicit diagnostic naming the exact symbol/feature — never a silent stub that
   returns success.
2. **Evidence-gated.** No subsystem is built ahead of demonstrated need from the
   dependency graph. Build the shortest valid path to a first frame.
3. **Host code behind interfaces.** All OS/GPU specifics sit behind traits so the
   core stays portable and testable; Linux can be evaluated later without rewrites.
4. **Game-specific vs. general.** Fortnite-specific workarounds are labelled as such
   and kept out of the reusable platform code.
5. **Independently testable.** Each subsystem ships with tests and small fixtures
   before game integration.

## Data flow to "first frame" (target path)

1. Parse & validate the main EXE (`tombox-pe`). ✔ tooling ready
2. Enumerate imports → dependency graph → classify required-at-startup modules.
3. Loader maps the image, applies relocations, builds an import thunk table bound to
   compat-library implementations; unresolved imports are reported, not faked.
4. Minimal compat libs bring the CRT + GDK init far enough to create a device.
5. Graphics backend creates a device/swapchain and presents the first rendered frame.

Steps 2+ are blocked on completing the download and extraction.
