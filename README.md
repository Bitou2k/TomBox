# TomBox — Xbox Series X|S compatibility runtime (research)

An attempt to run user-provided, already-dumped/decrypted **Xbox Series X|S** game builds
natively on Windows x64. The first and only test target is
**Fortnite Xbox Series X|S 2.0.1461.7919** (v14.50).

> **Status (2026-11-17): boots to the lobby; not playable.**
> The real game executable runs natively, logs in against a private backend, and reaches
> the Battle Royale lobby. The 2D interface renders correctly. The 3D scene does **not**
> render correctly yet, there is no audio, and input is only partly usable. See
> [What works](#what-works) and [Known problems](#known-problems) — nothing beyond those
> lists is claimed.

## How it works

The Series X|S CPU is x86-64, so the game's code runs directly on the host CPU: there is
no CPU emulation and no recompilation of game code. The work is at the platform boundary.

- **Executable**: the game's PE is started as an ordinary Windows process. Its Xbox-only
  DLL imports are satisfied by small compatibility DLLs placed next to it.
- **Graphics (`d3d12_xs`)**: wraps a real PC Direct3D 12 device and presents the Xbox
  D3D12.x interface the game expects — different vtable layouts, Xbox-only device,
  command-list and queue methods (`CreatePlacedResourceX`, `PresentX`, frame-pacing
  events, derived pipeline states, …), each reconstructed from the game's own call sites.
- **Shaders (`tombox-xshader`)**: the build ships no DXIL. Its shaders are precompiled
  AMD RDNA2 machine code inside the DXBC containers. The translator disassembles that
  code and emits HLSL, which is compiled at run time with Microsoft's DXC compiler (shader
  model 6 DXIL; FXC as fallback) and cached on disk.
- **Textures**: on console a texture lives at a game-chosen address in unified memory, in
  a tiled layout. The runtime mirrors that memory into PC textures every frame, using the
  texture-layout library (`xg_xs.dll`) from the dump itself to compute the layout.
- **Platform services**: the game runs against Microsoft's PC `xgameruntime.dll`
  (Gaming Services). Where that cannot serve an unregistered title, local replacements
  are hooked in: a single offline Xbox user and an empty Microsoft Store that owns nothing.
- **Network policy (`compat-common::net_guard`)**: by default every DNS lookup, socket
  connect and WinHTTP connection made by the game fails locally. Optionally all traffic
  is redirected to one host of your choosing (your own backend). It never talks to Epic.

