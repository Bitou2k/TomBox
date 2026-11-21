//! rpf-shaders <shaders.rpf> [<out dir>]: list (and optionally extract) the shader
//! containers stored in a RAGE shader package. Extracted files are named
//! `<16-hex content hash>.dxbc`, de-duplicated, with `names.txt` mapping hash -> names.
use std::collections::BTreeMap;
use std::process::ExitCode;

fn fnv1a(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &x| (h ^ x as u64).wrapping_mul(0x0000_0100_0000_01b3))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let Some(path) = args.get(1) else {
        eprintln!("usage: rpf-shaders <shaders.rpf> [<out dir>]");
        return ExitCode::from(2);
    };
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let recs = tombox_rpf::shaders::find_records(&data);
    let mut uniq: BTreeMap<u64, (usize, Vec<String>)> = BTreeMap::new();
    let mut bytes = 0usize;
    for (i, r) in recs.iter().enumerate() {
        bytes += r.container.len();
        uniq.entry(fnv1a(&r.container)).or_insert((i, Vec::new())).1.push(r.name.clone());
    }
    println!("{path}: {} shader records ({} distinct), {:.1} MB inflated", recs.len(), uniq.len(), bytes as f64 / 1e6);
    if let Some(dir) = args.get(2) {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("{dir}: {e}");
            return ExitCode::FAILURE;
        }
        let mut names = String::new();
        for (h, (i, n)) in &uniq {
            let _ = std::fs::write(format!("{dir}/{h:016x}.dxbc"), &recs[*i].container);
            names.push_str(&format!("{h:016x} {}\n", n.join(" ")));
        }
        let _ = std::fs::write(format!("{dir}/names.txt"), names);
        println!("wrote {} containers to {dir}", uniq.len());
    }
    ExitCode::SUCCESS
}
