//! xscan: run the extractor + RDNA2→HLSL translator over every Xbox shader container found
//! in a file (e.g. a .pak holding a ShaderArchive) and report coverage.
//!
//! Usage: xscan <file> [--examples <dir>] [--limit <n>]
//!
//! Finds `DXBC` containers by magic + validated header, de-duplicates by content hash,
//! translates each, and prints per-stage totals plus a histogram of failure reasons
//! (offsets normalised). With --examples, one sample blob per distinct failure is saved.

use std::collections::{BTreeMap, HashSet};
use std::process::ExitCode;
use tombox_xshader::{extract, hlsl, Stage};

fn fnv1a(b: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &x in b {
        h ^= x as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Strip run-specific details (offsets, register numbers) so failures group by cause.
fn normalise(e: &str) -> String {
    let mut out = String::new();
    let mut chars = e.chars().peekable();
    let mut in_angle = false;
    while let Some(c) = chars.next() {
        // Keep opcode numbers inside <...> (e.g. vop3<0x2ff>) — they identify the gap.
        if c == '<' {
            in_angle = true;
        } else if c == '>' {
            in_angle = false;
        }
        if in_angle {
            out.push(c);
            continue;
        }
        if c == '0' && chars.peek() == Some(&'x') {
            chars.next();
            while chars.peek().map(|c| c.is_ascii_hexdigit()).unwrap_or(false) {
                chars.next();
            }
            out.push('#');
        } else if c.is_ascii_digit() {
            while chars.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                chars.next();
            }
            out.push('#');
        } else {
            out.push(c);
        }
    }
    out
}

/// Minimal LZ4 *block* decompressor (bounds-checked; returns None on malformed input).
fn lz4_block(src: &[u8], out_len: usize) -> Option<Vec<u8>> {
    let mut out: Vec<u8> = Vec::with_capacity(out_len);
    let mut i = 0usize;
    while i < src.len() {
        let token = src[i];
        i += 1;
        let mut lit = (token >> 4) as usize;
        if lit == 15 {
            loop {
                let b = *src.get(i)?;
                i += 1;
                lit += b as usize;
                if b != 255 {
                    break;
                }
            }
        }
        out.extend_from_slice(src.get(i..i + lit)?);
        i += lit;
        if i >= src.len() {
            break; // last sequence has only literals
        }
        let off = u16::from_le_bytes([*src.get(i)?, *src.get(i + 1)?]) as usize;
        i += 2;
        let mut mlen = (token & 15) as usize;
        if mlen == 15 {
            loop {
                let b = *src.get(i)?;
                i += 1;
                mlen += b as usize;
                if b != 255 {
                    break;
                }
            }
        }
        mlen += 4;
        if off == 0 || off > out.len() {
            return None;
        }
        let start = out.len() - off;
        for k in 0..mlen {
            let b = out[start + k];
            out.push(b);
        }
        if out.len() > out_len {
            return None;
        }
    }
    if out.len() == out_len { Some(out) } else { None }
}

/// A UE `FSerializedShaderArchive` shader-entry table located inside a larger file.
struct Archive {
    /// (absolute file offset, compressed size, uncompressed size)
    entries: Vec<(usize, usize, usize)>,
}

/// Locate UE shader-archive entry tables structurally: a `TArray<FShaderCodeEntry>`
/// (i32 count, then 17-byte records {u64 Offset, u32 Size, u32 UncompressedSize, u8
/// Frequency}) whose offsets start at 0 and chain (Offset[i+1] == Offset[i] + Size[i]),
/// followed by `TArray<FFileCachePreloadEntry>` (16-byte) and `TArray<u32>` ShaderIndices,
/// after which the shader code begins.
fn find_archives(data: &[u8]) -> Vec<Archive> {
    let rd32 = |o: usize| data.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
    let rd64 = |o: usize| data.get(o..o + 8).map(|b| u64::from_le_bytes(b.try_into().unwrap()) as usize);
    let mut out = Vec::new();
    let mut p = 4usize;
    while p + 17 * 64 < data.len() {
        // Cheap prefilter: first record has Offset == 0 and a small Frequency byte.
        if data[p..p + 8] != [0u8; 8] || data[p + 16] > 8 {
            p += 1;
            continue;
        }
        let n = match rd32(p - 4) {
            Some(n) if (32..4_000_000).contains(&n) && p + n * 17 + 8 <= data.len() => n,
            _ => {
                p += 1;
                continue;
            }
        };
        let mut ok = true;
        let mut expect = 0usize;
        let mut recs = Vec::with_capacity(n);
        for k in 0..n {
            let r = p + k * 17;
            let (off, size, usize_) = (rd64(r).unwrap(), rd32(r + 8).unwrap(), rd32(r + 12).unwrap());
            if off != expect || size == 0 || size > usize_ || usize_ > 0x40_0000 || data[r + 16] > 8 {
                ok = false;
                break;
            }
            expect = off + size;
            recs.push((off, size, usize_));
        }
        if !ok {
            p += 1;
            continue;
        }
        let mut q = p + n * 17;
        let npre = match rd32(q) {
            Some(v) if v < 50_000_000 => v,
            _ => {
                p += 1;
                continue;
            }
        };
        q += 4 + npre * 16;
        let nidx = match rd32(q) {
            Some(v) if v < 200_000_000 => v,
            _ => {
                p += 1;
                continue;
            }
        };
        q += 4 + nidx * 4;
        if q + expect > data.len() {
            p += 1;
            continue;
        }
        eprintln!("shader archive: {n} entries, table @ {p:#x}, code @ {q:#x} ({expect} bytes)");
        out.push(Archive { entries: recs.into_iter().map(|(o, s, u)| (q + o, s, u)).collect() });
        p = q + expect;
    }
    out
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: {} <file> [--examples <dir>] [--limit <n>]", args[0]);
        return ExitCode::FAILURE;
    }
    let mut examples: Option<String> = None;
    let mut limit = usize::MAX;
    let mut i = 2;
    while i + 1 < args.len() {
        match args[i].as_str() {
            "--examples" => examples = Some(args[i + 1].clone()),
            "--limit" => limit = args[i + 1].parse().unwrap_or(usize::MAX),
            other => eprintln!("warning: ignoring {other}"),
        }
        i += 2;
    }
    let data = match std::fs::read(&args[1]) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("cannot read {}: {e}", args[1]);
            return ExitCode::FAILURE;
        }
    };
    if let Some(d) = &examples {
        let _ = std::fs::create_dir_all(d);
    }
    let mut seen: HashSet<u64> = HashSet::new();
    let mut found = 0usize;
    let mut by_stage: BTreeMap<String, (usize, usize)> = BTreeMap::new(); // stage -> (ok, fail)
    let mut errors: BTreeMap<String, (usize, u64)> = BTreeMap::new(); // reason -> (count, example hash)
    // Gather candidate blobs: UE shader-archive entries (LZ4 blocks) if the file holds
    // an archive, otherwise raw DXBC containers found by magic.
    let mut blobs: Vec<Vec<u8>> = Vec::new();
    let archives = find_archives(&data);
    let mut lz_fail = 0usize;
    for a in &archives {
        for &(off, size, usize_) in &a.entries {
            let src = &data[off..off + size];
            let dec = if size == usize_ { Some(src.to_vec()) } else { lz4_block(src, usize_) };
            match dec {
                Some(d) => blobs.push(d),
                None => lz_fail += 1,
            }
        }
    }
    if !archives.is_empty() {
        eprintln!("decompressed {} archive entries ({lz_fail} LZ4 failures)", blobs.len());
    } else {
        blobs.push(data.clone());
    }
    for buf in &blobs {
      let data = &buf[..];
      let mut p = 0usize;
      while p + 32 <= data.len() && seen.len() < limit {
        if &data[p..p + 4] != b"DXBC" {
            p += 1;
            continue;
        }
        let total = u32::from_le_bytes(data[p + 24..p + 28].try_into().unwrap()) as usize;
        let nchunks = u32::from_le_bytes(data[p + 28..p + 32].try_into().unwrap()) as usize;
        if total < 64 || total > 0x40_0000 || p + total > data.len() || nchunks == 0 || nchunks > 32 {
            p += 4;
            continue;
        }
        let blob = &data[p..p + total];
        found += 1;
        p += total;
        let h = fnv1a(blob);
        if !seen.insert(h) {
            continue;
        }
        let res = std::panic::catch_unwind(|| match extract(blob) {
            Err(e) => (String::from("?"), Err(format!("extract: {e}"))),
            Ok(ex) => {
                let st = match ex.stage {
                    Stage::Vertex => "VS",
                    Stage::Pixel => "PS",
                    Stage::Compute => "CS",
                    Stage::Geometry => "GS",
                    Stage::Hull => "HS",
                    Stage::Domain => "DS",
                    Stage::Mesh => "MS",
                    Stage::Amplification => "AS",
                    Stage::Unknown(_) => "??",
                };
                (st.to_string(), hlsl::translate(&ex).map(|_| ()))
            }
        })
        .unwrap_or_else(|_| (String::from("?"), Err("PANIC in translator".to_string())));
        let e = by_stage.entry(res.0).or_insert((0, 0));
        match res.1 {
            Ok(()) => e.0 += 1,
            Err(msg) => {
                e.1 += 1;
                let key = normalise(&msg);
                let ent = errors.entry(key).or_insert((0, h));
                if ent.0 == 0 {
                    if let Some(d) = &examples {
                        let _ = std::fs::write(format!("{d}/{h:016x}.dxbc"), blob);
                    }
                }
                ent.0 += 1;
            }
        }
      }
    }
    let (ok, fail) = by_stage.values().fold((0, 0), |a, v| (a.0 + v.0, a.1 + v.1));
    println!("containers found: {found}   unique: {}   translated: {ok}   failed: {fail}   ({:.1}% ok)",
        seen.len(), 100.0 * ok as f64 / (ok + fail).max(1) as f64);
    for (st, (o, f)) in &by_stage {
        println!("  {st}: ok={o} fail={f}");
    }
    let mut v: Vec<_> = errors.into_iter().collect();
    v.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    println!("--- failure reasons ({} distinct) ---", v.len());
    for (reason, (count, ex)) in v.iter().take(60) {
        println!("{count:>7}  {reason}   [e.g. {ex:016x}]");
    }
    ExitCode::SUCCESS
}
