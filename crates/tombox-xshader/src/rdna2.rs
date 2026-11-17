//! Minimal RDNA2 (GFX10.3, Xbox Series X|S) instruction decoder.
//!
//! This is the second stage of the shader translator: turn the extracted native ISA
//! (see [`crate::extract`]) into a structured instruction stream. The immediate goal is a
//! *correct length and format decoder* — every instruction classified into its encoding
//! family with the right dword count (crucially: inline 32-bit literals and 64-bit VOP3/
//! SMEM/etc. consume two dwords). Correct lengths are the foundation for both a textual
//! disassembly and the eventual recompiler, and they are self-validating: a correct decode
//! of a whole program lands exactly on its terminating `s_endpgm`.
//!
//! Encodings follow the public "RDNA2 Instruction Set Architecture" reference. Operand
//! field decoding (register names, mnemonics) is filled in incrementally; unknown opcodes
//! are preserved as raw words rather than guessed, per the project's no-fabrication rule.

/// RDNA2 instruction encoding family, from the high bits of the first dword.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enc {
    Sop2,
    Sopk,
    Sop1,
    Sopc,
    Sopp,
    Smem,
    Vop2,
    Vop1,
    Vopc,
    Vop3,  // VOP3A/B (also VOP3P share the 0xD prefix range)
    Vinterp,
    Ds,
    Mubuf,
    Mtbuf,
    Mimg,
    Flat,
    Exp,
    Unknown,
}

/// A decoded operand. RDNA2 vector (VGPR) operands are per-lane, which maps directly onto
/// a per-thread HLSL variable; scalar (SGPR) operands are wave-uniform. Special registers
/// and inline constants are modeled explicitly so the translator never has to re-parse bits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Opnd {
    Sgpr(u8),
    Vgpr(u8),
    VccLo,
    VccHi,
    ExecLo,
    ExecHi,
    M0,
    Scc,
    Null,
    /// Inline integer constant (the -16..64 and 0 encodings).
    ConstInt(i64),
    /// Inline float constant (0.5, 1.0, 2.0, 4.0 and negatives, 1/2pi).
    ConstF32(f32),
    /// Trailing 32-bit literal.
    Literal(u32),
    /// An operand code we don't model yet (preserved, not guessed).
    Raw(u16),
}

/// One decoded instruction: length-accurate, with structured destination + source operands
/// where the encoding is understood. Unknown opcodes keep `ops` empty rather than guessing.
#[derive(Debug, Clone)]
pub struct Insn {
    /// Byte offset within the ISA blob.
    pub offset: usize,
    /// Number of 32-bit dwords this instruction occupies (1 or 2, VOP3 can be 2).
    pub dwords: usize,
    pub enc: Enc,
    /// Opcode field (meaning depends on `enc`).
    pub op: u32,
    /// The raw dword(s) for reference / round-tripping.
    pub raw: Vec<u32>,
    /// Mnemonic when known, else a format tag like "vop3<0x1c7>".
    pub mnemonic: String,
    /// Destination operand(s). Most ALU ops have exactly one.
    pub dst: Vec<Opnd>,
    /// Source operands in encoding order.
    pub src: Vec<Opnd>,
    /// For memory/export/interp: extra fields the translator needs (offset, dmask, target…).
    pub aux: InsnAux,
}

/// Encoding-specific extra fields kept out of the generic operand list.
#[derive(Debug, Clone, Default)]
pub struct InsnAux {
    /// EXP target (0-63), DS/MUBUF offset, SMEM offset, etc. — meaning per encoding.
    pub imm: i64,
    /// EXP enable mask / MIMG dmask / interp attribute channel.
    pub mask: u8,
    /// Secondary immediate (e.g. VINTRP attr#, SMEM soffset code).
    pub imm2: i64,
}

