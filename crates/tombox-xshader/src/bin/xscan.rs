//! xscan: run the extractor + RDNA2→HLSL translator over every Xbox shader container found
//! in a file (e.g. a .pak holding a ShaderArchive) and report coverage.
//!
//! Usage: xscan <file> [--examples <dir>] [--limit <n>] [--jobs <n>] [--compile 1]
//!
//! `--compile 1` also compiles every translated shader (DXC by default, `--compiler fxc` for
//! fxc.exe; 4 jobs at below-normal priority unless `--jobs` is given) and stores the
//! bytecode in the runtime shader cache, so a full sweep both proves the HLSL compiles and
//! pre-warms the cache the game reads.
//!
//! Finds `DXBC` containers by magic + validated header, de-duplicates by content hash,
//! translates each, and prints per-stage totals plus a histogram of failure reasons
//! (offsets normalised). With --examples, one sample blob per distinct failure is saved.

use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Seek};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use tombox_xshader::{extract, hlsl, Stage};
use tombox_xshader::psocache::{self, BindingLayout, PsoCache, UserDataBinding, UserDataKind};

static PSO_CACHE: std::sync::OnceLock<PsoCache> = std::sync::OnceLock::new();
static WORKER_BINDINGS: std::sync::OnceLock<Vec<BindingLayout>> = std::sync::OnceLock::new();

fn layouts_for(blob: &[u8]) -> &[BindingLayout] {
    if let Some(layouts) = WORKER_BINDINGS.get() {
        return layouts;
    }
    psocache::shader_hash(blob).and_then(|hash| PSO_CACHE.get().map(|cache| cache.bindings(&hash))).unwrap_or(&[])
}

fn encode_bindings(layouts: &[BindingLayout]) -> Vec<u8> {
    let mut words = vec![layouts.len() as u32];
    for layout in layouts {
        words.extend([layout.metadata_index, layout.entries.len() as u32]);
        for entry in &layout.entries {
            let kind = match entry.kind {
                UserDataKind::ShaderResource => 0x300,
                UserDataKind::ConstantBuffer => 0x302,
                UserDataKind::UnorderedAccess => 0x303,
                UserDataKind::Sampler => 0x401,
                UserDataKind::Other(value) => value,
            };
            words.extend([kind, entry.byte_offset, entry.register]);
            words.extend(entry.raw_fields);
        }
    }
    words.into_iter().flat_map(u32::to_le_bytes).collect()
}

fn decode_bindings(bytes: &[u8]) -> Result<Vec<BindingLayout>, String> {
    if bytes.len() > 16 * 1024 * 1024 || bytes.len() % 4 != 0 {
        return Err("invalid worker binding data length".into());
    }
    let mut words = bytes.chunks_exact(4).map(|word| u32::from_le_bytes(word.try_into().unwrap()));
    let mut next = || words.next().ok_or("truncated worker binding data");
    let count = next()?;
    if count > 4096 {
        return Err("worker binding layout count exceeds limit".into());
    }
    let mut layouts = Vec::new();
    for _ in 0..count {
        let metadata_index = next()?;
        let count = next()?;
        if count > 4096 {
            return Err("worker binding entry count exceeds limit".into());
        }
        let mut entries = Vec::new();
        for _ in 0..count {
            let kind = match next()? {
                0x300 => UserDataKind::ShaderResource,
                0x302 => UserDataKind::ConstantBuffer,
                0x303 => UserDataKind::UnorderedAccess,
                0x401 => UserDataKind::Sampler,
                other => UserDataKind::Other(other),
            };
            entries.push(UserDataBinding {
                kind, byte_offset: next()?, register: next()?,
                raw_fields: [next()?, next()?, next()?, next()?],
            });
        }
        layouts.push(BindingLayout { metadata_index, entries });
    }
    if next().is_ok() {
        return Err("unexpected trailing worker binding data".into());
    }
    Ok(layouts)
}

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

// ---- optional FXC compile (Windows) -------------------------------------------------------------

