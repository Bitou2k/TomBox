//! Xbox Series shader container extraction.
//!
//! Fortnite XSX ships each shader as a DXBC container whose portable DXIL bitcode has
//! been stripped (`NOBC`) and replaced, inside the `DXIL` chunk, with an Xbox-native
//! `XDAT` payload. The real shader is zlib-compressed inside `XDAT`; inflating it yields
//! an `FSSM` container whose body is **native RDNA2 microcode** (gfx10xx, Series X|S).
//!
//! Chain:  `DXBC → DXIL chunk → XDAT → zlib → FSSM → RDNA2 ISA`
//!
//! This module parses that chain and returns the raw ISA plus the surrounding metadata.
//! All parsing is bounds-checked and never panics on malformed input (returns `Err`).
//! Reverse-engineered from dumped Fortnite shaders and the GDK UMD `xgs12_10.dll`; see
//! `docs/FORTNITE_ANALYSIS.md`. Field offsets inferred empirically are validated against
//! RDNA2 markers before use, so a wrong guess fails loudly rather than yielding garbage.

pub mod rdna2;

/// Little-endian readers with explicit bounds checks.
fn u32_at(b: &[u8], off: usize) -> Result<u32, String> {
    b.get(off..off + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| format!("u32 read out of bounds at {off:#x} (len {})", b.len()))
}
fn fourcc_at(b: &[u8], off: usize) -> Result<[u8; 4], String> {
    b.get(off..off + 4)
        .map(|s| [s[0], s[1], s[2], s[3]])
        .ok_or_else(|| format!("fourcc read out of bounds at {off:#x}"))
}

/// A DXBC container chunk.
#[derive(Debug, Clone)]
pub struct Chunk {
    pub fourcc: [u8; 4],
    pub offset: usize,
    pub size: usize,
}

/// Parsed DXBC container (the outer wrapper used by both PC and Xbox shaders).
#[derive(Debug, Clone)]
pub struct Dxbc {
    pub digest: [u8; 16],
    pub total_size: u32,
    pub chunks: Vec<Chunk>,
}

impl Dxbc {
    pub fn parse(b: &[u8]) -> Result<Dxbc, String> {
        if fourcc_at(b, 0)? != *b"DXBC" {
            return Err("not a DXBC container (bad magic)".into());
        }
        let mut digest = [0u8; 16];
        digest.copy_from_slice(b.get(4..20).ok_or("truncated digest")?);
        let _version = u32_at(b, 20)?;
        let total_size = u32_at(b, 24)?;
        let nchunks = u32_at(b, 28)? as usize;
        if nchunks > 64 {
            return Err(format!("implausible chunk count {nchunks}"));
        }
        let mut chunks = Vec::with_capacity(nchunks);
        for i in 0..nchunks {
            let off = u32_at(b, 32 + i * 4)? as usize;
            let size = u32_at(b, off + 4)? as usize;
            if off + 8 + size > b.len() {
                return Err(format!("chunk {i} at {off:#x} size {size} exceeds blob"));
            }
            chunks.push(Chunk { fourcc: fourcc_at(b, off)?, offset: off + 8, size });
        }
        Ok(Dxbc { digest, total_size, chunks })
    }

    pub fn chunk(&self, fourcc: &[u8; 4]) -> Option<&Chunk> {
        self.chunks.iter().find(|c| c.fourcc == *fourcc)
    }
}

/// Program type from the DXIL program header (D3D12_SHADER_VERSION_TYPE).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Pixel,
    Vertex,
    Geometry,
    Hull,
    Domain,
    Compute,
    Mesh,
    Amplification,
    Unknown(u16),
}
impl Stage {
    fn from_program_version(pv: u32) -> Stage {
        match (pv >> 16) & 0xffff {
            0 => Stage::Pixel,
            1 => Stage::Vertex,
            2 => Stage::Geometry,
            3 => Stage::Hull,
            4 => Stage::Domain,
            5 => Stage::Compute,
            13 => Stage::Mesh,
            14 => Stage::Amplification,
            x => Stage::Unknown(x as u16),
        }
    }
}

/// The fully-extracted shader: native RDNA2 ISA plus the metadata we understand.
#[derive(Debug, Clone)]
pub struct ShaderExtract {
    pub stage: Stage,
    /// Shader model major.minor from the DXIL program header.
    pub shader_model: (u32, u32),
    /// Whether the DXIL bitcode slot was the `NOBC` placeholder (always true for XSX).
    pub bitcode_stripped: bool,
    /// Inflated FSSM container bytes.
    pub fssm: Vec<u8>,
    /// Entry-point name recovered from FSSM, if found.
    pub entry: Option<String>,
    /// Native RDNA2 ISA bytes (the shader program, s_endpgm-terminated).
    pub isa: Vec<u8>,
    /// Offset of the ISA within the FSSM blob.
    pub isa_offset_in_fssm: usize,
    /// Presence of the standard signature/validation/root-sig chunks in the outer DXBC.
    pub has_input_sig: bool,
    pub has_output_sig: bool,
    pub has_psv0: bool,
    pub has_root_sig: bool,
}