/// Classify the encoding from the first dword. Order matters (most-specific first).
fn classify(w: u32) -> Enc {
    // Scalar ALU/flow (top bit 1 => 0x8..0xB prefixes), then vector/memory.
    if w >> 23 == 0b1_0111_1111 {
        return Enc::Sopp; // 0xBF8..0xBFF
    }
    if w >> 23 == 0b1_0111_1101 {
        return Enc::Sop1; // 0xBE8..
    }
    if w >> 23 == 0b1_0111_1110 {
        return Enc::Sopc; // 0xBF0..
    }
    if w >> 28 == 0b1011 {
        return Enc::Sopk; // 0xB0..0xBF (but SOP1/SOPC/SOPP already peeled off)
    }
    if w >> 30 == 0b10 {
        return Enc::Sop2; // 0x80..0xBF range minus the above
    }
    // Vector ALU
    if w >> 25 == 0b0111111 {
        return Enc::Vop1; // 0x7E..0x7F
    }
    if w >> 25 == 0b0111110 {
        return Enc::Vopc; // 0x7C..0x7D
    }
    if w >> 31 == 0 && (w >> 25) & 0b111111 < 0b111110 {
        // top bit 0 and not VOP1/VOPC => VOP2 (0x00..0x7B)
        return Enc::Vop2;
    }
    // VOP3 / VOP3P / VINTRP / memory share 0xC..0xF prefixes
    match w >> 26 {
        0b110101 => Enc::Vop3,    // 0xD4/0xD5 VOP3
        0b110011 => Enc::Vop3,    // 0xCC/0xCD VOP3P (treat as VOP3-length)
        0b110010 => Enc::Vinterp, // 0xC8 VINTRP
        0b110110 => Enc::Ds,      // 0xD8/0xD9
        0b111000 => Enc::Mubuf,   // 0xE0
        0b111010 => Enc::Mtbuf,   // 0xE8
        0b111100 => Enc::Mimg,    // 0xF0
        0b111101 => Enc::Smem,    // 0xF4 SMEM
        0b110111 => Enc::Flat,    // 0xDC/0xDD FLAT/GLOBAL/SCRATCH
        0b111110 => Enc::Exp,     // 0xF8 EXPORT
        _ => Enc::Unknown,
    }
}

/// Does a scalar source operand field equal the "literal constant" code (255)?
fn ssrc_is_lit(field: u32) -> bool {
    field == 255
}

/// Decode a full ISA blob into instructions. Stops after `s_endpgm` or at end of input.
pub fn disassemble(isa: &[u8]) -> Vec<Insn> {
    let words: Vec<u32> = isa
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    let mut out = Vec::new();
    let mut i = 0usize;
    while i < words.len() {
        let w = words[i];
        let enc = classify(w);
        // Determine instruction length (in dwords) and whether a trailing literal follows.
        let (base_len, lit) = instr_len(enc, w);
        let total = (base_len + if lit { 1 } else { 0 }).min(words.len() - i).max(1);
        let raw: Vec<u32> = words[i..i + total].to_vec();
        let op = opcode_field(enc, w);
        let mnemonic = mnemonic_for(enc, op);
        let (dst, src, aux) = decode_operands(enc, &raw);
        let insn = Insn { offset: i * 4, dwords: total, enc, op, raw, mnemonic, dst, src, aux };
        let is_endpgm = enc == Enc::Sopp && op == 0x01;
        out.push(insn);
        i += total;
        if is_endpgm {
            break;
        }
    }
    out
}

/// Human-readable operand string (for disassembly / debugging).
pub fn opnd_str(o: &Opnd) -> String {
    match o {
        Opnd::Sgpr(n) => format!("s{n}"),
        Opnd::Vgpr(n) => format!("v{n}"),
        Opnd::VccLo => "vcc_lo".into(),
        Opnd::VccHi => "vcc_hi".into(),
        Opnd::ExecLo => "exec_lo".into(),
        Opnd::ExecHi => "exec_hi".into(),
        Opnd::M0 => "m0".into(),
        Opnd::Scc => "scc".into(),
        Opnd::Null => "null".into(),
        Opnd::ConstInt(v) => format!("{v}"),
        Opnd::ConstF32(v) => format!("{v:?}"),
        Opnd::Literal(v) => format!("0x{v:x}"),
        Opnd::Raw(c) => format!("<op{c}>"),
    }
}

/// Decode an 8-bit scalar source/dest operand code (SSRC/SDST), per the RDNA2 ISA table.
fn ssrc8(code: u16, lit: Option<u32>) -> Opnd {
    match code {
        0..=105 => Opnd::Sgpr(code as u8),
        106 => Opnd::VccLo,
        107 => Opnd::VccHi,
        124 => Opnd::M0,
        125 => Opnd::Null,
        126 => Opnd::ExecLo,
        127 => Opnd::ExecHi,
        128 => Opnd::ConstInt(0),
        129..=192 => Opnd::ConstInt(code as i64 - 128), // 1..64
        193..=208 => Opnd::ConstInt(-(code as i64 - 192)), // -1..-16
        240 => Opnd::ConstF32(0.5),
        241 => Opnd::ConstF32(-0.5),
        242 => Opnd::ConstF32(1.0),
        243 => Opnd::ConstF32(-1.0),
        244 => Opnd::ConstF32(2.0),
        245 => Opnd::ConstF32(-2.0),
        246 => Opnd::ConstF32(4.0),
        247 => Opnd::ConstF32(-4.0),
        248 => Opnd::ConstF32(std::f32::consts::FRAC_1_PI * 0.5), // 1/(2*pi)
        253 => Opnd::Scc,
        255 => Opnd::Literal(lit.unwrap_or(0)),
        other => Opnd::Raw(other),
    }
}

