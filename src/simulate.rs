use crate::model::{
    DefinitionRecord, LayoutDump, MAX_DEFINITION_DEPTH, MemberRecord, ShaderParameterBaseType,
};
use anyhow::{Context, Result, bail};
use std::collections::{HashMap, HashSet};

// UE shader parameter structs are padded to 16 bytes
pub const STRUCT_ALIGNMENT: u32 = 16;
// constant arrays use 16-byte slots, resource arrays use 8-byte pointers
const ARRAY_ELEMENT_ALIGNMENT: u32 = 16;
const RESOURCE_POINTER_SIZE: u32 = 8;
const RESOURCE_POINTER_ALIGNMENT: u32 = 8;
// uniform buffer bindings use 16-byte wrappers
const BINDING_WRAPPER_SIZE: u32 = 16;

#[derive(Clone, Debug)]
pub struct SimulatedDefinition {
    pub member_offsets: Vec<u32>,
    pub size: u32,
}

pub fn simulate_definition(
    definitions: &HashMap<u32, DefinitionRecord>,
    definition_id: u32,
) -> Result<SimulatedDefinition> {
    let mut memo = HashMap::new();
    let mut in_progress = HashSet::new();
    simulate_into(definitions, definition_id, &mut memo, &mut in_progress, 0)
}

pub fn verify_definition(
    definitions: &HashMap<u32, DefinitionRecord>,
    definition_id: u32,
) -> Result<()> {
    let definition = definitions
        .get(&definition_id)
        .with_context(|| format!("missing definition {definition_id}"))?;
    compare_definition(definitions, definition)
}

pub fn verify_dump(dump: &LayoutDump) -> (usize, Vec<String>) {
    let mut ids: Vec<u32> = dump.definitions.keys().copied().collect();
    ids.sort_unstable();
    let mut exact = 0usize;
    let mut failures = Vec::new();
    for id in ids {
        let definition = &dump.definitions[&id];
        match compare_definition(&dump.definitions, definition) {
            Ok(()) => exact += 1,
            Err(error) => failures.push(format!("{}: {error:#}", definition.type_name)),
        }
    }
    (exact, failures)
}

fn compare_definition(
    definitions: &HashMap<u32, DefinitionRecord>,
    definition: &DefinitionRecord,
) -> Result<()> {
    let simulated = simulate_definition(definitions, definition.id)?;
    for (member, simulated_offset) in definition.members.iter().zip(&simulated.member_offsets) {
        if member.offset != *simulated_offset {
            bail!(
                "member '{}' captured offset 0x{:X}, simulated 0x{:X}",
                member.name,
                member.offset,
                simulated_offset
            );
        }
    }
    if definition.size != simulated.size {
        bail!(
            "captured size {}, simulated {}",
            definition.size,
            simulated.size
        );
    }
    Ok(())
}

fn simulate_into(
    definitions: &HashMap<u32, DefinitionRecord>,
    definition_id: u32,
    memo: &mut HashMap<u32, SimulatedDefinition>,
    in_progress: &mut HashSet<u32>,
    depth: usize,
) -> Result<SimulatedDefinition> {
    if let Some(existing) = memo.get(&definition_id) {
        return Ok(existing.clone());
    }
    if depth >= MAX_DEFINITION_DEPTH {
        bail!("shader metadata nesting exceeded {MAX_DEFINITION_DEPTH} definitions");
    }
    if !in_progress.insert(definition_id) {
        bail!("recursive shader metadata cycle at definition {definition_id}");
    }
    let definition = definitions
        .get(&definition_id)
        .with_context(|| format!("missing definition {definition_id}"))?;
    let mut cursor = 0u32;
    let mut member_offsets = Vec::with_capacity(definition.members.len());
    for member in &definition.members {
        let (size, alignment) = member_layout(definitions, member, memo, in_progress, depth)
            .with_context(|| format!("member '{}'", member.name))?;
        // use the real size so a scalar can pack into a float3 tail
        let offset = cursor
            .checked_next_multiple_of(alignment)
            .and_then(|offset| offset.checked_add(size).map(|next| (offset, next)));
        let Some((offset, next)) = offset else {
            bail!("member '{}' overflows the struct layout", member.name);
        };
        member_offsets.push(offset);
        cursor = next;
    }
    let size = if definition.members.is_empty() {
        0
    } else {
        cursor
            .checked_next_multiple_of(STRUCT_ALIGNMENT)
            .context("struct size overflowed")?
    };
    let simulated = SimulatedDefinition {
        member_offsets,
        size,
    };
    in_progress.remove(&definition_id);
    memo.insert(definition_id, simulated.clone());
    Ok(simulated)
}