#[cfg(windows)]
mod fxc {
    use std::ffi::c_void;
    use std::sync::OnceLock;
    extern "system" {
        fn LoadLibraryA(n: *const u8) -> *mut c_void;
        fn GetProcAddress(h: *mut c_void, n: *const u8) -> *mut c_void;
    }
    type D3DCompileFn = unsafe extern "system" fn(
        *const c_void, usize, *const u8, *const c_void, *mut c_void, *const u8, *const u8, u32, u32,
        *mut *mut c_void, *mut *mut c_void,
    ) -> i32;
    fn func() -> Option<D3DCompileFn> {
        static F: OnceLock<usize> = OnceLock::new();
        let p = *F.get_or_init(|| unsafe {
            let h = LoadLibraryA(b"d3dcompiler_47.dll\0".as_ptr());
            if h.is_null() { 0 } else { GetProcAddress(h, b"D3DCompile\0".as_ptr()) as usize }
        });
        if p == 0 { None } else { Some(unsafe { std::mem::transmute::<usize, D3DCompileFn>(p) }) }
    }
    unsafe fn take(blob: *mut c_void) -> Vec<u8> {
        if blob.is_null() {
            return Vec::new();
        }
        let vt = *(blob as *const *const usize);
        let ptr: unsafe extern "system" fn(*mut c_void) -> *const u8 = std::mem::transmute(*vt.add(3));
        let len: unsafe extern "system" fn(*mut c_void) -> usize = std::mem::transmute(*vt.add(4));
        let rel: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(*vt.add(2));
        let v = std::slice::from_raw_parts(ptr(blob), len(blob)).to_vec();
        rel(blob);
        v
    }
    pub fn compile(hlsl: &str, profile: &str) -> Result<Vec<u8>, String> {
        let f = func().ok_or("d3dcompiler_47.dll unavailable")?;
        let target = format!("{profile}\0");
        let (mut code, mut errs) = (std::ptr::null_mut(), std::ptr::null_mut());
        let hr = unsafe {
            f(hlsl.as_ptr() as *const c_void, hlsl.len(), b"tombox\0".as_ptr(), std::ptr::null(), std::ptr::null_mut(),
                b"main\0".as_ptr(), target.as_ptr(), 0, 0, &mut code, &mut errs)
        };
        let err = String::from_utf8_lossy(&unsafe { take(errs) }).into_owned();
        let bytes = unsafe { take(code) };
        if hr < 0 || bytes.is_empty() {
            // First "error" line is the cause; warnings precede it.
            let line = err.lines().find(|l| l.contains("error")).unwrap_or(err.lines().next().unwrap_or("")).to_string();
            let msg = line.split("): ").nth(1).unwrap_or(&line).to_string();
            return Err(format!("fxc: {msg}"));
        }
        Ok(bytes)
    }
}
#[cfg(not(windows))]
mod fxc {
    pub fn compile(_hlsl: &str, _profile: &str) -> Result<Vec<u8>, String> {
        Err("fxc: compile mode needs Windows".into())
    }
}

