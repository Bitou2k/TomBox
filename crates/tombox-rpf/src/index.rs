//! Mount index for the second title's level package: what each recovered file is called (the
//! resource names it carries) and the map placement records it holds. Built once by
//! `rpf-index`; a viewer then reads files on demand by offset from the original package.
//!
//! Placement record layout (0x90 bytes; see tools/rage/placements.py for the evidence):
//!   +0x04 u64 id, +0x0c f32x4 quaternion (inverse rotation), +0x34 f32x3 position,
//!   +0x40 u32 joaat(model name), +0x44 u32 flags, +0x50 f32 scale XY, +0x54 f32 scale Z,
//!   +0x58 u16 index, +0x5a u16 parent, +0x5c f32 draw distance.
//! Records are found by scanning with the same checks as placements.py (the map resource's
//! own header is not decoded yet).

pub const STRIDE: usize = 0x90;

/// Resource tag at +0x10 of the level resources that hold placements.
pub const RESOURCE_TAG: &[u8; 4] = b"0DRP";

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// Offset of the record in the inflated file.
    pub at: u32,
    pub id: u64,
    pub quat: [f32; 4],
    pub pos: [f32; 3],
    pub name: u32,
    pub flags: u32,
    pub scale: [f32; 2],
    pub index: u16,
    pub parent: u16,
    pub lod: f32,
}

