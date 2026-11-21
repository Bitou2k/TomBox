use std::{fs::File, io::Cursor};
use tombox_xshader::psocache::PsoCache;

#[test]
fn malformed_inputs_return_errors_without_panicking() {
    for size in 0..128 {
        assert!(PsoCache::parse(&vec![0; size]).is_err());
    }
    let mut header = vec![0; 128];
    header[..4].copy_from_slice(b"PS\0\x09");
    for field in [0x50, 0x54, 0x58, 0x5c, 0x60] {
        let mut invalid = header.clone();
        invalid[field..field + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(PsoCache::parse(&invalid).is_err());
    }
    assert!(PsoCache::read(&mut Cursor::new(Vec::<u8>::new()), 1).is_err());
    assert!(PsoCache::read(&mut Cursor::new(vec![0; 128]), 0).is_err());
}

#[test]
fn real_archive_metadata_is_readable() {
    let Ok(path) = std::env::var("TOMBOX_PSO_ARCHIVE") else { return };
    let mut archive = File::open(path).unwrap();
    PsoCache::read(&mut archive, 0x10579000).unwrap();
}
