//! CLI: extract native RDNA2 ISA from an Xbox Series shader DXBC blob.
//!
//! Usage: xshader <shader.dxbc> [--isa-out <file.bin>] [--fssm-out <file.bin>]
//!
//! Prints the DXBC→XDAT→zlib→FSSM→RDNA2 chain it walked and a hex/word dump of the ISA.

use std::process::ExitCode;
use tombox_xshader::{extract, Stage};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: {} <shader.dxbc> [--isa-out f] [--fssm-out f]", args[0]);
        return ExitCode::FAILURE;
    }
    let path = &args[1];
    let mut isa_out = None;
    let mut fssm_out = None;
    let mut i = 2;
    while i + 1 < args.len() {
        match args[i].as_str() {
            "--isa-out" => isa_out = Some(args[i + 1].clone()),
            "--fssm-out" => fssm_out = Some(args[i + 1].clone()),
            other => eprintln!("warning: ignoring unknown arg {other}"),
        }
        i += 2;
    }

    let blob = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let ex = match extract(&blob) {
        Ok(ex) => ex,
        Err(e) => {
            eprintln!("extract failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    let stage = match ex.stage {
        Stage::Pixel => "Pixel".into(),
        Stage::Vertex => "Vertex".into(),
        Stage::Geometry => "Geometry".into(),
        Stage::Hull => "Hull".into(),
        Stage::Domain => "Domain".into(),
        Stage::Compute => "Compute".into(),
        Stage::Mesh => "Mesh".into(),
        Stage::Amplification => "Amplification".into(),
        Stage::Unknown(x) => format!("Unknown({x})"),
    };

    println!("== {path} ==");
    println!(
        "stage={stage}  SM={}.{}  bitcode_stripped={}  entry={:?}",
        ex.shader_model.0, ex.shader_model.1, ex.bitcode_stripped, ex.entry
    );
    println!(
        "DXBC chunks: inputSig={} outputSig={} PSV0={} rootSig={}",
        ex.has_input_sig, ex.has_output_sig, ex.has_psv0, ex.has_root_sig
    );
    println!(
        "FSSM: {} bytes   ISA: {} bytes ({} dwords) @ FSSM+{:#x}",
        ex.fssm.len(),
        ex.isa.len(),
        ex.isa.len() / 4,
        ex.isa_offset_in_fssm
    );

    // Word dump of the ISA with SOPP markers annotated.
    println!("--- RDNA2 ISA (little-endian dwords) ---");
    for (idx, w4) in ex.isa.chunks(4).enumerate() {
        if w4.len() < 4 {
            break;
        }
        let w = u32::from_le_bytes([w4[0], w4[1], w4[2], w4[3]]);
        let note = annotate(w);
        println!("  [{:>3}] {:#06x}: {:08x}  {}", idx, idx * 4, w, note);
    }

    if let Some(f) = isa_out {
        if let Err(e) = std::fs::write(&f, &ex.isa) {
            eprintln!("cannot write {f}: {e}");
        } else {
            println!("wrote ISA -> {f}");
        }
    }
    if let Some(f) = fssm_out {
        if let Err(e) = std::fs::write(&f, &ex.fssm) {
            eprintln!("cannot write {f}: {e}");
        } else {
            println!("wrote FSSM -> {f}");
        }
    }
    ExitCode::SUCCESS
}

/// Minimal annotation of SOPP control/sync ops (full disassembler is a separate module).
fn annotate(w: u32) -> &'static str {
    if (w >> 23) != 0x17f {
        return "";
    }
    match (w >> 16) & 0x7f {
        0x00 => "s_nop",
        0x01 => "s_endpgm",
        0x02 => "s_branch",
        0x04 => "s_cbranch_scc0",
        0x05 => "s_cbranch_scc1",
        0x06 => "s_cbranch_vccz",
        0x07 => "s_cbranch_vccnz",
        0x08 => "s_cbranch_execz",
        0x09 => "s_cbranch_execnz",
        0x0c => "s_waitcnt",
        0x1f => "s_code_end",
        0x20 => "s_endpgm_saved",
        _ => "s_<sopp>",
    }
}
