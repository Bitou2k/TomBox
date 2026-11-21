//! Carving: find the compressed streams in a package's data area without its (encrypted)
//! table of contents, inflate them and classify the result by its leading bytes.
//!
//! Observed in the second title's packages: data blocks are zlib streams (`78 DA`) holding
//! XML text, resources with a `PSIN` header, and nested RPF9 packages; nothing there is
//! encrypted except the table of contents. File names are not recoverable this way (they are
//! in the encrypted table), so carved files are named by their offset.

/// One carved stream.
pub struct Carved {
    pub offset: u64,
    /// Compressed length consumed.
    pub packed: usize,
    pub data: Vec<u8>,
}

/// Classification of carved data: (kind, file extension).
pub fn classify(d: &[u8]) -> (&'static str, &'static str) {
    let t = d.strip_prefix(b"\xef\xbb\xbf").unwrap_or(d);
    let t = { let mut k = 0; while k < t.len().min(64) && t[k].is_ascii_whitespace() { k += 1; } &t[k..] };
    if t.starts_with(b"<?xml") || t.starts_with(b"<") && t.len() > 1 && t[1].is_ascii_alphabetic() {
        return ("xml", "xml");
    }
    match d.get(..4) {
        Some(b"PSIN") => ("resource (PSIN)", "psin"),
        Some(b"9FPR") => ("nested RPF9 package", "rpf"),
        Some(b"DDS ") => ("DDS texture", "dds"),
        Some(b"RIFF") => ("RIFF", "riff"),
        Some(b"DXBC") => ("shader container", "dxbc"),
        Some(b"RBF0") => ("RAGE binary format (RBF)", "rbf"),
        Some(b"GFX*") | Some(b"CFX*") => ("Scaleform GFx movie", "gfx"),
        _ => {
            let text = d.iter().take(512).filter(|&&c| c == b'\n' || c == b'\r' || c == b'\t' || (0x20..0x7f).contains(&c)).count();
            if d.len() >= 16 && text * 100 >= d.len().min(512) * 97 { ("text", "txt") } else { ("unknown binary", "bin") }
        }
    }
}

/// Inflate every zlib stream that starts in `buf` (positions are reported as `base + i`).
/// A candidate must have a valid zlib header and inflate to completion within `limit` bytes.
pub fn carve(buf: &[u8], base: u64, limit: usize, mut each: impl FnMut(Carved)) {
    let mut i = 0usize;
    while i + 2 <= buf.len() {
        let (cmf, flg) = (buf[i], buf[i + 1]);
        let header = cmf == 0x78 && (u16::from(cmf) << 8 | u16::from(flg)) % 31 == 0 && flg & 0x20 == 0;
        if !header {
            i += 1;
            continue;
        }
        let mut st = miniz_oxide::inflate::stream::InflateState::new_boxed(miniz_oxide::DataFormat::Zlib);
        let mut out = vec![0u8; 0];
        let mut chunk = vec![0u8; 1 << 16];
        let mut pos = i;
        let mut ok = false;
        loop {
            let r = miniz_oxide::inflate::stream::inflate(&mut st, &buf[pos..], &mut chunk, miniz_oxide::MZFlush::None);
            pos += r.bytes_consumed;
            out.extend_from_slice(&chunk[..r.bytes_written]);
            match r.status {
                Ok(miniz_oxide::MZStatus::StreamEnd) => {
                    ok = true;
                    break;
                }
                Ok(_) if out.len() <= limit && (r.bytes_consumed > 0 || r.bytes_written > 0) => continue,
                _ => break,
            }
        }
        if ok && out.len() >= 8 {
            each(Carved { offset: base + i as u64, packed: pos - i, data: out });
            i = pos;
        } else {
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carves_streams_between_noise_and_classifies() {
        let xml = b"<?xml version=\"1.0\"?><a>hello</a>".to_vec();
        let mut psin = b"PSIN".to_vec();
        psin.extend([7u8; 300]);
        let mut f = vec![0x5au8; 50];
        f.extend(miniz_oxide::deflate::compress_to_vec_zlib(&xml, 9));
        f.extend([0u8; 30]);
        f.extend(miniz_oxide::deflate::compress_to_vec_zlib(&psin, 9));
        f.extend([0x78u8, 0xda, 1, 2, 3]); // a header with garbage behind it
        let mut got = vec![];
        carve(&f, 1000, 1 << 20, |c| got.push(c));
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].offset, 1050);
        assert_eq!(classify(&got[0].data).1, "xml");
        assert_eq!(classify(&got[1].data).1, "psin");
    }
}
