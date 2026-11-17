//! xpair: how well can Xbox shaders be paired with shaders of a PC build of the same game?
//!
//! Usage: xpair <xbox pak or archive> <pc .ushaderbytecode>...
//!
//! Exploratory: reports, for candidate pairing keys built from engine metadata that both
//! platforms carry, how many Xbox shaders have a PC shader with the same key and how
//! ambiguous the key is. It pairs nothing by itself.
use std::collections::HashMap;
use tombox_xshader::uearchive::{find_archives, lz4_block, parse_head, split_entry};

/// Input and output signature of a DXBC container as sorted "NAME<index>" lists
/// (ISGN/OSGN: 24-byte elements; ISG1/OSG1: 32-byte, OSG5: 28-byte, both with a leading
/// stream field). Element order and register packing are compiler-specific, names are not.
fn sig_key(c: &[u8]) -> Vec<u8> {
    let rd = |o: usize| c.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize).unwrap_or(0);
    let n = rd(28);
    let mut parts: [Vec<String>; 2] = Default::default();
    for i in 0..n.min(64) {
        let off = rd(32 + 4 * i);
        let Some(tag) = c.get(off..off + 4) else { continue };
        let (which, esz, skip) = match tag {
            b"ISGN" => (0, 24, 0),
            b"OSGN" => (1, 24, 0),
            b"ISG1" => (0, 32, 4),
            b"OSG1" => (1, 32, 4),
            b"OSG5" => (1, 28, 4),
            _ => continue,
        };
        let body = off + 8;
        let count = rd(body).min(64);
        for e in 0..count {
            let eo = body + 8 + e * esz + skip;
            let name_off = body + rd(eo);
            let name: String = c.get(name_off..).unwrap_or(&[]).iter().take_while(|&&b| b != 0).take(48).map(|&b| (b as char).to_ascii_uppercase()).collect();
            // The Xbox compiler appends the interpolation mode to the name ("TEXCOORD10_centroid").
            // and then carries the semantic index inside the name with element index 0.
            match name.strip_suffix("_CENTROID") {
                Some(n) => parts[which].push(n.to_string()),
                None => parts[which].push(format!("{name}{}", rd(eo + 4))),
            }
        }
    }
    parts[0].sort();
    parts[1].sort();
    format!("{}|{}", parts[0].join(","), parts[1].join(",")).into_bytes()
}

struct Rec {
    sig: Vec<u8>,
    freq: u8,
    head: Vec<u8>,
    hashes: Vec<u32>,
    parsed: bool,
}

fn load(path: &str) -> Vec<Rec> {
    let data = std::fs::read(path).expect("read");
    let mut out = Vec::new();
    for a in find_archives(&data) {
        for (k, &(off, size, usize_)) in a.entries.iter().enumerate() {
            let src = &data[off..off + size];
            let Some(d) = (if size == usize_ { Some(src.to_vec()) } else { lz4_block(src, usize_) }) else { continue };
            let Some(parts) = split_entry(&d) else { continue };
            let ph = parse_head(parts.head);
            out.push(Rec {
                sig: sig_key(parts.container),
                freq: a.freq[k],
                head: parts.head.to_vec(),
                hashes: ph.as_ref().map(|p| p.1[3].clone()).unwrap_or_default(),
                parsed: ph.is_some(),
            });
        }
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: xpair <xbox pak> <pc archive>...");
        std::process::exit(2);
    }
    let xb = load(&args[0]);
    let mut pc = Vec::new();
    for p in &args[1..] {
        pc.extend(load(p));
    }
    println!("xbox entries: {}  (head parsed: {})", xb.len(), xb.iter().filter(|r| r.parsed).count());
    println!("pc entries:   {}  (head parsed: {})", pc.len(), pc.iter().filter(|r| r.parsed).count());
    if std::env::var("XPAIR_DEBUG").is_ok() {
        // Show how signatures differ within a layout-hash match.
        let hk = |r: &Rec| {
            let mut v = vec![r.freq];
            r.hashes.iter().for_each(|h| v.extend_from_slice(&h.to_le_bytes()));
            v
        };
        let mut m: HashMap<Vec<u8>, Vec<&Rec>> = HashMap::new();
        for r in &pc {
            m.entry(hk(r)).or_default().push(r);
        }
        let mut shown = 0;
        for r in xb.iter().step_by(997) {
            if let Some(c) = m.get(&hk(r)) {
                if !c.iter().any(|p| p.sig == r.sig) && shown < 8 {
                    shown += 1;
                    println!("xbox freq {} : {}", r.freq, String::from_utf8_lossy(&r.sig));
                    let mut sigs: Vec<&[u8]> = c.iter().map(|p| &p.sig[..]).collect();
                    sigs.sort();
                    sigs.dedup();
                    for s in sigs.iter().take(4) {
                        println!("   pc ({} candidates, {} distinct sigs): {}", c.len(), sigs.len(), String::from_utf8_lossy(s));
                    }
                }
            }
        }
    }
    type KeyFn = fn(&Rec) -> Vec<u8>;
    let keys: [(&str, KeyFn); 5] = [
        ("stage + layout hashes (in order) + signatures", |r| {
            let mut v = vec![r.freq];
            r.hashes.iter().for_each(|h| v.extend_from_slice(&h.to_le_bytes()));
            v.extend_from_slice(&r.sig);
            v
        }),
        ("stage + layout hashes (sorted, non-zero) + signatures", |r| {
            let mut h: Vec<u32> = r.hashes.iter().copied().filter(|&x| x != 0).collect();
            h.sort();
            let mut v = vec![r.freq];
            h.iter().for_each(|h| v.extend_from_slice(&h.to_le_bytes()));
            v.extend_from_slice(&r.sig);
            v
        }),
        ("stage + whole header", |r| [&[r.freq][..], &r.head[..]].concat()),
        ("stage + layout hashes (in order)", |r| {
            let mut v = vec![r.freq];
            r.hashes.iter().for_each(|h| v.extend_from_slice(&h.to_le_bytes()));
            v
        }),
        ("stage + layout hashes (sorted, non-zero)", |r| {
            let mut h: Vec<u32> = r.hashes.iter().copied().filter(|&x| x != 0).collect();
            h.sort();
            let mut v = vec![r.freq];
            h.iter().for_each(|h| v.extend_from_slice(&h.to_le_bytes()));
            v
        }),
    ];
    for (name, kf) in keys {
        let mut m: HashMap<Vec<u8>, usize> = HashMap::new();
        for r in &pc {
            *m.entry(kf(r)).or_insert(0) += 1;
        }
        let mut hit = 0usize;
        let mut buckets = [0usize; 5]; // 1, 2-10, 11-100, 101-1000, >1000 candidates
        for r in &xb {
            if let Some(&n) = m.get(&kf(r)) {
                hit += 1;
                buckets[match n { 1 => 0, 2..=10 => 1, 11..=100 => 2, 101..=1000 => 3, _ => 4 }] += 1;
            }
        }
        println!("\nkey: {name}\n  xbox shaders with a PC candidate: {hit} of {} ({:.1}%)", xb.len(), 100.0 * hit as f64 / xb.len() as f64);
        println!("  candidates per hit: 1: {}  2-10: {}  11-100: {}  101-1000: {}  >1000: {}", buckets[0], buckets[1], buckets[2], buckets[3], buckets[4]);
    }
}
