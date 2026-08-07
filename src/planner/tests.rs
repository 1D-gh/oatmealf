use super::*;
use crate::dump;
use std::time::{SystemTime, UNIX_EPOCH};

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn test_file_plan(text: &str) -> FilePlan {
    FilePlan {
        source_path: PathBuf::from("Test.h"),
        relative_path: PathBuf::from("Test.h"),
        original_text: text.to_string(),
        newline: "\n",
        utf8_bom: false,
        replacements: BTreeMap::new(),
        insert_before: BTreeMap::new(),
        renames: BTreeMap::new(),
    }
}

fn test_member(name: &str, source_line: usize) -> MemberRecord {
    MemberRecord {
        index: 0,
        name: name.to_string(),
        offset: 0,
        base_type: ShaderParameterBaseType::Float32,
        rows: 1,
        columns: 1,
        elements: 0,
        precision: 0,
        shader_type: String::new(),
        struct_type: String::new(),
        source_line,
        nested_definition_id: 0,
    }
}

fn test_definition(member: MemberRecord) -> DefinitionRecord {
    DefinitionRecord {
        id: 1,
        type_name: "FTestParameters".to_string(),
        layout_name: "FTestParameters".to_string(),
        shader_variable_name: "Test".to_string(),
        source_file: "Test.h".to_string(),
        source_line: 1,
        size: 16,
        use_case: 0,
        binding_flags: 0,
        usage_flags: 0,
        members: vec![member],
    }
}

#[test]
fn fixture_adds_missing_member_without_touching_input() {
    let root = fixture_root();
    let mut game = dump::read(&root.join("game-layouts.json"), 1).unwrap();
    let editor = dump::read(&root.join("editor-layouts.json"), 1).unwrap();
    let mut added = game.definitions[&1].members[1].clone();
    added.index = 2;
    added.name = "NewValue".to_string();
    added.offset = 32;
    game.definitions.get_mut(&1).unwrap().members.push(added);
    game.definitions.get_mut(&1).unwrap().size = 48;
    game.layouts.get_mut("Test").unwrap().constant_buffer_size = 48;
    game.layouts.get_mut("Test").unwrap().rhi_hash ^= 0x0010_0000;
    let mut planner = SourcePlanner::new(&game, &editor, &root).unwrap();
    planner.plan(&["Test".to_string()]);
    planner.add_rename_references().unwrap();
    assert!(!planner.has_unresolved(), "{:?}", planner.unresolved);

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let output = std::env::temp_dir().join(format!("oatmealf-planner-{unique}"));
    let files = planner.emit(&output).unwrap();
    let relative = "Engine/Source/Runtime/Test/TestShaderParameters.h";
    assert_eq!(files, [relative.to_string()]);
    let generated = fs::read_to_string(output.join(relative)).unwrap();
    assert!(generated.contains("SHADER_PARAMETER_ARRAY(FVector4f, oatmealfPadding_Test, [1])"));
    assert!(generated.contains("oatmealfPad"));
    let original =
        fs::read_to_string(root.join("Engine/Source/Runtime/Test/TestShaderParameters.h")).unwrap();
    assert!(original.contains("oatmealfPad"));
    assert!(!original.contains("oatmealfPadding_Test"));
    fs::remove_dir_all(output).unwrap();
}

#[test]
fn missing_editor_layout_keeps_independent_candidates() {
    let root = fixture_root();
    let mut game = dump::read(&root.join("game-layouts.json"), 1).unwrap();
    let editor = dump::read(&root.join("editor-layouts.json"), 1).unwrap();

    game.definitions.get_mut(&1).unwrap().size = 48;
    game.layouts.get_mut("Test").unwrap().constant_buffer_size = 48;
    game.layouts.get_mut("Test").unwrap().rhi_hash ^= 0x0010_0000;

    let mut game_only = game.layouts["Test"].clone();
    game_only.name = "GameOnly".to_string();
    game.layouts.insert(game_only.name.clone(), game_only);

    let mut planner = SourcePlanner::new(&game, &editor, &root).unwrap();
    planner.plan(&[]);
    assert!(!planner.has_unresolved());
    assert_eq!(planner.missing_layouts.len(), 1);
    assert_eq!(planner.missing_layouts[0].name, "GameOnly");
    assert_eq!(planner.missing_layouts[0].plugin, None);
    assert_eq!(
        planner
            .outcomes
            .iter()
            .find(|outcome| outcome.name == "GameOnly")
            .unwrap()
            .status,
        "not loaded"
    );
    assert!(planner.can_emit());
    planner.add_rename_references().unwrap();
    assert!(planner.can_emit());

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let output = std::env::temp_dir().join(format!("oatmealf-partial-{unique}"));
    let files = planner.emit(&output).unwrap();
    assert_eq!(
        files,
        ["Engine/Source/Runtime/Test/TestShaderParameters.h".to_string()]
    );
    fs::remove_dir_all(output).unwrap();
}