/// Decode a 9-bit source code (VOP src0 / VOP3 sources): 256+ selects a VGPR.
fn src9(code: u16, lit: Option<u32>) -> Opnd {
    if code >= 256 {
        Opnd::Vgpr((code - 256) as u8)
    } else {
        ssrc8(code, lit)
    }
}

/// Decode destination + source operands and encoding-specific aux fields.
/// Only the understood encodings are filled; others return empties (never guessed).
fn decode_operands(enc: Enc, raw: &[u32]) -> (Vec<Opnd>, Vec<Opnd>, InsnAux) {
    let w = raw[0];
    let lit = raw.get(1).copied(); // trailing literal (if the length decode added one)
    let mut aux = InsnAux::default();
    let sgpr_pair = |base: u16| Opnd::Sgpr(base as u8); // base of an SGPR range
    match enc {
        Enc::Sop2 => {
            let sdst = (w >> 16) & 0x7f;
            let ssrc1 = (w >> 8) & 0xff;
            let ssrc0 = w & 0xff;
            (vec![ssrc8(sdst as u16, None)],
             vec![ssrc8(ssrc0 as u16, lit), ssrc8(ssrc1 as u16, lit)], aux)
        }
        Enc::Sop1 => {
            let sdst = (w >> 16) & 0x7f;
            let ssrc0 = w & 0xff;
            (vec![ssrc8(sdst as u16, None)], vec![ssrc8(ssrc0 as u16, lit)], aux)
        }
        Enc::Sopk => {
            let sdst = (w >> 16) & 0x7f;
            aux.imm = ((w & 0xffff) as i16) as i64; // sign-extended SIMM16
            (vec![ssrc8(sdst as u16, None)], vec![], aux)
        }
        Enc::Sopc => {
            let ssrc1 = (w >> 8) & 0xff;
            let ssrc0 = w & 0xff;
            (vec![Opnd::Scc], vec![ssrc8(ssrc0 as u16, lit), ssrc8(ssrc1 as u16, lit)], aux)
        }
        Enc::Sopp => {
            aux.imm = ((w & 0xffff) as i16) as i64;
            (vec![], vec![], aux)
        }
        Enc::Smem => {
            let sdata = (w >> 6) & 0x7f;
            let sbase = (w & 0x3f) * 2; // SBASE is an even-aligned SGPR pair, stored /2
            aux.imm = (lit.unwrap_or(raw.get(1).copied().unwrap_or(0)) & 0x1f_ffff) as i64;
            if let Some(d1) = raw.get(1) {
                aux.imm = (d1 & 0x1f_ffff) as i64; // 21-bit OFFSET in dword1
            }
            (vec![ssrc8(sdata as u16, None)], vec![sgpr_pair(sbase as u16)], aux)
        }
        Enc::Vop2 => {
            let vdst = (w >> 17) & 0xff;
            let vsrc1 = (w >> 9) & 0xff;
            let src0 = w & 0x1ff;
            (vec![Opnd::Vgpr(vdst as u8)],
             vec![src9(src0 as u16, lit), Opnd::Vgpr(vsrc1 as u8)], aux)
        }
        Enc::Vop1 => {
            let vdst = (w >> 17) & 0xff;
            let src0 = w & 0x1ff;
            (vec![Opnd::Vgpr(vdst as u8)], vec![src9(src0 as u16, lit)], aux)
        }
        Enc::Vopc => {
            let vsrc1 = (w >> 9) & 0xff;
            let src0 = w & 0x1ff;
            (vec![Opnd::VccLo], vec![src9(src0 as u16, lit), Opnd::Vgpr(vsrc1 as u8)], aux)
        }
        Enc::Vop3 => {
            let vdst = w & 0xff;
            let d1 = raw.get(1).copied().unwrap_or(0);
            let s0 = d1 & 0x1ff;
            let s1 = (d1 >> 9) & 0x1ff;
            let s2 = (d1 >> 18) & 0x1ff;
            (vec![Opnd::Vgpr(vdst as u8)],
             vec![src9(s0 as u16, None), src9(s1 as u16, None), src9(s2 as u16, None)], aux)
        }
        Enc::Vinterp => {
            let vdst = (w >> 18) & 0xff;
            let attr = (w >> 10) & 0x3f;
            let chan = (w >> 8) & 0x3;
            let vsrc = w & 0xff;
            aux.imm = attr as i64;
            aux.imm2 = chan as i64;
            (vec![Opnd::Vgpr(vdst as u8)], vec![Opnd::Vgpr(vsrc as u8)], aux)
        }
        Enc::Exp => {
            let en = (w & 0xf) as u8;
            let target = (w >> 4) & 0x3f;
            aux.imm = target as i64;
            aux.mask = en;
            let d1 = raw.get(1).copied().unwrap_or(0);
            let s = vec![
                Opnd::Vgpr((d1 & 0xff) as u8),
                Opnd::Vgpr(((d1 >> 8) & 0xff) as u8),
                Opnd::Vgpr(((d1 >> 16) & 0xff) as u8),
                Opnd::Vgpr(((d1 >> 24) & 0xff) as u8),
            ];
            (vec![], s, aux)
        }
        _ => (vec![], vec![], aux),
    }
}

