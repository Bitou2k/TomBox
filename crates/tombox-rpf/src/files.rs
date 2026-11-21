//! Files in a package's data area, recovered without the (encrypted) table of contents.
//!
//! A file is one zlib stream, or several consecutive streams when it is longer than 64 KiB:
//! observed in the second title's packages, an XML file continues in the stream that starts
//! right after one that inflated to exactly 65,536 bytes. [`each_file`] reads a byte range of
//! a package in windows, carves the streams (see [`crate::carve`]) and joins continuations.

use std::io::{Read, Seek, SeekFrom};

/// One recovered file.
pub struct File {
    /// Package offset of its first stream.
    pub offset: u64,
    /// Package offset just past its last stream.
    pub end: u64,
    /// Number of streams joined.
    pub streams: u32,
    pub data: Vec<u8>,
}

const WIN: u64 = 64 << 20;
/// Bytes read past a window so a stream starting near its end can complete. A single stream
/// inflates to at most 64 KiB in the packages seen, so its packed size is far below this.
const OVERLAP: u64 = 16 << 20;
/// Largest inflated stream accepted.
pub const STREAM_LIMIT: usize = 256 << 20;

fn joins(p: &File, offset: u64) -> bool {
    p.data.len() % 65536 == 0 && offset >= p.end && offset - p.end < 16
}

/// Call `each` for every file whose first stream starts in `[start, end)`. A file that starts
/// before `end` is completed even when its continuation streams lie past it, so adjacent
/// ranges can be processed independently: the next range drops files starting before the
/// previous range's last `end` (see [`File::end`]).
pub fn each_file<F: Read + Seek>(f: &mut F, file_len: u64, start: u64, end: u64, mut progress: impl FnMut(u64), mut each: impl FnMut(File)) -> std::io::Result<()> {
    let end = end.min(file_len);
    let mut base = start;
    let mut next_start = start;
    let mut pending: Option<File> = None;
    let mut buf = Vec::new();
    while base < file_len {
        // Past the range only a pending file's continuation is wanted: read small windows.
        let win = if base >= end { 1 << 20 } else { WIN.min(end - base).max(1 << 20) };
        let len = (win + OVERLAP).min(file_len - base);
        buf.resize(len as usize, 0);
        f.seek(SeekFrom::Start(base))?;
        f.read_exact(&mut buf)?;
        let scan_end = win.min(len);
        let from = next_start.saturating_sub(base).min(scan_end) as usize;
        let mut stop = false;
        crate::carve::carve_range(&buf[from..], base + from as u64, STREAM_LIMIT, scan_end as usize - from, |c| {
            if stop || c.offset < next_start {
                return;
            }
            if let Some(p) = pending.as_mut().filter(|p| joins(p, c.offset)) {
                p.end = c.offset + c.packed as u64;
                p.streams += 1;
                p.data.extend_from_slice(&c.data);
                next_start = p.end;
                return;
            }
            if c.offset >= end {
                stop = true;
                return;
            }
            next_start = c.offset + c.packed as u64;
            if let Some(p) = pending.take() {
                each(p);
            }
            pending = Some(File { offset: c.offset, end: next_start, streams: 1, data: c.data });
        });
        base += scan_end;
        progress(base.min(end));
        if base >= end && (stop || pending.as_ref().map_or(true, |p| p.data.len() % 65536 != 0 || p.end + 16 < base)) {
            break;
        }
    }
    if let Some(p) = pending.take() {
        each(p);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn z(d: &[u8]) -> Vec<u8> {
        miniz_oxide::deflate::compress_to_vec_zlib(d, 6)
    }

    #[test]
    fn joins_64k_streams_and_completes_past_range_end() {
        let big: Vec<u8> = (0..65536u32).map(|i| (i % 253) as u8).collect();
        let tail = b"tail of the long file".to_vec();
        let other = b"a separate small file".to_vec();
        let mut pkg = vec![0u8; 3];
        pkg.extend(z(&big));
        let second = pkg.len() as u64;
        pkg.extend(z(&tail));
        pkg.extend([0u8; 5]);
        let third = pkg.len() as u64;
        pkg.extend(z(&other));
        let len = pkg.len() as u64;

        // Range covering only the first stream: the file is completed with its continuation,
        // and the separate file after it is not reported.
        let mut got = vec![];
        each_file(&mut std::io::Cursor::new(&pkg), len, 0, second, |_| {}, |f| got.push(f)).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].streams, 2);
        assert_eq!(got[0].data.len(), 65536 + tail.len());
        assert_eq!(got[0].end, third - 5);

        // The next range starts inside the joined file; the caller drops what starts before
        // the previous range's last end.
        let mut got2 = vec![];
        each_file(&mut std::io::Cursor::new(&pkg), len, second, len, |_| {}, |f| got2.push(f)).unwrap();
        let kept: Vec<_> = got2.iter().filter(|f| f.offset >= got[0].end).collect();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].offset, third);
        assert_eq!(kept[0].data, other);
    }
}
