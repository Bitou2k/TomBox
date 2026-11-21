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

## Network isolation (2026-11-17)

* **Incident:** once the title's online subsystems started (first reached on 2026-11-17 when
  shader translation got the game past pipeline creation), the title issued real WinHTTP
  requests that succeeded — at least analytics to `datarouter.ol.epicgames.com`. That
  violated the project rule against contacting Epic production services. Runs before the
  guard below (a handful, a few minutes each) had network access.
* **Mitigation:** `compat-common::net_guard` now patches, inside the title process, the
  Winsock name-resolution/connect/sendto entry points and WinHTTP connect/open/send so they
  fail locally (installed on the first compat-layer call, i.e. before the online subsystem
  starts; verified: "Unable to create WinHttp Connection, failing request").
* **Known gaps:** `ConnectEx` to a literal IP is not intercepted; isolation is in-process
  only (no OS firewall rule — the session is not elevated). A proper sandbox (AppContainer
  without network capability, or a firewall rule) would be stronger.

## Audio

* `acphal`: `IAcpHal` is an inert object (no SHAPE DSP): XMA voices never decode. No audio.

## Shader translator (tombox-xshader) — unverified assumptions

* Typed-buffer element type (float vs integer) is inferred from use (`infer_tbuf_types`):
  linear scan, majority vote, float when there is no evidence. Agrees with the PC build's
  declarations on the comparable pairs (`xpair --decls`), but it is a heuristic. Signed vs
  unsigned is not recovered reliably (declared `uint4` unless a signed operation is seen);
  a `uint` declaration over an `SINT` view is formally a type mismatch in D3D12.
* Typed *textures* are always declared float; the PC reference shows a few `Texture2D<uint>`
  / `Texture3D<uint>` (7 of 4,553 pairs) that are therefore wrong.
* Register slots differ between the Xbox and PC compilers in a sizeable share of shaders, and
  the pairing key (stage + layout hashes + signature names) often pairs different
  permutations; reference comparison by execution needs a stricter key.
* **Structured-buffer element size is a heuristic** (`sbuf_words`): 1 dword touched → 4
  bytes, 2..7 → 16, else rounded up to 16. Derived from one title's PC build. A buffer with
  stride 8/12/20/…, or a 16-byte buffer of which a shader reads only the first dword, will be
  addressed wrongly and silently. Proper fix: the runtime knows the real stride when the
  view is created; either validate it against the bound shader and report mismatches, or
  feed strides to the shader. Not started.
* `precise` is applied to position outputs only; other depth-equality dependencies (e.g.
  pixel-shader depth output) are not covered.
* Scalar buffer loads (`s_buffer_load`) are read as structured-buffer accesses only when the
  descriptor is unambiguously from the SRV/UAV table; with several candidate descriptors they
  are still treated as constant-buffer reads, which is wrong if one candidate is a buffer.
  The structured read declares a 4-dword element and indexes past it when the real stride is
  larger (works with DXC's raw-buffer lowering; not guaranteed by HLSL).
* MUBUF store opcodes 0x1e/0x1f were swapped by analogy with the load pair; unverified.
* Indexed constant-buffer arrays assume a 16-byte element stride.
* Compute thread-group id registers are assumed to follow the user-data SGPRs.
* PS entry ABI assumes perspective-centre barycentrics in v0/v1 and position in v2..v5.
* `v_readfirstlane` / `v_mbcnt` / `s_bcnt1(exec)` are translated under a one-lane-per-
  invocation model (exact only when the compiler used them for wave-level bookkeeping).
* Translated output has not yet been validated against rendered pixels.

## Placed textures / texel mirroring (2026-11-17)

- **State assumption.** Uploads transition the PC texture from its *creation* state to
  COPY_DEST and back; the title's own barriers are not tracked. Wrong if the title has moved
  the texture to another state when the upload list runs.
- **No write notification.** Changes are found by hashing title memory each frame under a
  byte budget (96 MiB new, 8 MiB re-check). A changed texture can lag by several frames, and
  the scan costs CPU every frame. Write-watch (`MEM_WRITE_WATCH`) would be the proper fix.
- **Racy reads.** Title threads write while we read (repaired on a later frame); a decommit
  racing the read would fault.
- **Streamed mips.** Subresources whose memory is not committed are skipped; the PC texture's
  corresponding mips hold undefined data until the title maps them. Whether the title limits
  sampling to resident mips has not been checked.
- **Not mirrored:** Texture3D, multi-plane formats, render-target/depth placed textures.
- **Layout source.** Element offsets come from the dump's real `xg_xs.dll` (vtable slots
  verified by probe for layout 261 only; other tile modes untested).