/// Compile with `fxc.exe` in a child process that is killed after `limit` seconds: an
/// in-process D3DCompile cannot be cancelled, and some generated shaders take minutes.
/// Returns Ok(seconds) or Err(reason).
fn compile_child(hlsl: &str, profile: &str, out: Option<&std::path::Path>, limit: u64, fxc: bool) -> Result<f64, String> {
    use std::process::{Command, Stdio};
    static FXC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let compiler = if fxc { "fxc" } else { "dxc" };
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join("tombox_sweep_tmp");
    let _ = std::fs::create_dir_all(&dir);
    let id = N.fetch_add(1, Ordering::Relaxed);
    let src = dir.join(format!("{}_{id}.hlsl", std::process::id()));
    let tmp_out = dir.join(format!("{}_{id}.cso", std::process::id()));
    std::fs::write(&src, hlsl).map_err(|e| format!("{compiler}: cannot write temp file: {e}"))?;
    let t0 = std::time::Instant::now();
    let mut command = if fxc {
        let exe = FXC.get_or_init(|| {
            std::env::var("FXC").unwrap_or_else(|_| r"C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64\fxc.exe".to_string())
        });
        let mut command = Command::new(exe);
        command.args(["/nologo", "/T", profile, "/E", "main", "/Fo"]).arg(&tmp_out).arg(&src);
        command
    } else {
        let exe = std::env::current_exe().map_err(|e| format!("dxc: cannot locate xscan: {e}"))?;
        let mut command = Command::new(exe);
        command.args(["--dxc-worker", profile]).arg(&src).arg(&tmp_out);
        command
    };
    let mut child = command
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{compiler}: cannot start compiler: {e}"))?;
    // Drain stderr on a helper thread so a chatty compiler cannot block on a full pipe.
    let mut stderr = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = stderr.as_mut() {
            let _ = std::io::Read::read_to_string(e, &mut s);
        }
        s
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if t0.elapsed().as_secs() >= limit => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(e) => return Err(format!("{compiler}: wait failed: {e}")),
        }
    };
    let err = reader.join().unwrap_or_default();
    let secs = t0.elapsed().as_secs_f64();
    let _ = std::fs::remove_file(&src);
    let res = match status {
        None => Err(format!("{compiler}: timeout after {limit} s")),
        Some(status) if status.success() && std::fs::metadata(&tmp_out).is_ok_and(|m| m.len() != 0) => {
            if let Some(out) = out {
                std::fs::rename(&tmp_out, out).map(|_| secs).map_err(|e| format!("{compiler}: cannot store result: {e}"))
            } else {
                Ok(secs)
            }
        }
        Some(_) => {
            let line = err.lines().find(|l| l.contains("error")).unwrap_or(err.lines().next().unwrap_or("no output")).to_string();
            if fxc {
                Err(format!("fxc: {}", line.split("): ").nth(1).unwrap_or(&line)))
            } else {
                Err(line)
            }
        }
    };
    let _ = std::fs::remove_file(&tmp_out);
    res
}

/// Same key and directory as the runtime's compiled-shader cache (d3d12_xs/shader_xlat.rs),
/// so a compile sweep pre-warms the cache the game uses.
fn cache_path(hlsl: &str, profile: &str) -> std::path::PathBuf {
    let key = fnv1a(hlsl.as_bytes()) ^ fnv1a(profile.as_bytes()).rotate_left(17);
    std::env::temp_dir().join("tombox_shader_cache").join("by_hlsl").join(format!("{key:016x}.cso"))
}

fn stage_name(s: &Stage) -> &'static str {
    match s {
        Stage::Vertex => "VS",
        Stage::Pixel => "PS",
        Stage::Compute => "CS",
        Stage::Geometry => "GS",
        Stage::Hull => "HS",
        Stage::Domain => "DS",
        Stage::Mesh => "MS",
        Stage::Amplification => "AS",
        Stage::Unknown(_) => "??",
    }
}

/// Slowest compiles seen in this run: (seconds, hash, HLSL lines).
static SLOW: Mutex<Vec<(f64, u64, usize)>> = Mutex::new(Vec::new());
static LIMIT: AtomicUsize = AtomicUsize::new(120);
/// 1 = compile with fxc.exe (SM5 DXBC) instead of the default DXC (SM6 DXIL).
static USE_FXC: AtomicUsize = AtomicUsize::new(0);