/// RDNA2 SOPP opcodes we key on to locate/validate the ISA.
const S_ENDPGM: u32 = 0xBF81_0000; // SOPP op=1
const S_CODE_END: u32 = 0xBF9F_0000; // SOPP op=0x1f, used as code padding
fn is_sopp(w: u32) -> bool {
    (w >> 23) == 0x17f
}

/// Extract the native RDNA2 ISA (and metadata) from a full shader DXBC blob.
pub fn extract(dxbc: &[u8]) -> Result<ShaderExtract, String> {
    let container = Dxbc::parse(dxbc)?;
    let dxil = container
        .chunk(b"DXIL")
        .ok_or("no DXIL chunk (not an Xbox shader?)")?;
    let body = &dxbc[dxil.offset..dxil.offset + dxil.size];

    // DxilProgramHeader: ProgramVersion u32, SizeInUint32 u32, then DxilBitcodeHeader
    // { DxilMagic 'DXIL', DxilVersion, BitcodeOffset, BitcodeSize }. XDAT follows (off 24).
    let program_version = u32_at(body, 0)?;
    let stage = Stage::from_program_version(program_version);
    let sm = ((program_version >> 4) & 0xf, program_version & 0xf);
    let bc_off = u32_at(body, 16)? as usize; // from start of DxilBitcodeHeader (body+8)
    let bc = body.get(8 + bc_off..8 + bc_off + 4).unwrap_or(&[]);
    let bitcode_stripped = bc == b"NOBC";

    let xdat = body.get(24..).ok_or("DXIL chunk too small for XDAT")?;
    if fourcc_at(xdat, 0)? != *b"XDAT" {
        return Err(format!(
            "expected XDAT at DXIL+24, found {:?}",
            String::from_utf8_lossy(&fourcc_at(xdat, 0)?)
        ));
    }

    // The shader is a zlib stream inside XDAT. The XDAT section table layout is only
    // partially understood, so locate the stream robustly by scanning for the zlib
    // header (0x78 0x9c / 0x78 0xda / 0x78 0x01); inflate self-terminates. Pick the
    // first blob that inflates to an FSSM container.
    let fssm = find_and_inflate_fssm(xdat)?;

    let (isa, isa_off) = locate_isa(&fssm)?;
    let entry = find_entry_name(&fssm);

    Ok(ShaderExtract {
        stage,
        shader_model: sm,
        bitcode_stripped,
        entry,
        isa,
        isa_offset_in_fssm: isa_off,
        fssm,
        has_input_sig: container.chunk(b"ISG1").is_some() || container.chunk(b"ISGN").is_some(),
        has_output_sig: container.chunk(b"OSG1").is_some() || container.chunk(b"OSGN").is_some(),
        has_psv0: container.chunk(b"PSV0").is_some(),
        has_root_sig: container.chunk(b"RTS0").is_some(),
    })
}

/// Scan a blob for zlib streams and return the first that inflates to an `FSSM` container.
fn find_and_inflate_fssm(xdat: &[u8]) -> Result<Vec<u8>, String> {
    let mut last_err = String::from("no zlib stream found in XDAT");
    let mut i = 8; // past "XDAT" + version
    while i + 2 < xdat.len() {
        if xdat[i] == 0x78 && matches!(xdat[i + 1], 0x01 | 0x9c | 0xda) {
            match miniz_oxide::inflate::decompress_to_vec_zlib(&xdat[i..]) {
                Ok(out) if out.len() >= 8 && &out[0..4] == b"FSSM" => return Ok(out),
                Ok(out) => last_err = format!("inflated {} bytes but magic={:?}", out.len(), &out[0..4.min(out.len())]),
                Err(e) => last_err = format!("inflate at {i:#x} failed: {e:?}"),
            }
        }
        i += 1;
    }
    Err(last_err)
}

