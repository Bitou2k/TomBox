//! Binding trace (diagnostic, only with the render-target capture enabled).
//!
//! Records every descriptor the title writes (constant-buffer, shader-resource,
//! unordered-access views and samplers, including copies between heaps) and the descriptor
//! tables bound on each command list, so that a traced draw can be reported with what its
//! shaders actually see: the bytes of each constant buffer, and which resource every view
//! refers to (with the view's own stride/format).
//!
//! Limits: constant-buffer contents are read from upload-heap buffers only (the title maps
//! those itself; a default-heap buffer has no CPU-visible memory). A slot is reported only
//! if its descriptor was written during the current or the previous frame, because the
//! title leaves unused slots of a table untouched and stale views from an older draw would
//! otherwise be listed.

use core::ffi::c_void;
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

#[derive(Clone)]
pub enum Desc {
    Cbv { va: u64, size: u32 },
    Srv { res: usize, desc: Option<[u8; 40]> },
    Uav { res: usize, desc: Option<[u8; 40]> },
    Sampler { desc: [u8; 0x34] },
}

#[derive(Clone)]
struct Entry {
    d: Desc,
    frame: u32,
}

struct Heap {
    cpu: usize,
    gpu: u64,
    count: usize,
    inc: usize,
}

struct Buf {
    size: u64,
    res: usize,
    /// D3D12_HEAP_TYPE (1 default, 2 upload, 3 readback); 0 = unknown.
    heap_type: u32,
    mapped: usize,
}

#[derive(Default)]
struct State {
    descs: HashMap<usize, Entry>,
    heaps: Vec<Heap>,
    /// descriptor increment per D3D12_DESCRIPTOR_HEAP_TYPE
    inc: [usize; 4],
    buffers: BTreeMap<u64, Buf>,
    /// heap object -> D3D12_HEAP_TYPE, for buffers placed in heaps
    heap_types: HashMap<usize, u32>,
    /// (list, compute?) -> root parameter index -> GPU descriptor handle
    tables: HashMap<(usize, bool), Vec<u64>>,
    frame: u32,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap();
    f(g.get_or_insert_with(State::default))
}

unsafe fn vt(obj: *mut c_void) -> *const usize {
    *(obj as *const *const usize)
}

pub fn set_increment(heap_type: u32, inc: u32) {
    with(|s| {
        if (heap_type as usize) < 4 {
            s.inc[heap_type as usize] = inc as usize;
        }
    });
}

pub fn increment(heap_type: u32) -> usize {
    with(|s| s.inc.get(heap_type as usize).copied().unwrap_or(0))
}

pub fn note_heap(cpu: usize, gpu: u64, count: u32, heap_type: u32) {
    with(|s| {
        let inc = s.inc.get(heap_type as usize).copied().unwrap_or(0);
        s.heaps.retain(|h| h.cpu != cpu);
        s.heaps.push(Heap { cpu, gpu, count: count as usize, inc });
    });
}

pub fn note_desc(dest: usize, d: Desc) {
    with(|s| {
        let frame = s.frame;
        s.descs.insert(dest, Entry { d, frame });
    });
}

/// Mirror `CopyDescriptorsSimple` / one range pair of `CopyDescriptors`.
pub fn copy_desc(dest: usize, src: usize, n: u32, heap_type: u32) {
    with(|s| {
        let inc = s.inc.get(heap_type as usize).copied().unwrap_or(0);
        if inc == 0 {
            return;
        }
        let frame = s.frame;
        for i in 0..n as usize {
            match s.descs.get(&(src + i * inc)).cloned() {
                Some(mut e) => {
                    e.frame = frame;
                    s.descs.insert(dest + i * inc, e);
                }
                None => {
                    s.descs.remove(&(dest + i * inc));
                }
            }
        }
    });
}

pub fn note_heap_type(heap: usize, heap_type: u32) {
    with(|s| {
        s.heap_types.insert(heap, heap_type);
    });
}
pub fn heap_type_of(heap: usize) -> u32 {
    with(|s| s.heap_types.get(&heap).copied().unwrap_or(0))
}

/// Remember a buffer resource (takes one reference so the pointer stays valid; released by
/// `end_frame` once the title has dropped the buffer).
pub unsafe fn note_buffer(res: *mut c_void, size: u64, heap_type: u32) {
    if res.is_null() {
        return;
    }
    let get_va: unsafe extern "system" fn(*mut c_void) -> u64 = core::mem::transmute(*vt(res).add(11));
    let va = get_va(res);
    if va == 0 {
        return;
    }
    let addref: unsafe extern "system" fn(*mut c_void) -> u32 = core::mem::transmute(*vt(res).add(1));
    addref(res);
    let old = with(|s| s.buffers.insert(va, Buf { size, res: res as usize, heap_type, mapped: 0 }));
    if let Some(b) = old {
        let release: unsafe extern "system" fn(*mut c_void) -> u32 = core::mem::transmute(*vt(b.res as *mut c_void).add(2));
        release(b.res as *mut c_void);
    }
}