/// `--mnemonics 1`: also print, per instruction mnemonic, how many shaders use it and how
/// many of those translate.
static MNEM: AtomicUsize = AtomicUsize::new(0);
static MNEM_USE: std::sync::Mutex<std::collections::BTreeMap<String, (u64, u64)>> = std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Translate (and optionally compile) one container. Returns (stage, result, compiled_now).
fn process(blob: &[u8], compile: bool, use_cache: bool, dxc_limit: Option<u64>) -> (String, Result<(), String>, bool) {
    let r = std::panic::catch_unwind(|| match extract(blob) {
        Err(e) => (String::from("?"), Err(format!("extract: {e}")), false),
        Ok(ex) => {
            let st = stage_name(&ex.stage).to_string();
            // Input-keyed cache (same as the runtime): a hit needs no translation. Skipped
            // with --mnemonics (which needs the translation result).
            let dxc_path = compile && use_cache && USE_FXC.load(Ordering::Relaxed) == 0;
            if dxc_path && MNEM.load(Ordering::Relaxed) == 0
                && std::fs::metadata(tombox_xshader::dxc::blob_cache_path(blob, "dxc")).is_ok_and(|m| m.len() != 0)
            {
                return (st, Ok(()), false);
            }
            // `--infer-mesh-abi 1` (inherited by worker processes through the environment):
            // mesh shaders take the inferred-ABI path instead of the strict one.
            let opts = hlsl::Options { infer_mesh_abi: std::env::var_os("TOMBOX_INFER_MESH_ABI").is_some(), ..Default::default() };
            let tr = hlsl::translate_with_pso_opts(&ex, layouts_for(blob), &opts);
            if MNEM.load(Ordering::Relaxed) != 0 {
                // Instruction usage: (shaders using it, of which translated).
                let names: std::collections::BTreeSet<String> =
                    tombox_xshader::rdna2::disassemble(&ex.isa).into_iter().map(|i| i.mnemonic).collect();
                let mut g = MNEM_USE.lock().unwrap();
                for n in names {
                    let e = g.entry(n).or_insert((0u64, 0u64));
                    e.0 += 1;
                    e.1 += tr.is_ok() as u64;
                }
            }
            match tr {
                Err(e) => (st, Err(e), false),
                Ok(t) if !compile => {
                    let _ = t;
                    (st, Ok(()), false)
                }
                Ok(t) if USE_FXC.load(Ordering::Relaxed) != 0 => {
                    let path = cache_path(&t.hlsl, t.profile);
                    if use_cache && std::fs::metadata(&path).is_ok_and(|m| m.len() != 0) {
                        return (st, Ok(()), false);
                    }
                    let _ = fxc::compile; // in-process FXC kept for reference; FXC runs as a killable child
                    let output = use_cache.then_some(path.as_path());
                    match compile_child(&t.hlsl, t.profile, output, LIMIT.load(Ordering::Relaxed) as u64, true) {
                        Ok(secs) => {
                            if secs > 5.0 {
                                SLOW.lock().unwrap().push((secs, fnv1a(blob), t.hlsl.lines().count()));
                            }
                            (st, Ok(()), true)
                        }
                        Err(e) => (st, Err(e), true),
                    }
                }
                Ok(t) => {
                    // Default: DXC (shader model 6.0), in process.
                    let profile = tombox_xshader::dxc::profile_for(t.profile);
                    let path = tombox_xshader::dxc::cache_path(&t.hlsl, &profile);
                    if use_cache {
                        if let Ok(code) = std::fs::read(&path) {
                            if !code.is_empty() {
                                tombox_xshader::dxc::store_blob_cache(blob, "dxc", &code);
                                return (st, Ok(()), false);
                            }
                        }
                    }
                    let compiled = if let Some(limit) = dxc_limit {
                        compile_child(&t.hlsl, &profile, use_cache.then_some(path.as_path()), limit, false)
                    } else {
                        let t0 = std::time::Instant::now();
                        tombox_xshader::dxc::compile(&t.hlsl, &profile).map(|code| {
                            if use_cache {
                                let _ = std::fs::write(&path, code);
                            }
                            t0.elapsed().as_secs_f64()
                        })
                    };
                    match compiled {
                        Ok(secs) => {
                            if secs > 5.0 {
                                SLOW.lock().unwrap().push((secs, fnv1a(blob), t.hlsl.lines().count()));
                            }
                            if use_cache {
                                if let Ok(code) = std::fs::read(&path) {
                                    tombox_xshader::dxc::store_blob_cache(blob, "dxc", &code);
                                }
                            }
                            (st, Ok(()), true)
                        }
                        Err(e) => (st, Err(e), true),
                    }
                }
            }
        }
    });
    r.unwrap_or_else(|_| (String::from("?"), Err("PANIC in translator".to_string()), false))
}

