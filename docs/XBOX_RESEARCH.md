# Xbox Series X|S Execution Environment — Research

**Evidence discipline.** Each claim is tagged:
`[DOC]` authoritative public documentation · `[PUB]` credible public/community
reverse-engineering · `[INF]` inferred from multiple observations ·
`[HYP]` hypothesis to verify · `[NDA]` known to exist but gated/undocumented
publicly, must be established from binary evidence.

This file is updated as the actual Fortnite binary is analyzed. Nothing here is
implemented on the strength of `[HYP]` alone.

---

## 1. CPU and ABI

- `[DOC]` Xbox Series X|S use a semi-custom **AMD Zen 2** CPU, **x86-64**. Series X:
  8 cores @ 3.8/3.6 GHz; Series S similar at lower clocks. AVX2 available.
- `[INF]` Therefore game code is **native x86-64** using the standard Windows x64
  ABI (RCX/RDX/R8/R9 + stack, 16-byte alignment, Rax return, shadow space,
  `.pdata`/`.xdata` unwind). **No CPU instruction translation is required** — this
  is the single most important architectural fact and separates this project from
  classic console emulation. *To confirm from the binary:* machine = AMD64, presence
  of a `.pdata` exception directory, ordinary SSE/AVX usage.
- `[HYP]` Possible use of instructions assuming features the host must also have
  (AVX2, BMI, etc.). Host is Ryzen 7 5700G (Zen 3) — superset of Zen 2 ISA, so
  feature availability is not expected to be a blocker. Verify via instruction scan.

## 2. Operating system

- `[DOC]`/`[INF]` The console runs a Windows-derived OS ("GameOS", a partition of the
  Xbox system software). Titles are built with the **Microsoft GDK**; the Xbox
  extension (GDKX) is confidential.
- `[INF]` The game is a **PE32+ (x64)** image using the MSVC toolchain and CRT, like a
  Windows app, but linked against **Xbox variants** of system DLLs and against
  **GDK runtime DLLs** that have no desktop-Windows equivalent.
- `[NDA]` The exact GameOS syscall surface and the behavior of Xbox variants of
  `kernel32`/`kernelbase`/`ntdll`-like modules are not public. Strategy: identify
  which imported symbols the binary actually uses and implement compatibility
  behavior for those, individually, with tests — not blanket assumptions that an
  Xbox API equals its Windows namesake (CLAUDE.md principle).

## 3. Packaging and filesystem

- `[DOC]` Xbox/GDK titles are packaged as **MSIXVC** (an MSIX variant). The authoritative
  config is **`MicrosoftGame.config`**; packaging tools generate an
  **`AppxManifest.xml`** from it. Installed content is a largely **flat file layout**;
  under MSIXVC, executables designated in the config remain **encrypted** at rest,
  while other files are directly accessible. (Sources: Microsoft GDK packaging docs.)
- `[INF]` "Already dumped and decrypted" should therefore mean: the EXE/DLLs are present
  as ordinary readable PE files (not encrypted XVC blobs), alongside
  `MicrosoftGame.config` and assets. **This is the premise to verify first** after
  extraction (do NOT assume an encrypted XVC — per CLAUDE.md). Key files to look for:
  `MicrosoftGame.config`, `AppxManifest.xml`, `*.exe`, `*.dll`, `Resources.pri`,
  asset `.pak`/`.ucas`/`.utoc` (Unreal) containers.

## 4. Graphics — Direct3D 12.x

- `[DOC]` The Xbox GDK Game OS graphics API is **Direct3D 12.x** ("D3D12X"), the
  console variant of D3D12. Microsoft documents that it differs from PC D3D12; the
  specific differences page is **NDA-gated** (`devdocs.xbox.com/nda`). Public naming
  observed: `d3d12_x` (Xbox One) and `_xs` for Series-console-specific surface
  (e.g. `D3D12_MESH_SHADER_TIER` Series entries).
