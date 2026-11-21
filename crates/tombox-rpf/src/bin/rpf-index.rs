//! rpf-index <package> <out dir> [--threads N] [--chunk BYTES] [--start B] [--len B]:
//! build a mount index of a package without extracting it. One pass inflates every stream
//! (see tombox_rpf::files), and records per file its offset, extent, size, kind and the
//! resource names it carries, plus every map placement record found (tombox_rpf::index).
//! Nothing is written but the index; a viewer reads files from the package by offset.
//!
//! Output, in <out dir>:
//!   files.tsv     index, offset, end, streams, size, tag (+0x10, hex), kind, names (name.#ext;...)
//!   entities.bin  72-byte little-endian records: u32 file index, u32 offset in file, u64 id,
//!                 f32x4 quaternion, f32x3 position, u32 model name hash, u32 flags,
//!                 f32 scale XY, f32 scale Z, u16 index, u16 parent, f32 draw distance, u32 0
//!   index.txt     package path, length, modification time, counts
use std::io::Write;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use tombox_rpf::{carve, files, index};

struct FileRow {
    offset: u64,
    end: u64,
    streams: u32,
    size: usize,
    tag: [u8; 4],
    kind: &'static str,
    names: Vec<(String, String)>,
    placements: Vec<index::Placement>,
}

fn row(f: files::File) -> FileRow {
    let tag = f.data.get(0x10..0x14).map(|t| [t[0], t[1], t[2], t[3]]).unwrap_or([0; 4]);
    FileRow {
        offset: f.offset,
        end: f.end,
        streams: f.streams,
        size: f.data.len(),
        tag,
        kind: carve::classify(&f.data).0,
        names: index::names(&f.data),
        placements: index::placements(&f.data),
    }
}