fn process_bounded(blob: &[u8], compile: bool, use_cache: bool, seconds: u64) -> (String, Result<(), String>, bool) {
    use std::process::{Command, Stdio};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join("tombox_sweep_tmp");
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let input = dir.join(format!("shader_{}_{id}.dxbc", std::process::id()));
    let sidecar = input.with_extension("bindings");
    let run = || -> Result<_, String> {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(&input, blob).map_err(|e| e.to_string())?;
        let layouts = layouts_for(blob);
        if !layouts.is_empty() {
            std::fs::write(&sidecar, encode_bindings(layouts)).map_err(|e| e.to_string())?;
        }
        let mut child = Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
            .arg("--shader-worker")
            .arg(if compile { "1" } else { "0" })
            .arg(if use_cache { "1" } else { "0" })
            .arg(&input)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let start = std::time::Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                break status;
            }
            if start.elapsed().as_secs() >= seconds {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("shader pipeline: timeout after {seconds} s"));
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        let mut output = String::new();
        child.stdout.take().ok_or("shader worker: no stdout")?
            .read_to_string(&mut output).map_err(|e| e.to_string())?;
        let mut fields = output.trim_end_matches(['\r', '\n']).splitn(3, '\t');
        let stage = fields.next().filter(|field| !field.is_empty()).ok_or("shader worker: no result")?;
        let invoked = fields.next() == Some("1");
        let diagnostics = fields.next().ok_or("shader worker: incomplete result")?;
        let result = if status.success() { Ok(()) } else { Err(diagnostics.to_owned()) };
        Ok((stage.to_owned(), result, invoked))
    };
    let result = run();
    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&sidecar);
    result.unwrap_or_else(|error| (String::from("?"), Err(error), false))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if let [_, worker, compile, cache, input] = args.as_slice() {
        if worker == "--shader-worker" {
            return shader_worker(input, compile == "1", cache == "1");
        }
    }
    if let [_, worker, profile, input, output] = args.as_slice() {
        if worker == "--dxc-worker" {
            return match dxc_worker(profile, input, output) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("{error}");
                    ExitCode::FAILURE
                }
            };
        }
    }
    if args.len() < 2 {
        eprintln!("usage: {} <file> [--examples <dir>] [--limit <n>] [--jobs <n>] [--compile 1] [--fresh 1] [--dxc-limit <seconds>] [--shader-limit <seconds>] [--infer-mesh-abi 1]", args[0]);
        return ExitCode::FAILURE;
    }
    let mut examples: Option<String> = None;
    let mut limit = usize::MAX;
    let mut jobs = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let mut compile = false;
    let mut use_cache = true;
    let mut dxc_limit = None;
    let mut shader_limit = None;
    let mut pso_offset = None;
    let mut pso_archive = None;
    let mut i = 2;
    while i + 1 < args.len() {
        match args[i].as_str() {
            "--examples" => examples = Some(args[i + 1].clone()),
            "--limit" => limit = args[i + 1].parse().unwrap_or(usize::MAX),
            "--jobs" => jobs = args[i + 1].parse().unwrap_or(jobs).max(1),
            "--compile" => compile = args[i + 1] == "1",
            "--infer-mesh-abi" => {
                if args[i + 1] == "1" {
                    std::env::set_var("TOMBOX_INFER_MESH_ABI", "1");
                }
            }
            "--fresh" => use_cache = args[i + 1] != "1",
            "--pso-archive" => pso_archive = Some(args[i + 1].clone()),
            "--pso-offset" => {
                let value = &args[i + 1];
                pso_offset = match value.strip_prefix("0x") {
                    Some(hex) => u64::from_str_radix(hex, 16).ok(),
                    None => value.parse().ok(),
                };
                if pso_offset.is_none() {
                    eprintln!("--pso-offset requires a byte offset");
                    return ExitCode::FAILURE;
                }
            }
            "--dxc-limit" => match args[i + 1].parse::<u64>() {
                Ok(seconds @ 1..) => dxc_limit = Some(seconds),
                _ => {
                    eprintln!("--dxc-limit requires a positive number of seconds");
                    return ExitCode::FAILURE;
                }
            },
            "--shader-limit" => match args[i + 1].parse::<u64>() {
                Ok(seconds @ 1..) => shader_limit = Some(seconds),
                _ => {
                    eprintln!("--shader-limit requires a positive number of seconds");
                    return ExitCode::FAILURE;
                }
            },
            "--mnemonics" => MNEM.store((args[i + 1] == "1") as usize, Ordering::Relaxed),
            "--fxc-limit" => LIMIT.store(args[i + 1].parse().unwrap_or(120), Ordering::Relaxed),
            "--compiler" => USE_FXC.store((args[i + 1] == "fxc") as usize, Ordering::Relaxed),
            other => eprintln!("warning: ignoring {other}"),
        }
        i += 2;
    }
    if shader_limit.is_some() && (dxc_limit.is_some() || USE_FXC.load(Ordering::Relaxed) != 0 || MNEM.load(Ordering::Relaxed) != 0) {
        eprintln!("--shader-limit is for translation or DXC sweeps without --dxc-limit or --mnemonics");
        return ExitCode::FAILURE;
    }
    if let Some(offset) = pso_offset {
        let path = pso_archive.as_deref().unwrap_or(&args[1]);
        let loaded = std::fs::File::open(path).and_then(|mut file| PsoCache::read(&mut file, offset));
        match loaded {
            Ok(cache) => { let _ = PSO_CACHE.set(cache); }
            Err(error) => {
                eprintln!("PSO cache: {error}");
                return ExitCode::FAILURE;
            }
        }
    } else if pso_archive.is_some() {
        eprintln!("--pso-archive requires --pso-offset");
        return ExitCode::FAILURE;
    }
    let blobs = match load_blobs(&args[1], limit) {
        Ok(blobs) => blobs,
        Err(e) => {
            eprintln!("cannot read {}: {e}", args[1]);
            return ExitCode::FAILURE;
        }
    };
    if let Some(d) = &examples {
        let _ = std::fs::create_dir_all(d);
    }
    if compile {
        if use_cache {
            let _ = std::fs::create_dir_all(std::env::temp_dir().join("tombox_shader_cache").join("by_hlsl"));
        }
        // A full sweep runs for a long time on someone's desktop: stay out of the way.
        #[cfg(windows)]
        unsafe {
            extern "system" {
                fn GetCurrentProcess() -> *mut std::ffi::c_void;
                fn SetPriorityClass(h: *mut std::ffi::c_void, class: u32) -> i32;
            }
            SetPriorityClass(GetCurrentProcess(), 0x4000); // BELOW_NORMAL_PRIORITY_CLASS
        }
        if !args.iter().any(|a| a == "--jobs") {
            jobs = jobs.min(4);
        }
    }
    // Unique containers.
    let mut seen: HashSet<u64> = HashSet::new();
    let mut found = 0usize;
    let mut work: Vec<(u64, &[u8])> = Vec::new();
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
            if seen.insert(h) {
                work.push((h, blob));
            }
        }
    }
    // Translate (and compile) in parallel.
    type Tally = (BTreeMap<String, (usize, usize)>, BTreeMap<String, (usize, u64)>, usize);
    let tally: Mutex<Tally> = Mutex::new((BTreeMap::new(), BTreeMap::new(), 0));
    let next = AtomicUsize::new(0);
    let started = std::time::Instant::now();
    std::thread::scope(|s| {
        for _ in 0..jobs {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= work.len() {
                    break;
                }
                let (h, blob) = work[i];
                let (stage, res, compiled) = if let Some(seconds) = shader_limit {
                    process_bounded(blob, compile, use_cache, seconds)
                } else {
                    process(blob, compile, use_cache, dxc_limit)
                };
                let mut t = tally.lock().unwrap();
                if compiled {
                    t.2 += 1;
                }
                let e = t.0.entry(stage).or_insert((0, 0));
                match res {
                    Ok(()) => e.0 += 1,
                    Err(msg) => {
                        e.1 += 1;
                        let key = normalise(&msg);
                        let ent = t.1.entry(key).or_insert((0, h));
                        if ent.0 < 3 {
                            if let Some(d) = &examples {
                                let _ = std::fs::write(format!("{d}/{h:016x}.dxbc"), blob);
                            }
                        }
                        ent.0 += 1;
                    }
                }
                drop(t);
                if (i + 1) % 5000 == 0 {
                    eprintln!("  {}/{} ({:.0} s)", i + 1, work.len(), started.elapsed().as_secs_f64());
                }
            });
        }
    });
    let (by_stage, errors, compiled_now) = tally.into_inner().unwrap();
    let (ok, fail) = by_stage.values().fold((0, 0), |a, v| (a.0 + v.0, a.1 + v.1));
    println!("containers found: {found}   unique: {}   {}: {ok}   failed: {fail}   ({:.2}% ok)",
        seen.len(), if compile { "translated+compiled" } else { "translated" }, 100.0 * ok as f64 / (ok + fail).max(1) as f64);
    if compile {
        println!("  compiler invocations: {compiled_now} (including failed compiles)");
    }
    for (st, (o, f)) in &by_stage {
        println!("  {st}: ok={o} fail={f}");
    }
    let mut v: Vec<_> = errors.into_iter().collect();
    v.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    let mut slow = SLOW.lock().unwrap().clone();
    slow.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    if !slow.is_empty() {
        println!("--- {} compiles took more than 5 s; slowest ---", slow.len());
        for (secs, h, lines) in slow.iter().take(25) {
            println!("{secs:>8.1} s  {h:016x}  {lines} HLSL lines");
        }
    }
    println!("--- failure reasons ({} distinct) ---", v.len());
    for (reason, (count, ex)) in v.iter().take(80) {
        println!("{count:>7}  {reason}   [e.g. {ex:016x}]");
    }
    if MNEM.load(Ordering::Relaxed) != 0 {
        println!("--- mnemonics (shaders using, translated) ---");
        for (n, (used, ok)) in MNEM_USE.lock().unwrap().iter() {
            println!("MNEM {n} {used} {ok}");
        }
    }
    ExitCode::SUCCESS
}