#[test]
fn missing_editor_layout_identifies_its_plugin() {
    let fixture = fixture_root();
    let mut game = dump::read(&fixture.join("game-layouts.json"), 1).unwrap();
    let editor = dump::read(&fixture.join("editor-layouts.json"), 1).unwrap();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("oatmealf-missing-plugin-{unique}"));
    let plugin = root.join("Engine/Plugins/Experimental/ExampleWater");
    let header = plugin.join("Source/Runtime/Public/ExampleWaterParameters.h");
    fs::create_dir_all(header.parent().unwrap()).unwrap();
    fs::create_dir_all(root.join("Engine/Source")).unwrap();
    fs::write(plugin.join("ExampleWater.uplugin"), "{}\n").unwrap();
    fs::write(
        &header,
        "BEGIN_GLOBAL_SHADER_PARAMETER_STRUCT(FExampleWaterParameters, )\n",
    )
    .unwrap();

    let mut definition = game.definitions[&1].clone();
    definition.id = 2;
    definition.type_name = "FExampleWaterParameters".to_string();
    definition.layout_name = "FExampleWaterParameters".to_string();
    definition.shader_variable_name = "ExampleWater".to_string();
    definition.source_file = "Unknown".to_string();
    game.definitions.insert(definition.id, definition);

    let mut game_only = game.layouts["Test"].clone();
    game_only.name = "ExampleWater".to_string();
    game_only.type_name = "FExampleWaterParameters".to_string();
    game_only.layout_name = "FExampleWaterParameters".to_string();
    game_only.root_definition_id = 2;
    game.layouts.insert(game_only.name.clone(), game_only);

    let mut planner = SourcePlanner::new(&game, &editor, &root).unwrap();
    planner.plan(&["ExampleWater".to_string()]);
    assert!(!planner.has_unresolved());
    assert_eq!(planner.missing_layouts.len(), 1);
    assert_eq!(
        planner.missing_layouts[0].plugin.as_deref(),
        Some("ExampleWater")
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn identifies_a_shared_parameter_table() {
    let plan = FilePlan {
        source_path: PathBuf::from("Test.h"),
        relative_path: PathBuf::from("Test.h"),
        original_text: concat!(
            "#define TEST_PARAMETER_TABLE \\\n",
            "\tTEST_MEMBER(float, Existing) \\\n",
            "\tTEST_MEMBER(float, Anchor) \\\n",
            "BEGIN_SHADER_PARAMETER_STRUCT(FTest, )\n",
            "\tTEST_PARAMETER_TABLE // shared by generated structs\n",
            "END_SHADER_PARAMETER_STRUCT()\n",
        )
        .to_string(),
        newline: "\n",
        utf8_bom: false,
        replacements: BTreeMap::new(),
        insert_before: BTreeMap::new(),
        renames: BTreeMap::new(),
    };
    assert_eq!(
        shared_macro_for_anchor(&plan, 3, 4, 6).as_deref(),
        Some("TEST_PARAMETER_TABLE")
    );
}

#[test]
fn shared_parameter_table_insertion_is_modeled_when_consumers_agree() {
    use crate::model::RESOURCE_ARRAY_NAMES;

    fn float_member(index: usize, name: &str, offset: u32, source_line: usize) -> MemberRecord {
        MemberRecord {
            index,
            name: name.to_string(),
            offset,
            base_type: ShaderParameterBaseType::Float32,
            rows: 1,
            columns: 1,
            elements: 0,
            precision: 0,
            shader_type: String::new(),
            struct_type: String::new(),
            source_line,
            nested_definition_id: 0,
        }
    }
    fn shared_definition(
        id: u32,
        type_name: &str,
        variable: &str,
        begin_line: usize,
        members: Vec<MemberRecord>,
    ) -> DefinitionRecord {
        let size = if members
            .iter()
            .any(|member| member.base_type == ShaderParameterBaseType::Texture)
        {
            32
        } else {
            16
        };
        DefinitionRecord {
            id,
            type_name: type_name.to_string(),
            layout_name: type_name.to_string(),
            shader_variable_name: variable.to_string(),
            source_file: "C:/Fake/Engine/Source/Runtime/Test/SharedTable.h".to_string(),
            source_line: begin_line,
            size,
            use_case: 0,
            binding_flags: 0,
            usage_flags: 0,
            members,
        }
    }
    fn shared_layout(
        name: &str,
        type_name: &str,
        root: u32,
        constant_buffer_size: u32,
        resources: Vec<ResourceEntry>,
    ) -> LayoutRecord {
        let mut layout = LayoutRecord::new(name.to_string(), type_name.to_string());
        layout.layout_name = type_name.to_string();
        layout.root_definition_id = root;
        layout.constant_buffer_size = constant_buffer_size;
        layout.static_slot = 255;
        for array in RESOURCE_ARRAY_NAMES {
            layout.arrays.insert(array.to_string(), Vec::new());
        }
        layout.arrays.insert("Resources".to_string(), resources);
        layout
    }
    fn dump(layouts: Vec<LayoutRecord>, definitions: Vec<DefinitionRecord>) -> LayoutDump {
        LayoutDump {
            path: PathBuf::from("in-memory"),
            layouts: layouts
                .into_iter()
                .map(|layout| (layout.name.clone(), layout))
                .collect(),
            definitions: definitions
                .into_iter()
                .map(|definition| (definition.id, definition))
                .collect(),
        }
    }

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("oatmealf-shared-{unique}"));
    let source_directory = root.join("Engine/Source/Runtime/Test");
    fs::create_dir_all(&source_directory).unwrap();
    fs::write(
        source_directory.join("SharedTable.h"),
        concat!(
            "#define TEST_SHARED_TABLE \\\n",
            "\tTEST_MEMBER(float, SharedExisting) \\\n",
            "\tTEST_MEMBER(float, SharedAnchor) \\\n",
            "\n",
            "BEGIN_SHADER_PARAMETER_STRUCT(FAlphaParameters, )\n",
            "\tTEST_SHARED_TABLE\n",
            "\tSHADER_PARAMETER(float, AlphaTail)\n",
            "END_SHADER_PARAMETER_STRUCT()\n",
            "\n",
            "BEGIN_SHADER_PARAMETER_STRUCT(FBetaParameters, )\n",
            "\tTEST_SHARED_TABLE\n",
            "END_SHADER_PARAMETER_STRUCT()\n",
        ),
    )
    .unwrap();

    let game = dump(
        vec![
            shared_layout(
                "Alpha",
                "FAlphaParameters",
                1,
                32,
                vec![ResourceEntry {
                    offset: 16,
                    resource_type: ShaderParameterBaseType::Texture,
                }],
            ),
            shared_layout("Beta", "FBetaParameters", 2, 16, Vec::new()),
        ],
        vec![
            shared_definition(
                1,
                "FAlphaParameters",
                "Alpha",
                5,
                vec![
                    float_member(0, "SharedExisting", 0, 6),
                    float_member(1, "NewShared", 4, 6),
                    float_member(2, "SharedAnchor", 8, 6),
                    MemberRecord {
                        index: 3,
                        name: "AlphaTexture".to_string(),
                        offset: 16,
                        base_type: ShaderParameterBaseType::Texture,
                        rows: 0,
                        columns: 0,
                        elements: 0,
                        precision: 0,
                        shader_type: "Texture2D".to_string(),
                        struct_type: String::new(),
                        source_line: 6,
                        nested_definition_id: 0,
                    },
                    float_member(4, "AlphaTail", 24, 7),
                ],
            ),
            shared_definition(
                2,
                "FBetaParameters",
                "Beta",
                9,
                vec![
                    float_member(0, "SharedExisting", 0, 10),
                    float_member(1, "NewShared", 4, 10),
                    float_member(2, "SharedAnchor", 8, 10),
                ],
            ),
        ],
    );
    let editor = dump(
        vec![
            shared_layout("Alpha", "FAlphaParameters", 1, 16, Vec::new()),
            shared_layout("Beta", "FBetaParameters", 2, 16, Vec::new()),
        ],
        vec![
            shared_definition(
                1,
                "FAlphaParameters",
                "Alpha",
                5,
                vec![
                    float_member(0, "SharedExisting", 0, 6),
                    float_member(1, "SharedAnchor", 4, 6),
                    float_member(2, "AlphaTail", 8, 7),
                ],
            ),
            shared_definition(
                2,
                "FBetaParameters",
                "Beta",
                9,
                vec![
                    float_member(0, "SharedExisting", 0, 10),
                    float_member(1, "SharedAnchor", 4, 10),
                ],
            ),
        ],
    );

    let mut planner = SourcePlanner::new(&game, &editor, &root).unwrap();
    planner.plan(&[]);
    assert!(!planner.has_unresolved(), "{:?}", planner.unresolved);
    assert!(planner.can_emit());
    assert!(
        planner
            .outcomes
            .iter()
            .any(|outcome| outcome.status == "modeled exact"),
        "{:?}",
        planner.outcomes
    );
    let rendered = planner
        .files
        .values()
        .find(|plan| plan.relative_path.ends_with("SharedTable.h"))
        .unwrap()
        .render()
        .unwrap();
    assert!(rendered.contains("TEST_MEMBER(float, NewShared) \\"));
    assert!(rendered.contains("SHADER_PARAMETER_TEXTURE(Texture2D, AlphaTexture)"));

    let mut changed_anchor = float_member(1, "SharedAnchor", 4, 6);
    changed_anchor.base_type = ShaderParameterBaseType::Uint32;
    let game_shape = dump(
        vec![shared_layout(
            "Alpha",
            "FAlphaParameters",
            1,
            16,
            vec![ResourceEntry {
                offset: 0,
                resource_type: ShaderParameterBaseType::Srv,
            }],
        )],
        vec![shared_definition(
            1,
            "FAlphaParameters",
            "Alpha",
            5,
            vec![float_member(0, "SharedExisting", 0, 6), changed_anchor],
        )],
    );
    let editor_shape = dump(
        vec![shared_layout(
            "Alpha",
            "FAlphaParameters",
            1,
            16,
            Vec::new(),
        )],
        vec![shared_definition(
            1,
            "FAlphaParameters",
            "Alpha",
            5,
            vec![
                float_member(0, "SharedExisting", 0, 6),
                float_member(1, "SharedAnchor", 4, 6),
            ],
        )],
    );
    let mut shape_planner = SourcePlanner::new(&game_shape, &editor_shape, &root).unwrap();
    shape_planner.plan(&[]);
    assert!(
        shape_planner
            .unresolved
            .iter()
            .any(|issue| issue.contains("shared parameter table")),
        "{:?}",
        shape_planner.unresolved
    );
    assert!(!shape_planner.can_emit());

    let game_trailing = dump(
        vec![shared_layout(
            "Alpha",
            "FAlphaParameters",
            1,
            16,
            vec![ResourceEntry {
                offset: 0,
                resource_type: ShaderParameterBaseType::Srv,
            }],
        )],
        vec![shared_definition(
            1,
            "FAlphaParameters",
            "Alpha",
            5,
            vec![
                float_member(0, "SharedExisting", 0, 6),
                float_member(1, "SharedAnchor", 4, 6),
                float_member(2, "NewShared", 8, 6),
            ],
        )],
    );
    let editor_trailing = dump(
        vec![shared_layout(
            "Alpha",
            "FAlphaParameters",
            1,
            16,
            Vec::new(),
        )],
        vec![shared_definition(
            1,
            "FAlphaParameters",
            "Alpha",
            5,
            vec![
                float_member(0, "SharedExisting", 0, 6),
                float_member(1, "SharedAnchor", 4, 6),
            ],
        )],
    );
    let mut trailing_planner = SourcePlanner::new(&game_trailing, &editor_trailing, &root).unwrap();
    trailing_planner.plan(&[]);
    assert!(
        !trailing_planner.has_unresolved(),
        "{:?}",
        trailing_planner.unresolved
    );
    assert!(trailing_planner.can_emit());
    let trailing_rendered = trailing_planner
        .files
        .values()
        .find(|plan| plan.relative_path.ends_with("SharedTable.h"))
        .unwrap()
        .render()
        .unwrap();
    assert_eq!(
        trailing_rendered.matches("NewShared").count(),
        1,
        "{trailing_rendered}"
    );
    let alpha_block = trailing_rendered.find("FAlphaParameters").unwrap();
    let appended = trailing_rendered
        .find("SHADER_PARAMETER(float, NewShared)")
        .unwrap();
    assert!(appended > alpha_block, "{trailing_rendered}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn packing_mismatch_blocks_definition_planning() {
    let root = fixture_root();
    let mut game = dump::read(&root.join("game-layouts.json"), 1).unwrap();
    let editor = dump::read(&root.join("editor-layouts.json"), 1).unwrap();

    game.layouts.get_mut("Test").unwrap().rhi_hash ^= 1;
    game.definitions.get_mut(&1).unwrap().members[1].offset = 20;

    let mut planner = SourcePlanner::new(&game, &editor, &root).unwrap();
    planner.plan(&[]);
    assert!(planner.has_unresolved());
    assert!(
        planner.unresolved[0].contains("packing model cannot reproduce the captured game"),
        "{:?}",
        planner.unresolved
    );
}

#[test]
fn padding_requires_the_struct_size_to_match_the_constant_buffer() {
    let root = fixture_root();
    let mut game = dump::read(&root.join("game-layouts.json"), 1).unwrap();
    let mut editor = dump::read(&root.join("editor-layouts.json"), 1).unwrap();

    game.layouts.get_mut("Test").unwrap().constant_buffer_size = 48;
    game.layouts.get_mut("Test").unwrap().rhi_hash ^= 0x0010_0000;
    editor.layouts.get_mut("Test").unwrap().constant_buffer_size = 16;

    let mut planner = SourcePlanner::new(&game, &editor, &root).unwrap();
    planner.plan(&[]);
    assert!(planner.has_unresolved());
    assert!(
        planner.unresolved[0].contains("does not equal its constant buffer size"),
        "{:?}",
        planner.unresolved
    );
}

#[test]
fn existing_padding_requires_an_editor_rebuild_before_rerunning() {
    let fixture = fixture_root();
    let mut game = dump::read(&fixture.join("game-layouts.json"), 1).unwrap();
    let editor = dump::read(&fixture.join("editor-layouts.json"), 1).unwrap();
    game.layouts.get_mut("Test").unwrap().constant_buffer_size = 48;
    game.layouts.get_mut("Test").unwrap().rhi_hash ^= 0x0010_0000;

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("oatmealf-padding-rerun-{unique}"));
    let source_directory = root.join("Engine/Source/Runtime/Test");
    fs::create_dir_all(&source_directory).unwrap();
    fs::write(
        source_directory.join("TestShaderParameters.h"),
        concat!(
            "BEGIN_GLOBAL_SHADER_PARAMETER_STRUCT(FTestParameters, )\n",
            "\tSHADER_PARAMETER(float, Existing)\n",
            "\tSHADER_PARAMETER(FVector4f, oatmealfPad)\n",
            "\tSHADER_PARAMETER_ARRAY(FVector4f, oatmealfPadding_Test, [1])\n",
            "END_GLOBAL_SHADER_PARAMETER_STRUCT()\n",
        ),
    )
    .unwrap();

    let mut planner = SourcePlanner::new(&game, &editor, &root).unwrap();
    planner.plan(&[]);
    assert!(!planner.can_emit());
    assert!(
        planner
            .unresolved
            .iter()
            .any(|issue| issue.contains("rebuild and restart the editor")),
        "{:?}",
        planner.unresolved
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn padding_a_definition_nested_in_another_layout_requires_review() {
    use crate::model::RESOURCE_ARRAY_NAMES;

    fn layout(name: &str, type_name: &str, root: u32, size: u32) -> LayoutRecord {
        let mut layout = LayoutRecord::new(name.to_string(), type_name.to_string());
        layout.layout_name = type_name.to_string();
        layout.root_definition_id = root;
        layout.constant_buffer_size = size;
        layout.render_targets_offset = u16::MAX;
        layout.static_slot = u8::MAX;
        layout.binding_flags = 1;
        for array in RESOURCE_ARRAY_NAMES {
            layout.arrays.insert(array.to_string(), Vec::new());
        }
        layout.rhi_hash = hash::compute_rhi_hash(&layout);
        layout
    }

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("oatmealf-shared-padding-{unique}"));
    let source_directory = root.join("Engine/Source/Runtime/Test");
    fs::create_dir_all(&source_directory).unwrap();
    fs::write(
        source_directory.join("SharedPadding.h"),
        concat!(
            "BEGIN_SHADER_PARAMETER_STRUCT(FChildParameters, )\n",
            "\tSHADER_PARAMETER(FVector4f, Value)\n",
            "END_SHADER_PARAMETER_STRUCT()\n",
            "\n",
            "BEGIN_SHADER_PARAMETER_STRUCT(FParentParameters, )\n",
            "\tSHADER_PARAMETER_STRUCT_INCLUDE(FChildParameters, Child)\n",
            "END_SHADER_PARAMETER_STRUCT()\n",
        ),
    )
    .unwrap();

    let child = DefinitionRecord {
        id: 1,
        type_name: "FChildParameters".to_string(),
        layout_name: "FChildParameters".to_string(),
        shader_variable_name: "Child".to_string(),
        source_file: "C:/Build/Engine/Source/Runtime/Test/SharedPadding.h".to_string(),
        source_line: 1,
        size: 16,
        use_case: 0,
        binding_flags: 0,
        usage_flags: 0,
        members: vec![MemberRecord {
            index: 0,
            name: "Value".to_string(),
            offset: 0,
            base_type: ShaderParameterBaseType::Float32,
            rows: 1,
            columns: 4,
            elements: 0,
            precision: 0,
            shader_type: String::new(),
            struct_type: String::new(),
            source_line: 2,
            nested_definition_id: 0,
        }],
    };
    let parent = DefinitionRecord {
        id: 2,
        type_name: "FParentParameters".to_string(),
        layout_name: "FParentParameters".to_string(),
        shader_variable_name: "Parent".to_string(),
        source_file: "C:/Build/Engine/Source/Runtime/Test/SharedPadding.h".to_string(),
        source_line: 5,
        size: 16,
        use_case: 0,
        binding_flags: 0,
        usage_flags: 0,
        members: vec![MemberRecord {
            index: 0,
            name: "Child".to_string(),
            offset: 0,
            base_type: ShaderParameterBaseType::IncludedStruct,
            rows: 0,
            columns: 0,
            elements: 0,
            precision: 0,
            shader_type: String::new(),
            struct_type: "FChildParameters".to_string(),
            source_line: 6,
            nested_definition_id: 1,
        }],
    };

    let editor_child = layout("Child", "FChildParameters", 1, 16);
    let editor_parent = layout("Parent", "FParentParameters", 2, 16);
    let game_child = layout("Child", "FChildParameters", 1, 32);
    let game_parent = layout("Parent", "FParentParameters", 2, 32);
    let definitions = HashMap::from([(1, child), (2, parent)]);
    let editor = LayoutDump {
        path: PathBuf::from("editor"),
        layouts: BTreeMap::from([
            ("Child".to_string(), editor_child),
            ("Parent".to_string(), editor_parent),
        ]),
        definitions: definitions.clone(),
    };
    let game = LayoutDump {
        path: PathBuf::from("game"),
        layouts: BTreeMap::from([
            ("Child".to_string(), game_child),
            ("Parent".to_string(), game_parent),
        ]),
        definitions,
    };

    let mut planner = SourcePlanner::new(&game, &editor, &root).unwrap();
    planner.plan(&[]);
    assert!(!planner.can_emit());
    let child_outcome = planner
        .outcomes
        .iter()
        .find(|outcome| outcome.name == "Child")
        .unwrap();
    assert_eq!(child_outcome.status, "unresolved");
    assert!(
        child_outcome
            .detail
            .contains("shared by multiple registered layouts"),
        "{}",
        child_outcome.detail
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn conflicting_shared_definition_targets_are_not_modeled() {
    let root = fixture_root();
    let mut game = dump::read(&root.join("game-layouts.json"), 1).unwrap();
    let mut editor = dump::read(&root.join("editor-layouts.json"), 1).unwrap();

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
    let test_hash = hash::compute_rhi_hash(&game.layouts["Test"]);
    game.layouts.get_mut("Test").unwrap().rhi_hash = test_hash;

    let mut game_alias = game.layouts["Test"].clone();
    game_alias.name = "TestAlias".to_string();
    let mut conflicting_definition = game.definitions[&1].clone();
    conflicting_definition.id = 2;
    conflicting_definition.members[0].name = "ConflictingMember".to_string();
    game.definitions.insert(2, conflicting_definition);
    game_alias.root_definition_id = 2;
    game.layouts.insert(game_alias.name.clone(), game_alias);
    let mut editor_alias = editor.layouts["Test"].clone();
    editor_alias.name = "TestAlias".to_string();
    editor
        .layouts
        .insert(editor_alias.name.clone(), editor_alias);

    let mut planner = SourcePlanner::new(&game, &editor, &root).unwrap();
    planner.plan(&[]);
    assert!(!planner.can_emit());
    assert_eq!(
        planner
            .outcomes
            .iter()
            .filter(|outcome| outcome.status == "unresolved")
            .count(),
        2,
        "{:?}",
        planner.outcomes
    );
    assert!(
        planner
            .outcomes
            .iter()
            .all(|outcome| outcome.status != "modeled exact"),
        "{:?}",
        planner.outcomes
    );
}

#[test]
fn member_fallback_ignores_comments_and_usage_sites() {
    let plan = test_file_plan(concat!(
        "// RecoveredValue is assigned below.\n",
        "AssignValue(RecoveredValue);\n",
        "SHADER_PARAMETER(float, RecoveredValue)\n",
    ));
    let member = test_member("RecoveredValue", 0);
    let definition = test_definition(member.clone());
    assert_eq!(find_member_line(&plan, &definition, &member).unwrap(), 3);
}

#[test]
fn member_fallback_rejects_a_unique_non_declaration() {
    let plan = test_file_plan("// RecoveredValue was removed.\n");
    let member = test_member("RecoveredValue", 0);
    let definition = test_definition(member.clone());
    assert!(find_member_line(&plan, &definition, &member).is_err());
}

#[test]
fn member_source_line_must_be_a_declaration() {
    let plan = test_file_plan(concat!(
        "// RecoveredValue is mentioned here.\n",
        "SHADER_PARAMETER(float, RecoveredValue)\n",
    ));
    let member = test_member("RecoveredValue", 1);
    let definition = test_definition(member.clone());
    assert_eq!(find_member_line(&plan, &definition, &member).unwrap(), 2);
}

#[test]
fn declaration_classifier_only_strips_leading_block_comments() {
    assert!(is_parameter_declaration_line(
        "/* generated */ SHADER_PARAMETER(float, RecoveredValue)",
        "RecoveredValue"
    ));
    assert!(!is_parameter_declaration_line(
        "// SHADER_PARAMETER(float, RecoveredValue)",
        "RecoveredValue"
    ));
    assert!(!is_parameter_declaration_line(
        "/* SHADER_PARAMETER(float, RecoveredValue) */",
        "RecoveredValue"
    ));
    assert!(!is_parameter_declaration_line(
        "prefix /* generated */ SHADER_PARAMETER(float, RecoveredValue)",
        "RecoveredValue"
    ));
}

#[test]
fn rename_with_an_unreserved_duplicate_declaration_requires_review() {
    let fixture = fixture_root();
    let game = dump::read(&fixture.join("game-layouts.json"), 1).unwrap();
    let editor = dump::read(&fixture.join("editor-layouts.json"), 1).unwrap();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("oatmealf-rename-collision-{unique}"));
    let source_directory = root.join("Engine/Source/Runtime/Test");
    fs::create_dir_all(&source_directory).unwrap();
    let source = source_directory.join("RenameCollision.h");
    fs::write(
        &source,
        concat!(
            "BEGIN_SHADER_PARAMETER_STRUCT(FPrimaryParameters, )\n",
            "\tSHADER_PARAMETER(float, oatmealfOld)\n",
            "END_SHADER_PARAMETER_STRUCT()\n",
            "BEGIN_SHADER_PARAMETER_STRUCT(FUnrelatedParameters, )\n",
            "\tSHADER_PARAMETER(float, oatmealfOld)\n",
            "END_SHADER_PARAMETER_STRUCT()\n",
            "void Assign(FPrimaryParameters& Parameters)\n",
            "{ Parameters.oatmealfOld = 1.0f; }\n",
        ),
    )
    .unwrap();

    let mut planner = SourcePlanner::new(&game, &editor, &root).unwrap();
    planner
        .global_renames
        .insert("oatmealfOld".to_string(), "Recovered".to_string());
    planner
        .removed_declarations
        .entry("oatmealfOld".to_string())
        .or_default()
        .insert(declaration_location(&source, 2));
    planner.add_rename_references().unwrap();

    assert!(!planner.can_emit());
    assert!(
        planner
            .unresolved
            .iter()
            .any(|issue| issue.contains("outside its reserved edit")),
        "{:?}",
        planner.unresolved
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn unreserved_structural_edit_requires_review() {
    let root = fixture_root();
    let game = dump::read(&root.join("game-layouts.json"), 1).unwrap();
    let editor = dump::read(&root.join("editor-layouts.json"), 1).unwrap();
    let mut planner = SourcePlanner::new(&game, &editor, &root).unwrap();
    let key = PathBuf::from("Unreserved.h");
    let mut plan = test_file_plan("SHADER_PARAMETER(float, Existing)\n");
    plan.replace_line(1, Some("SHADER_PARAMETER(uint32, Existing)".to_string()))
        .unwrap();
    planner.files.insert(key, plan);

    planner.add_rename_references().unwrap();
    assert!(!planner.can_emit());
    assert!(
        planner
            .unresolved
            .iter()
            .any(|issue| issue.contains("unreserved structural source edit")),
        "{:?}",
        planner.unresolved
    );
}

#[test]
fn recorded_windows_source_path_resolves_on_the_host_platform() {
    let root = fixture_root();
    let game = dump::read(&root.join("game-layouts.json"), 1).unwrap();
    let editor = dump::read(&root.join("editor-layouts.json"), 1).unwrap();
    let mut planner = SourcePlanner::new(&game, &editor, &root).unwrap();
    let resolved = planner
        .resolve_source_path(
            r"C:\build\UnrealEngine\Engine\Source\Runtime\Test\TestShaderParameters.h",
        )
        .unwrap();
    assert!(resolved.ends_with("Engine/Source/Runtime/Test/TestShaderParameters.h"));
}

#[test]
fn recorded_engine_plugin_source_path_resolves() {
    let fixture = fixture_root();
    let game = dump::read(&fixture.join("game-layouts.json"), 1).unwrap();
    let editor = dump::read(&fixture.join("editor-layouts.json"), 1).unwrap();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("oatmealf-plugin-source-{unique}"));
    fs::create_dir_all(root.join("Engine/Source")).unwrap();
    let plugin_source = root.join("Engine/Plugins/Example/Source/Runtime/Public/Example.h");
    fs::create_dir_all(plugin_source.parent().unwrap()).unwrap();
    fs::write(&plugin_source, "// fixture\n").unwrap();

    let mut planner = SourcePlanner::new(&game, &editor, &root).unwrap();
    let resolved = planner
        .resolve_source_path(
            r"C:\build\UnrealEngine\Engine\Plugins\Example\Source\Runtime\Public\Example.h",
        )
        .unwrap();
    assert_eq!(resolved, plugin_source.canonicalize().unwrap());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn source_definition_index_finds_core_definitions_and_skips_non_source_bytes() {
    let fixture = fixture_root();
    let game = dump::read(&fixture.join("game-layouts.json"), 1).unwrap();
    let editor = dump::read(&fixture.join("editor-layouts.json"), 1).unwrap();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("oatmealf-lazy-source-{unique}"));
    let core_source = root.join("Engine/Source/Runtime/Test/CoreParameters.h");
    let plugin_source = root.join("Engine/Plugins/Example/Source/Bad.h");
    fs::create_dir_all(core_source.parent().unwrap()).unwrap();
    fs::create_dir_all(plugin_source.parent().unwrap()).unwrap();
    fs::write(
        &core_source,
        "BEGIN_GLOBAL_SHADER_PARAMETER_STRUCT(FCoreParameters, )\n",
    )
    .unwrap();
    fs::write(&plugin_source, [0xff, 0xfe]).unwrap();

    let mut planner = SourcePlanner::new(&game, &editor, &root).unwrap();
    assert!(
        planner
            .source_contains_shader_definition("FCoreParameters")
            .unwrap()
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn engine_root_accepts_root_engine_and_source_directories() {
    let root = fixture_root().canonicalize().unwrap();
    assert_eq!(normalize_engine_root(&root).unwrap(), root);
    assert_eq!(normalize_engine_root(&root.join("Engine")).unwrap(), root);
    assert_eq!(
        normalize_engine_root(&root.join("Engine/Source")).unwrap(),
        root
    );
}

#[test]
fn nested_leaf_expansion_is_bounded() {
    let root = fixture_root();
    let game = dump::read(&root.join("game-layouts.json"), 1).unwrap();
    let editor = dump::read(&root.join("editor-layouts.json"), 1).unwrap();
    let planner = SourcePlanner::new(&game, &editor, &root).unwrap();
    let mut nested = game.definitions[&1].members[0].clone();
    nested.base_type = ShaderParameterBaseType::NestedStruct;
    nested.nested_definition_id = 1;
    nested.elements = 65_537;

    let error = planner
        .find_nested_leaf_access_member(&nested, &editor.definitions[&1].members[0])
        .unwrap_err();
    assert!(format!("{error:#}").contains("too many array elements"));
}

#[test]
fn deleting_the_last_line_of_a_continued_macro_is_rejected() {
    let plan = test_file_plan(concat!(
        "#define TEST_PARAMETER_TABLE \\\n",
        "\tTEST_MEMBER(float, Existing) \\\n",
        "\tTEST_MEMBER(float, oatmealfTail)\n",
        "BEGIN_SHADER_PARAMETER_STRUCT(FTest, )\n",
    ));
    assert!(is_safe_macro_line_deletion(&plan, 2));
    assert!(!is_safe_macro_line_deletion(&plan, 3));
}

#[test]
fn removing_placeholder_from_a_shared_parameter_table_is_rejected() {
    let root = fixture_root();
    let game = dump::read(&root.join("game-layouts.json"), 1).unwrap();
    let editor = dump::read(&root.join("editor-layouts.json"), 1).unwrap();
    let mut planner = SourcePlanner::new(&game, &editor, &root).unwrap();
    let key = PathBuf::from("Test.h");
    planner.files.insert(
        key.clone(),
        test_file_plan(concat!(
            "#define TEST_PARAMETER_TABLE \\\n",
            "\tTEST_MEMBER(float, oatmealfLegacy) \\\n",
            "\tTEST_MEMBER(float, Existing)\n",
            "BEGIN_SHADER_PARAMETER_STRUCT(FTestParameters, )\n",
            "\tTEST_PARAMETER_TABLE // shared declaration\n",
            "END_SHADER_PARAMETER_STRUCT()\n",
        )),
    );
    let member = test_member("oatmealfLegacy", 2);
    let mut definition = test_definition(member.clone());
    definition.source_line = 4;

    let error = planner
        .remove_placeholder(&key, &definition, &member)
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("shared parameter table TEST_PARAMETER_TABLE"),
        "{error:#}"
    );
}

#[test]
fn generated_struct_required_by_two_files_requires_review() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("oatmealf-two-parents-{unique}"));
    let source_directory = root.join("Engine/Source/Runtime/Test");
    fs::create_dir_all(&source_directory).unwrap();
    let parent_a = source_directory.join("ParentA.h");
    let parent_b = source_directory.join("ParentB.h");
    fs::write(&parent_a, "// Parent A\n").unwrap();
    fs::write(&parent_b, "// Parent B\n").unwrap();

    let generated_definition = DefinitionRecord {
        id: 3,
        type_name: "FGeneratedParameters".to_string(),
        layout_name: "FGeneratedParameters".to_string(),
        shader_variable_name: "Generated".to_string(),
        source_file: String::new(),
        source_line: 1,
        size: 16,
        use_case: 0,
        binding_flags: 0,
        usage_flags: 0,
        members: vec![test_member("Value", 1)],
    };
    let game = LayoutDump {
        path: PathBuf::from("game"),
        layouts: BTreeMap::new(),
        definitions: HashMap::from([(3, generated_definition)]),
    };
    let editor = LayoutDump {
        path: PathBuf::from("editor"),
        layouts: BTreeMap::new(),
        definitions: HashMap::new(),
    };
    let mut planner = SourcePlanner::new(&game, &editor, &root).unwrap();
    let key_a = planner.ensure_file(&parent_a).unwrap();
    let key_b = planner.ensure_file(&parent_b).unwrap();
    let member = MemberRecord {
        index: 0,
        name: "Generated".to_string(),
        offset: 0,
        base_type: ShaderParameterBaseType::NestedStruct,
        rows: 0,
        columns: 0,
        elements: 0,
        precision: 0,
        shader_type: String::new(),
        struct_type: "FGeneratedParameters".to_string(),
        source_line: 1,
        nested_definition_id: 3,
    };

    let container = test_definition(test_member("Existing", 1));
    planner
        .ensure_target_struct_available(&member, &key_a, 1, &container, "Generated")
        .unwrap();
    let error = planner
        .ensure_target_struct_available(&member, &key_b, 1, &container, "Generated")
        .unwrap_err();
    assert!(format!("{error:#}").contains("required by multiple source files"));
    fs::remove_dir_all(root).unwrap();
}

fn gap_layout(name: &str, type_name: &str, constant_buffer_size: u32) -> LayoutRecord {
    use crate::model::RESOURCE_ARRAY_NAMES;

    let mut layout = LayoutRecord::new(name.to_string(), type_name.to_string());
    layout.layout_name = type_name.to_string();
    layout.root_definition_id = 1;
    layout.constant_buffer_size = constant_buffer_size;
    layout.render_targets_offset = u16::MAX;
    layout.static_slot = u8::MAX;
    layout.binding_flags = 1;
    for array in RESOURCE_ARRAY_NAMES {
        layout.arrays.insert(array.to_string(), Vec::new());
    }
    layout.rhi_hash = hash::compute_rhi_hash(&layout);
    layout
}

fn gap_member(
    index: usize,
    name: &str,
    offset: u32,
    columns: u32,
    source_line: usize,
) -> MemberRecord {
    MemberRecord {
        index,
        name: name.to_string(),
        offset,
        base_type: ShaderParameterBaseType::Float32,
        rows: 1,
        columns,
        elements: 0,
        precision: 0,
        shader_type: String::new(),
        struct_type: String::new(),
        source_line,
        nested_definition_id: 0,
    }
}

fn gap_definition(
    type_name: &str,
    source_file: &str,
    size: u32,
    members: Vec<MemberRecord>,
) -> DefinitionRecord {
    DefinitionRecord {
        id: 1,
        type_name: type_name.to_string(),
        layout_name: type_name.to_string(),
        shader_variable_name: "Gap".to_string(),
        source_file: source_file.to_string(),
        source_line: 1,
        size,
        use_case: 0,
        binding_flags: 0,
        usage_flags: 0,
        members,
    }
}

fn gap_dump(layout: LayoutRecord, definition: DefinitionRecord) -> LayoutDump {
    LayoutDump {
        path: PathBuf::from("in-memory"),
        layouts: BTreeMap::from([(layout.name.clone(), layout)]),
        definitions: HashMap::from([(1, definition)]),
    }
}

#[test]
fn mid_struct_growth_is_not_modeled_as_trailing_padding() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("oatmealf-mid-struct-{unique}"));
    let source_directory = root.join("Engine/Source/Runtime/Test");
    fs::create_dir_all(&source_directory).unwrap();
    fs::write(
        source_directory.join("GapParameters.h"),
        concat!(
            "BEGIN_GLOBAL_SHADER_PARAMETER_STRUCT(FGapParameters, )\n",
            "\tSHADER_PARAMETER(float, Existing)\n",
            "\tSHADER_PARAMETER(FVector4f, Tail)\n",
            "END_GLOBAL_SHADER_PARAMETER_STRUCT()\n",
        ),
    )
    .unwrap();

    let recorded = "C:/Build/Engine/Source/Runtime/Test/GapParameters.h";
    let game = gap_dump(
        gap_layout("Gap", "FGapParameters", 48),
        gap_definition(
            "FGapParameters",
            recorded,
            48,
            vec![
                gap_member(0, "Existing", 0, 1, 2),
                gap_member(1, "Inserted", 16, 4, 3),
                gap_member(2, "Tail", 32, 4, 3),
            ],
        ),
    );
    let editor = gap_dump(
        gap_layout("Gap", "FGapParameters", 32),
        gap_definition(
            "FGapParameters",
            recorded,
            32,
            vec![
                gap_member(0, "Existing", 0, 1, 2),
                gap_member(1, "Tail", 16, 4, 3),
            ],
        ),
    );

    let mut planner = SourcePlanner::new(&game, &editor, &root).unwrap();
    planner.plan(&[]);
    planner.add_rename_references().unwrap();
    assert!(!planner.has_unresolved(), "{:?}", planner.unresolved);
    assert!(planner.can_emit());
    assert_eq!(planner.outcomes[0].status, "modeled exact");

    let rendered = planner
        .files
        .values()
        .find(|plan| plan.relative_path.ends_with("GapParameters.h"))
        .unwrap()
        .render()
        .unwrap();
    assert!(!rendered.contains("oatmealfPadding"), "{rendered}");
    let inserted = rendered
        .find("SHADER_PARAMETER(FVector4f, Inserted)")
        .unwrap();
    let tail = rendered.find("SHADER_PARAMETER(FVector4f, Tail)").unwrap();
    assert!(inserted < tail, "{rendered}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn legacy_shaderdrift_placeholder_still_allows_trailing_padding() {
    assert!(is_generated_placeholder("ShaderDriftPadding_InstancedView"));
    assert!(is_generated_placeholder("oatmealfPadding_View"));
    assert!(!is_generated_placeholder("SceneDepthTexture"));

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("oatmealf-legacy-padding-{unique}"));
    let source_directory = root.join("Engine/Source/Runtime/Test");
    fs::create_dir_all(&source_directory).unwrap();
    fs::write(
        source_directory.join("LegacyParameters.h"),
        concat!(
            "BEGIN_GLOBAL_SHADER_PARAMETER_STRUCT(FLegacyParameters, )\n",
            "\tSHADER_PARAMETER(float, Existing)\n",
            "\tSHADER_PARAMETER(FVector4f, ShaderDriftPad)\n",
            "END_GLOBAL_SHADER_PARAMETER_STRUCT()\n",
        ),
    )
    .unwrap();

    let recorded = "C:/Build/Engine/Source/Runtime/Test/LegacyParameters.h";
    let game = gap_dump(
        gap_layout("Legacy", "FLegacyParameters", 48),
        gap_definition(
            "FLegacyParameters",
            recorded,
            48,
            vec![
                gap_member(0, "Existing", 0, 1, 2),
                gap_member(1, "RealValue", 16, 4, 3),
                gap_member(2, "NewTail", 32, 4, 3),
            ],
        ),
    );
    let editor = gap_dump(
        gap_layout("Legacy", "FLegacyParameters", 32),
        gap_definition(
            "FLegacyParameters",
            recorded,
            32,
            vec![
                gap_member(0, "Existing", 0, 1, 2),
                gap_member(1, "ShaderDriftPad", 16, 4, 3),
            ],
        ),
    );

    let mut planner = SourcePlanner::new(&game, &editor, &root).unwrap();
    planner.plan(&[]);
    planner.add_rename_references().unwrap();
    assert!(!planner.has_unresolved(), "{:?}", planner.unresolved);
    assert_eq!(planner.outcomes[0].status, "modeled exact");
    assert!(
        planner.outcomes[0].detail.contains("placeholder constants"),
        "{}",
        planner.outcomes[0].detail
    );

    let rendered = planner
        .files
        .values()
        .find(|plan| plan.relative_path.ends_with("LegacyParameters.h"))
        .unwrap()
        .render()
        .unwrap();
    assert!(
        rendered.contains("SHADER_PARAMETER_ARRAY(FVector4f, oatmealfPadding_Legacy, [1])"),
        "{rendered}"
    );
    assert!(rendered.contains("ShaderDriftPad"), "{rendered}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn trailing_insertions_append_inside_the_consumer_block() {
    use crate::model::RESOURCE_ARRAY_NAMES;

    fn vec4_member(index: usize, name: &str, offset: u32, source_line: usize) -> MemberRecord {
        MemberRecord {
            index,
            name: name.to_string(),
            offset,
            base_type: ShaderParameterBaseType::Float32,
            rows: 1,
            columns: 4,
            elements: 0,
            precision: 0,
            shader_type: String::new(),
            struct_type: String::new(),
            source_line,
            nested_definition_id: 0,
        }
    }
    fn tail_definition(
        id: u32,
        type_name: &str,
        source_file: &str,
        begin_line: usize,
        size: u32,
        members: Vec<MemberRecord>,
    ) -> DefinitionRecord {
        DefinitionRecord {
            id,
            type_name: type_name.to_string(),
            layout_name: type_name.to_string(),
            shader_variable_name: type_name.trim_start_matches('F').to_string(),
            source_file: source_file.to_string(),
            source_line: begin_line,
            size,
            use_case: 0,
            binding_flags: 0,
            usage_flags: 0,
            members,
        }
    }
    fn tail_layout(name: &str, type_name: &str, root: u32, size: u32) -> LayoutRecord {
        let mut layout = LayoutRecord::new(name.to_string(), type_name.to_string());
        layout.layout_name = type_name.to_string();
        layout.root_definition_id = root;
        layout.constant_buffer_size = size;
        layout.render_targets_offset = u16::MAX;
        layout.static_slot = u8::MAX;
        for array in RESOURCE_ARRAY_NAMES {
            layout.arrays.insert(array.to_string(), Vec::new());
        }
        layout
    }
    fn tail_dump(layouts: Vec<LayoutRecord>, definitions: Vec<DefinitionRecord>) -> LayoutDump {
        LayoutDump {
            path: PathBuf::from("in-memory"),
            layouts: layouts
                .into_iter()
                .map(|layout| (layout.name.clone(), layout))
                .collect(),
            definitions: definitions
                .into_iter()
                .map(|definition| (definition.id, definition))
                .collect(),
        }
    }

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("oatmealf-trailing-append-{unique}"));
    let source_directory = root.join("Engine/Source/Runtime/Test");
    fs::create_dir_all(&source_directory).unwrap();
    fs::write(
        source_directory.join("SharedTail.h"),
        concat!(
            "#define TAIL_SHARED_TABLE \\\n",
            "\tTAIL_MEMBER(FVector4f, SharedExisting) \\\n",
            "\tTAIL_MEMBER(FVector4f, SharedAnchor) \\\n",
            "\n",
            "BEGIN_SHADER_PARAMETER_STRUCT(FTailAlphaParameters, )\n",
            "\tTAIL_SHARED_TABLE\n",
            "\tSHADER_PARAMETER(FVector4f, AlphaTail)\n",
            "END_SHADER_PARAMETER_STRUCT()\n",
            "\n",
            "BEGIN_SHADER_PARAMETER_STRUCT(FTailBetaParameters, )\n",
            "\tTAIL_SHARED_TABLE\n",
            "END_SHADER_PARAMETER_STRUCT()\n",
        ),
    )
    .unwrap();

    let recorded = "C:/Build/Engine/Source/Runtime/Test/SharedTail.h";
    let game = tail_dump(
        vec![
            tail_layout("TailAlpha", "FTailAlphaParameters", 1, 64),
            tail_layout("TailBeta", "FTailBetaParameters", 2, 64),
        ],
        vec![
            tail_definition(
                1,
                "FTailAlphaParameters",
                recorded,
                5,
                64,
                vec![
                    vec4_member(0, "SharedExisting", 0, 2),
                    vec4_member(1, "NewShared", 16, 2),
                    vec4_member(2, "SharedAnchor", 32, 3),
                    vec4_member(3, "AlphaTail", 48, 7),
                ],
            ),
            tail_definition(
                2,
                "FTailBetaParameters",
                recorded,
                10,
                64,
                vec![
                    vec4_member(0, "SharedExisting", 0, 2),
                    vec4_member(1, "NewShared", 16, 2),
                    vec4_member(2, "SharedAnchor", 32, 3),
                    vec4_member(3, "BetaTail", 48, 0),
                ],
            ),
        ],
    );
    let editor = tail_dump(
        vec![
            tail_layout("TailAlpha", "FTailAlphaParameters", 1, 48),
            tail_layout("TailBeta", "FTailBetaParameters", 2, 32),
        ],
        vec![
            tail_definition(
                1,
                "FTailAlphaParameters",
                recorded,
                5,
                48,
                vec![
                    vec4_member(0, "SharedExisting", 0, 2),
                    vec4_member(1, "SharedAnchor", 16, 3),
                    vec4_member(2, "AlphaTail", 32, 7),
                ],
            ),
            tail_definition(
                2,
                "FTailBetaParameters",
                recorded,
                10,
                32,
                vec![
                    vec4_member(0, "SharedExisting", 0, 2),
                    vec4_member(1, "SharedAnchor", 16, 3),
                ],
            ),
        ],
    );

    let mut planner = SourcePlanner::new(&game, &editor, &root).unwrap();
    planner.plan(&[]);
    planner.add_rename_references().unwrap();
    assert!(!planner.has_unresolved(), "{:?}", planner.unresolved);
    assert!(planner.can_emit());
    assert!(
        planner
            .outcomes
            .iter()
            .all(|outcome| outcome.status == "modeled exact"),
        "{:?}",
        planner.outcomes
    );

    let rendered = planner
        .files
        .values()
        .find(|plan| plan.relative_path.ends_with("SharedTail.h"))
        .unwrap()
        .render()
        .unwrap();
    assert!(
        rendered.contains("TAIL_MEMBER(FVector4f, NewShared) \\"),
        "{rendered}"
    );
    assert_eq!(rendered.matches("BetaTail").count(), 1, "{rendered}");
    let beta_block = rendered.find("FTailBetaParameters").unwrap();
    let beta_tail = rendered
        .find("SHADER_PARAMETER(FVector4f, BetaTail)")
        .unwrap();
    assert!(beta_tail > beta_block, "{rendered}");
    fs::remove_dir_all(root).unwrap();
}

