use std::fs;
use std::io::{self, Cursor};
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use miniz_oxide::deflate::compress_to_vec;
use tombox_xshader::{rpf, Stage};

const PIXEL: &[u8] = include_bytes!("../../../analysis/shaders/fortnite_ps_sample.dxbc");
const VERTEX: &[u8] = include_bytes!("../../../analysis/shaders/fortnite_vs_sample.dxbc");
const BUFFER_SIZE: usize = 256 * 1024;

fn archive() -> Vec<u8> {
    let mut archive = vec![0; 0x40];
    archive[..4].copy_from_slice(b"9FPR");
    archive
}

fn append(archive: &mut Vec<u8>, name: &str, payload: &[u8], inflated: usize) -> u64 {
    let offset = archive.len() as u64;
    archive.extend_from_slice(name.as_bytes());
    archive.extend_from_slice(&[0, 3]);
    archive.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    archive.extend_from_slice(&[0; 4]);
    archive.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    archive.extend_from_slice(&(inflated as u32).to_le_bytes());
    archive.extend_from_slice(payload);
    offset
}

#[test]
fn resumes_scanning_after_bad_candidates_and_handles_buffer_boundaries() {
    let mut data = archive();
    data.resize(0x40 + BUFFER_SIZE - 4, 0);
    let vertex = append(&mut data, "VS_Example", &compress_to_vec(VERTEX, 6), VERTEX.len());
    let pixel_deflate = compress_to_vec(PIXEL, 6);
    append(&mut data, "PS_WrongSize", &pixel_deflate, PIXEL.len() - 1);
    let mut trailing_bytes = pixel_deflate.clone();
    trailing_bytes.extend_from_slice(b"EXTRA");
    append(&mut data, "PS_ExtraBytes", &trailing_bytes, PIXEL.len());
    let mut malformed = PIXEL.to_vec();
    malformed[32..36].copy_from_slice(&u32::MAX.to_le_bytes());
    append(&mut data, "PS_BadChunk", &compress_to_vec(&malformed, 6), malformed.len());
    let mut nested = Vec::new();
    let nested_offset = append(&mut nested, "PS_Example", &pixel_deflate, PIXEL.len());
    let bad = append(&mut data, "BAD_Record", &nested, 4096);
    let pixel = bad + "BAD_Record".len() as u64 + 18 + nested_offset;
    let duplicate = append(&mut data, "PS_Example", &pixel_deflate, PIXEL.len());
    append(&mut data, "PS_Truncated", &pixel_deflate, PIXEL.len());
    data.truncate(data.len() - 2);

    let mut records = Vec::new();
    rpf::scan(Cursor::new(data), |record| {
        assert_eq!(record.shader_model, (6, 0));
        let expected = if record.stage == Stage::Vertex { VERTEX } else { PIXEL };
        assert_eq!(record.dxbc, expected);
        records.push((record.offset, record.name.to_owned(), record.stage));
        Ok(())
    })
    .unwrap();
    assert_eq!(
        records,
        [
            (vertex, "VS_Example".to_owned(), Stage::Vertex),
            (pixel, "PS_Example".to_owned(), Stage::Pixel),
            (duplicate, "PS_Example".to_owned(), Stage::Pixel),
        ],
    );
}

#[test]
fn recognizes_a_length_prefixed_name_and_the_rpf_record_trailer() {
    let mut data = archive();
    let name = "AlphaResolve:CS_AlphaTransmissionDenoise_2871f2e9_Wrapped";
    let name_len = (name.len() + 1) as u8;
    assert!(name_len.is_ascii_graphic());
    data.push(name_len);
    let deflate = compress_to_vec(PIXEL, 6);
    let mut stored = deflate.clone();
    stored.extend_from_slice(&[0xa5; 12]);
    let offset = append(&mut data, name, &stored, PIXEL.len());

    let mut count = 0;
    rpf::scan(Cursor::new(data), |record| {
        count += 1;
        assert_eq!(record.name, name);
        assert_eq!(record.offset, offset);
        assert_eq!(record.compressed_size, deflate.len());
        assert_eq!(record.dxbc, PIXEL);
        Ok(())
    })
    .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn validates_the_archive_and_propagates_visitor_errors() {
    let mut data = archive();
    assert_eq!(
        rpf::scan(Cursor::new(&data[..63]), |_| unreachable!()).unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof,
    );
    data[0] = b'8';
    assert_eq!(
        rpf::scan(Cursor::new(&data), |_| unreachable!()).unwrap_err().kind(),
        io::ErrorKind::InvalidData,
    );
    data[0] = b'9';
    append(&mut data, "VS_Example", &compress_to_vec(VERTEX, 6), VERTEX.len());
    assert_eq!(
        rpf::scan(Cursor::new(data), |_| Err(io::Error::other("visitor stopped")))
            .unwrap_err()
            .to_string(),
        "visitor stopped",
    );
}

struct TestDirectory(PathBuf);

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn cli_filters_and_exports_dxbc_that_xshader_can_read() {
    let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = TestDirectory(std::env::temp_dir().join(format!(
        "tombox-rpf-{}-{suffix}", std::process::id()
    )));
    fs::create_dir(&root.0).unwrap();
    let input = root.0.join("shader input.rpf");
    let output = root.0.join("shader output");
    let mut data = archive();
    append(&mut data, "VS_Example", &compress_to_vec(VERTEX, 6), VERTEX.len());
    let pixel = append(&mut data, "Misleading_Vertex_Name", &compress_to_vec(PIXEL, 6), PIXEL.len());
    fs::write(&input, data).unwrap();

    let scan = || Command::new(env!("CARGO_BIN_EXE_xshader-rpf"))
        .arg(&input)
        .args(["--stage", "pixel", "--out"])
        .arg(&output)
        .output()
        .unwrap();
    let result = scan();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let manifest = String::from_utf8(result.stdout).unwrap();
    assert_eq!(manifest.lines().count(), 2);
    assert!(manifest.contains(&format!("{pixel:012x}\tMisleading_Vertex_Name\tpixel\t6.0\t")));
    assert_eq!(fs::read_dir(&output).unwrap().count(), 1);

    let dxbc = output.join(format!("{pixel:012x}.dxbc"));
    assert_eq!(fs::read(&dxbc).unwrap(), PIXEL);
    let result = Command::new(env!("CARGO_BIN_EXE_xshader")).arg(&dxbc).output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert!(String::from_utf8_lossy(&result.stdout).contains("stage=Pixel"));
    assert!(scan().status.success());
    fs::write(&dxbc, b"existing file").unwrap();
    assert!(!scan().status.success());
    assert_eq!(fs::read(&dxbc).unwrap(), b"existing file");
}
