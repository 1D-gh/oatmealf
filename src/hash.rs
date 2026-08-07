use crate::model::{LayoutDump, LayoutRecord, MAX_DEFINITION_DEPTH, ShaderParameterBaseType};
use anyhow::{Result, bail};
use std::collections::HashSet;
use xxhash_rust::xxh3::xxh3_64;

pub fn validate(dump: &LayoutDump) -> Result<usize> {
    for layout in dump.layouts.values() {
        let rhi_hash = compute_rhi_hash(layout);
        if rhi_hash != layout.rhi_hash {
            bail!(
                "RHI hash model failed for {}: dump=0x{:08X}, computed=0x{:08X}",
                layout.name,
                layout.rhi_hash,
                rhi_hash
            );
        }
        let serialized = serialize_definition(dump, layout.root_definition_id)?;
        let metadata_hash = xxh3_64(&serialized) as u32;
        if metadata_hash != layout.metadata_hash {
            bail!(
                "metadata hash model failed for {}: dump=0x{:08X}, computed=0x{:08X}",
                layout.name,
                layout.metadata_hash,
                metadata_hash
            );
        }
    }
    Ok(dump.layouts.len())
}

pub fn compute_rhi_hash(layout: &LayoutRecord) -> u32 {
    let mut hash = layout.constant_buffer_size << 16;
    hash |= u32::from(layout.binding_flags) << 8;
    if layout.static_slot != 255 {
        hash |= 1;
    }
    if layout.flags & 0x01 != 0 {
        hash |= 1 << 1;
    }
    if layout.flags & 0x02 != 0 {
        hash |= 1 << 2;
    }
    if layout.flags & 0x08 != 0 {
        hash |= 1 << 3;
    }

    let resources = &layout.arrays["Resources"];
    for resource in resources {
        hash ^= u32::from(resource.offset);
    }

    // UE packs resource types from the end, four per word
    let mut count = resources.len();
    while count >= 4 {
        count -= 1;
        hash ^= resources[count].resource_type as u32;
        count -= 1;
        hash ^= (resources[count].resource_type as u32) << 8;
        count -= 1;
        hash ^= (resources[count].resource_type as u32) << 16;
        count -= 1;
        hash ^= (resources[count].resource_type as u32) << 24;
    }
    while count >= 2 {
        count -= 1;
        hash ^= resources[count].resource_type as u32;
        count -= 1;
        hash ^= (resources[count].resource_type as u32) << 16;
    }
    while count > 0 {
        count -= 1;
        hash ^= resources[count].resource_type as u32;
    }
    hash ^ string_crc32(&layout.layout_name)
}

// cap inline nested metadata so fan-in cannot grow exponentially
const MAX_SERIALIZED_DEFINITION_BYTES: usize = 16 * 1024 * 1024;

pub fn serialize_definition(dump: &LayoutDump, definition_id: u32) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    serialize_definition_into(dump, definition_id, &mut output, &mut HashSet::new(), 0)?;
    Ok(output)
}

fn serialize_definition_into(
    dump: &LayoutDump,
    definition_id: u32,
    output: &mut Vec<u8>,
    stack: &mut HashSet<u32>,
    depth: usize,
) -> Result<()> {
    if depth >= MAX_DEFINITION_DEPTH {
        bail!("shader metadata nesting exceeded {MAX_DEFINITION_DEPTH} definitions");
    }
    if !stack.insert(definition_id) {
        bail!("recursive shader metadata cycle at definition {definition_id}");
    }
    let definition = &dump.definitions[&definition_id];
    for member in &definition.members {
        if output.len() > MAX_SERIALIZED_DEFINITION_BYTES {
            bail!(
                "serialized shader metadata exceeded {MAX_SERIALIZED_DEFINITION_BYTES} bytes; the capture metadata is malformed"
            );
        }
        output.extend_from_slice(&member.offset.to_le_bytes());
        output.push(member.base_type as u8);
        write_unreal_tchar_bytes(output, &member.name);
        output.extend_from_slice(&member.elements.to_le_bytes());

        match member.base_type {
            ShaderParameterBaseType::Int32
            | ShaderParameterBaseType::Uint32
            | ShaderParameterBaseType::Float32 => {
                output.push(member.precision);
                output.extend_from_slice(&member.rows.to_le_bytes());
                output.extend_from_slice(&member.columns.to_le_bytes());
            }
            ShaderParameterBaseType::NestedStruct | ShaderParameterBaseType::IncludedStruct => {
                if member.nested_definition_id == 0 {
                    bail!("struct member '{}' has no nested definition", member.name);
                }
                // nested structs are hashed inline
                serialize_definition_into(
                    dump,
                    member.nested_definition_id,
                    output,
                    stack,
                    depth + 1,
                )?;
            }
            base_type if is_read_only_rhi_resource(base_type) || is_rdg_resource(base_type) => {
                write_unreal_tchar_bytes(output, &member.shader_type);
            }
            _ => {}
        }
    }
    stack.remove(&definition_id);
    Ok(())
}

