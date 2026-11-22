// Translator revision: a hash of this crate's source files, so caches keyed by the input
// shader (not by the generated HLSL) are invalidated whenever the translator changes.
use std::path::{Path, PathBuf};

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn main() {
    let mut list = Vec::new();
    files(Path::new("src"), &mut list);
    list.sort();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in &list {
        for &b in p.to_string_lossy().replace('\\', "/").as_bytes().iter().chain(std::fs::read(p).unwrap_or_default().iter()) {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    println!("cargo:rustc-env=TOMBOX_XSHADER_REV={h:016x}");
    println!("cargo:rerun-if-changed=src");
}
