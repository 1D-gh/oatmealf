use crate::model::{
    DefinitionRecord, LayoutDump, LayoutRecord, RESOURCE_ARRAY_NAMES, ShaderParameterBaseType,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;

const FORMAT: &str = "oatmealf-layouts";
const LEGACY_FORMAT: &str = "shader-drift-check-layouts";
const VERSION: u32 = 1;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct EvidenceProcess {
    name: String,
    id: u32,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct EvidenceFile {
    format: String,
    version: u32,
    captured_at: String,
    process: EvidenceProcess,
    layouts: Vec<LayoutRecord>,
    definitions: Vec<DefinitionRecord>,
}

pub fn write(
    path: &Path,
    process_name: &str,
    process_id: u32,
    captured_at: &str,
    dump: &LayoutDump,
) -> Result<()> {
    let mut definitions: Vec<DefinitionRecord> = dump.definitions.values().cloned().collect();
    definitions.sort_by_key(|definition| definition.id);
    let evidence = EvidenceFile {
        format: FORMAT.to_string(),
        version: VERSION,
        captured_at: captured_at.to_string(),
        process: EvidenceProcess {
            name: process_name.to_string(),
            id: process_id,
        },
        layouts: dump.layouts.values().cloned().collect(),
        definitions,
    };
    let file = File::create(path)
        .with_context(|| format!("could not create capture {}", path.display()))?;
    let mut output = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut output, &evidence)?;
    writeln!(output)?;
    output.flush()?;
    Ok(())
}

pub fn read(path: &Path, minimum_layout_count: usize) -> Result<LayoutDump> {
    let evidence = read_evidence(path)?;
    validate_format(&evidence, path)?;
    validate_capture_metadata(&evidence, path)?;

    let mut layouts = BTreeMap::new();
    for layout in evidence.layouts {
        let name = layout.name.clone();
        if layouts.insert(name.clone(), layout).is_some() {
            bail!("duplicate layout '{name}' in {}", path.display());
        }
    }
    let mut definitions = HashMap::new();
    for definition in evidence.definitions {
        let id = definition.id;
        if definitions.insert(id, definition).is_some() {
            bail!("duplicate definition id {id} in {}", path.display());
        }
    }
    let dump = LayoutDump {
        path: path.canonicalize().unwrap_or_else(|_| path.to_path_buf()),
        layouts,
        definitions,
    };
    validate(&dump, minimum_layout_count)?;
    Ok(dump)
}

pub fn validate_pair(game_path: &Path, editor_path: &Path) -> Result<()> {
    let game_canonical = game_path
        .canonicalize()
        .with_context(|| format!("could not locate game evidence {}", game_path.display()))?;
    let editor_canonical = editor_path
        .canonicalize()
        .with_context(|| format!("could not locate editor evidence {}", editor_path.display()))?;
    if game_canonical == editor_canonical {
        bail!(
            "game and editor evidence refer to the same capture; provide one capture from each process"
        );
    }

    let game = read_evidence(&game_canonical)?;
    let editor = read_evidence(&editor_canonical)?;
    validate_format(&game, &game_canonical)?;
    validate_format(&editor, &editor_canonical)?;
    validate_capture_metadata(&game, &game_canonical)?;
    validate_capture_metadata(&editor, &editor_canonical)?;

    if game.process.name.eq_ignore_ascii_case(&editor.process.name) {
        bail!(
            "game and editor evidence were both captured from '{}'; provide one capture from the game and one from the editor",
            game.process.name
        );
    }
    if looks_like_editor(&game.process.name) && looks_like_shipping_game(&editor.process.name) {
        bail!(
            "game and editor evidence appear to be reversed; pass the shipping-game capture to --game-layouts"
        );
    }
    Ok(())
}

fn read_evidence(path: &Path) -> Result<EvidenceFile> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("could not read layout evidence {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("invalid layout evidence JSON: {}", path.display()))
}

fn validate_format(evidence: &EvidenceFile, path: &Path) -> Result<()> {
    if !matches!(evidence.format.as_str(), FORMAT | LEGACY_FORMAT) || evidence.version != VERSION {
        bail!(
            "unsupported layout evidence format '{}' version {} in {}",
            evidence.format,
            evidence.version,
            path.display()
        );
    }
    Ok(())
}

fn validate_capture_metadata(evidence: &EvidenceFile, path: &Path) -> Result<()> {
    if evidence.process.name.trim().is_empty()
        || evidence.process.id == 0
        || evidence.captured_at.trim().is_empty()
    {
        bail!(
            "layout evidence has incomplete capture metadata: {}",
            path.display()
        );
    }
    Ok(())
}

fn looks_like_editor(process_name: &str) -> bool {
    process_name.to_ascii_lowercase().contains("unrealeditor")
}

fn looks_like_shipping_game(process_name: &str) -> bool {
    let process_name = process_name.to_ascii_lowercase();
    process_name.contains("-win64-shipping") || process_name.contains("-linux-shipping")
}

fn validate(dump: &LayoutDump, minimum_layout_count: usize) -> Result<()> {
    if dump.layouts.len() < minimum_layout_count {
        bail!(
            "expected at least {minimum_layout_count} registered layouts in {}",
            dump.path.display()
        );
    }
    if minimum_layout_count > 1 {
        let view = dump.layouts.get("View").with_context(|| {
            format!(
                "evidence has no View uniform buffer: {}",
                dump.path.display()
            )
        })?;
        if view.type_name != "FViewUniformShaderParameters" {
            bail!(
                "unexpected View metadata type '{}' in {}",
                view.type_name,
                dump.path.display()
            );
        }
    }
    for layout in dump.layouts.values() {
        if layout.name.is_empty()
            || layout.type_name.is_empty()
            || layout.layout_name.is_empty()
            || layout.root_definition_id == 0
            || !dump.definitions.contains_key(&layout.root_definition_id)
        {
            bail!(
                "layout '{}' has incomplete metadata in {}",
                layout.name,
                dump.path.display()
            );
        }
        for name in RESOURCE_ARRAY_NAMES {
            let Some(resources) = layout.arrays.get(name) else {
                bail!("layout '{}' is missing {name}", layout.name);
            };
            if resources
                .iter()
                .any(|resource| resource.resource_type == ShaderParameterBaseType::Invalid)
            {
                bail!("layout '{}' has an invalid resource type", layout.name);
            }
        }
        if layout.arrays.len() != RESOURCE_ARRAY_NAMES.len()
            || layout
                .arrays
                .keys()
                .any(|name| !RESOURCE_ARRAY_NAMES.contains(&name.as_str()))
        {
            bail!(
                "layout '{}' contains an unsupported resource table",
                layout.name
            );
        }
    }
    for definition in dump.definitions.values() {
        if definition.id == 0 || definition.type_name.is_empty() {
            bail!("definition {} is missing its type name", definition.id);
        }
        for (index, member) in definition.members.iter().enumerate() {
            if member.index != index || member.base_type == ShaderParameterBaseType::Invalid {
                bail!(
                    "definition {} has an invalid member at index {index}",
                    definition.id
                );
            }
            if member.nested_definition_id != 0
                && !dump.definitions.contains_key(&member.nested_definition_id)
            {
                bail!(
                    "definition {} references missing definition {}",
                    definition.id,
                    member.nested_definition_id
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn incomplete_evidence_is_rejected() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("game-layouts.json");
        let mut value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(fixture).unwrap()).unwrap();
        value["definitions"] = serde_json::json!([]);
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("oatmealf-incomplete-{unique}.json"));
        fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
        let error = read(&path, 1).unwrap_err();
        assert!(error.to_string().contains("incomplete metadata"));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn evidence_round_trip_is_readable_and_lossless() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("game-layouts.json");
        let original = read(&fixture, 1).unwrap();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("oatmealf-round-trip-{unique}.json"));
        write(
            &path,
            "FixtureGame",
            1,
            "2026-07-13T00:00:00-04:00",
            &original,
        )
        .unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"format\": \"oatmealf-layouts\""));
        assert!(text.contains("\"rhiHash\": \"0xCF869FDC\""));
        assert!(text.contains("C:/Build/Engine/Source/Runtime/Test/TestShaderParameters.h"));
        assert!(!text.contains("source64"));
        let round_trip = read(&path, 1).unwrap();
        assert_eq!(round_trip.layouts.len(), original.layouts.len());
        assert_eq!(round_trip.definitions.len(), original.definitions.len());
        assert_eq!(round_trip.layouts["Test"].rhi_hash, 0xCF86_9FDC);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn legacy_v1_evidence_is_still_readable() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("game-layouts.json");
        let text =
            fs::read_to_string(fixture)
                .unwrap()
                .replacen("oatmealf-layouts", LEGACY_FORMAT, 1);
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("oatmealf-legacy-{unique}.json"));
        fs::write(&path, text).unwrap();

        let dump = read(&path, 1).unwrap();
        assert_eq!(dump.layouts.len(), 1);
        assert_eq!(dump.definitions.len(), 1);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn unknown_resource_tables_are_rejected() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("game-layouts.json");
        let mut value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(fixture).unwrap()).unwrap();
        value["layouts"][0]["resourceTables"]["Unexpected"] = serde_json::json!([]);
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("oatmealf-resource-table-{unique}.json"));
        fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();

        let error = read(&path, 1).unwrap_err();
        assert!(format!("{error:#}").contains("unsupported resource table"));
        fs::remove_file(path).unwrap();
    }
}
