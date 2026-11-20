//! Inspector for RAGE package files (`.rpf`).
//!
//! What is established and what is not:
//!
//! * **RPF7** (documented by the modding community for an earlier RAGE title): a 16-byte
//!   header `magic "RPF7" (stored little-endian, so the bytes read "7FPR"), entry count,
//!   names length, encryption tag`, followed by 16-byte entries and the name table. The tag
//!   is `OPEN` (plain), or one of two values meaning the table is encrypted.
//! * **RPF9**: observed in exactly one file (8,880 bytes). Its first 0x40 bytes are plain;
//!   everything after is either high-entropy data or zero padding. The header fields are
//!   reported by offset. Names given to them below are guesses and are marked as such.
//!
//! This crate reads structure only. Package contents are encrypted with keys held by the
//! title, and recovering or applying those keys is outside this project's scope: the title
//! decrypts its own packages when it runs.
//!
//! Input is untrusted: every read is bounds-checked and nothing is allocated from sizes found
//! in the file.

/// A contiguous part of the file with one character.
#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    pub start: usize,
    pub end: usize,
    pub kind: RegionKind,
    /// Shannon entropy in bits per byte (0 for an empty region).
    pub entropy: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionKind {
    Header,
    /// All zero bytes.
    Padding,
    /// Entropy of 7.2 bits/byte or more over the region: encrypted or compressed.
    HighEntropy,
    /// Anything else: plain structured data.
    Plain,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Header {
    V7 { entry_count: u32, names_length: u32, encryption: u32 },
    /// The sixteen little-endian dwords of the 0x40-byte header, verbatim.
    V9 { words: [u32; 16] },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rpf {
    pub version: u8,
    pub size: usize,
    pub header: Header,
    pub regions: Vec<Region>,
}

pub const RPF7_OPEN: u32 = 0x4e45_504f; // "OPEN"

fn u32_at(b: &[u8], o: usize) -> Result<u32, String> {
    let s = b.get(o..o + 4).ok_or_else(|| format!("truncated: need 4 bytes at {o:#x}, file is {:#x}", b.len()))?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

pub fn entropy(b: &[u8]) -> f64 {
    if b.is_empty() {
        return 0.0;
    }
    let mut c = [0usize; 256];
    for &x in b {
        c[x as usize] += 1;
    }
    let n = b.len() as f64;
    let e: f64 = c.iter().filter(|&&v| v > 0).map(|&v| { let p = v as f64 / n; -p * p.log2() }).sum();
    e.max(0.0)
}

/// Split `b[from..]` into zero padding and non-zero runs. A zero run counts as padding only
/// from 64 bytes up, so zeros inside data do not split it.
fn map_regions(b: &[u8], from: usize) -> Vec<Region> {
    const MIN_PAD: usize = 64;
    let mut out = Vec::new();
    let mut i = from;
    let mut run_start = from;
    while i < b.len() {
        if b[i] == 0 {
            let z = i;
            while i < b.len() && b[i] == 0 {
                i += 1;
            }
            if i - z >= MIN_PAD {
                if z > run_start {
                    out.push((run_start, z, false));
                }
                out.push((z, i, true));
                run_start = i;
            }
        } else {
            i += 1;
        }
    }
    if b.len() > run_start {
        out.push((run_start, b.len(), false));
    }
    out.into_iter()
        .map(|(start, end, pad)| {
            let e = entropy(&b[start..end]);
            // A short region cannot reach 8 bits/byte: scale the bar by what its length allows.
            let ceiling = ((end - start) as f64).log2().min(8.0);
            let kind = if pad { RegionKind::Padding } else if e >= 0.9 * ceiling { RegionKind::HighEntropy } else { RegionKind::Plain };
            Region { start, end, kind, entropy: e }
        })
        .collect()
}

/// Parse the header and map the rest of the file.
pub fn inspect(b: &[u8]) -> Result<Rpf, String> {
    let magic = u32_at(b, 0)?;
    if magic >> 8 != 0x0052_5046 {
        return Err(format!("not an RPF file: magic {magic:#010x} (expected \"RPF\" + version digit)"));
    }
    let digit = (magic & 0xff) as u8;
    let (header, header_len) = match digit {
        b'7' => (Header::V7 { entry_count: u32_at(b, 4)?, names_length: u32_at(b, 8)?, encryption: u32_at(b, 12)? }, 16),
        b'9' => {
            let mut words = [0u32; 16];
            for (i, w) in words.iter_mut().enumerate() {
                *w = u32_at(b, i * 4)?;
            }
            (Header::V9 { words }, 0x40)
        }
        d if d.is_ascii_digit() => return Err(format!("RPF version {} is not supported (known: 7, 9)", d as char)),
        d => return Err(format!("not an RPF file: version byte {d:#04x}")),
    };
    let mut regions = vec![Region { start: 0, end: header_len, kind: RegionKind::Header, entropy: entropy(&b[..header_len]) }];
    regions.extend(map_regions(b, header_len));
    Ok(Rpf { version: digit - b'0', size: b.len(), header, regions })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, mut s: u32) -> Vec<u8> {
        (0..n).map(|_| { s ^= s << 13; s ^= s >> 17; s ^= s << 5; (s >> 8) as u8 }).collect()
    }

    #[test]
    fn rpf7_open_header() {
        let mut b = Vec::new();
        for w in [0x5250_4637u32, 3, 40, RPF7_OPEN] {
            b.extend_from_slice(&w.to_le_bytes());
        }
        b.extend_from_slice(&[1u8; 48]);
        let r = inspect(&b).unwrap();
        assert_eq!(r.version, 7);
        assert_eq!(r.header, Header::V7 { entry_count: 3, names_length: 40, encryption: RPF7_OPEN });
        assert_eq!(r.regions[1].kind, RegionKind::Plain);
    }

    #[test]
    fn rpf9_regions_are_mapped() {
        let mut b = vec![0u8; 0x40];
        b[..4].copy_from_slice(&0x5250_4639u32.to_le_bytes());
        b[4..8].copy_from_slice(&0x360u32.to_le_bytes());
        b.extend(noise(0x510, 7));
        b.resize(0x1000, 0);
        b.extend(noise(0x12b0, 99));
        let r = inspect(&b).unwrap();
        assert_eq!(r.version, 9);
        let kinds: Vec<_> = r.regions.iter().map(|x| (x.start, x.end, x.kind)).collect();
        assert_eq!(kinds, vec![
            (0, 0x40, RegionKind::Header),
            (0x40, 0x550, RegionKind::HighEntropy),
            (0x550, 0x1000, RegionKind::Padding),
            (0x1000, 0x22b0, RegionKind::HighEntropy),
        ]);
        match r.header {
            Header::V9 { words } => assert_eq!(words[1], 0x360),
            _ => panic!(),
        }
    }

    #[test]
    fn rejects_other_files_and_truncation() {
        assert!(inspect(b"PK\x03\x04aaaaaaaaaaaa").is_err());
        assert!(inspect(b"9F").is_err());
        assert!(inspect(b"9FPR\x00\x00").unwrap_err().contains("truncated"));
        assert!(inspect(b"8FPRxxxxxxxxxxxxxxxx").unwrap_err().contains("not supported"));
        // every prefix of a valid file either parses or fails cleanly
        let mut b = vec![0u8; 0x40];
        b[..4].copy_from_slice(&0x5250_4639u32.to_le_bytes());
        b.extend(noise(300, 3));
        for n in 0..b.len() {
            let _ = inspect(&b[..n]);
        }
    }
}
