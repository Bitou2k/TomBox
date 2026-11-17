# Compatibility Matrix

The API compatibility matrix. Populated from the **actual imports** of the Fortnite
binary once extracted. Empty by design right now — we do not pre-list APIs we have
not observed the binary use.

## Evidence levels (CLAUDE.md §1)

| # | State | Meaning |
|---|---|---|
| 1 | Observed | seen in the supplied build |
| 2 | Documented | authoritative docs support it |
| 3 | Inferred | multiple independent observations |
| 4 | Hypothesized | plausible, unverified |
| 5 | Implemented | code exists |
| 6 | Compiles | builds |
| 7 | Unit tested | behavior tested in isolation |
| 8 | Integration tested | tested with other subsystems |
| 9 | Verified vs. target | checked against the real game |
| 10 | End-to-end | repeatable working test |

## Toolkit capabilities (not game APIs)

| Capability | Level | Evidence |
|---|---|---|
| PE32+ header/section parsing | 7 | `tombox-pe` tests; kernel32/ntdll parsed |
| Import/delay-import enumeration | 7 | self-binary 10 modules/91 funcs; unit tests |
| Export enumeration | 7 | kernel32 1671 names/206 forwarders |
| Base relocation summary | 6 | parsed on self-binary; needs dedicated assert test |
| TLS directory parse | 6 | parsed on self-binary; needs dedicated assert test |
| Malformed-input safety (no panic) | 7 | random + PE-shaped garbage + truncation sweep |

## Xbox/GDK API matrix

_(To be filled from `pe-inspect --json` of the real EXE. Columns: symbol · module ·
ordinal/NID · signature source · semantics evidence · required phase · PC equivalent ·
compat feasible? · impl state · tests · confidence · limitations.)_

**None recorded yet** — the binary is still downloading.

## Known limitations

- No API is implemented. No game runs.
- Base-reloc and TLS parsers are exercised but lack value-asserting unit tests
  (tracked in `TECHNICAL_DEBT.md`).
