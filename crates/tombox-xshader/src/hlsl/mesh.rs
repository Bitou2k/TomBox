use super::*;

impl Emitter<'_> {
    pub(super) fn mesh_sendmsg(&mut self, ins: &Insn) -> Result<(), String> {
        if ins.aux.imm as u32 & 0xffff != 9 {
            return Err(format!("unsupported mesh sendmsg {}", ins.aux.imm));
        }
        self.line("tb_vertex_count = m0 & 0xfffu; tb_primitive_count = (m0 >> 12) & 0xfffu;");
        self.line("SetMeshOutputCounts(tb_vertex_count, tb_primitive_count);");
        Ok(())
    }

    pub(super) fn mesh_exp(&mut self, ins: &Insn) -> Result<(), String> {
        if ins.aux.imm2 & 1 != 0 {
            return Err("compressed mesh export unsupported".into());
        }
        let target = ins.aux.imm as u32;
        let Some(crate::meta::MeshPipelineInfo::Mesh { max_vertices, max_primitives, .. }) = self.ex.mesh_info.as_ref() else {
            return Err("mesh export requires PSV output bounds".into());
        };
        let vertex_guard = format!("(exec_lo & 1u) && tid < tb_vertex_count && tid < {max_vertices}u");
        let primitive_guard = format!("(exec_lo & 1u) && tid < tb_primitive_count && tid < {max_primitives}u");
        if target == 20 {
            if ins.aux.mask != 1 {
                return Err("mesh primitive export requires one packed source".into());
            }
            let packed = r(&ins.src[0])?;
            self.line(&format!("if ({primitive_guard}) {{ tb_indices[tid] = uint3(({packed}) & 1023u, (({packed}) >> 10) & 1023u, (({packed}) >> 20) & 1023u); tb_primitives[tid].culled = (({packed}) >> 31) != 0u; }}"));
            return Ok(());
        }
        if target != 12 && !(32..64).contains(&target) {
            return Err(format!("mesh export target {target} unsupported"));
        }
        for component in 0..4 {
            if ins.aux.mask >> component & 1 == 0 { continue; }
            let source = r(&ins.src[component])?;
            if target == 12 {
                self.line(&format!("if ({vertex_guard}) tb_vertices[tid].position.{} = asfloat({source});", XYZW[component]));
            } else if let Some((ei, ci)) = sig_lookup(&self.ex.output_sig, target - 32, component) {
                let element = &self.ex.output_sig[ei];
                let (_, count) = sig_type(element);
                let suffix = if count == 1 { String::new() } else { format!(".{}", XYZW[ci]) };
                let value = match element.comp_type { 1 => source, 2 => format!("asint({source})"), _ => format!("asfloat({source})") };
                self.line(&format!("if ({vertex_guard}) tb_vertices[tid].o{ei}{suffix} = {value};"));
            } else {
                return Err(format!("mesh export param {} component {component} has no signature", target - 32));
            }
        }
        Ok(())
    }

    pub(super) fn mesh_finish(&mut self, h: &mut String) -> Result<(), String> {
        if let Some(register) = self.live.iter().find(|r| r.starts_with('v')) {
            return Err(format!("mesh initial VGPR ABI unresolved: {register}"));
        }
        if let Some(register) = self.live.iter().find(|r| r.starts_with('s')) {
            return Err(format!("mesh initial SGPR ABI unresolved: {register}"));
        }
        let Some(crate::meta::MeshPipelineInfo::Mesh { threads: Some(threads), payload_bytes: 0, max_vertices, max_primitives, topology: "triangle" }) = self.ex.mesh_info.as_ref() else {
            return Err("mesh requires PSV thread dimensions, triangle topology and zero payload".into());
        };
        if *threads != [128, 1, 1] || *max_vertices == 0 || *max_vertices > 128 || *max_primitives == 0 || *max_primitives > 128 {
            return Err("mesh dimensions outside verified 128-thread ABI".into());
        }
        let (vertices, primitives) = (*max_vertices, *max_primitives);
        if self.insns.iter().enumerate().any(|(index, ins)| {
            ins.enc == Enc::Sopp && (Self::branch_cond(ins.op).is_some() || (ins.op == 1 && index + 1 < self.insns.len()))
        }) {
            return Err("mesh control flow requires verified uniform allocation dominance".into());
        }
        let allocation_count = self.insns.iter().filter(|i| i.enc == Enc::Sopp && i.op == 0x10 && i.aux.imm as u32 & 0xffff == 9).count();
        if allocation_count != 1 { return Err("mesh requires exactly one GS_ALLOC_REQ".into()); }
        let allocation = self.insns.iter().position(|i| i.enc == Enc::Sopp && i.op == 0x10).unwrap();
        let preceding = allocation.checked_sub(1).and_then(|index| self.insns.get(index));
        let count = preceding.and_then(|ins| {
            if ins.mnemonic != "s_mov_b32" || ins.dst.first() != Some(&Opnd::M0) { return None; }
            match ins.src.first()? { Opnd::Literal(value) => Some(*value), Opnd::ConstInt(value) => Some(*value as u32), _ => None }
        }).ok_or("mesh output counts require a verified uniform immediate allocation")?;
        let (allocated_vertices, allocated_primitives) = (count & 0xfff, (count >> 12) & 0xfff);
        if allocated_vertices > vertices as u32 || allocated_primitives > primitives as u32 || (allocated_vertices == 0) != (allocated_primitives == 0) {
            return Err("mesh output allocation exceeds PSV bounds or has mismatched zero counts".into());
        }
        if self.insns[..allocation].iter().any(|ins| ins.enc == Enc::Exp) {
            return Err("mesh output export precedes allocation".into());
        }
        h.push_str("struct TbMeshVertex { float4 position : SV_Position;\n");
        for (ei, e) in self.ex.output_sig.iter().enumerate() {
            if e.sysval == 1 { continue; }
            if e.sysval != 0 { return Err(format!("mesh system output {} unsupported", e.sysval)); }
            let _ = writeln!(h, "{} o{ei} : {};", sig_type(e).0, semantic(e));
        }
        h.push_str("};\nstruct TbMeshPrimitive { bool culled : SV_CullPrimitive; };\n");
        if self.uses_lds { h.push_str("groupshared uint tb_lds[8192];\n"); }
        let _ = writeln!(h, "[numthreads(128, 1, 1)]\n[outputtopology(\"triangle\")]\nvoid main(uint tid : SV_GroupIndex, uint3 gid : SV_GroupID, out vertices TbMeshVertex tb_vertices[{vertices}], out indices uint3 tb_indices[{primitives}], out primitives TbMeshPrimitive tb_primitives[{primitives}]) {{");
        h.push_str("uint exec_lo = 1u, exec_hi = 0u, vcc_lo = 0u, vcc_hi = 0u, scc = 0u, m0 = 0u, nul = 0u;\n");
        h.push_str("uint tb_vertex_count = 0u, tb_primitive_count = 0u;\n");
        h.push_str(&self.reg_decls());
        h.push_str(&self.body);
        h.push_str("}\n");
        Ok(())
    }
}