pub fn set_table(list: usize, compute: bool, index: u32, handle: u64) {
    with(|s| {
        let v = s.tables.entry((list, compute)).or_default();
        if (index as usize) < 64 {
            if v.len() <= index as usize {
                v.resize(index as usize + 1, 0);
            }
            v[index as usize] = handle;
        }
    });
}

/// Called at every present: advances the frame counter and, now and then, drops buffers the
/// title has released (our reference is the only one left).
pub unsafe fn end_frame() {
    let dead: Vec<usize> = with(|s| {
        s.frame = s.frame.wrapping_add(1);
        if s.frame % 64 != 0 {
            return Vec::new();
        }
        let mut dead = Vec::new();
        s.buffers.retain(|_, b| {
            let r = b.res as *mut c_void;
            let addref: unsafe extern "system" fn(*mut c_void) -> u32 = core::mem::transmute(*vt(r).add(1));
            let release: unsafe extern "system" fn(*mut c_void) -> u32 = core::mem::transmute(*vt(r).add(2));
            addref(r);
            if release(r) == 1 {
                dead.push(b.res);
                false
            } else {
                true
            }
        });
        // Descriptor records for heaps are kept; only very old ones are dropped.
        if s.frame % 1024 == 0 {
            let f = s.frame;
            s.descs.retain(|_, e| f.wrapping_sub(e.frame) < 4096);
        }
        dead
    });
    for r in dead {
        let r = r as *mut c_void;
        let release: unsafe extern "system" fn(*mut c_void) -> u32 = core::mem::transmute(*vt(r).add(2));
        release(r);
    }
}

fn fmt_words(bytes: &[u8]) -> String {
    let mut out = String::new();
    for (i, row) in bytes.chunks(16).enumerate() {
        out.push_str(&format!("      [{i:3}]"));
        for w in row.chunks(4) {
            if w.len() < 4 {
                break;
            }
            let u = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
            let f = f32::from_bits(u);
            // Show as float when it looks like one, else as hex (integers, bit patterns).
            if u == 0 {
                out.push_str("            0");
            } else if f.is_finite() && f.abs() >= 1e-6 && f.abs() < 1e9 {
                out.push_str(&format!(" {f:12.6}"));
            } else {
                out.push_str(&format!("   0x{u:08x}"));
            }
        }
        out.push('\n');
    }
    out
}

