use tombox_xshader::rdna2::{disassemble, Opnd};

#[test]
fn scalar_memory_offset_is_a_source_register() {
    for instruction in [0xf408_0206u32, 0xf420_0084] {
        for offset in [0u32, 4, 37, 105] {
            let words = [instruction, offset << 25];
            let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
            let decoded = disassemble(&bytes);
            assert_eq!(decoded[0].src.len(), 2);
            assert_eq!(decoded[0].src[1], Opnd::Sgpr(offset as u8));
        }
        let words = [instruction, 0xfa00_0000];
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        assert_eq!(disassemble(&bytes)[0].src.len(), 1);
    }
}

#[test]
fn entry_scalar_offset_is_tracked_in_translation() {
    use tombox_xshader::{ShaderExtract, Stage};
    let words = [0xf408_0206u32, 0xfa00_0000, 0xf420_0084, 0x4a00_0000, 0xbf810000];
    let isa: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let ex = ShaderExtract {
        stage: Stage::Compute, shader_model: (6, 5), bitcode_stripped: true,
        fssm: vec![], entry: Some("main".into()), isa_data: isa.clone(), isa,
        isa_offset_in_fssm: 0, isa_preamble: [0; 4], has_input_sig: true,
        has_output_sig: true, has_psv0: true, has_root_sig: false,
        input_sig: vec![], output_sig: vec![], resources: vec![], gs_info: None,
        mesh_info: None, root_sig: None,
    };
    let translated = tombox_xshader::hlsl::translate(&ex).unwrap();
    assert!(translated.hlsl.lines().next().unwrap().split_whitespace().any(|word| word == "s37"));
    assert!(translated.hlsl.contains("+ s37"));
}
