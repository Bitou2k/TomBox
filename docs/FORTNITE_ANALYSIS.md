# Fortnite Xbox Series X|S — Target Analysis

Target: **Fortnite Xbox Series X|S, version 2.0.1461.7919**.

## Source archive

| Field | Value |
|---|---|
| Source URL | https://ia801000.us.archive.org/10/items/fortnite-xbox-series-xs/436609B6.FortniteClient_2.0.1461.7919_neutral__9ncxwbgmmv7m8_xs.zip |
| Container | ZIP (`application/zip`), server supports range requests |
| Size | 27,385,950,541 bytes (25.51 GB) |
| Download started (UTC) | recorded in `input/original/download_metadata.json` |
| SHA-256 (archive) | _pending — computed after download completes_ |
| Status | **download in progress** |

The filename component `_xs` and `9ncxwbgmmv7m8` (a Store package family-name style
token) are consistent with an Xbox Series MSIXVC-origin package. To verify after
extraction, not assume.

## Dump inventory
_Pending extraction._ Will record full tree, per-file size + SHA-256, candidate
executables/modules, asset containers, config/manifest files, and unknowns.

## Executables and modules
_Pending._ Expected: a main `FortniteClient*.exe` (PE32+/x64) plus dependent DLLs.

## Dependencies / imports
_Pending._ Will be generated with `pe-inspect --json` and classified into
Xbox/GDK vs. standard Windows vs. unknown (see `COMPATIBILITY.md`).

## Startup sequence
_Pending._ Will be mapped via static analysis (entry → CRT → GDK init → RHI/device).

## Engine
_Hypothesis:_ Unreal Engine (Fortnite). Version to be recovered from strings/metadata.

## Dependency-graph tiers (CLAUDE.md §5) — to populate
1. Required to load the EXE
2. Required to enter initialization
3. Required to create the first window/device
4. Required to render the first frame
5. Required to reach the main menu
6. Required for gameplay
7. Optional platform functionality

## Target-specific findings
_Pending._
