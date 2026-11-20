//! rpf-inspect <file.rpf>: print the header fields and region map of a RAGE package file.
use std::process::ExitCode;
use tombox_rpf::{inspect, Header, RegionKind, RPF7_OPEN};

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: rpf-inspect <file.rpf>");
        return ExitCode::from(2);
    };
    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let r = match inspect(&data) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("{path}: RPF{} , {} bytes", r.version, r.size);
    match &r.header {
        Header::V7 { entry_count, names_length, encryption } => {
            let enc = if *encryption == RPF7_OPEN { "OPEN (table is plain)".to_string() } else { format!("{encryption:#010x} (table is encrypted)") };
            println!("  entries {entry_count}, names {names_length} bytes, encryption {enc}");
        }
        Header::V9 { words } => {
            println!("  header (0x40 bytes, plain); field meanings are not established:");
            for (i, w) in words.iter().enumerate().skip(1) {
                println!("    +{:#04x}  {w:#010x}  {w}", i * 4);
            }
        }
    }
    println!("  regions:");
    for g in &r.regions {
        let kind = match g.kind {
            RegionKind::Header => "header",
            RegionKind::Padding => "zero padding",
            RegionKind::HighEntropy => "high entropy (encrypted or compressed)",
            RegionKind::Plain => "plain data",
        };
        println!("    {:#08x}..{:#08x}  {:>8} bytes  entropy {:.2}  {kind}", g.start, g.end, g.end - g.start, g.entropy);
    }
    if r.regions.iter().skip(1).all(|g| g.kind != RegionKind::Plain) {
        println!("  nothing after the header is readable as plain data: the table of contents and the contents are encrypted.");
    }
    ExitCode::SUCCESS
}
