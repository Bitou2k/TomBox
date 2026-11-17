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

    // Structured RDNA2 decode (length-accurate), self-validated by landing on s_endpgm.
    println!("--- RDNA2 disassembly ---");
    let insns = tombox_xshader::rdna2::disassemble(&ex.isa);
    let mut consumed = 0usize;
    let mut ended = false;
    for ins in &insns {
        let raw: Vec<String> = ins.raw.iter().map(|w| format!("{w:08x}")).collect();
        println!(
            "  {:#06x}: {:<20} {:<8} {}",
            ins.offset,
            raw.join(" "),
            format!("{:?}", ins.enc).to_lowercase(),
            ins.mnemonic
        );
        consumed += ins.dwords * 4;
        if ins.mnemonic == "s_endpgm" {
            ended = true;
        }
    }
    let total = ex.isa.len();
    println!(
        "decode: {} instrs, consumed {}/{} bytes, terminated_on_s_endpgm={} (aligned_landing={})",
        insns.len(),
        consumed,
        total,
        ended,
        ended && consumed <= total && (total - consumed) < 4
    );

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