fn f32_at(b: &[u8], o: usize) -> f32 {
    f32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

/// Cheap checks shared with the full parse: unit quaternion and plausible scales.
fn quick(b: &[u8], o: usize) -> bool {
    let q: [f32; 4] = std::array::from_fn(|k| f32_at(b, o + 0x0c + 4 * k));
    let n: f64 = q.iter().map(|&x| f64::from(x) * f64::from(x)).sum();
    let (sxy, sz) = (f32_at(b, o + 0x50), f32_at(b, o + 0x54));
    (n - 1.0).abs() < 2e-3 && sxy > 0.001 && sxy < 1000.0 && sz > 0.001 && sz < 1000.0
}

/// The record at `o`, if it passes every check.
pub fn record(b: &[u8], o: usize) -> Option<Placement> {
    if o + STRIDE > b.len() || !quick(b, o) {
        return None;
    }
    let q: [f32; 4] = std::array::from_fn(|k| f32_at(b, o + 0x0c + 4 * k));
    if !q.iter().all(|x| x.is_finite()) {
        return None;
    }
    let pos: [f32; 3] = std::array::from_fn(|k| f32_at(b, o + 0x34 + 4 * k));
    if !pos.iter().all(|x| x.is_finite() && x.abs() < 50000.0) {
        return None;
    }
    let lod = f32_at(b, o + 0x5c);
    if !lod.is_finite() {
        return None;
    }
    Some(Placement {
        at: o as u32,
        id: u64::from_le_bytes(b[o + 4..o + 12].try_into().unwrap()),
        quat: q,
        pos,
        name: u32_at(b, o + 0x40),
        flags: u32_at(b, o + 0x44),
        scale: [f32_at(b, o + 0x50), f32_at(b, o + 0x54)],
        index: u16::from_le_bytes([b[o + 0x58], b[o + 0x59]]),
        parent: u16::from_le_bytes([b[o + 0x5a], b[o + 0x5b]]),
        lod,
    })
}

/// Every run of placement records in a level resource: a record is accepted when it passes
/// [`record`] and the record before or after it does too; runs are then walked forward.
pub fn placements(b: &[u8]) -> Vec<Placement> {
    let mut out = Vec::new();
    if b.get(0x10..0x14) != Some(RESOURCE_TAG.as_slice()) {
        return out;
    }
    let mut o = 0usize;
    let mut covered = 0usize; // runs already walked end here
    while o + STRIDE <= b.len() {
        if o >= covered && quick(b, o) && record(b, o).is_some() {
            let prev = o >= STRIDE && record(b, o - STRIDE).is_some();
            if prev || record(b, o + STRIDE).is_some() {
                let mut k = o;
                while let Some(r) = record(b, k) {
                    out.push(r);
                    k += STRIDE;
                }
                covered = k;
            }
        }
        o += 4;
    }
    out
}

/// Resource names carried in a file: `name.#xx` strings, in file order. Drawable resources
/// name themselves `name.#dr` (fragments `name.#ft`); in about half of them the name lies past
/// the first 16 KiB. Most files carry one name; files of several low-detail models carry one
/// per model. Returned as (name, extension).
pub fn names(b: &[u8]) -> Vec<(String, String)> {
    let ok = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut out: Vec<(String, String)> = Vec::new();
    let mut i = 0;
    while i + 3 < b.len() {
        if b[i] == b'.' && b[i + 1] == b'#' && ok(b[i + 2]) && ok(b[i + 3]) {
            let mut s = i;
            while s > 0 && ok(b[s - 1]) {
                s -= 1;
            }
            if i - s >= 3 {
                let n = String::from_utf8_lossy(&b[s..i]).into_owned();
                let e = String::from_utf8_lossy(&b[i + 2..i + 4]).into_owned();
                if !out.iter().any(|(a, x)| *a == n && *x == e) {
                    out.push((n, e));
                }
            }
            i += 4;
        } else {
            i += 1;
        }
    }
    out
}

/// Jenkins one-at-a-time hash of the lower-cased name (RAGE's `joaat`).
pub fn joaat(s: &str) -> u32 {
    let mut h: u32 = 0;
    for c in s.bytes().map(|c| c.to_ascii_lowercase()) {
        h = h.wrapping_add(u32::from(c));
        h = h.wrapping_add(h << 10);
        h ^= h >> 6;
    }
    h = h.wrapping_add(h << 3);
    h ^= h >> 11;
    h.wrapping_add(h << 15)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(b: &mut [u8], o: usize, pos: [f32; 3], name: u32) {
        let q = [0.0f32, 0.0, 0.70710677, 0.70710677];
        for k in 0..4 {
            b[o + 0x0c + 4 * k..o + 0x10 + 4 * k].copy_from_slice(&q[k].to_le_bytes());
        }
        for k in 0..3 {
            b[o + 0x34 + 4 * k..o + 0x38 + 4 * k].copy_from_slice(&pos[k].to_le_bytes());
        }
        b[o + 0x40..o + 0x44].copy_from_slice(&name.to_le_bytes());
        b[o + 0x50..o + 0x54].copy_from_slice(&1.0f32.to_le_bytes());
        b[o + 0x54..o + 0x58].copy_from_slice(&2.0f32.to_le_bytes());
        b[o + 0x5a..o + 0x5c].copy_from_slice(&0xffffu16.to_le_bytes());
        b[o + 0x5c..o + 0x60].copy_from_slice(&(-1.0f32).to_le_bytes());
    }

    #[test]
    fn joaat_known_value() {
        // Widely published value for this algorithm on "a".
        assert_eq!(joaat("a"), 0xca2e9442);
        assert_eq!(joaat("ABC"), joaat("abc"));
    }

    #[test]
    fn finds_runs_but_not_lone_records_or_untagged_files() {
        let mut b = vec![0u8; 0x1000];
        b[0x10..0x14].copy_from_slice(RESOURCE_TAG);
        rec(&mut b, 0x64, [1.0, 2.0, 3.0], 7);
        rec(&mut b, 0x64 + STRIDE, [4.0, 5.0, 6.0], 8);
        rec(&mut b, 0x64 + 2 * STRIDE, [7.0, 8.0, 9.0], 9);
        rec(&mut b, 0x800, [1.0, 1.0, 1.0], 10); // alone: rejected
        let p = placements(&b);
        assert_eq!(p.iter().map(|r| r.name).collect::<Vec<_>>(), vec![7, 8, 9]);
        assert_eq!(p[1].pos, [4.0, 5.0, 6.0]);
        assert_eq!(p[0].scale, [1.0, 2.0]);
        assert_eq!(p[0].parent, 0xffff);
        b[0x10] = 0;
        assert!(placements(&b).is_empty());
    }

    #[test]
    fn names_from_resource_strings() {
        let mut b = b"\0\0lamp_post_01.#dr\0junk.#\0ab.#dr\0x_fence.#td".to_vec();
        b.extend([0u8; 10]);
        assert_eq!(names(&b), vec![("lamp_post_01".into(), "dr".into()), ("x_fence".into(), "td".into())]);
    }
}