/// Text describing every table bound on `list` (graphics or compute).
pub unsafe fn dump(list: usize, compute: bool, name: &dyn Fn(usize) -> String) -> String {
    // Collect under the lock, read memory after it (Map may take driver locks).
    struct Item {
        param: usize,
        slot: usize,
        /// None: no record written this frame (`age` frames old, or never recorded).
        d: Option<Desc>,
        age: u32,
    }
    let (items, frame) = with(|s| {
        let mut items = Vec::new();
        let Some(tables) = s.tables.get(&(list, compute)) else { return (items, s.frame) };
        for (param, &h) in tables.iter().enumerate() {
            if h == 0 {
                continue;
            }
            let Some(heap) = s.heaps.iter().find(|x| x.gpu != 0 && x.inc != 0 && h >= x.gpu && h < x.gpu + (x.count * x.inc) as u64) else { continue };
            let first = (h - heap.gpu) as usize / heap.inc;
            // A table ends where the next table bound on this list begins (the title
            // allocates them back to back), or after 64 slots.
            let next = tables.iter().filter(|&&o| o > h && o < heap.gpu + (heap.count * heap.inc) as u64).min().copied();
            let extent = next.map(|o| (o - h) as usize / heap.inc).unwrap_or(64).min(64);
            for slot in 0..extent {
                if first + slot >= heap.count {
                    break;
                }
                match s.descs.get(&(heap.cpu + (first + slot) * heap.inc)) {
                    Some(e) if s.frame.wrapping_sub(e.frame) <= 1 => items.push(Item { param, slot, d: Some(e.d.clone()), age: 0 }),
                    Some(e) => items.push(Item { param, slot, d: None, age: s.frame.wrapping_sub(e.frame) }),
                    None => items.push(Item { param, slot, d: None, age: u32::MAX }),
                }
            }
        }
        (items, s.frame)
    });
    let mut out = format!("  bindings ({} list {list:#x}, frame {frame}):\n", if compute { "compute" } else { "graphics" });
    let mut shown: Vec<(u64, u32)> = Vec::new();
    for it in items {
        let Some(d) = it.d.clone() else {
            if it.age == u32::MAX {
                out.push_str(&format!("    param[{}]+{} (no descriptor recorded)
", it.param, it.slot));
            }
            continue;
        };
        match d {
            Desc::Cbv { va, size } => {
                out.push_str(&format!("    param[{}]+{} CBV va={va:#x} size={size}", it.param, it.slot));
                if shown.contains(&(va, size)) {
                    out.push_str(" (contents shown above)
");
                    continue;
                }
                shown.push((va, size));
                let found = with(|s| {
                    s.buffers.range(..=va).next_back().filter(|(base, b)| va < **base + b.size).map(|(base, b)| (*base, b.res, b.heap_type, b.mapped, b.size))
                });
                match found {
                    Some((base, res, heap_type, mut mapped, bsize)) => {
                        if mapped == 0 && heap_type == 2 {
                            let map: unsafe extern "system" fn(*mut c_void, u32, *const c_void, *mut *mut u8) -> i32 = core::mem::transmute(*vt(res as *mut c_void).add(8));
                            let mut p: *mut u8 = core::ptr::null_mut();
                            if map(res as *mut c_void, 0, core::ptr::null(), &mut p) >= 0 && !p.is_null() {
                                mapped = p as usize;
                                with(|s| {
                                    if let Some(b) = s.buffers.get_mut(&base) {
                                        b.mapped = mapped;
                                    }
                                });
                            }
                        }
                        if mapped != 0 {
                            let off = (va - base) as usize;
                            let n = (size as usize).min((bsize as usize).saturating_sub(off)).min(8192);
                            let bytes = core::slice::from_raw_parts((mapped + off) as *const u8, n);
                            out.push_str(&format!(" (buffer {res:#x}+{off:#x})\n"));
                            out.push_str(&fmt_words(bytes));
                        } else {
                            out.push_str(&format!(" (buffer {res:#x} heapType={heap_type}: not CPU-readable)\n"));
                        }
                    }
                    None => out.push_str(" (no known buffer at that address)\n"),
                }
            }
            Desc::Srv { res, desc } | Desc::Uav { res, desc } => {
                let is_srv = matches!(it.d, Some(Desc::Srv { .. }));
                let kind = if is_srv { "SRV" } else { "UAV" };
                let view = match desc {
                    Some(d) => {
                        let u = |o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap());
                        let q = |o: usize| u64::from_le_bytes(d[o..o + 8].try_into().unwrap());
                        if is_srv {
                            // D3D12_SHADER_RESOURCE_VIEW_DESC: Format, ViewDimension, Mapping, union@16
                            match u(4) {
                                1 => format!("fmt={} BUFFER first={} n={} stride={} flags={:#x}", u(0), q(16), u(24), u(28), u(32)),
                                dim => format!("fmt={} dim={dim} map={:#x} mip0={} mips={} a={} b={}", u(0), u(8), u(16), u(20) as i32, u(24), u(28)),
                            }
                        } else {
                            // D3D12_UNORDERED_ACCESS_VIEW_DESC: Format, ViewDimension, union@8
                            match u(4) {
                                1 => format!("fmt={} BUFFER first={} n={} stride={} counterOff={} flags={:#x}", u(0), q(8), u(16), u(20), q(24), u(32)),
                                dim => format!("fmt={} dim={dim} mip={} a={} b={}", u(0), u(8), u(12), u(16)),
                            }
                        }
                    }
                    None => "default view".to_string(),
                };
                let what = if res == 0 {
                    "null resource".to_string()
                } else {
                    let buf = with(|s| s.buffers.values().find(|b| b.res == res).map(|b| (b.size, b.heap_type)));
                    match buf {
                        Some((size, ht)) => format!("buffer {res:#x} size={size} heapType={ht}"),
                        None => name(res),
                    }
                };
                out.push_str(&format!("    param[{}]+{} {kind} {what} | {view}\n", it.param, it.slot));
            }
            Desc::Sampler { desc } => {
                let u = |o: usize| u32::from_le_bytes(desc[o..o + 4].try_into().unwrap());
                let f = |o: usize| f32::from_bits(u(o));
                out.push_str(&format!(
                    "    param[{}]+{} SAMPLER filter={:#x} addr={}/{}/{} bias={} aniso={} cmp={} lod=[{},{}]\n",
                    it.param, it.slot, u(0), u(4), u(8), u(12), f(0x10), u(0x14), u(0x18), f(0x2c), f(0x30)));
            }
        }
    }
    out
}
