use oatmealf::dump;
use oatmealf::hash;
use oatmealf::model::{
    DefinitionRecord, LayoutDump, LayoutRecord, MemberRecord, RESOURCE_ARRAY_NAMES, ResourceEntry,
    ShaderParameterBaseType,
};
use oatmealf::output;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use xxhash_rust::xxh3::xxh3_64;

#[test]
fn definition_edit_runs_through_the_output_pipeline() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");
    let mut game = dump::read(&fixture.join("game-layouts.json"), 1).unwrap();
    let editor = dump::read(&fixture.join("editor-layouts.json"), 1).unwrap();

    game.definitions.get_mut(&1).unwrap().members[0].base_type = ShaderParameterBaseType::Uint32;
    game.layouts
        .get_mut("Test")
        .unwrap()
        .arrays
        .get_mut("Resources")
        .unwrap()
        .push(ResourceEntry {
            offset: 0,
            resource_type: ShaderParameterBaseType::Texture,
        });
    game.layouts.get_mut("Test").unwrap().rhi_hash = hash::compute_rhi_hash(&game.layouts["Test"]);
    game.layouts.get_mut("Test").unwrap().metadata_hash =
        xxh3_64(&hash::serialize_definition(&game, 1).unwrap()) as u32;

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("oatmealf-definition-emit-{unique}"));
    fs::create_dir_all(&root).unwrap();
    let game_path = root.join("game-layouts.json");
    let editor_path = root.join("editor-layouts.json");
    let output_path = root.join("Generated");
    dump::write(&game_path, "FixtureGame", 1, "2026-07-20T00:00:00Z", &game).unwrap();
    dump::write(
        &editor_path,
        "FixtureEditor",
        2,
        "2026-07-20T00:00:00Z",
        &editor,
    )
    .unwrap();

    let summary = output::generate_with_minimum(
        &game_path,
        &editor_path,
        &fixture,
        &output_path,
        &[],
        1,
        false,
    )
    .unwrap();
    assert!(!summary.needs_review());

    let generated =
        fs::read_to_string(output_path.join("Engine/Source/Runtime/Test/TestShaderParameters.h"))
            .unwrap();
    assert!(generated.contains("SHADER_PARAMETER(uint32, Existing)"));
    assert!(!generated.contains("SHADER_PARAMETER(float, Existing)"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rename_and_reshape_runs_through_the_output_pipeline() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");
    let mut game = dump::read(&fixture.join("game-layouts.json"), 1).unwrap();
    let editor = dump::read(&fixture.join("editor-layouts.json"), 1).unwrap();

    let renamed = &mut game.definitions.get_mut(&1).unwrap().members[1];
    renamed.columns = 3;
    game.layouts
        .get_mut("Test")
        .unwrap()
        .arrays
        .get_mut("Resources")
        .unwrap()
        .push(ResourceEntry {
            offset: 0,
            resource_type: ShaderParameterBaseType::Texture,
        });
    game.layouts.get_mut("Test").unwrap().rhi_hash = hash::compute_rhi_hash(&game.layouts["Test"]);
    game.layouts.get_mut("Test").unwrap().metadata_hash =
        xxh3_64(&hash::serialize_definition(&game, 1).unwrap()) as u32;

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("oatmealf-rename-reshape-{unique}"));
    let source_directory = root.join("Engine/Source/Runtime/Test");
    fs::create_dir_all(&source_directory).unwrap();
    fs::write(
        source_directory.join("TestShaderParameters.h"),
        concat!(
            "BEGIN_GLOBAL_SHADER_PARAMETER_STRUCT(FTestParameters, )\n",
            "\tSHADER_PARAMETER(float, Existing)\n",
            "\tSHADER_PARAMETER(FVector4f, oatmealfPad)\n",
            "END_GLOBAL_SHADER_PARAMETER_STRUCT()\n",
            "inline void ReadValue(const FTestParameters& Parameters)\n",
            "{\n",
            "\t(void)Parameters.oatmealfPad;\n",
            "}\n",
        ),
    )
    .unwrap();
    let game_path = root.join("game-layouts.json");
    let editor_path = root.join("editor-layouts.json");
    let output_path = root.join("Generated");
    dump::write(&game_path, "FixtureGame", 1, "2026-07-20T00:00:00Z", &game).unwrap();
    dump::write(
        &editor_path,
        "FixtureEditor",
        2,
        "2026-07-20T00:00:00Z",
        &editor,
    )
    .unwrap();

    let summary =
        output::generate_with_minimum(&game_path, &editor_path, &root, &output_path, &[], 1, false)
            .unwrap();
    assert!(!summary.needs_review());

    let generated =
        fs::read_to_string(output_path.join("Engine/Source/Runtime/Test/TestShaderParameters.h"))
            .unwrap();
    assert!(generated.contains("SHADER_PARAMETER(FVector3f, RealValue)"));
    assert!(generated.contains("Parameters.RealValue"));
    assert!(!generated.contains("oatmealfPad"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn leading_block_comment_cannot_hide_an_unreserved_declaration() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");
    let mut game = dump::read(&fixture.join("game-layouts.json"), 1).unwrap();
    let editor = dump::read(&fixture.join("editor-layouts.json"), 1).unwrap();

    let renamed = &mut game.definitions.get_mut(&1).unwrap().members[1];
    renamed.columns = 3;
    game.layouts
        .get_mut("Test")
        .unwrap()
        .arrays
        .get_mut("Resources")
        .unwrap()
        .push(ResourceEntry {
            offset: 0,
            resource_type: ShaderParameterBaseType::Texture,
        });
    game.layouts.get_mut("Test").unwrap().rhi_hash = hash::compute_rhi_hash(&game.layouts["Test"]);
    game.layouts.get_mut("Test").unwrap().metadata_hash =
        xxh3_64(&hash::serialize_definition(&game, 1).unwrap()) as u32;

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("oatmealf-commented-rename-{unique}"));
    let source_directory = root.join("Engine/Source/Runtime/Test");
    fs::create_dir_all(&source_directory).unwrap();
    fs::write(
        source_directory.join("TestShaderParameters.h"),
        concat!(
            "BEGIN_GLOBAL_SHADER_PARAMETER_STRUCT(FTestParameters, )\n",
            "\tSHADER_PARAMETER(float, Existing)\n",
            "\tSHADER_PARAMETER(FVector4f, oatmealfPad)\n",
            "END_GLOBAL_SHADER_PARAMETER_STRUCT()\n",
            "BEGIN_GLOBAL_SHADER_PARAMETER_STRUCT(FUnrelatedParameters, )\n",
            "\t/* unrelated */ SHADER_PARAMETER(FVector4f, oatmealfPad)\n",
            "END_GLOBAL_SHADER_PARAMETER_STRUCT()\n",
        ),
    )
    .unwrap();
    let game_path = root.join("game-layouts.json");
    let editor_path = root.join("editor-layouts.json");
    let output_path = root.join("Generated");
    dump::write(&game_path, "FixtureGame", 1, "2026-07-20T00:00:00Z", &game).unwrap();
    dump::write(
        &editor_path,
        "FixtureEditor",
        2,
        "2026-07-20T00:00:00Z",
        &editor,
    )
    .unwrap();

    let summary =
        output::generate_with_minimum(&game_path, &editor_path, &root, &output_path, &[], 1, false)
            .unwrap();
    assert!(summary.needs_review());
    assert!(!output_path.join("Engine").exists());
    let report = fs::read_to_string(output_path.join("report.md")).unwrap();
    assert!(report.contains("declared outside its reserved edit"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn placeholder_bindings_flow_through_the_output_pipeline() {
    fn member(
        index: usize,
        name: &str,
        offset: u32,
        base_type: ShaderParameterBaseType,
        shader_type: &str,
        columns: u32,
        source_line: usize,
    ) -> MemberRecord {
        MemberRecord {
            index,
            name: name.to_string(),
            offset,
            base_type,
            rows: if columns == 0 { 0 } else { 1 },
            columns,
            elements: 0,
            precision: 0,
            shader_type: shader_type.to_string(),
            struct_type: String::new(),
            source_line,
            nested_definition_id: 0,
        }
    }
    fn layout(constant_buffer_size: u32, resources: Vec<ResourceEntry>) -> LayoutRecord {
        let mut layout = LayoutRecord::new("Res".to_string(), "FResParameters".to_string());
        layout.layout_name = "FResParameters".to_string();
        layout.root_definition_id = 1;
        layout.constant_buffer_size = constant_buffer_size;
        layout.render_targets_offset = u16::MAX;
        layout.static_slot = u8::MAX;
        for array in RESOURCE_ARRAY_NAMES {
            layout.arrays.insert(array.to_string(), Vec::new());
        }
        layout.arrays.insert("Resources".to_string(), resources);
        layout
    }
    fn dump_of(layout: LayoutRecord, members: Vec<MemberRecord>, size: u32) -> LayoutDump {
        let definition = DefinitionRecord {
            id: 1,
            type_name: "FResParameters".to_string(),
            layout_name: "FResParameters".to_string(),
            shader_variable_name: "Res".to_string(),
            source_file: "C:/Build/Engine/Source/Runtime/Test/ResHeader.h".to_string(),
            source_line: 1,
            size,
            use_case: 0,
            binding_flags: 0,
            usage_flags: 0,
            members,
        };
        let mut dump = LayoutDump {
            path: PathBuf::from("in-memory"),
            layouts: BTreeMap::from([("Res".to_string(), layout)]),
            definitions: HashMap::from([(1, definition)]),
        };
        let rhi = hash::compute_rhi_hash(&dump.layouts["Res"]);
        dump.layouts.get_mut("Res").unwrap().rhi_hash = rhi;
        dump.layouts.get_mut("Res").unwrap().metadata_hash =
            xxh3_64(&hash::serialize_definition(&dump, 1).unwrap()) as u32;
        dump
    }

    let editor_members = vec![
        member(0, "Existing", 0, ShaderParameterBaseType::Float32, "", 4, 2),
        member(
            1,
            "ExistingVolume",
            16,
            ShaderParameterBaseType::RdgTexture,
            "Texture3D",
            0,
            3,
        ),
        member(
            2,
            "ExistingSampler",
            24,
            ShaderParameterBaseType::Sampler,
            "SamplerState",
            0,
            4,
        ),
        member(3, "Tail", 32, ShaderParameterBaseType::Float32, "", 4, 5),
    ];
    let mut game_members = editor_members.clone();
    game_members.push(member(
        4,
        "NewVolume",
        48,
        ShaderParameterBaseType::RdgTexture,
        "Texture3D",
        0,
        5,
    ));
    let editor_resources = vec![
        ResourceEntry {
            offset: 16,
            resource_type: ShaderParameterBaseType::RdgTexture,
        },
        ResourceEntry {
            offset: 24,
            resource_type: ShaderParameterBaseType::Sampler,
        },
    ];
    let mut game_resources = editor_resources.clone();
    game_resources.push(ResourceEntry {
        offset: 48,
        resource_type: ShaderParameterBaseType::RdgTexture,
    });
    let game = dump_of(layout(48, game_resources), game_members, 64);
    let editor = dump_of(layout(48, editor_resources), editor_members, 48);

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("oatmealf-binding-pipeline-{unique}"));
    let source_directory = root.join("Engine/Source/Runtime/Test");
    fs::create_dir_all(&source_directory).unwrap();
    fs::write(
        source_directory.join("ResHeader.h"),
        concat!(
            "BEGIN_GLOBAL_SHADER_PARAMETER_STRUCT(FResParameters, )\n",
            "\tSHADER_PARAMETER(FVector4f, Existing)\n",
            "\tSHADER_PARAMETER_RDG_TEXTURE(Texture3D, ExistingVolume)\n",
            "\tSHADER_PARAMETER_SAMPLER(SamplerState, ExistingSampler)\n",
            "\tSHADER_PARAMETER(FVector4f, Tail)\n",
            "END_GLOBAL_SHADER_PARAMETER_STRUCT()\n",
        ),
    )
    .unwrap();
    fs::write(
        source_directory.join("ResFill.cpp"),
        concat!(
            "void SetupResParameters(FResParameters& OutParameters)\n",
            "{\n",
            "\tOutParameters.ExistingVolume = SystemTextures.BlackVolumeDummy;\n",
            "\tOutParameters.ExistingSampler = TStaticSamplerState<SF_Bilinear, AM_Clamp, AM_Clamp, AM_Clamp>::GetRHI();\n",
            "\tOutParameters.Existing = ComputeExisting();\n",
            "\tOutParameters.Tail = ComputeTail();\n",
            "}\n",
        ),
    )
    .unwrap();

    let game_path = root.join("game-layouts.json");
    let editor_path = root.join("editor-layouts.json");
    let output_path = root.join("Generated");
    dump::write(&game_path, "FixtureGame", 1, "2026-07-24T00:00:00Z", &game).unwrap();
    dump::write(
        &editor_path,
        "FixtureEditor",
        2,
        "2026-07-24T00:00:00Z",
        &editor,
    )
    .unwrap();

    let summary =
        output::generate_with_minimum(&game_path, &editor_path, &root, &output_path, &[], 1, false)
            .unwrap();
    assert!(!summary.needs_review());

    let report = fs::read_to_string(output_path.join("report.md")).unwrap();
    assert!(report.contains("**Result: READY**"), "{report}");
    assert!(
        report.contains("## Generated placeholder bindings"),
        "{report}"
    );
    assert!(
        report.contains("`FResParameters::NewVolume` = `SystemTextures.BlackVolumeDummy`"),
        "{report}"
    );
    let generated_cpp =
        fs::read_to_string(output_path.join("Engine/Source/Runtime/Test/ResFill.cpp")).unwrap();
    assert!(
        generated_cpp.contains("\tOutParameters.NewVolume = SystemTextures.BlackVolumeDummy;"),
        "{generated_cpp}"
    );
    fs::remove_dir_all(root).unwrap();
}