fn main() -> ExitCode {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 3 {
        eprintln!("usage: rpf-index <package> <out dir> [--threads N] [--chunk BYTES] [--start B] [--len B]");
        return ExitCode::from(2);
    }
    let (path, out) = (a[1].clone(), std::path::PathBuf::from(&a[2]));
    let num = |s: &str| match s.strip_prefix("0x") {
        Some(h) => u64::from_str_radix(h, 16).ok(),
        None => s.parse().ok(),
    };
    let (mut threads, mut chunk, mut start, mut range) = (4u64, 1u64 << 30, 0u64, u64::MAX);
    let mut i = 3;
    while i + 1 < a.len() {
        let Some(v) = num(&a[i + 1]) else {
            eprintln!("bad number for {}: {}", a[i], a[i + 1]);
            return ExitCode::from(2);
        };
        match a[i].as_str() {
            "--threads" => threads = v.clamp(1, 64),
            "--chunk" => chunk = v.max(16 << 20),
            "--start" => start = v,
            "--len" => range = v,
            o => {
                eprintln!("unknown option {o}");
                return ExitCode::from(2);
            }
        }
        i += 2;
    }
    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let file_len = meta.len();
    if start >= file_len {
        eprintln!("--start {start:#x} is past the end of the file ({file_len:#x})");
        return ExitCode::from(2);
    }
    let total = start.saturating_add(range).min(file_len);
    let chunks: Vec<(u64, u64)> = (0..).map(|k| start + k * chunk).take_while(|&s| s < total).map(|s| (s, (s + chunk).min(total))).collect();
    let results: Vec<Mutex<Option<Result<Vec<FileRow>, String>>>> = chunks.iter().map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let done = AtomicU64::new(0);
    let t0 = std::time::Instant::now();
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let k = next.fetch_add(1, Ordering::Relaxed);
                let Some(&(cs, ce)) = chunks.get(k) else { break };
                let mut rows = Vec::new();
                let mut last = cs;
                let r = std::fs::File::open(&path).and_then(|mut f| {
                    files::each_file(&mut f, file_len, cs, ce, |p| {
                        done.fetch_add(p.saturating_sub(last), Ordering::Relaxed);
                        last = last.max(p);
                    }, |file| rows.push(row(file)))
                });
                *results[k].lock().unwrap() = Some(r.map(|_| rows).map_err(|e| format!("bytes {cs:#x}..{ce:#x}: {e}")));
            });
        }
        while results.iter().any(|r| r.lock().unwrap().is_none()) {
            std::thread::sleep(std::time::Duration::from_secs(5));
            let d = done.load(Ordering::Relaxed);
            let el = t0.elapsed().as_secs_f64();
            eprint!("\r{:.2}% {:.0} MB/s   ", 100.0 * d as f64 / (total - start).max(1) as f64, d as f64 / 1e6 / el.max(1e-3));
        }
    });
    eprintln!();
    let _ = std::fs::create_dir_all(&out);
    let mut rows: Vec<FileRow> = Vec::new();
    let mut failed = 0;
    for r in results {
        match r.into_inner().unwrap() {
            Some(Ok(v)) => {
                // A file completed by the previous chunk owns the streams after its first one.
                let edge = rows.last().map_or(0, |r| r.end);
                rows.extend(v.into_iter().filter(|r| r.offset >= edge));
            }
            Some(Err(e)) => {
                eprintln!("read error, {e}");
                failed += 1;
            }
            None => failed += 1,
        }
    }
    let write = || -> std::io::Result<(usize, usize)> {
        let mut ft = std::io::BufWriter::new(std::fs::File::create(out.join("files.tsv"))?);
        let mut eb = std::io::BufWriter::new(std::fs::File::create(out.join("entities.bin"))?);
        writeln!(ft, "index\toffset\tend\tstreams\tsize\ttag\tkind\tnames")?;
        let mut ents = 0;
        for (n, r) in rows.iter().enumerate() {
            let names: Vec<String> = r.names.iter().map(|(a, e)| format!("{a}.#{e}")).collect();
            let tag: String = r.tag.iter().map(|b| format!("{b:02x}")).collect();
            writeln!(ft, "{n}\t{:#x}\t{:#x}\t{}\t{}\t{tag}\t{}\t{}", r.offset, r.end, r.streams, r.size, r.kind, names.join(";"))?;
            for p in &r.placements {
                let mut b = Vec::with_capacity(72);
                b.extend((n as u32).to_le_bytes());
                b.extend(p.at.to_le_bytes());
                b.extend(p.id.to_le_bytes());
                p.quat.iter().for_each(|x| b.extend(x.to_le_bytes()));
                p.pos.iter().for_each(|x| b.extend(x.to_le_bytes()));
                b.extend(p.name.to_le_bytes());
                b.extend(p.flags.to_le_bytes());
                p.scale.iter().for_each(|x| b.extend(x.to_le_bytes()));
                b.extend(p.index.to_le_bytes());
                b.extend(p.parent.to_le_bytes());
                b.extend(p.lod.to_le_bytes());
                b.extend(0u32.to_le_bytes());
                debug_assert_eq!(b.len(), 72);
                eb.write_all(&b)?;
                ents += 1;
            }
        }
        let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
        let named = rows.iter().filter(|r| !r.names.is_empty()).count();
        std::fs::write(
            out.join("index.txt"),
            format!("package\t{path}\nlength\t{file_len}\nmtime\t{mtime}\nrange\t{start:#x}..{total:#x}\nfiles\t{}\nnamed files\t{named}\nentities\t{ents}\nfailed chunks\t{failed}\nseconds\t{:.0}\n", rows.len(), t0.elapsed().as_secs_f64()),
        )?;
        Ok((ents, named))
    };
    match write() {
        Ok((ents, named)) => {
            println!("{path}: {} files ({named} named), {ents} placements, {failed} failed chunks, {:.0} s", rows.len(), t0.elapsed().as_secs_f64());
            if failed > 0 { ExitCode::FAILURE } else { ExitCode::SUCCESS }
        }
        Err(e) => {
            eprintln!("writing {}: {e}", out.display());
            ExitCode::FAILURE
        }
    }
}
