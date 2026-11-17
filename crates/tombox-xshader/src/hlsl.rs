//! RDNA2 → HLSL translator (stage 3 of the shader pipeline).
//!
//! Model: one HLSL invocation == one RDNA2 *lane*. Every VGPR/SGPR is a `uint` holding the
//! raw 32-bit pattern (floats via asfloat/asuint), so no type inference is needed. Lane
//! masks (exec, vcc, compare results in SGPRs) are represented by their bit for *this*
//! lane (0/1). Descriptor-table pointers in user SGPRs are tracked symbolically: an
//! `s_load_dwordxN` from a never-written SGPR is a descriptor fetch from a root table, and
//! the way the descriptor is consumed (s_buffer_load → CBV, MIMG resource → SRV, MIMG
//! sampler → sampler) classifies the table. Slots map 1:1 to HLSL registers because the
//! title's root signature (UE layout) uses one SRV/CBV/sampler table per stage from
//! register 0.
//!
//! Scope: forward-only control flow (no loops yet). Anything not understood returns `Err`
//! naming the instruction — the caller must fall back; nothing is guessed.

use crate::meta::SigElement;
use crate::rdna2::{disassemble, Enc, Insn, Opnd};
use crate::{ShaderExtract, Stage};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;

pub struct Translated {
    pub hlsl: String,
    pub entry: &'static str,
    /// FXC/D3DCompile target profile.
    pub profile: &'static str,
}

#[derive(Clone, Debug)]
enum Sym {
    Table(u8),
    Desc { table: u8, off: u32, k: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Res {
    Cb(u32),
    Tex(u32, u32), // slot, dim
    Samp(u32),
    Uav(u32, u32), // slot, dim
}

const XYZW: [&str; 4] = ["x", "y", "z", "w"];

fn name(o: &Opnd) -> Result<String, String> {
    Ok(match o {
        Opnd::Sgpr(n) => format!("s{n}"),
        Opnd::Vgpr(n) => format!("v{n}"),
        Opnd::VccLo => "vcc_lo".into(),
        Opnd::VccHi => "vcc_hi".into(),
        Opnd::ExecLo => "exec_lo".into(),
        Opnd::ExecHi => "exec_hi".into(),
        Opnd::M0 => "m0".into(),
        Opnd::Scc => "scc".into(),
        Opnd::Null => "nul".into(),
        other => return Err(format!("operand {other:?} is not assignable")),
    })
}

/// Operand as a `uint` rvalue expression.
fn r(o: &Opnd) -> Result<String, String> {
    Ok(match o {
        Opnd::ConstInt(v) => format!("{:#x}u", *v as i32 as u32),
        Opnd::ConstF32(f) => format!("{:#x}u", f.to_bits()),
        Opnd::Literal(v) => format!("{v:#x}u"),
        Opnd::Raw(c) => return Err(format!("unmodelled operand code {c}")),
        other => name(other)?,
    })
}

fn f(o: &Opnd) -> Result<String, String> {
    Ok(format!("asfloat({})", r(o)?))
}

/// High half of a 64-bit scalar operand.
fn hi(o: &Opnd) -> Opnd {
    match o {
        Opnd::Sgpr(n) => Opnd::Sgpr(n + 1),
        Opnd::VccLo => Opnd::VccHi,
        Opnd::ExecLo => Opnd::ExecHi,
        Opnd::ConstInt(v) => Opnd::ConstInt(if *v < 0 { -1 } else { 0 }),
        Opnd::Null => Opnd::Null,
        _ => Opnd::ConstInt(0),
    }
}

fn sig_type(e: &SigElement) -> (String, usize) {
    let comps = (8 - e.mask.leading_zeros() as usize).max(1);
    let base = match e.comp_type {
        1 => "uint",
        2 => "int",
        _ => "float",
    };
    (if comps == 1 { base.to_string() } else { format!("{base}{comps}") }, comps)
}

fn semantic(e: &SigElement) -> String {
    format!("{}{}", e.name, e.index)
}

struct Emitter<'a> {
    ex: &'a ShaderExtract,
    insns: Vec<Insn>,
    res: HashMap<usize, Vec<Res>>, // per-instruction resolved resources
    cb_size: BTreeMap<u32, u32>,   // slot -> dwords
    tex: BTreeMap<u32, u32>,       // slot -> dim
    samp: BTreeSet<u32>,
    uav: BTreeMap<u32, u32>,       // slot -> dim
    is_cs: bool,
    regs: BTreeSet<String>,
    body: String,
    tmp: usize,
    ps_attrs: Vec<&'a SigElement>,
    out_params: Vec<&'a SigElement>,
}

pub fn translate(ex: &ShaderExtract) -> Result<Translated, String> {
    let (profile, is_ps) = match ex.stage {
        Stage::Vertex => ("vs_5_0", false),
        Stage::Pixel => ("ps_5_0", true),
        Stage::Compute => ("cs_5_0", false),
        other => return Err(format!("stage {other:?} not supported yet")),
    };
    let insns = disassemble(&ex.isa);
    let mut em = Emitter {
        ex,
        insns,
        res: HashMap::new(),
        cb_size: BTreeMap::new(),
        tex: BTreeMap::new(),
        samp: BTreeSet::new(),
        uav: BTreeMap::new(),
        is_cs: ex.stage == Stage::Compute,
        regs: BTreeSet::new(),
        body: String::new(),
        tmp: 0,
        ps_attrs: ex.input_sig.iter().filter(|e| e.sysval == 0 && e.rw_mask != 0).collect(),
        out_params: ex.output_sig.iter().filter(|e| e.sysval == 0).collect(),
    };
    em.resolve_resources()?;
    em.emit_body(is_ps)?;
    let hlsl = em.finish(is_ps)?;
    Ok(Translated { hlsl, entry: "main", profile })
}

impl<'a> Emitter<'a> {
    /// Root-signature range type (0 SRV, 1 UAV, 2 CBV, 3 Sampler) of the descriptor table
    /// whose pointer lives in user SGPR `sgpr` at shader entry. The FSSM register table
    /// maps user SGPR (base + i) to root parameter i, with base 8 for graphics stages and
    /// 4 for compute (observed: PS s8/s9/s10 = params 0/1/2, VS s12 = param 4, CS s5/s7 =
    /// params 1/3). `None` when the root signature is unavailable or the SGPR is not a table.
    fn table_kind(&self, sgpr: u8) -> Option<u32> {
        let base = if self.is_cs { 4 } else { 8 };
        let idx = (sgpr as usize).checked_sub(base)?;
        match &self.ex.root_sig.as_ref()?.params.get(idx)?.kind {
            crate::meta::RootParamKind::Table(r) if r.len() == 1 && r[0].base_register == 0 => Some(r[0].range_type),
            _ => None,
        }
    }