fn shader_worker(input: &str, compile: bool, cache: bool) -> ExitCode {
    let sidecar = std::path::Path::new(input).with_extension("bindings");
    if sidecar.exists() {
        let decoded = std::fs::read(sidecar).map_err(|error| error.to_string())
            .and_then(|data| decode_bindings(&data));
        match decoded {
            Ok(layouts) => { let _ = WORKER_BINDINGS.set(layouts); }
            Err(error) => {
                println!("?\t0\t{error}");
                return ExitCode::FAILURE;
            }
        }
    }
    let (stage, result, attempted) = match std::fs::read(input) {
        Ok(blob) => process(&blob, compile, cache, None),
        Err(error) => (String::from("?"), Err(error.to_string()), false),
    };
    let invoked = usize::from(attempted);
    match result {
        Ok(()) => {
            println!("{stage}\t{invoked}\t");
            ExitCode::SUCCESS
        }
        Err(error) => {
            let line = error.lines().next().unwrap_or("shader worker: no diagnostics");
            println!("{stage}\t{invoked}\t{}", &line[..line.floor_char_boundary(800)]);
            ExitCode::FAILURE
        }
    }
}

fn dxc_worker(profile: &str, input: &str, output: &str) -> Result<(), String> {
    let source = std::fs::read_to_string(input).map_err(|e| format!("dxc: cannot read HLSL: {e}"))?;
    let bytecode = tombox_xshader::dxc::compile(&source, profile)?;
    std::fs::write(output, bytecode).map_err(|e| format!("dxc: cannot write DXIL: {e}"))
}