/// Base length (dwords) for an encoding, plus whether an inline literal source is present.
fn instr_len(enc: Enc, w: u32) -> (usize, bool) {
    match enc {
        // Scalar: 1 dword base. Literal when a source field == 255.
        Enc::Sop2 => (1, ssrc_is_lit(w & 0xff) || ssrc_is_lit((w >> 8) & 0xff)),
        Enc::Sopc => (1, ssrc_is_lit(w & 0xff) || ssrc_is_lit((w >> 8) & 0xff)),
        Enc::Sop1 => (1, ssrc_is_lit(w & 0xff)),
        Enc::Sopk => (1, false), // SIMM16 is inline in the instruction word
        Enc::Sopp => (1, false),
        // SMEM is 2 dwords on GFX10.
        Enc::Smem => (2, false),
        // Vector ALU: VOP2/VOP1/VOPC are 1 dword; literal when src0==255.
        Enc::Vop2 => (1, ssrc_is_lit(w & 0x1ff & 0xff) && (w & 0x1ff) == 255),
        Enc::Vop1 => (1, (w & 0x1ff) == 255),
        Enc::Vopc => (1, (w & 0x1ff) == 255),
        // VOP3/VOP3P: 2 dwords.
        Enc::Vop3 => (2, false),
        Enc::Vinterp => (1, false),
        // Memory / DS / FLAT / MIMG / MUBUF / MTBUF: 2 dwords.
        Enc::Ds | Enc::Mubuf | Enc::Mtbuf | Enc::Mimg | Enc::Flat => (2, false),
        // Export: 2 dwords.
        Enc::Exp => (2, false),
        Enc::Unknown => (1, false),
    }
}

/// Extract the opcode field for an encoding (used for mnemonic lookup / endpgm detection).
fn opcode_field(enc: Enc, w: u32) -> u32 {
    match enc {
        Enc::Sop2 => (w >> 23) & 0x7f,
        Enc::Sopk => (w >> 23) & 0x1f,
        Enc::Sop1 => (w >> 8) & 0xff,
        Enc::Sopc => (w >> 16) & 0x7f,
        Enc::Sopp => (w >> 16) & 0x7f,
        Enc::Smem => (w >> 18) & 0xff,
        Enc::Vop2 => (w >> 25) & 0x3f,
        Enc::Vop1 => (w >> 9) & 0xff,
        Enc::Vopc => (w >> 17) & 0xff,
        Enc::Vop3 => w & 0x3ff,
        Enc::Vinterp => (w >> 16) & 0x3,
        _ => (w >> 18) & 0xff,
    }
}

/// Known mnemonics for the opcodes we rely on (control flow + common scalar). The rest are
/// tagged by family+opcode so the output is honest about what is and isn't yet named.
fn mnemonic_for(enc: Enc, op: u32) -> String {
    let named = match enc {
        Enc::Sopp => match op {
            0x00 => Some("s_nop"),
            0x01 => Some("s_endpgm"),
            0x02 => Some("s_branch"),
            0x04 => Some("s_cbranch_scc0"),
            0x05 => Some("s_cbranch_scc1"),
            0x06 => Some("s_cbranch_vccz"),
            0x07 => Some("s_cbranch_vccnz"),
            0x08 => Some("s_cbranch_execz"),
            0x09 => Some("s_cbranch_execnz"),
            0x0c => Some("s_waitcnt"),
            0x1f => Some("s_code_end"),
            _ => None,
        },
        Enc::Sop1 => match op {
            0x03 => Some("s_mov_b32"),
            0x04 => Some("s_mov_b64"),
            _ => None,
        },
        Enc::Smem => match op {
            0x00 => Some("s_load_dword"),
            0x01 => Some("s_load_dwordx2"),
            0x02 => Some("s_load_dwordx4"),
            0x03 => Some("s_load_dwordx8"),
            _ => None,
        },
        Enc::Exp => Some("exp"),
        _ => None,
    };
    match named {
        Some(s) => s.to_string(),
        None => format!("{:?}<{:#x}>", enc, op).to_lowercase(),
    }
}