Details and evidence are in `docs/` (see [Documentation](#documentation)).

## What works

Verified on one machine (Windows 11, x64) with the Fortnite build above:

| Area | State |
|---|---|
| Process start, engine init, asset loading from the dump | works |
| Title screen, login screen, text, 2D UI | renders correctly |
| Login against a private backend (network redirect + local Xbox user) | works |
| Game-mode select, settings menus | render correctly |
| Battle Royale lobby | loads; UI correct, **3D scene wrong** |
| Shader translation (translate only, whole game archive) | 105,506 of 105,506 unique shaders |
| Translated shaders compile (DXC) | whole-archive sweep in progress; see `docs/DEVELOPMENT_LOG.md` for the current count. Compiling is not the same as rendering correctly |
| DXC output accepted by Direct3D 12 | checked for 3 compute fixtures (`xpso`); not yet run in the game |
| Mouse | works (see limitations) |
| Audio | none |
| Gameplay / matches | not attempted |

## Known problems

- **3D rendering is incorrect.** The lobby stage, backdrop and floor now render with plausible
  tones and the character is in place, but it is almost unlit (a dark silhouette), a white
  rectangle flickers in the upper part of the picture (infinities in the bloom chain) and the
  floor centre is too bright.
- **A shader that fails to compile is fatal.** Every shader now translates, but if the
  generated HLSL for one does not compile, pipeline creation fails and the engine exits.
- **First-time shader compiles are slow** (FXC took up to minutes; DXC about a second, not yet measured in the game) and
  can trip the engine's stall watchdog the first time a screen is opened. Compiled shaders
  are cached, so a given shader stalls at most once.
- **Approximations in translated shaders**: texture lookups inside shader loops always use
  the sharpest mip; whether a typed buffer holds floats or integers is inferred from how
  the shader uses it (checked against a PC build's declarations, not exact); typed
  textures are always assumed float.
- **Texture mirroring is polling-based**: changes are detected by hashing game memory each
  frame; mips the game has not streamed in yet are left undefined.
- **Window**: the image is stretched to the window, so resizing it to a non-16:9 shape
  distorts the picture. The game still thinks it is driving a 3840×2160 display.
- **No audio, no controller mapping work, no save data.**
- **Several pieces are specific to this exact Fortnite build**: the local Xbox user and
  store hooks locate game functions through that build's symbol table and refuse to
  install on any other executable.
- Other user-related platform calls (cloud saves, system dialogs) still go to Microsoft's
  runtime and fail.

The full list of unverified assumptions is in `docs/TECHNICAL_DEBT.md`.

## Requirements

- Windows 10/11 x64 with a Direct3D 12 GPU.
- Microsoft Gaming Services installed (provides `xgameruntime.dll`).
- Rust toolchain (`x86_64-pc-windows-msvc`); Windows SDK (`dxcompiler.dll` + `dxil.dll`, found automatically or via `TOMBOX_DXC_DIR`;
  `d3dcompiler_47.dll` as fallback).
- Your own legally obtained, already-decrypted dump of the game. **No game files are
  provided here**, and the scripts assume the layout below.

## Build

```
cargo build --release
cargo test --release
```

The compatibility DLLs must be built with a static CRT:

```
set RUSTFLAGS=-C target-feature=+crt-static
cargo build --release -p d3d12_xs -p xmem -p xcrdapi -p acphal -p dxcompiler_xs -p mfplat
```

## Running the game

There is no installer or launcher yet; this is the manual procedure the development
scripts automate (`tools/dev/iter.sh`, which contains machine-specific paths).

1. Extract the dump to `input/extracted/` (game files end up under `input/extracted/Mount/`).
2. Map that folder to drive `G:` — the game expects its filesystem root there:
   `subst G: <repo>\input\extracted\Mount`
3. Copy the built `acphal.dll`, `d3d12_xs.dll`, `dxcompiler_xs.dll`, `mfplat.dll`,
   `xcrdapi.dll` and `xmem.dll` from `target/release/` into
   `G:\FortniteGame\Binaries\XSX\`.
4. Copy the **real** `xg_xs.dll` from the dump (`EmbeddedXvd/Windows/System32/`) into the
   same folder. Do not use the stub crate of the same name; texture layouts depend on the
   real library.
5. Optional: create `G:\FortniteGame\Binaries\XSX\tombox.cfg`:

   ```
   net_redirect=<your backend host>   # omit to block all network access
   gamertag=<name>                    # local offline user (default TomBoxPlayer)
   xuid=<decimal id>
   xbl_token=<token string your backend expects>
   ```
6. Run `G:\FortniteGame\Binaries\XSX\FortniteClient-XSX-Shipping.exe`.

Logs: compatibility-layer calls go to `%TEMP%\tombox_compat.log` (or `%TOMBOX_LOG%`).
The game itself only logs through the debugger channel; `tools/dev/mdbg.rs` is a small
tracer that captures it.

Useful environment variables: `TOMBOX_NET_REDIRECT` (same as `net_redirect`),
`TOMBOX_D3D12_DEBUG=1` (Direct3D validation layer), `TOMBOX_RTCAP=1` (diagnostic
render-target capture, triggered by creating `%TEMP%\tombox_capture_now`),
`TOMBOX_OWN_WINDOW=1` (present into a separate window instead of the game's).

## Repository layout

```
crates/tombox-pe         PE32+ parser (bounds-checked, fuzz/truncation tested)
crates/tombox-cli        pe-inspect and other analysis CLIs
crates/tombox-archive    safe archive inspection/extraction
crates/tombox-disasm     x86-64 disassembler front end with symbol resolution
crates/tombox-loader     early loader experiments (not used to run the game)
crates/tombox-xshader    RDNA2 shader extraction, disassembly and HLSL translation
crates/compat/*          the compatibility DLLs (d3d12_xs, xmem, xcrdapi, acphal, …)
                         and compat-common (logging, network policy, local user/store)
tools/dev/               development scripts and the tracer (not part of the runtime)
analysis/                shader test fixtures, evidence screenshots, inspection output
docs/                    research and engineering record
input/                   your dump (git-ignored, never committed)
```

## Documentation

- `docs/ARCHITECTURE.md` — execution model and subsystem boundaries
- `docs/XBOX_RESEARCH.md` — platform findings and references
- `docs/FORTNITE_ANALYSIS.md` — the target build: inventory, modules, dependencies
- `docs/COMPATIBILITY.md` — API matrix and feature status
- `docs/TECHNICAL_DEBT.md` — unverified assumptions, known bugs, risks
- `docs/DEVELOPMENT_LOG.md` — chronological log with commands, results and evidence
- `docs/DECISIONS.md` — architecture decision records

`ARCHITECTURE.md`, `XBOX_RESEARCH.md` and `COMPATIBILITY.md` lag behind the code; the
development log and technical-debt file are current.

## Scope and legal

Interoperability research on binaries the user is authorised to analyse.

- No console-security circumvention, key extraction or package decryption.
- No connection to Epic or Fortnite production services: network access is blocked unless
  explicitly redirected to a server you operate.
- The local Xbox user is an offline placeholder, not an Xbox Live account, and the local
  store grants no entitlements. No anti-cheat code is touched.
- Game executables and assets are not distributed with this project.