- **Extended sampler (slot 61).** Fields +0x3C/+0x54 of the Xbox desc are ignored.
- **XALLOC_ATTRIBUTES** bit layout is recalled from the public XDK header and matches the two
  attribute values seen; not checked against a header in this repo. Page size / memory type
  fields are ignored.

## Network redirect mode (2026-11-17)

- Policy (owner's instruction): traffic may leave the machine only towards
  `horizon.halcy0n.net`; never Epic. Enforced in-process only — no OS firewall rule.
- Not covered: `ConnectEx` to a literal IP, any resolver path that bypasses the hooked
  ws2_32 / WinHTTP exports, other processes.
- The Host header and TLS server name become the redirect host; the originally requested
  host name is not forwarded to the server (the server cannot tell which Epic service a
  request was meant for except by path).
- Non-WinHTTP TLS clients in the title (if any) will see a certificate for the redirect
  host while expecting another name; untested.
- Literal-IP destinations other than loopback / the redirect host's addresses are refused,
  including LAN addresses.

## Shader translator (2026-11-17, offline pass)

- "Translates" and "compiles" are established for the archive (sweep pending completion);
  **semantic correctness is not**. No translated shader has been compared against reference
  output.
- Structured-buffer accesses with a run-time byte offset assume the declared element size
  equals the view's stride.
- Optimistic descriptor join: a register holding a descriptor on one incoming path and
  unknown on the other is treated as the descriptor.
- Wave reductions map to `WaveActiveMin/Max`; host wave composition differs from the
  console's 64-lane waves (as HLSL permits). FXC fallback cannot compile these.
- Image atomics other than add/umax, and reductions other than f32 min/max, are refused.
- DXC-compiled graphics pipelines are untested in the game.

## Shader outputs and texel types (2026-11-19)

- **Colour-export tail is assumed to be replaced per target format on console.** The
  translator exports the operands of the two pack instructions in front of a compressed
  colour export instead of the packed halves. Evidence: the same tail appears on `uint4`
  outputs, where the literal behaviour cannot work. The replacement itself was not observed.
  Consequence if wrong: float targets get more precision than the console; no other effect.
- **Texel type of textures is inferred from use** (integer vs float), like typed buffers.
  No evidence = float. Agrees with the PC declarations on every paired shader checked, but
  the pairing covers a fraction of the archive.
- **Binding trace cost.** Diagnostic only; it holds a reference on every buffer and a map
  entry per descriptor. Not suitable for normal runs as written. The same tracking is what
  a run-time check of structured-buffer strides would need.

## Second title asset tools (tools/rage)
- Normal encoding chosen per model by fit to face normals because the shader's top-bit branch
  disagrees with the observed data (unresolved).
- Buffer-record layout in the resource block and same-count tie-breaking by record order are
  inferred. Model -> texture-pack link found (u64 key in the texture records), but shader
  parameters -> geometry (which texture on which part) is not decoded.
- Texture packs: 14% of textures use a BC-aware chunk codec (values 1-7 track BC1-BC7) with
  no available decoder; layout choice (d1 bits 24-26: 0 -> 0x2, 1-3 -> 0x105) is from visual
  checks; cube maps not untiled; ~1.8% of records' page spans are smaller than their declared
  mip chain; R8G8B8A8 records unconfirmed; fields marked unknown in `ttpf.py`.
- `levels.rpf`: only a 4 GiB slice carved. Placement records found by scanning, not via the map
  resource header; LOD hierarchy, flags and the per-file constant undecoded.
- Python, single-threaded; position/UV/normal only (colour, tangent streams ignored).
- DXC: some generated compute shaders take > 2 minutes or time out (9 of 400 at 300 s).
