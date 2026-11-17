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

## 2026-11-16 — Session 2: real binary + first compat DLLs

### Archive (downloaded, exact size match 27,385,950,541 bytes)
- `zip-inspect`: ZIP64, 834 entries, 0 symlinks/traversal → **safe**. SHA-256 in `input/original/sha256.txt`.
- Structure: `Mount/` (game) + `EmbeddedXvd/` (Xbox GameOS System32 DLLs) + `era.vbi`. NOT an encrypted XVC — loose decrypted files (premise holds).
- `MicrosoftGame.config`: main exe `FortniteClient-XSX-Shipping.exe`, `TargetDeviceFamily="Scarlett"`, `RequiresXboxLive=true`.
- Selectively extracted ~375 MB of code (Binaries + System32 + manifests); skipped 24 GB of `.ucas/.pak/.mp4` assets.

### Main EXE (`FortniteClient-XSX-Shipping.exe`, 157 MB)
- PE32+ / AMD64, 8 sections, LARGE_ADDRESS_AWARE, ASLR/DEP/HIGH_ENTROPY_VA, subsystem WINDOWS_GUI, linker 14.24 (MSVC2019), entry 0x229a298, image base 0x140000000, 2,045,522 base relocs.
- **67 import modules / 670 functions.** ~52 are standard Windows API-Sets/UCRT (bind to real Windows). Delay import: GameChat2.dll.
- **Xbox-only modules (the reimplement list):** d3d12_xs (6), xmem (10), xg_xs (1 `XGCreateTextureComputer`), GameInput (ord #1), XAudio2_9 (ords #1-6), AcpHal (6), XCRDAPI (`DStorageGetFactory`), dxcompiler_xs (`DxcCreateInstance`).
- `FD3D12DynamicRHI` present, `FScarlett`/`ScarlettRHI` absent → D3D12.x handled inside the normal D3D12 RHI → **D3D12.x is a dialect of PC D3D12**.
- Symbols: `Mount/Symbols/*-Symbols.bin` = UE console symbol DB, `[magic 0x076b85ee][size][zlib]` → 118 MB of demangled C++ names (GuardedMain/FEngineLoop/RHIInit present). Record layout `[rva][size][?]`×1,245,949 + length-prefixed string table (full decode deferred; names confirmed present).

### VALIDATED ON REAL HARDWARE (user)
- User launched the EXE natively on Windows: it loads to import resolution and only fails on missing Xbox-only DLLs → **confirms ADR-0002 (native x64 + import redirection), no CPU emulation.**
- Build 1 DLLs (xmem, d3d12_xs, xg_xs, xcrdapi): exports verified == imports via our own `pe-inspect`; static CRT; only import kernel32/ntdll/apisets. xmem heap-backed (real), rest fail-loud stubs, all call-logged to `%TEMP%\tombox_compat.log`.
- After build 1, loader advanced and requested `dxcompiler_xs.dll` + `AcpHal.dll` → Build 2 shipped (both verified).

### Next
- Await user's next-missing-DLL + the compat log (call order) → implement earliest-called Xbox API for real; D3D12 device creation is the expected wall (needs the graphics backend).

## 2026-11-16 — Session 2 cont.: boots to GDK runtime init (major milestone)

### Milestone reached: the real game runs its own engine code
After the compat DLLs + two xmem fixes, `FortniteClient-XSX-Shipping.exe` loads
natively and executes its own startup code. Verified via a purpose-built debug
tracer (scratchpad `mdbg.rs`: DEBUG_ONLY_THIS_PROCESS loop capturing
OutputDebugString, exceptions, exit-sink stack).

### Two real bugs found + fixed from runtime evidence (not guesses)
1. `XMemVirtualAlloc` passed Xbox allocation-type bits (0x20000000) to Windows
   `VirtualAlloc` → read as MEM_LARGE_PAGES → ERROR_INVALID_PARAMETER → null →
   UE memory init failed. Fix: mask to Windows-valid alloc-type bits.
2. `XMemVirtualAlloc` committed pages as PAGE_NOACCESS (Xbox prot value 0x1 passed
   through) → game wrote to committed page → ACCESS_VIOLATION (confirmed via
   minidump + disasm: `mov [rax],r15`, rax = our returned pointer). Fix: committed
   pages get READWRITE (EXECUTE_READWRITE if execute requested); reserve stays NOACCESS.

### Current wall: Xbox GDK runtime (XGameRuntime) init → clean exit(1)
OutputDebugString trace shows the GDK runtime bootstrapping its service objects
(`SErrorImpl`, `SUsersImpl`, `SGameImpl`, `SUserDeviceAssociationsImpl`,
`SMultiplayerNetworkingImpl`, `SSystemImpl`, `SFeatureImpl`, `SAsyncImpl`,
`SPersistentLocalStorageImpl` — all "Created"), then **all "Destroyed"** and the
process exits 1. `XERROR: HR:80070006 ... TaskQueue` during teardown. Graphics
(`d3d12_xs`) is NEVER reached — this is platform/GDK init, before the RHI.
Exit-sink stack walk: `main`/`WinMain` already returned (CRT teardown), i.e. a
**clean decision to quit**, not a crash.

### Why: the title needs the Xbox GameOS environment
- `MicrosoftGame.config` TargetDeviceFamily="Scarlett"; `appxmanifest.xml` has
  `<uap7:OSPackageDependency Name="Microsoft.GameOs" .../>` and an Xbox proxy
  entry point. `XGameRuntimeInitialize` expects the console runtime/identity.
- Package registration ruled out: `Add-AppxPackage -Register` → 0x80073CFF
  (no sideloading license); and the Microsoft.GameOs dependency can't be met on
  desktop Windows anyway.

### Next phase (decided): XGameRuntime / GDK compat shim
Reimplement the Xbox GDK runtime surface (XGameRuntimeInitialize succeed, XTaskQueue,
XAsync, XUser, XGame, PersistentLocalStorage, etc.) — the platform-services layer
anticipated by CLAUDE.md §B. Same incremental method: shim → run → read the call
log → implement the next required function. This is a substantial new subsystem.

### Tooling added this session
- `mdbg` debug tracer (OutputDebugString + exception + exit-stack capture) — to be
  formalized into the repo as `tombox-trace`.
- `mdparse` minidump parser (exception code/address/faulting module).

## 2026-11-16 — Session 2 cont.: root cause pinned to the instruction (UE project load)

### Milestone: the game boots into UE engine init; exit cause fully traced
Built a self-contained x64 disassembler (`crates/tombox-disasm`, iced-x86 + tombox-pe
RVA mapping + symbol resolution) and a full symbol resolver from the shipped
`FortniteClient-XSX-Shipping-Symbols.bin` (1,245,949 named functions; verified: EXE
entry RVA 0x229a298 -> WinMainCRTStartup). Extended the debug tracer (`mdbg`) with
EXE breakpoints + register/stack/FString capture + a continuing CreateFileW probe.

### The exit path, traced end to end (all confirmed at runtime)
`WinMainCRTStartup` -> `WinMain` (0xb45c64): calls `XGameRuntimeInitialize` (succeeds),
`GuardedMainWrapper` -> `GuardedMain` (0xb46088) -> `EnginePreInit` (0x2bcd12c) ->
`FEngineLoop::PreInitPreStartupScreen` (0xe6d82c) **returns 1** (captured eax=1 at
EnginePreInit's test). Bisected PreInitPreStartupScreen by runtime breakpoints to:
`IProjectManager::Get` -> virtual `[+0x10]` = **`FProjectManager::LoadProjectFile`**
(RVA 0x15071cc) returns FALSE -> PreInit bails before RHI (no d3d12_xs ever called).

### Root cause (exact)
`LoadProjectFile` -> `FProjectDescriptor::Load` (0x15072e4) ->
`FFileHelper::LoadFileToString("../../../FortniteGame/FortniteGame.uproject")`
**returns FALSE** (captured al=0). The game cannot read its **project descriptor** and
aborts. Notes:
- No `.uproject` exists anywhere in the package (archive, EmbeddedXvd, EXE strings).
- Placing a valid `.uproject` at the resolved relative path (Mount/FortniteGame/) did
  NOT satisfy it; the read still fails.
- A `CreateFileW` trace caught **zero** file opens -> the GDK/Xbox platform file layer
  uses a different API (almost certainly **`CreateFile2`**) and/or a package-mount
  namespace, not plain Win32 loose-file reads.

### Interpretation
On real Xbox the project descriptor is served by the console package file system
(`FXboxPlatformFile`) from the cooked package; our loose-file PC layout + the absent
Xbox platform-file semantics mean the read fails. **Next subsystem: Xbox platform
file / content mounting** (how the title resolves and reads packaged files), to make
`FFileHelper::LoadFileToString` succeed for the descriptor (and later, pak/IoStore).

### Tooling note
`crates/tombox-disasm` is committed. The diagnostic tracer (`mdbg`), minidump parser
(`mdparse`), symbol resolver (`symres`) and export/IAT readers currently live in the
session scratchpad and should be formalized into the repo (`tombox-trace`, `tombox-sym`).

## 2026-11-16 — Session 2 cont.: REAL D3D12 DEVICE CREATED; full engine init runs

### The game now runs its entire Unreal Engine init on PC
With G:\ mapped to the package (subst) and the compat layer, FortniteClient-XSX
boots through its real init, with full UE logging:
- FEngineLoop PreInit (project descriptor now loads from G:\FortniteGame\)
- IoStore/DirectStorage (XCRDAPI DStorageGetFactory -> forwarded to real PC
  dstorage.dll with PC IID substitution; Xbox/PC IDStorageFactory compatible)
- APU audio heap (AcpHal ApuCreateHeap/ApuAlloc/ApuHeapGetState -> real host memory)
- Chaos physics, localization, LogOnline/MCP, GDKInput (5 controller devices),
  Slate (UI user)
- D3D12RHI: XGCreateTextureComputer (REAL xg_xs.dll used as-is — pure RDNA2
  tiling math, self-contained: imports only msvcrt + api-ms-win-core + our xmem)
- **D3D12XboxCreateDevice -> created a REAL PC D3D12 device** (host d3d12.dll;
  Xbox device IID == PC ID3D12Device {189819f1-...}, hr=S_OK, live device handle)

### Current wall: D3D12.x device interface divergence
`RootDevice->QueryInterface(__uuidof(IDXGIDevice))` fails: Xbox D3D12.x merges the
DXGI device into ID3D12Device; PC keeps them separate. First point where D3D12.x
needs a **wrapper/translation**, not a plain forward. Next phase: a COM proxy for
the device (handle QI for IDXGIDevice + Xbox-specific methods, forward the rest),
then command queues, swapchain/present, resources, PSOs, shaders.

### Key technique discovered
Some Xbox DLLs are pure computation and run on PC as-is (real xg_xs.dll). Others
forward to PC equivalents with IID substitution (DStorage, D3D12 device). Only the
hardware/semantics-divergent surface needs real translation.

### Milestones (CLAUDE.md §12)
- [x] 12 Actual Fortnite reaches meaningful initialization — ACHIEVED (full UE init).
- [~] 11 Graphics device initialized — device CREATED; post-device RHI setup pending
  the D3D12.x wrapper.

## 2026-11-16 — Session 2 cont.: real GPU driven; D3D12.x translation layer underway

### The real binary drives our RTX 5060 Ti through D3D12 setup
FortniteClient-XSX runs its full engine and now performs real GPU work via a
D3D12.x -> PC D3D12 translation layer (crates/compat/d3d12_xs):
- Device wrapper (COM): QI returns self for ID3D12Device..Device8 and synthesizes
  the Xbox "DXGIDevice" (GetAdapter -> real PC IDXGIAdapter -> game reads RTX 5060 Ti,
  7891MB VRAM). 160-slot vtable: 0-43 forward via naked thunks; 44+ are Xbox
  extensions -> fail-loud E_NOTIMPL stub unless intercepted.
- Verified translations: CheckFeatureSupport (Xbox 64B vs PC 60B struct-size
  negotiation), CreateDescriptorHeap (wraps heap), CreateCommandQueueX slot 48
  (Xbox desc -> PC CreateCommandQueue, created a real compute queue hr=S_OK).
- Heap wrapper: fixes the Xbox(RAX)-vs-PC(sret `fn(this,Handle* retval)`) return
  convention for GetCPU/GPUDescriptorHandleForHeapStart.

### Current frontier: Xbox D3D12.x method signatures
Next blocker is the Xbox command-list create (device vtable slot 73): standard PC
CreateCommandList signature does NOT match (args mismapped -> E_INVALIDARG), so the
Xbox vtable relocates/extends methods and some take Xbox-specific signatures.
Resolving each needs the GDK d3d12_x headers or per-method RE. The remaining
graphics path (command lists, resource creation, PSOs/shaders, swapchain/present)
is a methodical per-method translation grind — the technique is proven; the volume
is large.

### Milestones
- [x] 12 meaningful initialization — done (full UE init + GPU device + queues).
- [~] 11 graphics device + required features — device/caps/heaps/queues done;
  command lists onward pending D3D12.x ABI reversing.

## 2026-11-16 — Session 2 cont.: real GPU command infrastructure up

### The game drives the real RTX 5060 Ti through D3D12 command setup
Continued the D3D12.x translation by reversing each Xbox method's ABI from its
call site (no headers needed):
- CreateCommandQueueX (slot 48): Xbox queue desc -> PC CreateCommandQueue. Real
  compute queue created (hr=S_OK).
- Command-list create (slot 73): reversed the Xbox signature from the call site —
  takes a params struct {Type@0, NodeMask@8} by pointer, not inline args. Translated
  to PC CreateCommandList (nodeMask must be non-zero). Real DIRECT list created.
- Close-on-create: PC CreateCommandList returns an OPEN list; the Xbox RHI expects
  it CLOSED then calls Reset() — closing it on create fixed an nvwgf2umx (driver)
  crash in FD3D12CommandListData::Reset.
- Descriptor-heap handle ABI: switched from wrapping the heap (which leaked our
  wrapper into real SetDescriptorHeaps -> driver crash) to patching the shared
  ID3D12DescriptorHeap vtable slots 9/10 in place (VirtualProtect) — heaps stay
  real, handle getters bridged (Xbox RAX vs PC `fn(this, Handle* retval)`).
- Robust 160-slot device vtable: standard methods forward, Xbox extensions default
  to fail-loud E_NOTIMPL stubs (no crashes), specific ones intercepted.

### Current frontier: command-list recording method slots
The game now Reset()s and records on the real command list, then its own Close()
fails -> the command-list recording methods (barriers, descriptor binds, draws —
~50) are at Xbox vtable slots that differ from PC, corrupting the recording. Fix
is a command-list wrapper that remaps Xbox slots -> PC methods AND unwraps on
ExecuteCommandLists. That object-graph wrapping (command lists, resources, PSOs)
+ swapchain/present + shader handling is the remaining rendering-pipeline work —
proven approach, large volume.

### Milestones
- [x] 12 meaningful initialization.
- [~] 11 graphics device + features: device, caps, descriptor heaps, command
  queues AND command-list creation/reset all driving the real GPU; command
  recording -> present is the next phase.

## 2026-11-16 — Session 2 cont.: Xbox D3D12.x command-list vtable solved

### Root cause of the descriptor-table crash: a +4 vtable slot shift
Enabled the D3D12 debug layer (TOMBOX_D3D12_DEBUG) in the shim — its validation
messages flow through OutputDebugString into the tracer, turning opaque driver
crashes into precise errors. It reported #646 INVALID_DESCRIPTOR_HANDLE in
SetGraphicsRootDescriptorTable(root=2, handle=<stack ptr>). Patched the shared
command-list vtable to log binds + caller RVA: the caller was
FD3D12DescriptorCache::SetDescriptorHeaps calling command-list **slot 32** with
(NumHeaps=2, ppHeaps=&{srvHeap,samplerHeap}) — i.e. it was **SetDescriptorHeaps**
(PC slot 28), not SetGraphicsRootDescriptorTable (PC slot 32).

Reverse-engineered the Xbox command-list vtable from call sites in the Fortnite
RHI: slots 0..=26 are identical to PC (verified: Close=9, Reset=10,
DrawInstanced=12, DrawIndexedInstanced=13, Dispatch=14, SetPipelineState=25,
ResourceBarrier=26), then **4 Xbox-specific methods are inserted after PC slot
27**, shifting every method >=28 by +4 (SetDescriptorHeaps 28->32,
OMSetRenderTargets 46->50, all verified).

Fix (cmdlist_wrap.rs): build an "Xbox-layout" vtable whose slot i holds the real
PC method pointer for the mapped PC slot (identity 0..=26, stub 27..=31, i-4 for
>=32), then repoint each real command-list object at it. No thunks needed — the
object IS the real list, so the real method runs on the real `this`. Slots 0..=26
stay identical so the runtime's own QI/GetType/AddRef/Release still work at
ExecuteCommandLists time.

Result: **#646 gone, all D3D12 validation clean.** The game now records and
submits GPU work correctly. New blocker is unrelated: IoStore/.ucas content
containers fail to open (DirectStorage path), fatal at pak mount.

### Milestones
- [x] 11 graphics device + required features: device, caps, descriptor heaps,
  command queues, command lists, descriptor-table binding all correct on the
  real GPU with zero D3D12 validation errors.

## 2026-11-16 — DirectStorage container open + found incomplete extraction

### Xbox DirectStorage OpenFile IID substitution
With the command-list vtable fixed, the next fatal was IoStore content: the Xbox
IoStore backend (FXSXFileIoStoreImpl::OpenContainer) opens .ucas containers via
DirectStorage factory->OpenFile (slot 4, PC-compatible slot). It passes the Xbox
IDStorageFile IID, which PC OpenFile rejects (like the factory IID). xcrdapi now
patches the real factory's vtable slot 4 to substitute PC IID_IDStorageFile
{5de95e7b-...}. Result: OpenFile succeeds, containers open, pakChunkEarly-XSX
mounts, plugins mount, device-profile CVars apply.

### ROOT ISSUE found: the dump was only partially extracted
The remaining fatal (appError Line 644) is almost certainly missing content: the
original ZIP is 834 entries / 26.9 GB, but only 905 MB (302 files) had been
extracted — just the smallest paks (global, pakChunkEarly, pakchunk0). Missing:
pakchunk10-XSX.ucas (21 GB — the bulk of the game), pakchunk12/7/8/2/5/11, all
Movies, 468 files total. Verified the 302 present files are byte-complete (no
truncation). Extracting the missing 468 now (disk has 294 GB free).

## 2026-11-16 — Full content + physical memory + resource/texture creation

After extracting the full 26.9GB dump (was partially extracted), cleared a chain
of real blockers past engine init into GPU resource creation:

1. **XMemAllocatePhysicalPages / XMemMapPhysicalPages** (xmem.dll): were stubs
   returning null -> fatal "AllocateTitlePhysicalPages failed" (Line 644).
   Implemented commit-on-map emulation of Xbox 64KB title physical pages (grant
   PFN tokens on allocate; VirtualAlloc-commit the reserved VA on map). The
   transient memory allocator now initializes (1792 DRAM pages).
2. **sret device thunks** (device_wrap): GetResourceAllocationInfo (25) and
   GetCustomHeapProperties (26) return >8-byte structs by value, so `this` is in
   rdx (rcx = hidden return ptr). The generic rcx-forwarding thunk null-derefed.
   Added fwd_sret! thunks that redirect rdx.
3. **CreateCommittedResource (27)**: intercepted to substitute PC IID_ID3D12Resource
   (already matched, but robust) — real resources create (buffers hr=0).
4. **GetResourceAllocationInfo (25)**: the real D3D12Core method faults deep in its
   texture-tiling sub-function (vtbl+0x25829) for Xbox texture descs even after
   full desc sanitization (valid device, valid 1x1 BGRA desc). Compute the info
   ourselves from the D3D12 64KB standard-swizzle tile model, over-estimating so
   the pool never under-reserves. Texture allocation crash cleared.

### Current frontier: GPU device removal
With CPU-side resource/texture creation working, a GPU operation during resource
setup triggers DXGI device-removed ("GPU Crashed or D3D Device Removed", Line 656).
Likely the unified-memory gap: Xbox places GPU resources on app physical pages
(GPU-visible on console); our commit-on-map gives CPU VirtualAlloc memory the GPU
can't access. Investigating.

## 2026-11-16 — ★ FIRST WINDOW: real Fortnite XSX binary opens on PC ★

MILESTONE 13 reached. After the presentation-init stubs (S_OK), the real
FortniteClient-XSX-Shipping.exe **created and presented its own window** on the
desktop (titled "Fortnite", blank first frame) before continuing init — the
actual supplied Xbox Series console binary driving a live D3D12 swap chain on the
RTX 5060 Ti. Confirmed visually by the user.

### Async asset streaming (DirectStorage read path)
After the window, the IoStore I/O thread (FXSXFileIoStoreImpl::StartRequests)
crashed: it enqueues DirectStorage reads on a queue that was null, because we had
only substituted the PC IID on OpenFile. Added CreateQueue (factory slot 3) IID
substitution (PC IID_IDStorageQueue {cfdbd83f-...}). The queue now creates and the
game advances to FFortniteEarlyPreLoadScreen + FAssetRegistry startup + a WinHttp
service call.

### Current frontier: DSTORAGE_REQUEST translation
Real DStorageCore now fail-fasts in DStorageQueue.cpp:287 with E_INVALIDARG — the
Xbox DSTORAGE_REQUEST layout passed to IDStorageQueue::EnqueueRequest differs from
PC. Next: intercept EnqueueRequest on the queue vtable, translate the Xbox request
(source file/offset/size + destination) to the PC DSTORAGE_REQUEST, or service the
read directly. This is the asset-streaming subsystem that gates reaching the menu.

### Milestones
- [x] 13 actual game renders a first frame (window created + presented).
- [~] 14 main menu: blocked on DirectStorage request translation (asset streaming).

## 2026-11-16 — DirectStorage complete; content fully loads

Full Xbox DirectStorage -> PC translation now works end to end:
- Factory IID substitution for OpenFile (slot 4), CreateQueue (slot 3),
  CreateStatusArray (slot 5).
- DSTORAGE_REQUEST Xbox->PC field reorder (Dest before Source on Xbox).
- Reads serviced directly via Win32 (CreateFileW in OpenFile -> OS handle map;
  positioned ReadFile in EnqueueRequest) because DStorageCore's own read path
  faults on the Xbox-origin requests; the game's Submit still fires status/fence.

Result: all 27GB content streams in, pakchunk10 (21GB) mounts, all 12 bundles
install ("All Bundles Installs Complete!"), asset registry starts.

### Current frontier: IoStore package-store build
An async task lambda crashes (AV read null) in the FPackageId->FPackageStoreEntry
map build (TMapBase::FindOrAddImpl) — the entries buffer [rbp+0xA0] is null.
Post-content-load engine init; needs tracing where that buffer is set up (likely a
stubbed Xbox path returning null, or an IoStore package-store format detail).

## 2026-11-16 — Post-content crash: container header not decompressed

Traced the package-store build crash (async lambda in FPackageStore::LoadContainers,
deserializing FContainerHeader). Root cause identified precisely via a crash-frame
dump: the source buffer handed to the FMemoryReaderView contains ZLIB-COMPRESSED
data (magic 0x78 0x9c), and the game deserializes it raw -> first bytes read as a
package id/count -> null entries array -> AV read null. (The size field is fine at
0x10000; an earlier read of the 40-bit size looked wrong but the byte at +0x18 is 0.)

The buffer is memory-mapped (0x7ff4_xxxx_xxxx), NOT one of our 4 DirectStorage dest
buffers, so it comes from a read path (FIoDispatcherImpl::ReadWithCallback) where the
chunk's decompression isn't applied on PC. On Xbox this chunk is decompressed
(hardware decompression via the Velocity Architecture / DirectStorage) before
deserialization.

NEXT: trace the container-header read/decompress path; either the engine expects
DirectStorage to decompress (CompressionFormat in the request) and our self-serviced
reads must zlib-inflate, or the engine's own decompress step is being skipped. All
DirectStorage self-service reads verified complete/non-zero (not the cause of bad
data elsewhere).

## 2026-11-16 — ContainerHeader decompression/routing (precise localization)

Deepened the package-store crash analysis:
- The source buffer handed to the FContainerHeader deserializer is zlib-compressed
  (78 9c) and memory-mapped (0x7ff4_xxxx_xxxx).
- Our 4 DirectStorage reads are all uncompressed plaintext (verified: readable
  strings, CompressionFormat=0, src==dst size) -> NOT the cause.
- FCompression::UncompressMemory IS called with zlib (78 9c) data during load, so
  the zlib path works. ScatterBlock's synchronous UncompressMemory (0x5c0789) is
  NOT hit before the crash; decompression goes via another caller
  (FDecompressAsyncTask).
- Conclusion: the ContainerHeader's *decompressed output is not routed to the
  deserializer* — the lambda's captured buffer ([this+0x18]) still points at the
  compressed source. It's a read-callback buffer-plumbing difference, not a missing
  decompressor.

NEXT: add log-and-continue (single-step replant) support to mdbg (cfw_handle
already does the trap-flag dance) so every UncompressMemory call + result + caller
can be logged; then find where the ContainerHeader read hands the compressed source
to the callback instead of the decompressed buffer. Chunk type = 0x0A
(ContainerHeader), read via FIoDispatcherImpl::ReadWithCallback from
FPackageStore::LoadContainers.

## 2026-11-16 — ROOT CAUSE: compressed container header read without decompression

Found the exact data behind the package-store crash:
- The buffer deserialized as FContainerHeader is the raw contents of
  pakchunk9-XSX.ucas at file offset 0 (verified: the exact bytes the game
  deserialized, 789ced..., are at offset 0 of pakchunk9-XSX.ucas and NOWHERE
  else).
- pakchunk9-XSX.utoc: 433 compressed blocks, 1 method = "Zlib", block size 64KB.
  block[0] (offset 0) = {compSize 2707, uncompSize 65536, methodIdx 1=Zlib}.
  It zlib-inflates cleanly to a valid 64KB container header (starts with an
  FIoContainerId).
- So the container header IS correctly Zlib-compressed per the TOC, but the engine
  hands the deserializer the RAW compressed block — it is not decompressing it.
- global/pakchunk0/pakChunkEarly have UNCOMPRESSED headers (no 78 9c at offset 0),
  which is why they loaded and pakchunk9 is the first container to crash.
- FCompression::UncompressMemory is called during load but only from the .pak path
  (FPakUncompressTask); the IoStore ScatterBlock decompress (0x5c0789) is never hit
  before the crash. So the IoStore compressed-block decompression is not running for
  this container-header read.

This is the hardest blocker so far: it's not a shim-returns-wrong-value bug, it's
the engine's IoStore read/decompress routing differing on PC. Fix options:
  (a) make the engine's IoStore decompression run for this read (trace why
      ScatterBlock/mmap path skips it),
  (b) intercept the container-header file read and return inflated data (needs file
      I/O hooking, scoped to not double-decompress content),
  (c) rewrite affected .ucas/.utoc to store uncompressed headers (offset-shift the
      whole container — deterministic but heavy).

## 2026-11-17 — Shader format cracked; RDNA2 translator foundation built

Reached the game's own "Gaming UI" window (black). Root-caused the black screen: every
graphics PSO fails because the shaders ship as Xbox-native code, DXIL stripped. Reversed the
full chain with evidence: `DXBC → DXIL(NOBC) → XDAT v3 → zlib(78 9c) → FSSM(v0x101) → native
RDNA2 ISA` (confirmed by s_waitcnt/s_endpgm/s_code_end/s_cbranch markers). Not encrypted.

Decision (user): build the real translator. Architecture: runtime JIT at PSO-create (Xbox
device slot 60/55), per-hash disk cache, backend RDNA2 → HLSL → dxc → DXIL. Rationale: RDNA2
VGPR ops are per-lane = per-thread, mapping cleanly to HLSL; dxc handles DXIL emit+signing.

Built `crates/tombox-xshader` (2 passing regression tests):
- `extract()`: DXBC→XDAT→zlib→FSSM→ISA, bounds-checked, validated against RDNA2 markers.
- `rdna2::disassemble()`: length-accurate decoder + full operand decode (SOP*/SMEM/VOP*/VINTRP/
  EXP), self-validated by landing exactly on s_endpgm (VS 67 instrs/400B, PS 117/604B).

Next: VOP opcode-semantics table → RDNA2→HLSL emitter (straight-line first) → dxc compile →
wire into create_gfx_pso_x with caching + fallback. See docs/FORTNITE_ANALYSIS.md.