fn member_layout(
    definitions: &HashMap<u32, DefinitionRecord>,
    member: &MemberRecord,
    memo: &mut HashMap<u32, SimulatedDefinition>,
    in_progress: &mut HashSet<u32>,
    depth: usize,
) -> Result<(u32, u32)> {
    let is_resource_pointer = matches!(
        member.base_type,
        ShaderParameterBaseType::Texture
            | ShaderParameterBaseType::Srv
            | ShaderParameterBaseType::Uav
            | ShaderParameterBaseType::Sampler
            | ShaderParameterBaseType::RdgTexture
            | ShaderParameterBaseType::RdgTextureSrv
            | ShaderParameterBaseType::RdgTextureNonPixelSrv
            | ShaderParameterBaseType::RdgTextureUav
            | ShaderParameterBaseType::RdgBufferSrv
            | ShaderParameterBaseType::RdgBufferUav
            | ShaderParameterBaseType::ResourceCollection
    );
    let (size, alignment) = if is_resource_pointer {
        (RESOURCE_POINTER_SIZE, RESOURCE_POINTER_ALIGNMENT)
    } else {
        match member.base_type {
            ShaderParameterBaseType::Bool => (4, 4),
            ShaderParameterBaseType::Int32
            | ShaderParameterBaseType::Uint32
            | ShaderParameterBaseType::Float32 => scalar_layout(member)?,
            ShaderParameterBaseType::NestedStruct | ShaderParameterBaseType::IncludedStruct => {
                if member.nested_definition_id == 0 {
                    bail!("struct member has no nested definition");
                }
                let nested = simulate_into(
                    definitions,
                    member.nested_definition_id,
                    memo,
                    in_progress,
                    depth + 1,
                )?;
                (nested.size, STRUCT_ALIGNMENT)
            }
            ShaderParameterBaseType::ReferencedStruct
            | ShaderParameterBaseType::RdgUniformBuffer => (BINDING_WRAPPER_SIZE, STRUCT_ALIGNMENT),
            other => bail!(
                "base type {} is not covered by the packing model",
                other.dump_name()
            ),
        }
    };
    if member.elements > 0 {
        if is_resource_pointer {
            let total = RESOURCE_POINTER_SIZE
                .checked_mul(member.elements)
                .context("resource array size overflowed")?;
            return Ok((total, RESOURCE_POINTER_ALIGNMENT));
        }
        let stride = size
            .checked_next_multiple_of(ARRAY_ELEMENT_ALIGNMENT)
            .context("array element size overflowed")?;
        let total = stride
            .checked_mul(member.elements)
            .context("array size overflowed")?;
        return Ok((total, ARRAY_ELEMENT_ALIGNMENT));
    }
    Ok((size, alignment))
}