fn load_blobs(path: &str, limit: usize) -> std::io::Result<Vec<Vec<u8>>> {
    let mut file = std::fs::File::open(path)?;
    let mut magic = [0; 4];
    let has_magic = match file.read_exact(&mut magic) {
        Ok(()) => &magic == b"9FPR",
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => false,
        Err(error) => return Err(error),
    };
    file.rewind()?;
    if has_magic {
        let mut seen = HashSet::new();
        let mut shaders = Vec::new();
        let mut records = 0;
        tombox_xshader::rpf::scan(file, |record| {
            records += 1;
            if seen.len() < limit && seen.insert(fnv1a(record.dxbc)) {
                shaders.push(record.dxbc.to_vec());
            }
            Ok(())
        })?;
        eprintln!("RPF9 shader records: {records}, unique containers selected: {}", shaders.len());
        return Ok(shaders);
    }

    let mut data = Vec::new();
    file.read_to_end(&mut data)?;
    let archives = find_archives(&data);
    if archives.is_empty() {
        return Ok(vec![data]);
    }
    let mut blobs = Vec::new();
    let mut lz_fail = 0;
    for archive in archives {
        for (off, size, unpacked) in archive.entries {
            let compressed = &data[off..off + size];
            let decoded = if size == unpacked {
                Some(compressed.to_vec())
            } else {
                lz4_block(compressed, unpacked)
            };
            match decoded {
                Some(blob) => blobs.push(blob),
                None => lz_fail += 1,
            }
        }
    }
    eprintln!("decompressed {} archive entries ({lz_fail} LZ4 failures)", blobs.len());
    Ok(blobs)
}
