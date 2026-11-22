//! rpf-carve <package> <out dir> [--max-files N] [--kinds xml,psin,...] [--start B] [--len B]:
//! inflate and save the zlib streams in a package's data area (named by offset), with a
//! summary. Reads the file in windows (tombox_rpf::files), so packages of any size work;
//! --start/--len (bytes, decimal or 0x hex) limit the scan to a range of a very large package.
use std::collections::BTreeMap;
use std::process::ExitCode;

fn main() -> ExitCode {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 3 {
        eprintln!("usage: rpf-carve <package> <out dir> [--max-files N] [--kinds xml,psin,rpf,txt,bin] [--start B] [--len B]");
        return ExitCode::from(2);
    }
    let (path, out) = (&a[1], std::path::PathBuf::from(&a[2]));
    let mut max_files = usize::MAX;
    let mut kinds: Option<Vec<String>> = None;
    let (mut start, mut range) = (0u64, u64::MAX);
    let num = |s: &str| match s.strip_prefix("0x") {
        Some(h) => u64::from_str_radix(h, 16).ok(),
        None => s.parse().ok(),
    };
    let mut i = 3;
    while i + 1 < a.len() {
        match a[i].as_str() {
            "--max-files" => max_files = a[i + 1].parse().unwrap_or(usize::MAX),
            "--kinds" => kinds = Some(a[i + 1].split(',').map(String::from).collect()),
            "--start" | "--len" => match num(&a[i + 1]) {
                Some(v) if a[i] == "--start" => start = v,
                Some(v) => range = v,
                None => {
                    eprintln!("bad number for {}: {}", a[i], a[i + 1]);
                    return ExitCode::from(2);
                }
            },
            o => eprintln!("ignoring {o}"),
        }
        i += 2;
    }
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let file_len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if start >= file_len {
        eprintln!("--start {start:#x} is past the end of the file ({file_len:#x})");
        return ExitCode::from(2);
    }
    let total = start.saturating_add(range).min(file_len);
    let _ = std::fs::create_dir_all(&out);
    let mut stats: BTreeMap<&'static str, (usize, u64)> = BTreeMap::new();
    let mut written = 0usize;
    // Stops early once enough files are written: files are visited in offset order.
    let mut done = false;
    let r = tombox_rpf::files::each_file(
        &mut f,
        file_len,
        start,
        total,
        |p| eprint!("\r{:.1}% ", 100.0 * (p - start) as f64 / (total - start).max(1) as f64),
        |file| {
            if done {
                return;
            }
            let (kind, ext) = tombox_rpf::carve::classify(&file.data);
            let e = stats.entry(kind).or_insert((0, 0));
            e.0 += 1;
            e.1 += file.data.len() as u64;
            if kinds.as_ref().map(|k| k.iter().any(|x| x == ext)).unwrap_or(true) {
                let _ = std::fs::write(out.join(format!("{:012x}.{ext}", file.offset)), &file.data);
                written += 1;
                done = written >= max_files;
            }
        },
    );
    if let Err(e) = r {
        eprintln!("\nread error: {e}");
    }
    eprintln!();
    println!("{path}: bytes {start:#x}..{total:#x} of {file_len}");
    for (k, (n, b)) in &stats {
        println!("  {n:>8}  {:>10.1} MB  {k}", *b as f64 / 1e6);
    }
    println!("wrote {written} files to {}", out.display());
    ExitCode::SUCCESS
}