fn scalar_layout(member: &MemberRecord) -> Result<(u32, u32)> {
    if member.rows == 1 {
        return Ok(match member.columns {
            1 => (4, 4),
            2 => (8, 8),
            3 => (12, 16),
            4 => (16, 16),
            other => bail!("no packing rule for a {other}-column vector"),
        });
    }
    match (member.rows, member.columns) {
        (3, 4) => Ok((48, 16)),
        (4, 4) => Ok((64, 16)),
        (rows, columns) => bail!("no packing rule for a {rows}x{columns} matrix"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dump;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    fn member(
        index: usize,
        name: &str,
        base_type: ShaderParameterBaseType,
        rows: u32,
        columns: u32,
        elements: u32,
    ) -> MemberRecord {
        MemberRecord {
            index,
            name: name.to_string(),
            offset: 0,
            base_type,
            rows,
            columns,
            elements,
            precision: 0,
            shader_type: String::new(),
            struct_type: String::new(),
            source_line: 0,
            nested_definition_id: 0,
        }
    }

    fn definition(members: Vec<MemberRecord>) -> HashMap<u32, DefinitionRecord> {
        let mut definitions = HashMap::new();
        definitions.insert(
            1,
            DefinitionRecord {
                id: 1,
                type_name: "FSimulated".to_string(),
                layout_name: "FSimulated".to_string(),
                shader_variable_name: "Simulated".to_string(),
                source_file: String::new(),
                source_line: 1,
                size: 0,
                use_case: 0,
                binding_flags: 0,
                usage_flags: 0,
                members,
            },
        );
        definitions
    }

    #[test]
    fn fixture_captures_are_reproduced() {
        for name in ["game-layouts.json", "editor-layouts.json"] {
            let dump = dump::read(&fixture(name), 1).unwrap();
            let (exact, failures) = verify_dump(&dump);
            assert_eq!(exact, dump.definitions.len(), "{name}: {failures:?}");
            assert!(failures.is_empty(), "{name}: {failures:?}");
        }
    }

    #[test]
    fn perturbed_capture_is_rejected() {
        let mut dump = dump::read(&fixture("game-layouts.json"), 1).unwrap();
        dump.definitions.get_mut(&1).unwrap().members[1].offset += 16;
        let (_, failures) = verify_dump(&dump);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("captured offset 0x20"));
    }

    #[test]
    fn scalars_pack_into_a_vector_tail() {
        use ShaderParameterBaseType::{Float32, Uint32};
        let definitions = definition(vec![
            member(0, "Direction", Float32, 1, 3, 0),
            member(1, "Length", Float32, 1, 1, 0),
            member(2, "Mask", Uint32, 1, 4, 0),
        ]);
        let simulated = simulate_definition(&definitions, 1).unwrap();
        assert_eq!(simulated.member_offsets, [0x00, 0x0C, 0x10]);
        assert_eq!(simulated.size, 32);
    }

    #[test]
    fn resources_use_pointer_alignment() {
        use ShaderParameterBaseType::{Float32, Sampler, Texture};
        let definitions = definition(vec![
            member(0, "Scale", Float32, 1, 1, 0),
            member(1, "Source", Texture, 1, 1, 0),
            member(2, "SourceSampler", Sampler, 1, 1, 0),
            member(3, "Bias", Float32, 1, 1, 0),
        ]);
        let simulated = simulate_definition(&definitions, 1).unwrap();
        assert_eq!(simulated.member_offsets, [0x00, 0x08, 0x10, 0x18]);
        assert_eq!(simulated.size, 32);
    }

    #[test]
    fn array_elements_are_padded_to_sixteen_bytes() {
        use ShaderParameterBaseType::Float32;
        let definitions = definition(vec![
            member(0, "Weights", Float32, 1, 4, 3),
            member(1, "Transforms", Float32, 4, 4, 2),
        ]);
        let simulated = simulate_definition(&definitions, 1).unwrap();
        assert_eq!(simulated.member_offsets, [0x00, 0x30]);
        assert_eq!(simulated.size, 0x30 + 0x80);
    }

    #[test]
    fn unsupported_base_types_are_reported() {
        use ShaderParameterBaseType::RdgTextureAccess;
        let definitions = definition(vec![member(0, "Access", RdgTextureAccess, 1, 1, 0)]);
        let error = simulate_definition(&definitions, 1).unwrap_err();
        assert!(format!("{error:#}").contains("not covered by the packing model"));
    }

    #[test]
    fn uniform_buffer_bindings_occupy_sixteen_byte_slots() {
        use ShaderParameterBaseType::{Float32, RdgUniformBuffer, ReferencedStruct};
        let definitions = definition(vec![
            member(0, "Scale", Float32, 1, 1, 0),
            member(1, "SceneTextures", ReferencedStruct, 1, 1, 0),
            member(2, "Strata", RdgUniformBuffer, 1, 1, 0),
            member(3, "Bias", Float32, 1, 1, 0),
        ]);
        let simulated = simulate_definition(&definitions, 1).unwrap();
        assert_eq!(simulated.member_offsets, [0x00, 0x10, 0x20, 0x30]);
        assert_eq!(simulated.size, 64);
    }

    #[test]
    fn resource_arrays_use_eight_byte_pointer_elements() {
        use ShaderParameterBaseType::{Float32, Texture};
        let definitions = definition(vec![
            member(0, "Sources", Texture, 1, 1, 2),
            member(1, "Scale", Float32, 1, 1, 0),
        ]);
        let simulated = simulate_definition(&definitions, 1).unwrap();
        assert_eq!(simulated.member_offsets, [0x00, 0x10]);
        assert_eq!(simulated.size, 32);
    }

    #[test]
    fn implausible_member_geometry_is_rejected() {
        use ShaderParameterBaseType::Float32;
        let definitions = definition(vec![member(0, "Huge", Float32, 1, 4, u32::MAX)]);
        let error = simulate_definition(&definitions, 1).unwrap_err();
        assert!(format!("{error:#}").contains("overflowed"));
    }

    #[test]
    fn excessive_definition_nesting_is_rejected() {
        let mut definitions = HashMap::new();
        for id in 1..=MAX_DEFINITION_DEPTH as u32 + 1 {
            let members = if id <= MAX_DEFINITION_DEPTH as u32 {
                let mut nested =
                    member(0, "Nested", ShaderParameterBaseType::NestedStruct, 0, 0, 0);
                nested.nested_definition_id = id + 1;
                vec![nested]
            } else {
                Vec::new()
            };
            definitions.insert(
                id,
                DefinitionRecord {
                    id,
                    type_name: format!("FNested{id}"),
                    layout_name: format!("FNested{id}"),
                    shader_variable_name: format!("Nested{id}"),
                    source_file: String::new(),
                    source_line: 1,
                    size: 0,
                    use_case: 0,
                    binding_flags: 0,
                    usage_flags: 0,
                    members,
                },
            );
        }
        let error = simulate_definition(&definitions, 1).unwrap_err();
        assert!(format!("{error:#}").contains("nesting exceeded"));
    }
}
