use assert_cmd::Command;
use predicates::prelude::*;
use std::time::{SystemTime, UNIX_EPOCH};

fn review_command(label: &str) -> (Command, std::path::PathBuf) {
    let fixture_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("oatmealf-{label}-{unique}"));
    std::fs::create_dir(&root).unwrap();
    let mut game: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture_root.join("game-layouts.json")).unwrap())
            .unwrap();
    let mut editor: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture_root.join("editor-layouts.json")).unwrap())
            .unwrap();
    for evidence in [&mut game, &mut editor] {
        let template = evidence["layouts"][0].clone();
        let layouts = evidence["layouts"].as_array_mut().unwrap();
        layouts.clear();
        for index in 0..32 {
            let mut layout = template.clone();
            layout["name"] = if index == 0 {
                "View".into()
            } else {
                format!("Fixture{index:02}").into()
            };
            if index == 0 {
                layout["typeName"] = "FViewUniformShaderParameters".into();
                layout["layoutName"] = "FViewUniformShaderParameters".into();
                let record: oatmealf::model::LayoutRecord =
                    serde_json::from_value(layout.clone()).unwrap();
                layout["rhiHash"] =
                    format!("0x{:08X}", oatmealf::hash::compute_rhi_hash(&record)).into();
            }
            layouts.push(layout);
        }
    }
    let game_dump = root.join("game-layouts.json");
    let editor_dump = root.join("editor-layouts.json");
    std::fs::write(&game_dump, serde_json::to_vec_pretty(&game).unwrap()).unwrap();
    std::fs::write(&editor_dump, serde_json::to_vec_pretty(&editor).unwrap()).unwrap();
    let output = root.join("output");
    let mut command = Command::cargo_bin("oatmealf").unwrap();
    command
        .arg("--game-layouts")
        .arg(game_dump)
        .arg("--editor-layouts")
        .arg(editor_dump)
        .arg("--engine-root")
        .arg(&fixture_root)
        .args(["--layout", "MissingLayout", "--output"])
        .arg(&output);
    (command, root)
}

#[test]
fn help_is_short_and_names_the_normal_inputs() {
    Command::cargo_bin("oatmealf")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Compare UE 5.6 shader parameter layouts",
        ))
        .stdout(predicate::str::contains("--game <PROCESS>"))
        .stdout(predicate::str::contains("--engine-root <PATH>"));
}

#[test]
fn missing_saved_capture_is_an_error() {
    let fixture_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");
    Command::cargo_bin("oatmealf")
        .unwrap()
        .args([
            "--game-layouts",
            "missing-game-layouts.json",
            "--editor-layouts",
            "missing-editor-layouts.json",
            "--engine-root",
        ])
        .arg(fixture_root)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("layout evidence not found"));
}

#[test]
fn missing_engine_source_prints_rerun_guidance_and_succeeds() {
    let fixture_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("oatmealf-no-source-{unique}"));
    let engine_root = root.join("InstalledEngine");
    let output = root.join("output");
    std::fs::create_dir_all(engine_root.join("Engine/Binaries/Win64")).unwrap();

    Command::cargo_bin("oatmealf")
        .unwrap()
        .arg("--game-layouts")
        .arg(fixture_root.join("game-layouts.json"))
        .arg("--editor-layouts")
        .arg(fixture_root.join("editor-layouts.json"))
        .arg("--engine-root")
        .arg(&engine_root)
        .arg("--output")
        .arg(&output)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Unreal Engine source was not found under",
        ))
        .stdout(predicate::str::contains(
            r"Run oatmealf again with --engine-root C:\path\to\UnrealEngine",
        ))
        .stderr(predicate::str::is_empty());

    assert!(!output.exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn saved_captures_without_engine_source_print_guidance_and_succeed() {
    let fixture_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("oatmealf-saved-no-source-{unique}"));
    let output = root.join("output");

    Command::cargo_bin("oatmealf")
        .unwrap()
        .arg("--game-layouts")
        .arg(fixture_root.join("game-layouts.json"))
        .arg("--editor-layouts")
        .arg(fixture_root.join("editor-layouts.json"))
        .arg("--output")
        .arg(&output)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Saved layout evidence does not identify an Unreal Engine source tree.",
        ))
        .stdout(predicate::str::contains(
            r"Run oatmealf again with --engine-root C:\path\to\UnrealEngine",
        ))
        .stderr(predicate::str::is_empty());

    assert!(!output.exists());
}

#[test]
fn live_game_and_saved_game_dump_conflict() {
    Command::cargo_bin("oatmealf")
        .unwrap()
        .args(["--game", "ExampleGame", "--game-layouts", "game.json"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn live_editor_and_saved_editor_dump_conflict() {
    Command::cargo_bin("oatmealf")
        .unwrap()
        .args([
            "--editor",
            "ExampleEditor",
            "--editor-layouts",
            "editor.json",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn same_capture_cannot_be_used_for_both_sides() {
    let fixture_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");
    let capture = fixture_root.join("game-layouts.json");
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let output = std::env::temp_dir().join(format!("oatmealf-same-capture-{unique}"));
    Command::cargo_bin("oatmealf")
        .unwrap()
        .arg("--game-layouts")
        .arg(&capture)
        .arg("--editor-layouts")
        .arg(&capture)
        .arg("--engine-root")
        .arg(&fixture_root)
        .arg("--output")
        .arg(&output)
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "game and editor evidence refer to the same capture",
        ));
    assert!(!output.exists());
}

#[test]
fn empty_layout_name_is_an_error() {
    Command::cargo_bin("oatmealf")
        .unwrap()
        .arg("--layout=")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("layout name cannot be empty"));
}

#[test]
fn review_result_succeeds_by_default() {
    let (mut command, root) = review_command("review-default");
    command
        .assert()
        .success()
        .stdout(predicate::str::contains("Review needed."));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn existing_output_parent_gets_a_new_run_directory() {
    let (mut command, root) = review_command("output-parent");
    let output_parent = root.join("output");
    std::fs::create_dir_all(&output_parent).unwrap();
    let sentinel = output_parent.join("keep.txt");
    std::fs::write(&sentinel, "leave this alone").unwrap();

    command.assert().success();

    assert_eq!(
        std::fs::read_to_string(sentinel).unwrap(),
        "leave this alone"
    );
    let run_directories = std::fs::read_dir(&output_parent)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .collect::<Vec<_>>();
    assert_eq!(run_directories.len(), 1);
    assert!(run_directories[0].path().join("report.md").is_file());

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn dump_option_aliases_from_earlier_releases_still_parse() {
    let fixture_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");
    Command::cargo_bin("oatmealf")
        .unwrap()
        .args([
            "--game-dump",
            "missing-game-layouts.json",
            "--editor-dump",
            "missing-editor-layouts.json",
            "--engine-root",
        ])
        .arg(fixture_root)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("layout evidence not found"));
}

#[cfg(windows)]
#[test]
fn missing_named_process_recommends_the_current_capture_option() {
    let fixture_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");
    Command::cargo_bin("oatmealf")
        .unwrap()
        .arg("--game-layouts")
        .arg(fixture_root.join("game-layouts.json"))
        .args(["--editor", "OatmealfDefinitelyMissingEditor"])
        .arg("--engine-root")
        .arg(fixture_root)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--editor-layouts"))
        .stderr(predicate::str::contains("--editor-dump").not());
}