fn is_read_only_rhi_resource(base_type: ShaderParameterBaseType) -> bool {
    matches!(
        base_type,
        ShaderParameterBaseType::Texture
            | ShaderParameterBaseType::Srv
            | ShaderParameterBaseType::Sampler
            | ShaderParameterBaseType::ResourceCollection
    )
}

fn is_rdg_resource(base_type: ShaderParameterBaseType) -> bool {
    let value = base_type as u8;
    (ShaderParameterBaseType::RdgTexture as u8..=ShaderParameterBaseType::RdgUniformBuffer as u8)
        .contains(&value)
}

// match UE 5.6 TCHAR byte-count truncation
fn write_unreal_tchar_bytes(output: &mut Vec<u8>, value: &str) {
    let units: Vec<u16> = value.encode_utf16().collect();
    let mut bytes = Vec::with_capacity(units.len() * 2);
    for unit in &units {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    output.extend_from_slice(&bytes[..units.len()]);
}

fn string_crc32(value: &str) -> u32 {
    let table = crc_table();
    let mut crc = 0xffff_ffffu32;
    for code in value.encode_utf16() {
        for byte in [(code & 0xff) as u8, (code >> 8) as u8, 0, 0] {
            crc = (crc >> 8) ^ table[((crc ^ u32::from(byte)) & 0xff) as usize];
        }
    }
    !crc
}

fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    for (index, entry) in table.iter_mut().enumerate() {
        let mut value = index as u32;
        for _ in 0..8 {
            value = if value & 1 != 0 {
                (value >> 1) ^ 0xedb8_8320
            } else {
                value >> 1
            };
        }
        *entry = value;
    }
    table
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

    #[test]
    fn fixture_hashes_match_unreal() {
        let game = dump::read(&fixture("game-layouts.json"), 1).unwrap();
        let editor = dump::read(&fixture("editor-layouts.json"), 1).unwrap();
        assert_eq!(validate(&game).unwrap(), 1);
        assert_eq!(validate(&editor).unwrap(), 1);
        assert_eq!(
            xxh3_64(&serialize_definition(&game, 1).unwrap()),
            0x9D7A_6B62_F848_E03A
        );
        assert_eq!(
            xxh3_64(&serialize_definition(&editor, 1).unwrap()),
            0xE67C_F2AF_A121_44E9
        );
    }

    #[test]
    fn changed_captured_hash_is_rejected() {
        let mut game = dump::read(&fixture("game-layouts.json"), 1).unwrap();
        game.layouts.get_mut("Test").unwrap().metadata_hash ^= 1;
        assert!(validate(&game).is_err());
    }

    #[test]
    fn fan_in_expansion_is_rejected() {
        use crate::model::{DefinitionRecord, MemberRecord};
        use std::collections::{BTreeMap, HashMap};

        fn member(index: usize, nested_definition_id: u32) -> MemberRecord {
            MemberRecord {
                index,
                name: format!("m{index}"),
                offset: 0,
                base_type: if nested_definition_id == 0 {
                    ShaderParameterBaseType::Float32
                } else {
                    ShaderParameterBaseType::NestedStruct
                },
                rows: 1,
                columns: 1,
                elements: 0,
                precision: 0,
                shader_type: String::new(),
                struct_type: String::new(),
                source_line: 1,
                nested_definition_id,
            }
        }
        fn definition(id: u32, members: Vec<MemberRecord>) -> DefinitionRecord {
            DefinitionRecord {
                id,
                type_name: format!("FDef{id}"),
                layout_name: format!("FDef{id}"),
                shader_variable_name: format!("d{id}"),
                source_file: String::new(),
                source_line: 1,
                size: 16,
                use_case: 0,
                binding_flags: 0,
                usage_flags: 0,
                members,
            }
        }

        let levels = 28;
        let mut definitions = HashMap::new();
        for id in 1..levels {
            definitions.insert(
                id,
                definition(id, vec![member(0, id + 1), member(1, id + 1)]),
            );
        }
        definitions.insert(levels, definition(levels, vec![member(0, 0)]));
        let dump = LayoutDump {
            path: PathBuf::from("fan-in"),
            layouts: BTreeMap::new(),
            definitions,
        };

        let error = serialize_definition(&dump, 1).unwrap_err();
        assert!(format!("{error:#}").contains("exceeded"), "{error:#}");
    }
}