    /// Linear symbolic pass: classify descriptor loads and bind them to HLSL registers.
    fn resolve_resources(&mut self) -> Result<(), String> {
        let mut sym: HashMap<u8, Sym> = HashMap::new();
        let mut written: BTreeSet<u8> = BTreeSet::new();
        let insns = self.insns.clone();
        for (idx, ins) in insns.iter().enumerate() {
            let sg = |o: &Opnd| if let Opnd::Sgpr(n) = o { Some(*n) } else { None };
            match ins.enc {
                Enc::Sop1 if ins.mnemonic == "s_mov_b32" => {
                    if let Some(d) = ins.dst.first().and_then(sg) {
                        match ins.src.first().and_then(sg).and_then(|s| sym.get(&s).cloned()) {
                            Some(v) => {
                                sym.insert(d, v);
                            }
                            None => {
                                // A never-written user SGPR copied before use is still a table pointer.
                                let s = ins.src.first().and_then(sg);
                                match s {
                                    Some(s) if !written.contains(&s) => {
                                        sym.insert(d, Sym::Table(s));
                                    }
                                    _ => {
                                        sym.remove(&d);
                                    }
                                }
                            }
                        }
                        written.insert(d);
                    }
                }
                Enc::Smem if ins.op <= 4 => {
                    let n = 1u32 << ins.op;
                    let b = ins.src.first().and_then(sg).ok_or("s_load: bad base")?;
                    let d = ins.dst.first().and_then(sg).ok_or("s_load: bad dst")?;
                    let table = match sym.get(&b) {
                        Some(Sym::Table(t)) => *t,
                        None if !written.contains(&b) => b,
                        other => {
                            return Err(format!(
                                "s_load at {:#x}: base s{b} is not a root table pointer ({other:?})",
                                ins.offset
                            ))
                        }
                    };
                    if (ins.raw.get(1).copied().unwrap_or(0) >> 25) != 0x7d {
                        return Err(format!("s_load at {:#x}: dynamic soffset unsupported", ins.offset));
                    }
                    for k in 0..n {
                        sym.insert(d + k as u8, Sym::Desc { table, off: ins.aux.imm as u32, k });
                        written.insert(d + k as u8);
                    }
                }
                Enc::Smem => {
                    let n = 1u32 << (ins.op - 8);
                    let b = ins.src.first().and_then(sg).ok_or("s_buffer_load: bad base")?;
                    let d = ins.dst.first().and_then(sg).ok_or("s_buffer_load: bad dst")?;
                    let slot = match sym.get(&b) {
                        Some(Sym::Desc { off, k: 0, .. }) => off / 16,
                        other => {
                            return Err(format!(
                                "s_buffer_load at {:#x}: base s{b} is not a buffer descriptor ({other:?})",
                                ins.offset
                            ))
                        }
                    };
                    if (ins.raw.get(1).copied().unwrap_or(0) >> 25) != 0x7d {
                        return Err(format!("s_buffer_load at {:#x}: dynamic soffset unsupported", ins.offset));
                    }
                    let end = ins.aux.imm as u32 / 4 + n;
                    let e = self.cb_size.entry(slot).or_insert(0);
                    *e = (*e).max(end);
                    self.res.insert(idx, vec![Res::Cb(slot)]);
                    for k in 0..n {
                        sym.remove(&(d + k as u8));
                        written.insert(d + k as u8);
                    }
                }
                Enc::Mimg => {
                    let n = ins.src.len();
                    let rs = ins.src.get(n - 2).and_then(sg).ok_or("mimg: bad srsrc")?;
                    let ss = ins.src.get(n - 1).and_then(sg).ok_or("mimg: bad ssamp")?;
                    let dim = ins.aux.imm as u32;
                    let (tslot, ttable) = match sym.get(&rs) {
                        Some(Sym::Desc { off, k: 0, table }) => (off / 32, *table),
                        other => {
                            return Err(format!("mimg at {:#x}: s{rs} not an image descriptor ({other:?})", ins.offset))
                        }
                    };
                    let is_uav = self.table_kind(ttable) == Some(1);
                    let mut v = vec![if is_uav { Res::Uav(tslot, dim) } else { Res::Tex(tslot, dim) }];
                    let map = if is_uav { &mut self.uav } else { &mut self.tex };
                    if let Some(prev) = map.insert(tslot, dim) {
                        if prev != dim {
                            return Err(format!("image slot {tslot} used with two dimensions"));
                        }
                    }
                    if ins.mnemonic.starts_with("image_sample") || ins.mnemonic.starts_with("image_gather") {
                        let sslot = match sym.get(&ss) {
                            Some(Sym::Desc { off, k: 0, .. }) => off / 16,
                            other => {
                                return Err(format!("mimg at {:#x}: s{ss} not a sampler descriptor ({other:?})", ins.offset))
                            }
                        };
                        self.samp.insert(sslot);
                        v.push(Res::Samp(sslot));
                    }
                    self.res.insert(idx, v);
                }
                _ => {
                    for d in &ins.dst {
                        if let Some(n) = sg(d) {
                            sym.remove(&n);
                            written.insert(n);
                            if ins.mnemonic.ends_with("_b64") {
                                sym.remove(&(n + 1));
                                written.insert(n + 1);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn line(&mut self, s: &str) {
        self.body.push_str("    ");
        self.body.push_str(s);
        self.body.push('\n');
    }

    fn touch(&mut self, o: &Opnd) {
        if let Opnd::Sgpr(_) | Opnd::Vgpr(_) = o {
            if let Ok(n) = name(o) {
                self.regs.insert(n);
            }
        }
    }

    /// Guarded write to a VGPR (inactive lanes keep their value).
    fn wv(&mut self, dst: &Opnd, expr: &str) -> Result<(), String> {
        self.touch(dst);
        let d = name(dst)?;
        self.line(&format!("if (exec_lo & 1u) {d} = {expr};"));
        Ok(())
    }

    fn ws(&mut self, dst: &Opnd, expr: &str) -> Result<(), String> {
        self.touch(dst);
        let d = name(dst)?;
        self.line(&format!("{d} = {expr};"));
        Ok(())
    }

    fn emit_body(&mut self, is_ps: bool) -> Result<(), String> {
        // Branch targets (forward only).
        let off2idx: HashMap<usize, usize> = self.insns.iter().enumerate().map(|(i, x)| (x.offset, i)).collect();
        let mut target: HashMap<usize, usize> = HashMap::new();
        for (i, ins) in self.insns.iter().enumerate() {
            if ins.enc == Enc::Sopp && matches!(ins.op, 2 | 4..=9) {
                let t = ins.offset as i64 + 4 + ins.aux.imm * 4;
                let ti = *off2idx
                    .get(&(t as usize))
                    .ok_or_else(|| format!("branch at {:#x} targets mid-instruction {t:#x}", ins.offset))?;
                if ti <= i {
                    return Err(format!("backward branch (loop) at {:#x} not supported yet", ins.offset));
                }
                target.insert(i, ti);
            }
        }
        let labels: BTreeSet<usize> = target.values().copied().collect();
        for l in &labels {
            self.line(&format!("bool j{l} = false;"));
        }

        let insns = self.insns.clone();
        let mut open = false;
        let mut cur: Vec<usize> = vec![];
        for (i, ins) in insns.iter().enumerate() {
            let active: Vec<usize> =
                labels.iter().copied().filter(|&t| t > i && target.iter().any(|(&b, &bt)| bt == t && b < i)).collect();
            if active != cur {
                if open {
                    self.line("}");
                    open = false;
                }
                if !active.is_empty() {
                    let c: Vec<String> = active.iter().map(|t| format!("j{t}")).collect();
                    self.line(&format!("if (!({})) {{", c.join(" || ")));
                    open = true;
                }
                cur = active;
            }
            for o in ins.src.iter().chain(ins.dst.iter()) {
                self.touch(o);
            }
            self.emit_insn(i, ins, is_ps, target.get(&i).copied())
                .map_err(|e| format!("{e} [{} @ {:#x}]", ins.mnemonic, ins.offset))?;
        }
        if open {
            self.line("}");
        }
        Ok(())
    }

    /// Float source with VOP3 abs/neg modifiers applied.
    fn fm(&self, ins: &Insn, i: usize) -> Result<String, String> {
        let mut e = f(&ins.src[i])?;
        if ins.enc == Enc::Vop3 {
            if (ins.aux.mask >> i) & 1 != 0 {
                e = format!("abs({e})");
            }
            if (ins.aux.mask >> (3 + i)) & 1 != 0 {
                e = format!("(-{e})");
            }
        }
        Ok(e)
    }

    /// Wrap a float result with VOP3 omod/clamp and convert to uint bits.
    fn fres(&self, ins: &Insn, e: String) -> String {
        let mut e = e;
        if ins.enc == Enc::Vop3 {
            e = match ins.aux.imm2 {
                1 => format!("(({e}) * 2.0)"),
                2 => format!("(({e}) * 4.0)"),
                3 => format!("(({e}) * 0.5)"),
                _ => e,
            };
            if ins.aux.imm != 0 {
                e = format!("saturate({e})");
            }
        }
        format!("asuint({e})")
    }

    fn cmp_expr(m: &str, a: &Opnd, b: &Opnd) -> Result<String, String> {
        // m like "v_cmp_lt_f32" / "s_cmp_eq_i32"
        let parts: Vec<&str> = m.split('_').collect();
        let (cc, ty) = (parts[2], parts[3]);
        let (x, y) = match ty {
            "f32" => (f(a)?, f(b)?),
            "i32" => (format!("asint({})", r(a)?), format!("asint({})", r(b)?)),
            "u32" => (r(a)?, r(b)?),
            other => return Err(format!("compare type {other} unsupported")),
        };
        Ok(match cc {
            "f" => "false".into(),
            "lt" => format!("({x} < {y})"),
            "eq" => format!("({x} == {y})"),
            "le" => format!("({x} <= {y})"),
            "gt" => format!("({x} > {y})"),
            "lg" | "ne" => format!("({x} != {y})"),
            "ge" => format!("({x} >= {y})"),
            "o" => format!("({x} == {x} && {y} == {y})"),
            "u" => format!("({x} != {x} || {y} != {y})"),
            "nge" => format!("(!({x} >= {y}))"),
            "nlg" => format!("(!({x} != {y}))"),
            "ngt" => format!("(!({x} > {y}))"),
            "nle" => format!("(!({x} <= {y}))"),
            "neq" => format!("(!({x} == {y}))"),
            "nlt" => format!("(!({x} < {y}))"),
            "tru" | "t" => "true".into(),
            other => return Err(format!("compare cond {other} unsupported")),
        })
    }

    fn emit_insn(&mut self, idx: usize, ins: &Insn, is_ps: bool, br: Option<usize>) -> Result<(), String> {
        let m = ins.mnemonic.as_str();
        let s = &ins.src;
        let d = ins.dst.first().cloned();
        self.line(&format!("// {:#06x} {m}", ins.offset));
        match ins.enc {
            Enc::Sopp => {
                let cond = match ins.op {
                    2 => Some("true".to_string()),
                    4 => Some("scc == 0u".into()),
                    5 => Some("scc != 0u".into()),
                    6 => Some("(vcc_lo & 1u) == 0u".into()),
                    7 => Some("(vcc_lo & 1u) != 0u".into()),
                    8 => Some("(exec_lo & 1u) == 0u".into()),
                    9 => Some("(exec_lo & 1u) != 0u".into()),
                    0 | 1 | 0x0a | 0x0c | 0x1f | 0x20 | 0x21 | 0x23 | 0x24 | 0x25 => None,
                    _ => return Err("unsupported SOPP".into()),
                };
                if let (Some(c), Some(t)) = (cond, br) {
                    self.line(&format!("if ({c}) j{t} = true;"));
                }
                Ok(())
            }
            Enc::Vop2 | Enc::Vop1 | Enc::Vop3 | Enc::Vopc => self.emit_vec(ins),
            Enc::Sop2 => {
                let d = d.ok_or("no dst")?;
                let (a, b) = (r(&s[0])?, r(&s[1])?);
                let is64 = m.ends_with("_b64");
                let bit = |op: &str, a: &str, b: &str| -> Option<String> {
                    Some(match op {
                        "and" => format!("({a} & {b})"),
                        "or" => format!("({a} | {b})"),
                        "xor" => format!("({a} ^ {b})"),
                        "andn2" => format!("({a} & ~{b})"),
                        "orn2" => format!("({a} | ~{b})"),
                        "nand" => format!("(~({a} & {b}))"),
                        "nor" => format!("(~({a} | {b}))"),
                        "xnor" => format!("(~({a} ^ {b}))"),
                        _ => return None,
                    })
                };
                let opname = m.trim_start_matches("s_").trim_end_matches("_b32").trim_end_matches("_b64");
                if let Some(lo) = bit(opname, &a, &b) {
                    if is64 {
                        let (ah, bh) = (r(&hi(&s[0]))?, r(&hi(&s[1]))?);
                        let hi_e = bit(opname, &ah, &bh).unwrap();
                        let t = self.tmp;
                        self.tmp += 1;
                        self.line(&format!("uint t{t}l = {lo}; uint t{t}h = {hi_e};"));
                        self.ws(&d, &format!("t{t}l"))?;
                        let dh = hi(&d);
                        self.touch(&dh);
                        self.ws(&dh, &format!("t{t}h"))?;
                        self.line(&format!("scc = ((t{t}l | t{t}h) != 0u) ? 1u : 0u;"));
                    } else {
                        self.ws(&d, &lo)?;
                        self.line(&format!("scc = ({} != 0u) ? 1u : 0u;", name(&d)?));
                    }
                    return Ok(());
                }
                let e = match m {
                    "s_add_u32" | "s_add_i32" => format!("({a} + {b})"),
                    "s_sub_u32" | "s_sub_i32" => format!("({a} - {b})"),
                    "s_mul_i32" => format!("({a} * {b})"),
                    "s_lshl_b32" => format!("({a} << ({b} & 31u))"),
                    "s_lshr_b32" => format!("({a} >> ({b} & 31u))"),
                    "s_ashr_i32" => format!("asuint(asint({a}) >> ({b} & 31u))"),
                    "s_min_u32" => format!("min({a}, {b})"),
                    "s_max_u32" => format!("max({a}, {b})"),
                    "s_min_i32" => format!("asuint(min(asint({a}), asint({b})))"),
                    "s_max_i32" => format!("asuint(max(asint({a}), asint({b})))"),
                    "s_cselect_b32" => format!("((scc != 0u) ? {a} : {b})"),
                    "s_bfe_u32" => format!("(({a} >> ({b} & 31u)) & ((1u << (({b} >> 16) & 31u)) - 1u))"),
                    "s_lshl1_add_u32" => format!("(({a} << 1) + {b})"),
                    "s_lshl2_add_u32" => format!("(({a} << 2) + {b})"),
                    "s_lshl3_add_u32" => format!("(({a} << 3) + {b})"),
                    "s_lshl4_add_u32" => format!("(({a} << 4) + {b})"),
                    _ => return Err("unsupported SOP2".into()),
                };
                let scc = match m {
                    "s_add_u32" => Some(format!("(({a} + {b}) < {a}) ? 1u : 0u")),
                    "s_sub_u32" => Some(format!("({b} > {a}) ? 1u : 0u")),
                    "s_lshl_b32" | "s_lshr_b32" | "s_ashr_i32" | "s_bfe_u32" => Some("__NZ__".into()),
                    _ => None,
                };
                let t = self.tmp;
                self.tmp += 1;
                self.line(&format!("uint t{t} = {e};"));
                match scc {
                    Some(x) if x == "__NZ__" => self.line(&format!("scc = (t{t} != 0u) ? 1u : 0u;")),
                    Some(x) => self.line(&format!("scc = {x};")),
                    None => {}
                }
                self.ws(&d, &format!("t{t}"))
            }
            Enc::Sop1 => {
                let d = d.ok_or("no dst")?;
                let a = r(&s[0])?;
                match m {
                    "s_mov_b32" | "s_wqm_b32" => self.ws(&d, &a),
                    "s_mov_b64" | "s_wqm_b64" => {
                        let ah = r(&hi(&s[0]))?;
                        let t = self.tmp;
                        self.tmp += 1;
                        self.line(&format!("uint t{t}l = {a}; uint t{t}h = {ah};"));
                        self.ws(&d, &format!("t{t}l"))?;
                        let dh = hi(&d);
                        self.touch(&dh);
                        self.ws(&dh, &format!("t{t}h"))
                    }
                    "s_not_b32" => {
                        self.ws(&d, &format!("(~{a})"))?;
                        self.line(&format!("scc = ({} != 0u) ? 1u : 0u;", name(&d)?));
                        Ok(())
                    }
                    "s_cmov_b32" => self.ws(&d, &format!("((scc != 0u) ? {a} : {})", name(&d)?)),
                    "s_swappc_b64" => {
                        // Vertex-fetch shader call: inputs are materialised from the D3D input layout.
                        self.line("// (fetch-shader call: vertex attributes come from the input layout)");
                        Ok(())
                    }
                    _ if m.contains("saveexec") => {
                        let op = m.trim_start_matches("s_").split("_saveexec").next().unwrap_or("");
                        let e = match op {
                            "and" => format!("({a} & exec_lo)"),
                            "or" => format!("({a} | exec_lo)"),
                            "xor" => format!("({a} ^ exec_lo)"),
                            "andn2" => format!("({a} & ~exec_lo)"),
                            "orn2" => format!("({a} | ~exec_lo)"),
                            "andn1" => format!("(~{a} & exec_lo)"),
                            _ => return Err("unsupported saveexec".into()),
                        };
                        let t = self.tmp;
                        self.tmp += 1;
                        // Lane model: only bit 0 is meaningful.
                        self.line(&format!("uint t{t} = {e} & 1u;"));
                        self.ws(&d, "exec_lo")?;
                        if m.ends_with("_b64") {
                            let dh = hi(&d);
                            self.touch(&dh);
                            self.ws(&dh, "0u")?;
                        }
                        self.line(&format!("exec_lo = t{t}; scc = t{t};"));
                        Ok(())
                    }
                    _ => Err("unsupported SOP1".into()),
                }
            }
            Enc::Sopk => {
                let d = d.ok_or("no dst")?;
                let imm = format!("{:#x}u", ins.aux.imm as i32 as u32);
                match m {
                    "s_movk_i32" => self.ws(&d, &imm),
                    "s_addk_i32" => self.ws(&d, &format!("({} + {imm})", name(&d)?)),
                    "s_mulk_i32" => self.ws(&d, &format!("({} * {imm})", name(&d)?)),
                    _ if m.starts_with("s_cmpk_") => {
                        let fake = m.replace("s_cmpk_", "s_cmp_");
                        let c = Self::cmp_expr(&fake, &d, &Opnd::ConstInt(ins.aux.imm))?;
                        self.line(&format!("scc = {c} ? 1u : 0u;"));
                        Ok(())
                    }
                    _ => Err("unsupported SOPK".into()),
                }
            }
            Enc::Sopc => {
                let c = match m {
                    "s_bitcmp0_b32" => format!("((({} >> ({} & 31u)) & 1u) == 0u)", r(&s[0])?, r(&s[1])?),
                    "s_bitcmp1_b32" => format!("((({} >> ({} & 31u)) & 1u) != 0u)", r(&s[0])?, r(&s[1])?),
                    "s_cmp_eq_u64" => format!(
                        "({} == {} && {} == {})",
                        r(&s[0])?,
                        r(&s[1])?,
                        r(&hi(&s[0]))?,
                        r(&hi(&s[1]))?
                    ),
                    "s_cmp_lg_u64" => format!(
                        "({} != {} || {} != {})",
                        r(&s[0])?,
                        r(&s[1])?,
                        r(&hi(&s[0]))?,
                        r(&hi(&s[1]))?
                    ),
                    _ => Self::cmp_expr(m, &s[0], &s[1])?,
                };
                self.line(&format!("scc = {c} ? 1u : 0u;"));
                Ok(())
            }
            Enc::Smem => {
                if ins.op <= 4 {
                    self.line("// (descriptor fetch from root table — resolved statically)");
                    return Ok(());
                }
                let slot = match self.res.get(&idx).and_then(|v| v.first()) {
                    Some(Res::Cb(s)) => *s,
                    _ => return Err("unresolved constant buffer".into()),
                };
                let n = 1u32 << (ins.op - 8);
                let base = match d.ok_or("no dst")? {
                    Opnd::Sgpr(n) => n,
                    _ => return Err("bad dst".into()),
                };
                let w0 = ins.aux.imm as u32 / 4;
                for k in 0..n {
                    let w = w0 + k;
                    let dst = Opnd::Sgpr(base + k as u8);
                    self.touch(&dst);
                    self.ws(&dst, &format!("cb{slot}[{}].{}", w / 4, XYZW[(w % 4) as usize]))?;
                }
                Ok(())
            }
            Enc::Mimg => self.emit_mimg(idx, ins),
            Enc::Vinterp => {
                if !is_ps {
                    return Err("v_interp outside a pixel shader".into());
                }
                let attr = ins.aux.imm as usize;
                let chan = ins.aux.imm2 as usize;
                let e = *self.ps_attrs.get(attr).ok_or_else(|| format!("PS attr{attr} has no signature element"))?;
                let (_, comps) = sig_type(e);
                let val = if chan < comps {
                    let acc = format!("i.i{}.{}", e.register, XYZW[chan]);
                    if e.comp_type == 3 { format!("asuint({acc})") } else { format!("(uint)({acc})") }
                } else if chan == 3 {
                    "0x3f800000u".to_string()
                } else {
                    "0u".to_string()
                };
                // p1 computes a partial term in hardware; the full interpolated value is
                // already available per-pixel here, so both steps assign it.
                self.wv(&d.ok_or("no dst")?, &val)
            }
            Enc::Exp => self.emit_exp(ins, is_ps),
            _ => Err("unsupported encoding".into()),
        }
    }

    fn emit_vec(&mut self, ins: &Insn) -> Result<(), String> {
        let mut m = ins.mnemonic.clone();
        let s = &ins.src;
        let d = ins.dst.first().cloned().ok_or("no dst")?;
        if m == "v_nop" {
            return Ok(());
        }
        // Compares (VOPC, and VOP3 with a scalar mask destination).
        if m.starts_with("v_cmp_") {
            if m == "v_cmp_class_f32" {
                return Err("v_cmp_class unsupported".into());
            }
            let c = Self::cmp_expr(&m, &s[0], &s[1])?;
            self.ws(&d, &format!("(((exec_lo & 1u) != 0u) && {c}) ? 1u : 0u"))?;
            let dh = hi(&d);
            if !matches!(dh, Opnd::Null | Opnd::ConstInt(_)) {
                self.touch(&dh);
                self.ws(&dh, "0u")?;
            }
            return Ok(());
        }
        let vop3 = ins.enc == Enc::Vop3;
        let k = format!("asfloat({:#x}u)", ins.raw.last().copied().unwrap_or(0));
        let dn = name(&d)?;
        let fa = || self.fm(ins, 0);
        let fb = || self.fm(ins, 1);
        let fc = || self.fm(ins, 2);
        let (a, b) = (r(&s[0])?, s.get(1).map(r).transpose()?.unwrap_or_default());
        let c = s.get(2).map(|o| r(o)).transpose().unwrap_or(None).unwrap_or_default();
        if m.ends_with("_legacy_f32") {
            m = m.replace("_legacy", "");
        }
        let fexpr: Option<String> = match m.as_str() {
            "v_add_f32" => Some(format!("{} + {}", fa()?, fb()?)),
            "v_sub_f32" => Some(format!("{} - {}", fa()?, fb()?)),
            "v_subrev_f32" => Some(format!("{} - {}", fb()?, fa()?)),
            "v_mul_f32" => Some(format!("{} * {}", fa()?, fb()?)),
            "v_min_f32" => Some(format!("min({}, {})", fa()?, fb()?)),
            "v_max_f32" => Some(format!("max({}, {})", fa()?, fb()?)),
            "v_mac_f32" | "v_fmac_f32" => Some(format!("{} * {} + asfloat({dn})", fa()?, fb()?)),
            "v_madmk_f32" | "v_fmamk_f32" => Some(format!("{} * {k} + {}", fa()?, fb()?)),
            "v_madak_f32" | "v_fmaak_f32" => Some(format!("{} * {} + {k}", fa()?, fb()?)),
            "v_mad_f32" | "v_fma_f32" => Some(format!("{} * {} + {}", fa()?, fb()?, fc()?)),
            "v_min3_f32" => Some(format!("min(min({}, {}), {})", fa()?, fb()?, fc()?)),
            "v_max3_f32" => Some(format!("max(max({}, {}), {})", fa()?, fb()?, fc()?)),
            "v_med3_f32" => Some(format!(
                "max(min({0}, {1}), min(max({0}, {1}), {2}))",
                fa()?,
                fb()?,
                fc()?
            )),
            "v_cvt_f32_i32" => Some(format!("(float)asint({a})")),
            "v_cvt_f32_u32" => Some(format!("(float)({a})")),
            "v_cvt_f32_ubyte0" => Some(format!("(float)({a} & 0xffu)")),
            "v_cvt_f32_ubyte1" => Some(format!("(float)(({a} >> 8) & 0xffu)")),
            "v_cvt_f32_ubyte2" => Some(format!("(float)(({a} >> 16) & 0xffu)")),
            "v_cvt_f32_ubyte3" => Some(format!("(float)(({a} >> 24) & 0xffu)")),
            "v_cvt_f32_f16" => Some(format!("f16tof32({a})")),
            "v_fract_f32" => Some(format!("frac({})", fa()?)),
            "v_trunc_f32" => Some(format!("trunc({})", fa()?)),
            "v_ceil_f32" => Some(format!("ceil({})", fa()?)),
            "v_rndne_f32" => Some(format!("round({})", fa()?)),
            "v_floor_f32" => Some(format!("floor({})", fa()?)),
            "v_exp_f32" => Some(format!("exp2({})", fa()?)),
            "v_log_f32" => Some(format!("log2({})", fa()?)),
            "v_rcp_f32" | "v_rcp_iflag_f32" => Some(format!("1.0 / {}", fa()?)),
            "v_rsq_f32" => Some(format!("rsqrt({})", fa()?)),
            "v_sqrt_f32" => Some(format!("sqrt({})", fa()?)),
            "v_sin_f32" => Some(format!("sin({} * 6.28318530718)", fa()?)),
            "v_cos_f32" => Some(format!("cos({} * 6.28318530718)", fa()?)),
            _ => None,
        };
        if let Some(e) = fexpr {
            let e = self.fres(ins, e);
            return self.wv(&d, &e);
        }
        let mask = if vop3 { c.clone() } else { "vcc_lo".to_string() };
        let e = match m.as_str() {
            "v_mov_b32" => a,
            "v_cndmask_b32" => format!("(({mask} & 1u) != 0u) ? {b} : {a}"),
            "v_and_b32" => format!("({a} & {b})"),
            "v_or_b32" => format!("({a} | {b})"),
            "v_xor_b32" => format!("({a} ^ {b})"),
            "v_xnor_b32" => format!("(~({a} ^ {b}))"),
            "v_not_b32" => format!("(~{a})"),
            "v_lshrrev_b32" => format!("({b} >> ({a} & 31u))"),
            "v_lshlrev_b32" => format!("({b} << ({a} & 31u))"),
            "v_ashrrev_i32" => format!("asuint(asint({b}) >> ({a} & 31u))"),
            "v_add_nc_u32" => format!("({a} + {b})"),
            "v_sub_nc_u32" => format!("({a} - {b})"),
            "v_subrev_nc_u32" => format!("({b} - {a})"),
            "v_min_u32" => format!("min({a}, {b})"),
            "v_max_u32" => format!("max({a}, {b})"),
            "v_min_i32" => format!("asuint(min(asint({a}), asint({b})))"),
            "v_max_i32" => format!("asuint(max(asint({a}), asint({b})))"),
            "v_mul_u32_u24" => format!("(({a} & 0xffffffu) * ({b} & 0xffffffu))"),
            "v_mul_i32_i24" => format!("asuint((asint({a} << 8) >> 8) * (asint({b} << 8) >> 8))"),
            "v_mul_lo_u32" | "v_mul_lo_i32" => format!("({a} * {b})"),
            "v_mad_u32_u24" => format!("(({a} & 0xffffffu) * ({b} & 0xffffffu) + {c})"),
            "v_mad_i32_i24" => format!("asuint((asint({a} << 8) >> 8) * (asint({b} << 8) >> 8) + asint({c}))"),
            "v_bfe_u32" => format!("(({a} >> ({b} & 31u)) & ((1u << ({c} & 31u)) - 1u))"),
            "v_bfi_b32" => format!("(({a} & {b}) | (~{a} & {c}))"),
            "v_lshl_add_u32" => format!("(({a} << ({b} & 31u)) + {c})"),
            "v_add_lshl_u32" => format!("(({a} + {b}) << ({c} & 31u))"),
            "v_add3_u32" => format!("({a} + {b} + {c})"),
            "v_lshl_or_b32" => format!("(({a} << ({b} & 31u)) | {c})"),
            "v_and_or_b32" => format!("(({a} & {b}) | {c})"),
            "v_or3_b32" => format!("({a} | {b} | {c})"),
            "v_xor3_b32" => format!("({a} ^ {b} ^ {c})"),
            "v_cvt_u32_f32" => format!("(uint)max({}, 0.0)", fa()?),
            "v_cvt_i32_f32" => format!("asuint((int){})", fa()?),
            "v_cvt_flr_i32_f32" => format!("asuint((int)floor({}))", fa()?),
            "v_cvt_rpi_i32_f32" => format!("asuint((int)floor({} + 0.5))", fa()?),
            "v_cvt_f16_f32" => format!("f32tof16({})", fa()?),
            "v_pack_b32_f16" => format!("(f32tof16({}) | (f32tof16({}) << 16))", fa()?, fb()?),
            _ => return Err("unsupported vector op".into()),
        };
        self.wv(&d, &e)
    }

    fn emit_mimg(&mut self, idx: usize, ins: &Insn) -> Result<(), String> {
        let rs = self.res.get(&idx).cloned().ok_or("unresolved image")?;
        // Texel-addressed ops (no sampler): image_load / image_store, on SRVs or UAVs.
        if matches!(ins.mnemonic.as_str(), "image_load" | "image_store") {
            let (slot, dim, is_uav) = match rs.first() {
                Some(Res::Tex(t, d)) => (*t, *d, false),
                Some(Res::Uav(t, d)) => (*t, *d, true),
                _ => return Err("unresolved image".into()),
            };
            let ncoord = match dim {
                1 => 2,
                2 | 5 => 3,
                _ => return Err(format!("image dim {dim} unsupported for load/store")),
            };
            let nsa = ins.aux.imm2 as usize;
            let first = match ins.src[0] {
                Opnd::Vgpr(n) => n,
                _ => return Err("bad vaddr".into()),
            };
            let mut c = vec![];
            for i in 0..ncoord {
                let o = if nsa > 0 { ins.src[i] } else { Opnd::Vgpr(first + i as u8) };
                self.touch(&o);
                c.push(r(&o)?);
            }
            let vd = match ins.dst[0] {
                Opnd::Vgpr(n) => n,
                _ => return Err("bad vdata".into()),
            };
            if ins.mnemonic == "image_store" {
                if !is_uav {
                    return Err("image_store to a non-UAV table".into());
                }
                let mut comp = vec![];
                let mut n = 0u8;
                for ch in 0..4 {
                    if (ins.aux.mask >> ch) & 1 != 0 {
                        let o = Opnd::Vgpr(vd + n);
                        self.touch(&o);
                        comp.push(f(&o)?);
                        n += 1;
                    } else {
                        comp.push("0.0".into());
                    }
                }
                self.line(&format!(
                    "if (exec_lo & 1u) uav{slot}[uint{ncoord}({})] = float4({});",
                    c.join(", "),
                    comp.join(", ")
                ));
            } else {
                let t = self.tmp;
                self.tmp += 1;
                let e = if is_uav {
                    format!("uav{slot}[uint{ncoord}({})]", c.join(", "))
                } else {
                    format!("tex{slot}.Load(int{}({}, 0))", ncoord + 1, c.iter().map(|x| format!("(int){x}")).collect::<Vec<_>>().join(", "))
                };
                self.line(&format!("float4 t{t} = {e};"));
                let mut n = 0u8;
                for ch in 0..4 {
                    if (ins.aux.mask >> ch) & 1 != 0 {
                        self.wv(&Opnd::Vgpr(vd + n), &format!("asuint(t{t}.{})", XYZW[ch]))?;
                        n += 1;
                    }
                }
            }
            return Ok(());
        }
        let (tslot, dim) = match rs.first() {
            Some(Res::Tex(t, d)) => (*t, *d),
            _ => return Err("unresolved image".into()),
        };
        let sslot = match rs.get(1) {
            Some(Res::Samp(s)) => *s,
            _ => return Err("image op without sampler unsupported".into()),
        };
        let ncoord = match dim {
            1 => 2,
            2 | 3 | 5 => 3,
            _ => return Err(format!("image dim {dim} unsupported")),
        };
        // Address operands: explicit list with NSA, else consecutive VGPRs from vaddr.
        let nsa = ins.aux.imm2 as usize;
        let first = match ins.src[0] {
            Opnd::Vgpr(n) => n,
            _ => return Err("bad vaddr".into()),
        };
        let addr = |i: usize| -> Opnd {
            if nsa > 0 { ins.src[i] } else { Opnd::Vgpr(first + i as u8) }
        };
        let (pre, post) = match ins.mnemonic.as_str() {
            "image_sample" | "image_sample_lz" => (0usize, 0usize),
            "image_sample_l" => (0, 1),
            "image_sample_b" => (1, 0),
            _ => return Err("unsupported image op".into()),
        };
        let mut coords = vec![];
        for i in 0..ncoord {
            let o = addr(pre + i);
            self.touch(&o);
            coords.push(f(&o)?);
        }
        let cvec = format!("float{ncoord}({})", coords.join(", "));
        let call = match ins.mnemonic.as_str() {
            "image_sample" => format!("tex{tslot}.Sample(samp{sslot}, {cvec})"),
            "image_sample_lz" => format!("tex{tslot}.SampleLevel(samp{sslot}, {cvec}, 0.0)"),
            "image_sample_l" => {
                let o = addr(pre + ncoord);
                self.touch(&o);
                format!("tex{tslot}.SampleLevel(samp{sslot}, {cvec}, {})", f(&o)?)
            }
            _ => {
                let o = addr(0);
                self.touch(&o);
                format!("tex{tslot}.SampleBias(samp{sslot}, {cvec}, {})", f(&o)?)
            }
        };
        let _ = post;
        let t = self.tmp;
        self.tmp += 1;
        self.line(&format!("float4 t{t} = {call};"));
        let vd = match ins.dst[0] {
            Opnd::Vgpr(n) => n,
            _ => return Err("bad vdata".into()),
        };
        let mut n = 0u8;
        for ch in 0..4 {
            if (ins.aux.mask >> ch) & 1 != 0 {
                let dst = Opnd::Vgpr(vd + n);
                self.wv(&dst, &format!("asuint(t{t}.{})", XYZW[ch]))?;
                n += 1;
            }
        }
        Ok(())
    }

    fn emit_exp(&mut self, ins: &Insn, is_ps: bool) -> Result<(), String> {
        let target = ins.aux.imm as u32;
        let en = ins.aux.mask;
        let compr = ins.aux.imm2 & 1 != 0;
        let mut comp = vec![];
        if compr {
            for i in 0..4usize {
                let src = &ins.src[i / 2];
                let on = (en >> (i / 2 * 2)) & 1 != 0 || (en >> i) & 1 != 0;
                comp.push(if on {
                    format!("f16tof32(({} >> {}) & 0xffffu)", r(src)?, (i % 2) * 16)
                } else {
                    "0.0".into()
                });
            }
        } else {
            for i in 0..4usize {
                comp.push(if (en >> i) & 1 != 0 { f(&ins.src[i])? } else { "0.0".into() });
            }
        }
        let v4 = format!("float4({})", comp.join(", "));
        let (field, comps) = if is_ps {
            if target > 7 {
                return Err(format!("PS export target {target} unsupported"));
            }
            let e = self
                .ex
                .output_sig
                .iter()
                .find(|e| e.sysval == 64 && e.index == target)
                .ok_or_else(|| format!("no SV_Target{target} in output signature"))?;
            (format!("o.o{}", e.register), sig_type(e))
        } else if (12..=15).contains(&target) {
            if target != 12 {
                return Err(format!("position export {target} unsupported"));
            }
            let e = self.ex.output_sig.iter().find(|e| e.sysval == 1).ok_or("no SV_Position output")?;
            (format!("o.o{}", e.register), sig_type(e))
        } else if (32..64).contains(&target) {
            let e = *self
                .out_params
                .get((target - 32) as usize)
                .ok_or_else(|| format!("param export {} has no output signature element", target - 32))?;
            (format!("o.o{}", e.register), sig_type(e))
        } else {
            return Err(format!("export target {target} unsupported"));
        };
        let (ty, n) = comps;
        let sw = &"xyzw"[..n];
        self.line(&format!("if (exec_lo & 1u) {field} = ({ty})({v4}.{sw});"));
        Ok(())
    }

    fn finish(&mut self, is_ps: bool) -> Result<String, String> {
        let ex = self.ex;
        let mut h = String::new();
        let _ = writeln!(h, "// Generated by tombox-xshader from RDNA2 microcode ({:?}, {} instrs).", ex.stage, self.insns.len());
        for (slot, dwords) in &self.cb_size {
            let _ = writeln!(h, "cbuffer CB{slot} : register(b{slot}) {{ uint4 cb{slot}[{}]; }};", dwords.div_ceil(4).max(1));
        }
        for (slot, dim) in &self.tex {
            let ty = match dim {
                1 => "Texture2D<float4>",
                2 => "Texture3D<float4>",
                3 => "TextureCube<float4>",
                5 => "Texture2DArray<float4>",
                _ => return Err(format!("texture dim {dim} unsupported")),
            };
            let _ = writeln!(h, "{ty} tex{slot} : register(t{slot});");
        }
        for slot in &self.samp {
            let _ = writeln!(h, "SamplerState samp{slot} : register(s{slot});");
        }
        for (slot, dim) in &self.uav {
            let ty = match dim {
                1 => "RWTexture2D<float4>",
                2 => "RWTexture3D<float4>",
                5 => "RWTexture2DArray<float4>",
                _ => return Err(format!("UAV dim {dim} unsupported")),
            };
            let _ = writeln!(h, "{ty} uav{slot} : register(u{slot});");
        }
        if self.is_cs {
            // Compute ABI (from ClearCS + FSSM metadata): local thread id in v0..v2, the
            // thread-group id in the three SGPRs following the user-data (root-parameter)
            // registers, thread-group size in the ISA preamble dwords 1..3.
            let p = ex.isa_preamble;
            let (tx, ty_, tz) = (p[1].max(1), p[2].max(1), p[3].max(1));
            let nparams = ex.root_sig.as_ref().map(|r| r.params.len()).unwrap_or(4);
            let g = 4 + nparams;
            let _ = writeln!(h, "[numthreads({tx}, {ty_}, {tz})]
void main(uint3 gid : SV_GroupID, uint3 gtid : SV_GroupThreadID) {{");
            let _ = writeln!(h, "    uint exec_lo = 1u, exec_hi = 0u, vcc_lo = 0u, vcc_hi = 0u, scc = 0u, m0 = 0u, nul = 0u;");
            for r in ["v0", "v1", "v2"] {
                self.regs.insert(r.to_string());
            }
            for k in 0..3 {
                self.regs.insert(format!("s{}", g + k));
            }
            let decl: Vec<String> = self.regs.iter().map(|r| format!("{r} = 0u")).collect();
            let _ = writeln!(h, "    uint {};", decl.join(", "));
            let _ = writeln!(h, "    v0 = gtid.x; v1 = gtid.y; v2 = gtid.z;");
            let _ = writeln!(h, "    s{} = gid.x; s{} = gid.y; s{} = gid.z;", g, g + 1, g + 2);
            h.push_str(&self.body);
            h.push_str("}
");
            return Ok(h);
        }
        let _ = writeln!(h, "struct SIn {{");
        for e in &ex.input_sig {
            let (ty, _) = sig_type(e);
            let interp = if is_ps && e.comp_type != 3 { "nointerpolation " } else { "" };
            let _ = writeln!(h, "    {interp}{ty} i{} : {};", e.register, semantic(e));
        }
        let _ = writeln!(h, "}};\nstruct SOut {{");
        for e in &ex.output_sig {
            let (ty, _) = sig_type(e);
            let _ = writeln!(h, "    {ty} o{} : {};", e.register, semantic(e));
        }
        let _ = writeln!(h, "}};\nSOut main(SIn i) {{\n    SOut o = (SOut)0;");
        let _ = writeln!(h, "    uint exec_lo = 1u, exec_hi = 0u, vcc_lo = 0u, vcc_hi = 0u, scc = 0u, m0 = 0u, nul = 0u;");
        // Vertex attributes arrive in v[4 + 4*register + component] after the fetch call.
        let mut init = String::new();
        if !is_ps {
            for e in &ex.input_sig {
                let (_, comps) = sig_type(e);
                for c in 0..comps {
                    let v = format!("v{}", 4 + 4 * e.register as usize + c);
                    let acc = format!("i.i{}.{}", e.register, XYZW[c]);
                    let val = if e.comp_type == 3 { format!("asuint({acc})") } else { format!("(uint)({acc})") };
                    self.regs.insert(v.clone());
                    let _ = writeln!(init, "    {v} = {val};");
                }
                // Unsupplied components default to (0,0,0,1) like the hardware fetch.
                for c in comps..4 {
                    let v = format!("v{}", 4 + 4 * e.register as usize + c);
                    self.regs.insert(v.clone());
                    let _ = writeln!(init, "    {v} = {};", if c == 3 { "0x3f800000u" } else { "0u" });
                }
            }
        }
        if !self.regs.is_empty() {
            let decl: Vec<String> = self.regs.iter().map(|r| format!("{r} = 0u")).collect();
            let _ = writeln!(h, "    uint {};", decl.join(", "));
        }
        h.push_str(&init);
        h.push_str(&self.body);
        h.push_str("    return o;\n}\n");
        Ok(h)
    }
}
