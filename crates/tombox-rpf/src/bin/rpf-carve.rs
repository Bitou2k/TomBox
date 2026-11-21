//! rpf-carve <package> <out dir> [--max-files N] [--kinds xml,psin,...] [--start B] [--len B]:
//! inflate and save the zlib streams in a package's data area (named by offset), with a
//! summary. Reads the file in 64 MiB windows, so packages of any size work; --start/--len
//! (bytes, decimal or 0x hex) limit the scan to a range of a very large package.
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
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
    const WIN: u64 = 64 << 20;
    const OVERLAP: u64 = 16 << 20; // streams starting in a window may run into the next
    let mut stats: BTreeMap<&'static str, (usize, u64)> = BTreeMap::new();
    let (mut written, mut next_start) = (0usize, start);
    let mut base = start;
    let mut pending: Option<(u64, u64, Vec<u8>)> = None;
    let emit = |p: &(u64, u64, Vec<u8>), stats: &mut BTreeMap<&'static str, (usize, u64)>, written: &mut usize| {
        let (kind, ext) = tombox_rpf::carve::classify(&p.2);
        let e = stats.entry(kind).or_insert((0, 0));
        e.0 += 1;
        e.1 += p.2.len() as u64;
        if *written < max_files && kinds.as_ref().map(|k| k.iter().any(|x| x == ext)).unwrap_or(true) {
            let _ = std::fs::write(out.join(format!("{:012x}.{ext}", p.0)), &p.2);
            *written += 1;
        }
    };
    let mut buf = Vec::new();
    while base < total && written < max_files {
        let len = (WIN + OVERLAP).min(total - base);
        buf.resize(len as usize, 0);
        if f.seek(SeekFrom::Start(base)).is_err() || f.read_exact(&mut buf).is_err() {
            eprintln!("read error at {base:#x}");
            break;
        }
        let scan_end = if base + WIN >= total { len } else { WIN };
        let from = next_start.saturating_sub(base) as usize;
        tombox_rpf::carve::carve(&buf[from..scan_end as usize], base + from as u64, 256 << 20, |c| {
            if c.offset < next_start || written >= max_files {
                return;
            }
            next_start = c.offset + c.packed as u64;
            // Files are stored as consecutive zlib streams of 64 KiB each (observed: an XML
            // file continues in the stream that starts right after a 65,536-byte one).
            let joins = pending.as_ref().map(|p: &(u64, u64, Vec<u8>)| p.2.len() % 65536 == 0 && c.offset >= p.1 && c.offset - p.1 < 16).unwrap_or(false);
            if joins {
                let p = pending.as_mut().unwrap();
                p.1 = c.offset + c.packed as u64;
                p.2.extend_from_slice(&c.data);
                return;
            }
            if let Some(p) = pending.take() {
                emit(&p, &mut stats, &mut written);
            }
            pending = Some((c.offset, c.offset + c.packed as u64, c.data));
        });
        base += WIN;
        eprint!("\r{:.1}% ", 100.0 * (base.min(total) - start) as f64 / (total - start).max(1) as f64);
    }
    if let Some(p) = pending.take() {
        emit(&p, &mut stats, &mut written);
    }
    eprintln!();
    println!("{path}: bytes {start:#x}..{total:#x} of {file_len}");
    for (k, (n, b)) in &stats {
        println!("  {n:>8}  {:>10.1} MB  {k}", *b as f64 / 1e6);
    }
    println!("wrote {written} files to {}", out.display());
    ExitCode::SUCCESS
}
