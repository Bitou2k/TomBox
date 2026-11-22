use crate::meta::PsvResource;
use crate::psocache::{BindingLayout, UserDataKind};
use crate::Stage;

const FIRST_EXTERNAL_SLOT: u32 = 0xff00_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Binding {
    pub slot: u32,
    pub kind: u32,
    pub resource_type: u32,
}

#[derive(PartialEq, Eq)]
pub(super) struct Bindings {
    slots: Vec<(u32, u32)>,
    descriptors: Vec<(u8, u32, Binding)>,
    widths: Vec<(u8, u32, u8)>,
    direct: Vec<(u8, Binding)>,
    samplers: Vec<([u32; 4], Binding)>,
}

impl Bindings {
    pub fn from_layout(layout: &BindingLayout, resources: &[PsvResource], stage: Stage) -> Self {
        let mut slots = Vec::new();
        let mut descriptors = Vec::new();
        let mut widths = Vec::new();
        let mut direct = Vec::new();
        let mut samplers = Vec::new();
        for entry in &layout.entries {
            let (kind, types): (u32, &[u32]) = match entry.kind {
                UserDataKind::ShaderResource => (0, &[3, 4, 5]),
                UserDataKind::UnorderedAccess => (1, &[6, 7, 8, 9]),
                UserDataKind::ConstantBuffer => (2, &[2]),
                UserDataKind::Sampler => (3, &[1]),
                UserDataKind::Other(0x100) if entry.descriptor_dwords() == 2
                    && entry.byte_offset == 0 && entry.raw_fields[0] == 0 => (0, &[4]),
                _ => continue,
            };
            let register = entry.register;
            let matching: Vec<_> = resources.iter().filter(|resource| {
                resource.lower <= register && register <= resource.upper
                    && types.contains(&resource.res_type)
            }).collect();
            let hinted: Vec<_> = matching.iter().copied()
                .filter(|resource| resource.space == entry.register_space()).collect();
            let selected = if hinted.len() == 1 { hinted[0] }
                else if matching.len() == 1 { matching[0] }
                else { continue };
            let (space, resource_type) = (selected.space, selected.res_type);
            let address = (register, space);
            let slot = match slots.iter().position(|other| *other == address) {
                Some(slot) => slot,
                None => {
                    let slot = slots.len();
                    slots.push(address);
                    slot
                }
            };
            let Some(table) = entry.user_sgpr().checked_add(if stage == Stage::Mesh { 8 } else { 0 }) else { continue };
            let binding = Binding {
                slot: FIRST_EXTERNAL_SLOT + slot as u32,
                kind,
                resource_type,
            };
            if let Some(words) = entry.static_sampler_words() {
                if !samplers.contains(&(words, binding)) {
                    samplers.push((words, binding));
                }
                continue;
            }
            if matches!(entry.kind, UserDataKind::Other(0x100)) {
                if !direct.contains(&(table, binding)) {
                    direct.push((table, binding));
                }
                continue;
            }
            let record = (table, entry.byte_offset, binding);
            if !descriptors.contains(&record) {
                descriptors.push(record);
            }
            let width = (table, entry.byte_offset, entry.descriptor_dwords());
            if !widths.contains(&width) {
                widths.push(width);
            }
        }
        Self { slots, descriptors, widths, direct, samplers }
    }

    pub fn get(&self, table: u8, offset: u32, allowed_types: &[u32]) -> Result<Binding, String> {
        let mut candidates = self.descriptors.iter().filter_map(|(candidate_table, candidate_offset, binding)| {
            (*candidate_table == table && *candidate_offset == offset
                && allowed_types.contains(&binding.resource_type)).then_some(*binding)
        });
        let Some(binding) = candidates.next() else {
            return Err(format!("PSO bindings: no matching descriptor for s{table}+{offset:#x}"));
        };
        if candidates.next().is_some() {
            return Err(format!("PSO bindings: ambiguous descriptor for s{table}+{offset:#x}"));
        }
        Ok(binding)
    }

    pub fn address(&self, slot: u32) -> Option<(u32, u32)> {
        let index = slot.checked_sub(FIRST_EXTERNAL_SLOT)? as usize;
        self.slots.get(index).copied()
    }

    pub fn candidates(&self, table: u8, dwords: u32) -> Vec<(u8, u32)> {
        self.widths.iter().filter_map(|&(candidate_table, offset, width)| {
            (table == candidate_table && u32::from(width) == dwords).then_some((table, offset))
        }).collect()
    }

    pub fn direct(&self, table: u8) -> Option<Binding> {
        let mut matching = self.direct.iter().filter_map(|(candidate, binding)| (*candidate == table).then_some(*binding));
        let result = matching.next()?;
        matching.next().is_none().then_some(result)
    }

    pub fn sampler(&self, words: [u32; 4]) -> Option<Binding> {
        let mut matching = self.samplers.iter().filter_map(|(candidate, binding)| (*candidate == words).then_some(*binding));
        let result = matching.next()?;
        matching.next().is_none().then_some(result)
    }
}