fn res_member(
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

fn res_layout(constant_buffer_size: u32, resources: Vec<ResourceEntry>) -> LayoutRecord {
    use crate::model::RESOURCE_ARRAY_NAMES;

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

fn res_dumps(recorded: &str) -> (LayoutDump, LayoutDump) {
    let editor_members = vec![
        res_member(0, "Existing", 0, ShaderParameterBaseType::Float32, "", 4, 2),
        res_member(
            1,
            "ExistingVolume",
            16,
            ShaderParameterBaseType::RdgTexture,
            "Texture3D",
            0,
            3,
        ),
        res_member(
            2,
            "ExistingSampler",
            24,
            ShaderParameterBaseType::Sampler,
            "SamplerState",
            0,
            4,
        ),
        res_member(3, "Tail", 32, ShaderParameterBaseType::Float32, "", 4, 5),
    ];
    let mut game_members = editor_members.clone();
    game_members.push(res_member(
        4,
        "NewVolume",
        48,
        ShaderParameterBaseType::RdgTexture,
        "Texture3D",
        0,
        5,
    ));
    game_members.push(res_member(
        5,
        "NewSampler",
        56,
        ShaderParameterBaseType::Sampler,
        "SamplerState",
        0,
        5,
    ));
    let definition = |members: Vec<MemberRecord>, size: u32| DefinitionRecord {
        id: 1,
        type_name: "FResParameters".to_string(),
        layout_name: "FResParameters".to_string(),
        shader_variable_name: "Res".to_string(),
        source_file: recorded.to_string(),
        source_line: 1,
        size,
        use_case: 0,
        binding_flags: 0,
        usage_flags: 0,
        members,
    };
    let game = gap_dump(
        res_layout(
            48,
            vec![
                ResourceEntry {
                    offset: 16,
                    resource_type: ShaderParameterBaseType::RdgTexture,
                },
                ResourceEntry {
                    offset: 24,
                    resource_type: ShaderParameterBaseType::Sampler,
                },
                ResourceEntry {
                    offset: 48,
                    resource_type: ShaderParameterBaseType::RdgTexture,
                },
                ResourceEntry {
                    offset: 56,
                    resource_type: ShaderParameterBaseType::Sampler,
                },
            ],
        ),
        definition(game_members, 64),
    );
    let editor = gap_dump(
        res_layout(
            48,
            vec![
                ResourceEntry {
                    offset: 16,
                    resource_type: ShaderParameterBaseType::RdgTexture,
                },
                ResourceEntry {
                    offset: 24,
                    resource_type: ShaderParameterBaseType::Sampler,
                },
            ],
        ),
        definition(editor_members, 48),
    );
    (game, editor)
}

const RES_HEADER: &str = concat!(
    "BEGIN_GLOBAL_SHADER_PARAMETER_STRUCT(FResParameters, )\n",
    "\tSHADER_PARAMETER(FVector4f, Existing)\n",
    "\tSHADER_PARAMETER_RDG_TEXTURE(Texture3D, ExistingVolume)\n",
    "\tSHADER_PARAMETER_SAMPLER(SamplerState, ExistingSampler)\n",
    "\tSHADER_PARAMETER(FVector4f, Tail)\n",
    "END_GLOBAL_SHADER_PARAMETER_STRUCT()\n",
);

#[test]
fn placeholder_bindings_do_not_clone_locals_from_nested_scopes() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("oatmealf-bindings-{unique}"));
    let source_directory = root.join("Engine/Source/Runtime/Test");
    fs::create_dir_all(&source_directory).unwrap();
    fs::write(source_directory.join("ResHeader.h"), RES_HEADER).unwrap();
    fs::write(
            source_directory.join("ResFill.cpp"),
            concat!(
                "#include \"ResHeader.h\"\n",
                "\n",
                "void SetupResParameters(FRDGBuilder& GraphBuilder, FResParameters& OutParameters)\n",
                "{\n",
                "\tif (GHasVolume)\n",
                "\t{\n",
                "\t\tOutParameters.ExistingVolume = RealVolumeTexture;\n",
                "\t}\n",
                "\telse\n",
                "\t{\n",
                "\t\tconst FRDGSystemTextures& SystemTextures = FRDGSystemTextures::Get(GraphBuilder);\n",
                "\t\tOutParameters.ExistingVolume = SystemTextures.BlackVolumeDummy;\n",
                "\t}\n",
                "\tOutParameters.ExistingSampler = TStaticSamplerState<SF_Bilinear, AM_Clamp, AM_Clamp, AM_Clamp>::GetRHI();\n",
                "\tOutParameters.Existing = ComputeExisting();\n",
                "\tOutParameters.Tail = ComputeTail();\n",
                "}\n",
            ),
        )
        .unwrap();

    let (game, editor) = res_dumps("C:/Build/Engine/Source/Runtime/Test/ResHeader.h");
    let mut planner = SourcePlanner::new(&game, &editor, &root).unwrap();
    planner.plan(&[]);
    planner.plan_runtime_assignments().unwrap();
    planner.add_rename_references().unwrap();
    assert!(!planner.has_unresolved(), "{:?}", planner.unresolved);
    assert!(planner.review.is_empty(), "{:?}", planner.review);
    assert!(planner.can_emit());
    assert_eq!(
        planner.runtime_bindings.len(),
        2,
        "{:?}",
        planner.runtime_bindings
    );

    let rendered = planner
        .files
        .values()
        .find(|plan| plan.relative_path.ends_with("ResFill.cpp"))
        .unwrap()
        .render()
        .unwrap();
    let volume = rendered
            .find("\tOutParameters.NewVolume = FRDGSystemTextures::Get(GraphBuilder).VolumetricBlackAlphaOne;")
            .unwrap_or_else(|| panic!("{rendered}"));
    let sampler = rendered
            .find("\tOutParameters.NewSampler = TStaticSamplerState<SF_Bilinear, AM_Clamp, AM_Clamp, AM_Clamp>::GetRHI();")
            .unwrap_or_else(|| panic!("{rendered}"));
    let anchor = rendered
        .find("OutParameters.Tail = ComputeTail();")
        .unwrap();
    assert!(volume > anchor && sampler > anchor, "{rendered}");
    assert!(
        !rendered.contains("OutParameters.NewVolume = SystemTextures.BlackVolumeDummy"),
        "{rendered}"
    );

    let header = planner
        .files
        .values()
        .find(|plan| plan.relative_path.ends_with("ResHeader.h"))
        .unwrap()
        .render()
        .unwrap();
    assert!(
        header.contains("SHADER_PARAMETER_RDG_TEXTURE(Texture3D, NewVolume)"),
        "{header}"
    );

    let output = std::env::temp_dir().join(format!("oatmealf-bindings-out-{unique}"));
    let files = planner.emit(&output).unwrap();
    assert!(
        files.contains(&"Engine/Source/Runtime/Test/ResFill.cpp".to_string()),
        "{files:?}"
    );
    assert!(
        files.contains(&"Engine/Source/Runtime/Test/ResHeader.h".to_string()),
        "{files:?}"
    );
    fs::remove_dir_all(&output).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn unbound_members_keep_a_plain_review_note() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("oatmealf-unbound-{unique}"));
    let source_directory = root.join("Engine/Source/Runtime/Test");
    fs::create_dir_all(&source_directory).unwrap();
    fs::write(source_directory.join("ResHeader.h"), RES_HEADER).unwrap();

    let (game, editor) = res_dumps("C:/Build/Engine/Source/Runtime/Test/ResHeader.h");
    let mut planner = SourcePlanner::new(&game, &editor, &root).unwrap();
    planner.plan(&[]);
    planner.plan_runtime_assignments().unwrap();
    planner.add_rename_references().unwrap();
    assert!(!planner.has_unresolved(), "{:?}", planner.unresolved);
    assert!(planner.can_emit());
    assert!(planner.runtime_bindings.is_empty());
    assert_eq!(planner.review.len(), 2, "{:?}", planner.review);
    assert!(
        planner
            .review
            .iter()
            .all(|note| note.contains("no safe placeholder binding was found")),
        "{:?}",
        planner.review
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn constructor_sites_bind_with_bare_receivers() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("oatmealf-ctor-bindings-{unique}"));
    let source_directory = root.join("Engine/Source/Runtime/Test");
    fs::create_dir_all(&source_directory).unwrap();
    fs::write(source_directory.join("ResHeader.h"), RES_HEADER).unwrap();
    fs::write(
            source_directory.join("ResConstructor.cpp"),
            concat!(
                "FResParameters::FResParameters()\n",
                "{\n",
                "\tExistingVolume = SystemTextures.BlackVolumeDummy;\n",
                "\tExistingSampler = TStaticSamplerState<SF_Point, AM_Clamp, AM_Clamp, AM_Clamp>::GetRHI();\n",
                "\tExisting = DefaultExisting();\n",
                "\tTail = DefaultTail();\n",
                "}\n",
            ),
        )
        .unwrap();

    let (game, editor) = res_dumps("C:/Build/Engine/Source/Runtime/Test/ResHeader.h");
    let mut planner = SourcePlanner::new(&game, &editor, &root).unwrap();
    planner.plan(&[]);
    planner.plan_runtime_assignments().unwrap();
    planner.add_rename_references().unwrap();
    assert!(planner.review.is_empty(), "{:?}", planner.review);
    assert!(planner.can_emit());

    let rendered = planner
        .files
        .values()
        .find(|plan| plan.relative_path.ends_with("ResConstructor.cpp"))
        .unwrap()
        .render()
        .unwrap();
    assert!(
        rendered.contains("\tNewVolume = SystemTextures.BlackVolumeDummy;"),
        "{rendered}"
    );
    assert!(!rendered.contains(".NewVolume"), "{rendered}");
    let tail = rendered.find("Tail = DefaultTail();").unwrap();
    let inserted = rendered.find("\tNewVolume =").unwrap();
    assert!(inserted > tail, "{rendered}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn returned_local_with_one_outer_anchor_is_bound_before_return() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("oatmealf-return-bindings-{unique}"));
    let source_directory = root.join("Engine/Source/Runtime/Test");
    fs::create_dir_all(&source_directory).unwrap();
    fs::write(source_directory.join("ResHeader.h"), RES_HEADER).unwrap();
    fs::write(
            source_directory.join("ResFactory.cpp"),
            concat!(
                "FResParameters MakeRes(FRDGBuilder& GraphBuilder)\n",
                "{\n",
                "\tFResParameters Out;\n",
                "\tif (GHasVolume)\n",
                "\t{\n",
                "\t\tOut.ExistingVolume = RealVolumeTexture;\n",
                "\t}\n",
                "\telse\n",
                "\t{\n",
                "\t\tconst FRDGSystemTextures& SystemTextures = FRDGSystemTextures::Get(GraphBuilder);\n",
                "\t\tOut.ExistingVolume = SystemTextures.BlackVolumeDummy;\n",
                "\t}\n",
                "\tOut.ExistingSampler = TStaticSamplerState<SF_Bilinear>::GetRHI();\n",
                "\treturn Out;\n",
                "}\n",
            ),
        )
        .unwrap();

    let (game, editor) = res_dumps("C:/Build/Engine/Source/Runtime/Test/ResHeader.h");
    let mut planner = SourcePlanner::new(&game, &editor, &root).unwrap();
    planner.plan(&[]);
    planner.plan_runtime_assignments().unwrap();
    planner.add_rename_references().unwrap();
    assert!(planner.review.is_empty(), "{:?}", planner.review);
    assert_eq!(planner.runtime_bindings.len(), 2);

    let rendered = planner
        .files
        .values()
        .find(|plan| plan.relative_path.ends_with("ResFactory.cpp"))
        .unwrap()
        .render()
        .unwrap();
    let inserted = rendered.find("\tOut.NewVolume =").unwrap();
    let returned = rendered.find("\treturn Out;").unwrap();
    assert!(inserted < returned, "{rendered}");
    assert_eq!(rendered.matches("Out.NewVolume =").count(), 1, "{rendered}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn constructor_is_preferred_over_other_producer_functions() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("oatmealf-ctor-preferred-{unique}"));
    let source_directory = root.join("Engine/Source/Runtime/Test");
    fs::create_dir_all(&source_directory).unwrap();
    fs::write(source_directory.join("ResHeader.h"), RES_HEADER).unwrap();
    fs::write(
        source_directory.join("ResSites.cpp"),
        concat!(
            "FResParameters::FResParameters()\n",
            "{\n",
            "\tExistingVolume = SystemTextures.BlackVolumeDummy;\n",
            "\tExistingSampler = TStaticSamplerState<SF_Point>::GetRHI();\n",
            "\tExisting = DefaultExisting();\n",
            "\tTail = DefaultTail();\n",
            "}\n",
            "\n",
            "FResParameters MakeRes()\n",
            "{\n",
            "\tFResParameters Out;\n",
            "\tOut.ExistingVolume = SystemTextures.BlackVolumeDummy;\n",
            "\tOut.ExistingSampler = TStaticSamplerState<SF_Point>::GetRHI();\n",
            "\treturn Out;\n",
            "}\n",
        ),
    )
    .unwrap();

    let (game, editor) = res_dumps("C:/Build/Engine/Source/Runtime/Test/ResHeader.h");
    let mut planner = SourcePlanner::new(&game, &editor, &root).unwrap();
    planner.plan(&[]);
    planner.plan_runtime_assignments().unwrap();
    planner.add_rename_references().unwrap();
    assert!(planner.review.is_empty(), "{:?}", planner.review);
    assert_eq!(planner.runtime_bindings.len(), 2);

    let rendered = planner
        .files
        .values()
        .find(|plan| plan.relative_path.ends_with("ResSites.cpp"))
        .unwrap()
        .render()
        .unwrap();
    assert_eq!(rendered.matches("NewVolume =").count(), 1, "{rendered}");
    assert!(rendered.contains("\tNewVolume ="), "{rendered}");
    assert!(!rendered.contains("\tOut.NewVolume ="), "{rendered}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn receiver_type_must_be_declared_in_the_same_function() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("oatmealf-same-function-{unique}"));
    let source_directory = root.join("Engine/Source/Runtime/Test");
    fs::create_dir_all(&source_directory).unwrap();
    fs::write(source_directory.join("ResHeader.h"), RES_HEADER).unwrap();
    fs::write(
        source_directory.join("WrongReceiver.cpp"),
        concat!(
            "void MentionsCorrectType(FResParameters& Other)\n",
            "{\n",
            "}\n",
            "\n",
            "void SetupWrongType(FUnrelatedParameters& OutParameters)\n",
            "{\n",
            "\tOutParameters.ExistingVolume = SystemTextures.BlackVolumeDummy;\n",
            "\tOutParameters.ExistingSampler = TStaticSamplerState<SF_Point>::GetRHI();\n",
            "\tOutParameters.Existing = DefaultExisting();\n",
            "\tOutParameters.Tail = DefaultTail();\n",
            "}\n",
        ),
    )
    .unwrap();

    let (game, editor) = res_dumps("C:/Build/Engine/Source/Runtime/Test/ResHeader.h");
    let mut planner = SourcePlanner::new(&game, &editor, &root).unwrap();
    planner.plan(&[]);
    planner.plan_runtime_assignments().unwrap();
    assert!(planner.runtime_bindings.is_empty());
    assert_eq!(planner.review.len(), 2, "{:?}", planner.review);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn incomplete_binding_site_is_skipped_as_a_unit() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("oatmealf-incomplete-site-{unique}"));
    let source_directory = root.join("Engine/Source/Runtime/Test");
    fs::create_dir_all(&source_directory).unwrap();
    fs::write(source_directory.join("ResHeader.h"), RES_HEADER).unwrap();
    fs::write(
        source_directory.join("IncompleteFill.cpp"),
        concat!(
            "void SetupRes(FResParameters& OutParameters)\n",
            "{\n",
            "\tOutParameters.ExistingVolume = RealVolumeTexture;\n",
            "\tOutParameters.ExistingSampler = TStaticSamplerState<SF_Point>::GetRHI();\n",
            "\tOutParameters.Existing = DefaultExisting();\n",
            "\tOutParameters.Tail = DefaultTail();\n",
            "}\n",
        ),
    )
    .unwrap();

    let (game, editor) = res_dumps("C:/Build/Engine/Source/Runtime/Test/ResHeader.h");
    let mut planner = SourcePlanner::new(&game, &editor, &root).unwrap();
    planner.plan(&[]);
    planner.plan_runtime_assignments().unwrap();
    assert!(planner.runtime_bindings.is_empty());
    assert_eq!(planner.review.len(), 2, "{:?}", planner.review);
    let plan = planner
        .files
        .values()
        .find(|plan| plan.relative_path.ends_with("IncompleteFill.cpp"));
    assert!(plan.is_none(), "an incomplete site must not be edited");
    fs::remove_dir_all(root).unwrap();
}
