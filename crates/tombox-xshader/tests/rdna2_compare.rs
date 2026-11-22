use tombox_xshader::rdna2::{disassemble, Opnd};

#[test]
fn vopc_cmpx_writes_exec_and_f16_compares_are_split() {
    let words = [
        0x7c28_0300u32,
        0x7da2_0300,
        0x7dd0_0300,
        0xd496_006a,
        0x0002_0300,
        0xbf81_0000,
    ];
    let isa: Vec<_> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    let compares = disassemble(&isa);
    let got: Vec<_> = compares
        .iter()
        .take(4)
        .map(|insn| (insn.mnemonic.as_str(), insn.dst.first().copied()))
        .collect();
    assert_eq!(
        got,
        [
            ("v_cmpx_gt_f32", Some(Opnd::ExecLo)),
            ("v_cmpx_lt_u32", Some(Opnd::ExecLo)),
            ("v_cmp_u_f16", Some(Opnd::VccLo)),
            ("v_cmpx_ge_i32", Some(Opnd::ExecLo)),
        ]
    );
}