/// Locate the RDNA2 ISA section inside an FSSM blob.
///
/// The FSSM header holds a section descriptor whose 64-bit offset/size (observed at
/// +0xc0 / +0xc8) brackets the ISA. We read it, then VALIDATE that the range both
/// contains `s_endpgm` and is dword-aligned RDNA2; if that fails we fall back to a
/// marker-based scan (first SOPP-dense run ending at `s_endpgm`). Either way the
/// returned bytes are checked to contain `s_endpgm`, so we never return non-ISA.
fn locate_isa(fssm: &[u8]) -> Result<(Vec<u8>, usize), String> {
    if fssm.get(0..4) != Some(b"FSSM") {
        return Err("not an FSSM container".into());
    }
    // Candidate from the header descriptor (empirical offsets).
    if let (Ok(off64), Ok(size_lo)) = (u32_at(fssm, 0xc0), u32_at(fssm, 0xc8)) {
        let off = off64 as usize;
        let size = size_lo as usize;
        if off >= 0x40 && size >= 8 && off + size <= fssm.len() {
            let sect = &fssm[off..off + size];
            if range_contains_endpgm(sect) {
                // The ISA section begins with a 16-byte preamble (dword0 = code offset,
                // dwords 1..3 = 0) before the executable RDNA2 code. Skip it when present.
                let (code, code_off) = strip_isa_preamble(sect, off);
                return Ok((trim_to_endpgm(code), code_off));
            }
        }
    }
    // Fallback: find the dword-aligned s_endpgm, then walk back over SOPP/other code.
    let mut end = None;
    let mut p = 0;
    while p + 4 <= fssm.len() {
        if u32::from_le_bytes([fssm[p], fssm[p + 1], fssm[p + 2], fssm[p + 3]]) == S_ENDPGM {
            end = Some(p + 4);
            break;
        }
        p += 4;
    }
    let end = end.ok_or("no s_endpgm found in FSSM — not RDNA2?")?;
    // Walk backward while the words look like plausible code (nonzero, or SOPP).
    let mut start = end - 4;
    while start >= 4 {
        let w = u32::from_le_bytes([fssm[start - 4], fssm[start - 3], fssm[start - 2], fssm[start - 1]]);
        if w == 0 && !is_sopp(w) {
            break;
        }
        start -= 4;
    }
    Ok((fssm[start..end].to_vec(), start))
}

/// Strip the 16-byte ISA-section preamble (dword0 = code offset, dwords 1..3 zero) that
/// precedes the executable RDNA2 code. Returns the code slice and its absolute FSSM offset.
fn strip_isa_preamble(sect: &[u8], sect_off: usize) -> (&[u8], usize) {
    if sect.len() >= 16 {
        let d1 = u32::from_le_bytes([sect[4], sect[5], sect[6], sect[7]]);
        let d2 = u32::from_le_bytes([sect[8], sect[9], sect[10], sect[11]]);
        let d3 = u32::from_le_bytes([sect[12], sect[13], sect[14], sect[15]]);
        if d1 == 0 && d2 == 0 && d3 == 0 {
            return (&sect[16..], sect_off + 16);
        }
    }
    (sect, sect_off)
}

fn range_contains_endpgm(sect: &[u8]) -> bool {
    let mut p = 0;
    while p + 4 <= sect.len() {
        let w = u32::from_le_bytes([sect[p], sect[p + 1], sect[p + 2], sect[p + 3]]);
        if w == S_ENDPGM {
            return true;
        }
        p += 4;
    }
    false
}

/// Trim an ISA section to end just after the first `s_endpgm`, dropping `s_code_end` pad.
fn trim_to_endpgm(sect: &[u8]) -> Vec<u8> {
    let mut p = 0;
    while p + 4 <= sect.len() {
        let w = u32::from_le_bytes([sect[p], sect[p + 1], sect[p + 2], sect[p + 3]]);
        if w == S_ENDPGM {
            return sect[..p + 4].to_vec();
        }
        if w == S_CODE_END {
            break;
        }
        p += 4;
    }
    sect.to_vec()
}

/// Find the entry-point name (observed as an ASCII run near the FSSM tail, e.g. "Main").
fn find_entry_name(fssm: &[u8]) -> Option<String> {
    // Scan for a short printable ASCII run that looks like an identifier.
    let mut best: Option<String> = None;
    let mut cur = Vec::new();
    for &b in fssm {
        if b.is_ascii_alphanumeric() || b == b'_' {
            cur.push(b);
        } else {
            if cur.len() >= 3 && cur.len() <= 64 && cur[0].is_ascii_alphabetic() {
                let s = String::from_utf8_lossy(&cur).into_owned();
                // Prefer a name that isn't obviously a chunk fourcc.
                if !matches!(s.as_str(), "FSSM" | "XDAT" | "DXBC" | "DXIL" | "NOBC") {
                    best = Some(s);
                }
            }
            cur.clear();
        }
    }
    best
}
