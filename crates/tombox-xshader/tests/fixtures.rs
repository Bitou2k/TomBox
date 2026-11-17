//! Regression tests against real dumped Fortnite XSX shaders (committed in analysis/).
//!
//! These assert the two properties that make the extractor+decoder trustworthy:
//!   1. the DXBC→XDAT→zlib→FSSM→ISA chain recovers an s_endpgm-terminated RDNA2 program;
//!   2. the length-accurate decoder consumes the whole ISA and lands exactly on s_endpgm.
//! A wrong instruction length (e.g. mishandling a 2-dword VOP3 or an inline literal) would
//! desynchronize the stream and fail property 2, so this is a strong correctness check.

use std::path::PathBuf;
use tombox_xshader::{extract, rdna2, Stage};

fn fixture(name: &str) -> Vec<u8> {
    // tests run with CWD = crate dir; fixtures live at <repo>/analysis/shaders.
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../analysis/shaders");
    p.push(name);
    std::fs::read(&p).unwrap_or_else(|e| panic!("missing fixture {}: {e}", p.display()))
}

fn check(name: &str, want_stage: Stage) {
    let blob = fixture(name);
    let ex = extract(&blob).expect("extract");
    assert_eq!(ex.stage, want_stage, "{name}: stage");
    assert_eq!(ex.shader_model, (6, 0), "{name}: SM6.0");
    assert!(ex.bitcode_stripped, "{name}: DXIL should be NOBC-stripped");
    assert!(!ex.isa.is_empty(), "{name}: non-empty ISA");
    // ISA must end on s_endpgm (0xBF810000).
    let tail = &ex.isa[ex.isa.len() - 4..];
    assert_eq!(
        u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]),
        0xBF81_0000,
        "{name}: ISA must end on s_endpgm"
    );
    // Decoder must consume the whole ISA and terminate on s_endpgm.
    let insns = rdna2::disassemble(&ex.isa);
    let consumed: usize = insns.iter().map(|i| i.dwords * 4).sum();
    assert_eq!(consumed, ex.isa.len(), "{name}: decoder must consume all ISA bytes");
    let last = insns.last().expect("at least one insn");
    assert_eq!(last.mnemonic, "s_endpgm", "{name}: must terminate on s_endpgm");
}

#[test]
fn vertex_shader_extracts_and_decodes() {
    check("fortnite_vs_sample.dxbc", Stage::Vertex);
}

#[test]
fn pixel_shader_extracts_and_decodes() {
    check("fortnite_ps_sample.dxbc", Stage::Pixel);
}