- `[INF]` Expect the binary to import a console D3D12 module (e.g. `d3d12_x.dll` /
  `d3d12_xs.dll`) rather than PC `d3d12.dll`/`dxgi.dll`. Verify from imports.
- `[HYP]` D3D12.x is "closer to the metal": flattened/monolithic pipeline state,
  explicit memory placement in unified memory, direct access to some GPU features.
  A translation layer must address **command lists, resource/residency model,
  descriptor heaps, root signatures, barriers, fences, and presentation** — a shader
  translator alone is insufficient (CLAUDE.md §3D). Specifics are `[NDA]`; establish
  from the binary's actual API usage and from any shipped shader/PSO blobs.

## 5. Shaders

- `[DOC]`/`[INF]` The RDNA2 GPU consumes hardware microcode; D3D12 shaders authored as
  HLSL are compiled to **DXIL**, then (on console) precompiled offline into
  GPU-specific microcode stored in the pipeline/state database. (Microsoft's PC
  "Advanced Shader Delivery" uses an analogous SODB→PSDB model with vendor compilers;
  the console pipeline is related but `[NDA]`.)
- `[HYP]` The shipped Fortnite build may contain shaders as DXIL, as Xbox-precompiled
  microcode, or both (in a PSO/state cache). **This is a top technical risk.** If
  shaders are DXIL, a path to PC D3D12/DXC is plausible; if they are console
  microcode, we need either the DXIL source form or a microcode path. *Resolve by
  locating and identifying the shader container format in the extracted build.*

## 6. GDK runtime services (expected import surface)

- `[DOC]` Public GDK exposes (names to look for in imports): `XGameRuntime`
  (`XGameRuntimeInitialize`, lifecycle), **XTaskQueue** (async work queues),
  **XAsync** (async operation pattern), **XUser** (identity/profile), **XStore**,
  **XGameSave**/storage, **GameInput** (controllers), **XGameUI**, networking, audio
  (XAudio2.x). Each discovered symbol goes into `docs/COMPATIBILITY.md` with exact
  name, module, required-phase, PC-equivalent feasibility, and status.
- `[INF]` Fortnite is **Unreal Engine**; expect UE's platform abstraction to sit on top
  of these, which *narrows* the set of GDK calls actually exercised at startup.

## 7. Memory

- `[DOC]` Series X: 16 GB GDDR6 unified (10 GB "fast" + 6 GB), Series S: 10 GB. Unified
  memory means CPU and GPU share address space; GPU-visible allocations are a
  first-class concept.
- `[INF]` A PC has separate system RAM and VRAM. We cannot reproduce unified memory by
  allocating bytes; GPU-visible resources must be mapped through the host D3D12/Vulkan
  resource model. Do not hardcode console capacities.

## Open questions (to answer from the extracted binary)

1. Which exact modules does the main EXE import? (confirms GDK + D3D12.x surface)
2. Is the EXE a readable PE32+ or still an encrypted blob?
3. What is the Unreal Engine version and console RHI in use?
4. What shader/PSO container format ships, and is DXIL recoverable?
5. What is the entry/startup call sequence to first device creation?

## Sources (public)

- Microsoft GDK — "What is the Microsoft Game Development Kit?"
  https://learn.microsoft.com/en-us/gaming/gdk/docs/gdk-dev/intro/introduction
- GDK packaging (MSIXVC, MicrosoftGame.config, flat-file install):
  https://learn.microsoft.com/gaming/gdk/docs/features/common/packaging/overviews/packaging-getting-started-for-pc ,
  https://learn.microsoft.com/en-us/gaming/gdk/docs/features/common/game-config/microsoftgameconfig-overview
- Xbox devdocs graphics index (D3D12.x; differences page NDA-gated):
  https://devdocs.xbox.com/build/core-features/graphics/gc-graphics-toc
- Microsoft Advanced Shader Delivery (PC analog of offline shader precompilation):
  https://www.igorslab.de/en/microsofts-advanced-shader-delivery-amd-pcs/
