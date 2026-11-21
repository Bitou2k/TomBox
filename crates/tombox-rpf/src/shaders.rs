//! Shader records in RAGE shader packages (`shaders.rpf`, RPF9), located without the
//! package's table of contents (which is encrypted).
//!
//! Observed layout of one record (one title, 26,503 records, every one validated by
//! inflating it to a `DXBC` container of exactly the stated size):
//!
//! ```text
//! u8   name length including the terminating NUL
//! [u8] name, NUL-terminated (e.g. "VS_BlitGenAmbCube_0cfdb358_Wrapped")
//! u8   tag (meaning unknown; values seen 0x20..0x42)
//! u32  compressed size
//! u32  unknown
//! u32  compressed size (repeated)
//! u32  inflated size
//! [u8] raw deflate stream (no zlib header) of `compressed size` bytes
//! ```
//!
//! The repeated size is what makes a false match unlikely; the inflate-and-check is what
//! makes it certain. Only records that pass are returned.

pub struct ShaderRecord {
    pub offset: usize,
    pub name: String,
    pub tag: u8,
    pub container: Vec<u8>,
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// Try to read a record whose length byte is at `p`.
fn record_at(b: &[u8], p: usize) -> Option<(ShaderRecord, usize)> {
    let len = *b.get(p)? as usize;
    if len < 4 {
        return None;
    }
    let name = b.get(p + 1..p + len)?;
    if b.get(p + len) != Some(&0) || !name.iter().all(|c| (0x20..0x7f).contains(c)) {
        return None;
    }
    let h = p + 1 + len;
    let tag = *b.get(h)?;
    let (c, c2, u) = (u32_at(b, h + 1)?, u32_at(b, h + 9)?, u32_at(b, h + 13)?);
    if c != c2 || c < 16 || c > 8 << 20 || u < 32 || u > 64 << 20 {
        return None;
    }
    let s = h + 17;
    let stream = b.get(s..s + c as usize)?;
    let out = miniz_oxide::inflate::decompress_to_vec_with_limit(stream, u as usize).ok()?;
    if out.len() != u as usize || &out[..4] != b"DXBC" || u32_at(&out, 24)? != u {
        return None;
    }
    Some((ShaderRecord { offset: p, name: String::from_utf8_lossy(name).into_owned(), tag, container: out }, s + c as usize))
}

/// Every shader record in `b`, in file order.
pub fn find_records(b: &[u8]) -> Vec<ShaderRecord> {
    let mut out = Vec::new();
    let mut p = 0;
    while p + 32 < b.len() {
        match record_at(b, p) {
            Some((r, next)) => {
                out.push(r);
                p = next;
            }
            None => p += 1,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container(n: u32) -> Vec<u8> {
        let mut d = vec![0u8; n as usize];
        d[..4].copy_from_slice(b"DXBC");
        d[24..28].copy_from_slice(&n.to_le_bytes());
        for (i, x) in d.iter_mut().enumerate().skip(32) {
            *x = (i * 7) as u8;
        }
        d
    }

    fn record(name: &str, body: &[u8]) -> Vec<u8> {
        let z = miniz_oxide::deflate::compress_to_vec(body, 6);
        let mut r = vec![name.len() as u8 + 1];
        r.extend_from_slice(name.as_bytes());
        r.push(0);
        r.push(0x32);
        for w in [z.len() as u32, 0xdead, z.len() as u32, body.len() as u32] {
            r.extend_from_slice(&w.to_le_bytes());
        }
        r.extend_from_slice(&z);
        r
    }

    #[test]
    fn finds_records_between_noise() {
        let mut f = vec![0x5au8; 100];
        f.extend(record("VS_Test_Wrapped", &container(300)));
        f.extend([0u8; 66]);
        f.extend(record("PS_Other", &container(1000)));
        f.extend([1u8; 40]);
        let r = find_records(&f);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].name, "VS_Test_Wrapped");
        assert_eq!(r[0].offset, 100);
        assert_eq!(r[1].container.len(), 1000);
    }

    #[test]
    fn rejects_wrong_sizes_and_truncation() {
        let full = record("VS_Test", &container(300));
        // Inflated size off by one: the container no longer matches, nothing is returned.
        let mut bad_size = full.clone();
        let u = 1 + 8 + 1 + 12; // length byte, name + NUL, tag, three u32
        bad_size[u] = bad_size[u].wrapping_add(1);
        assert!(find_records(&bad_size).is_empty());
        // A damaged stream either fails to inflate or still yields a container of the stated
        // size (the format has no checksum); it is never returned with another size.
        let mut bad_stream = full.clone();
        let last = bad_stream.len() - 1;
        bad_stream[last] ^= 0xff;
        assert!(find_records(&bad_stream).iter().all(|r| r.container.len() == 300));
        // Every truncation is handled without a panic, and cutting the stream loses the record.
        for n in 0..full.len() {
            assert!(find_records(&full[..n]).is_empty());
        }
    }
}
