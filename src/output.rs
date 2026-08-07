use crate::cancel;
use crate::dump;
use crate::hash;
use crate::model::{DefinitionRecord, LayoutDump, MemberRecord};
use crate::planner::{LayoutOutcome, SourcePlanner, write_macro};
use crate::progress;
use crate::simulate;
use anyhow::{Result, bail};
use chrono::{Local, SecondsFormat};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub const PRODUCTION_MINIMUM_LAYOUTS: usize = 32;
const GAME_EXPORT_PATH: &str = "exports/game-layouts.json";
const EDITOR_EXPORT_PATH: &str = "exports/editor-layouts.json";

#[derive(Debug)]
pub struct GenerateSummary {
    review_items: usize,
    missing_layouts: usize,
}

impl GenerateSummary {
    pub fn needs_review(&self) -> bool {
        self.review_items > 0
    }

    pub fn has_missing_layouts(&self) -> bool {
        self.missing_layouts > 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RunResult {
    Ready,
    MissingLayouts,
    ReviewNeeded,
}

impl RunResult {
    fn report_label(self) -> &'static str {
        match self {
            Self::Ready => "READY",
            Self::MissingLayouts => "MISSING LAYOUTS",
            Self::ReviewNeeded => "REVIEW NEEDED",
        }
    }

    fn manifest_value(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::MissingLayouts => "missing_layouts",
            Self::ReviewNeeded => "review_needed",
        }
    }
}

pub fn generate(
    game_dump_path: &Path,
    editor_dump_path: &Path,
    engine_root: &Path,
    output_directory: &Path,
    requested_layouts: &[String],
    verbose: bool,
) -> Result<GenerateSummary> {
    generate_with_minimum(
        game_dump_path,
        editor_dump_path,
        engine_root,
        output_directory,
        requested_layouts,
        PRODUCTION_MINIMUM_LAYOUTS,
        verbose,
    )
}

pub fn generate_with_minimum(
    game_dump_path: &Path,
    editor_dump_path: &Path,
    engine_root: &Path,
    output_directory: &Path,
    requested_layouts: &[String],
    minimum_layouts: usize,
    verbose: bool,
) -> Result<GenerateSummary> {
    let output_directory = absolute_path(output_directory)?;
    if output_directory.exists() {
        bail!(
            "output path already exists; choose a new directory: {}",
            output_directory.display()
        );
    }

    // reject same-kind captures before they can look like a valid no-drift result
    progress::step("Verifying captured layouts...");
    dump::validate_pair(game_dump_path, editor_dump_path)?;
    let game = dump::read(game_dump_path, minimum_layouts)?;
    let editor = dump::read(editor_dump_path, minimum_layouts)?;
    let game_layouts_verified = hash::validate(&game)?;
    let editor_layouts_verified = hash::validate(&editor)?;
    cancel::check()?;

    if verbose {
        println!("Verified {game_layouts_verified} game layouts");
        println!("Verified {editor_layouts_verified} editor layouts");
        print_packing_summary("game", &game);
        print_packing_summary("editor", &editor);
    }

    let mut planner = SourcePlanner::new(&game, &editor, engine_root)?;
    progress::step(&format!("Comparing {} layouts...", game.layouts.len()));
    planner.plan(requested_layouts);
    cancel::check()?;
    planner.plan_runtime_assignments()?;
    cancel::check()?;
    if planner.can_emit() {
        planner.add_rename_references()?;
    }
    cancel::check()?;

    // preserve captures even when planning needs review
    progress::step("Writing output...");
    fs::create_dir_all(&output_directory)?;
    let exports_directory = output_directory.join("exports");
    fs::create_dir_all(&exports_directory)?;
    fs::copy(game_dump_path, output_directory.join(GAME_EXPORT_PATH))?;
    fs::copy(editor_dump_path, output_directory.join(EDITOR_EXPORT_PATH))?;

    let source_files = if planner.can_emit() {
        planner.emit(&output_directory)?
    } else {
        Vec::new()
    };

    write_report(
        &output_directory,
        &game,
        &editor,
        &planner,
        &source_files,
        !requested_layouts.is_empty(),
        verbose,
    )?;
    write_manifest(
        &output_directory,
        game_layouts_verified,
        editor_layouts_verified,
        &planner,
        &source_files,
    )?;

    let compared = planner.outcomes.len();
    let changed = planner
        .outcomes
        .iter()
        .filter(|outcome| !matches!(outcome.status.as_str(), "already exact" | "not loaded"))
        .count();
    let verified = planner
        .outcomes
        .iter()
        .filter(|outcome| outcome.status == "modeled exact")
        .count();
    let missing_layouts = planner.missing_layouts.len();
    let review_items = planner.unresolved.len() + planner.review.len();
    let result = run_result(&planner);

    println!();
    match result {
        RunResult::ReviewNeeded => println!("Review needed."),
        RunResult::MissingLayouts => println!("Some layouts were not loaded in the editor."),
        RunResult::Ready if changed > 0 => {
            println!("Shader layout changes modeled successfully.")
        }
        RunResult::Ready => println!("Shader layouts matched successfully."),
    }
    println!();
    println!(
        "{}",
        format_layout_count(compared, game.layouts.len(), !requested_layouts.is_empty())
    );
    println!(
        "{changed} changed layout{} compared",
        if changed == 1 { "" } else { "s" }
    );
    if missing_layouts > 0 {
        println!(
            "{missing_layouts} layout{} not loaded in the editor",
            if missing_layouts == 1 { "" } else { "s" }
        );
    }
    println!(
        "{} source file{} generated",
        source_files.len(),
        if source_files.len() == 1 { "" } else { "s" }
    );
    if !planner.runtime_bindings.is_empty() {
        println!(
            "{} placeholder binding{} generated",
            planner.runtime_bindings.len(),
            if planner.runtime_bindings.len() == 1 {
                ""
            } else {
                "s"
            }
        );
    }
    if changed > 0 {
        println!(
            "{verified}/{changed} changed layout{} verified",
            if changed == 1 { "" } else { "s" }
        );
    }
    if review_items > 0 {
        println!("{review_items} item(s) need review");
    }
    println!();
    println!("Output: {}", output_directory.display());

    Ok(GenerateSummary {
        review_items,
        missing_layouts,
    })
}

fn write_report(
    output_directory: &Path,
    game: &LayoutDump,
    editor: &LayoutDump,
    planner: &SourcePlanner<'_>,
    source_files: &[String],
    filtered: bool,
    verbose: bool,
) -> Result<()> {
    let changed: Vec<&LayoutOutcome> = planner
        .outcomes
        .iter()
        .filter(|outcome| !matches!(outcome.status.as_str(), "already exact" | "not loaded"))
        .collect();
    let modeled = changed
        .iter()
        .filter(|outcome| outcome.status == "modeled exact")
        .count();

    let mut report = String::new();
    report.push_str("# oatmealf report\n\n");
    report.push_str(&format!(
        "**Result: {}**\n\n",
        run_result(planner).report_label()
    ));
    let layout_count = format_layout_count(planner.outcomes.len(), game.layouts.len(), filtered);
    report.push_str(&format!("- {layout_count}\n"));
    report.push_str(&format!(
        "- {} changed layout{} compared\n",
        changed.len(),
        if changed.len() == 1 { "" } else { "s" },
    ));
    report.push_str(&format!(
        "- {modeled}/{} changed layout{} verified\n",
        changed.len(),
        if changed.len() == 1 { "" } else { "s" },
    ));
    if !planner.missing_layouts.is_empty() {
        report.push_str(&format!(
            "- {} layout{} not loaded in the editor\n",
            planner.missing_layouts.len(),
            if planner.missing_layouts.len() == 1 {
                ""
            } else {
                "s"
            }
        ));
    }
    report.push_str(&format!(
        "- {} source file{} generated\n\n",
        source_files.len(),
        if source_files.len() == 1 { "" } else { "s" }
    ));

    if changed.is_empty() {
        if filtered && planner.outcomes.is_empty() {
            report.push_str("No selected layouts were compared.\n");
        } else if planner.missing_layouts.is_empty() {
            report.push_str("The editor layouts already match the game.\n");
        }
    } else {
        report.push_str("## Changes\n\n");
        let mut outcomes = changed;
        outcomes.sort_by_key(|outcome| outcome.name.to_ascii_lowercase());
        for outcome in outcomes {
            write_layout_report(&mut report, outcome, game, editor)?;
        }
    }

    if !planner.missing_layouts.is_empty() {
        report.push_str("## Not loaded in editor\n\n");
        for layout in &planner.missing_layouts {
            if let Some(plugin) = &layout.plugin {
                report.push_str(&format!("- `{}`: {plugin} plugin\n", layout.name));
            } else {
                report.push_str(&format!("- `{}`\n", layout.name));
            }
        }
        report.push_str(
            "\nOpen the matching project with the required modules or plugins loaded and run again.\n\n",
        );
    }

    if !source_files.is_empty() {
        report.push_str("## Generated files\n\n");
        for file in source_files {
            report.push_str(&format!("- `{file}`\n"));
        }
        report.push('\n');
        report.push_str("These are copies. The supplied engine source was not changed.\n\n");
    }

    if !planner.runtime_bindings.is_empty() {
        report.push_str("## Generated placeholder bindings\n\n");
        for binding in &planner.runtime_bindings {
            report.push_str(&format!("- {binding}\n"));
        }
        report.push('\n');
        report.push_str(
            "These members use engine placeholder resources because the custom game data is not available in the editor.\n\n",
        );
    }

    if !planner.unresolved.is_empty() {
        report.push_str("## Needs review\n\n");
        for issue in &planner.unresolved {
            report.push_str(&format!("- {issue}\n"));
        }
        report.push('\n');
    }

    if !planner.review.is_empty() {
        if planner.unresolved.is_empty() {
            report.push_str("## Needs review\n\n");
        }
        for issue in &planner.review {
            report.push_str(&format!("- {issue}\n"));
        }
        report.push('\n');
    }

    if verbose {
        report.push_str("## Verification\n\n");
        report.push_str(&format!(
            "- Game: {} layout hashes verified\n",
            game.layouts.len()
        ));
        report.push_str(&format!(
            "- Editor: {} layout hashes verified\n",
            editor.layouts.len()
        ));
        report.push_str(&format!("- Game export: `{GAME_EXPORT_PATH}`\n"));
        report.push_str(&format!("- Editor export: `{EDITOR_EXPORT_PATH}`\n"));
    }

    fs::write(output_directory.join("report.md"), report)?;
    Ok(())
}

fn write_layout_report(
    report: &mut String,
    outcome: &LayoutOutcome,
    game: &LayoutDump,
    editor: &LayoutDump,
) -> Result<()> {
    report.push_str(&format!("### {}\n\n", outcome.name));
    let editor_layout = editor.layouts.get(&outcome.name);
    write_layout_result(report, outcome, editor_layout.is_some());

    let Some(game_layout) = game.layouts.get(&outcome.name) else {
        return Ok(());
    };
    let Some(editor_layout) = editor_layout else {
        return Ok(());
    };
    let game_definition = &game.definitions[&game_layout.root_definition_id];
    let editor_definition = &editor.definitions[&editor_layout.root_definition_id];
    if !editor_definition.source_file.is_empty() {
        report.push_str(&format!(
            "Source: `{}`\n\n",
            editor_definition.source_file.replace('`', "``")
        ));
    }

    if outcome.detail.contains("placeholder constants") {
        report.push_str(&format!("- {}.\n\n", outcome.detail));
        return Ok(());
    }

    let mut lines = Vec::new();
    let mut seen = HashSet::new();
    describe_definition_changes(
        game,
        editor,
        game_definition,
        editor_definition,
        &mut seen,
        &mut lines,
    );
    if game_layout.constant_buffer_size != editor_layout.constant_buffer_size {
        lines.push(format!(
            "Constant buffer size changes from {} to {} bytes.",
            editor_layout.constant_buffer_size, game_layout.constant_buffer_size
        ));
    }
    let game_resources = &game_layout.arrays["Resources"];
    let editor_resources = &editor_layout.arrays["Resources"];
    if game_resources != editor_resources {
        lines.push(format!(
            "Resource table changes from {} to {} entries.",
            editor_resources.len(),
            game_resources.len()
        ));
    }

    if lines.is_empty() {
        report.push_str(&format!("{}\n\n", outcome.detail));
    } else {
        for line in lines {
            report.push_str(&format!("- {line}\n"));
        }
        report.push('\n');
    }
    Ok(())
}

fn write_layout_result(report: &mut String, outcome: &LayoutOutcome, editor_present: bool) {
    report.push_str("```text\n");
    report.push_str(&format!(
        "Game hash:             0x{:08X}\n",
        outcome.game_rhi_hash
    ));
    if editor_present {
        report.push_str(&format!(
            "Current editor hash:   0x{:08X}\n",
            outcome.editor_rhi_hash
        ));
    } else {
        report.push_str("Current editor hash:   not available\n");
    }

    let result = match outcome.status.as_str() {
        "modeled exact" => {
            report.push_str(&format!(
                "Generated source hash: 0x{:08X}\n",
                outcome.game_rhi_hash
            ));
            "generated source matches the game"
        }
        "already exact" => "editor already matches the game",
        _ if !editor_present => "layout was not loaded in the editor",
        _ => "generated source needs review",
    };
    report.push_str(&format!("Result:                {result}\n```\n\n"));
}

fn describe_definition_changes(
    game: &LayoutDump,
    editor: &LayoutDump,
    wanted: &DefinitionRecord,
    current: &DefinitionRecord,
    seen: &mut HashSet<(u32, u32)>,
    output: &mut Vec<String>,
) {
    if !seen.insert((wanted.id, current.id)) {
        return;
    }
    let current_by_name: HashMap<&str, &MemberRecord> = current
        .members
        .iter()
        .map(|member| (member.name.as_str(), member))
        .collect();
    let mut matched = HashSet::new();
    let mut moved = Vec::new();

    for member in &wanted.members {
        if let Some(existing) = current_by_name.get(member.name.as_str()) {
            matched.insert(existing.index);
            if !member.same_shape(existing) {
                output.push(format!(
                    "Change `{}` from {} to {}.",
                    member.name,
                    member_description(existing),
                    member_description(member)
                ));
            } else if member.offset != existing.offset {
                moved.push(format!(
                    "`{}` 0x{:X} -> 0x{:X}",
                    member.name, existing.offset, member.offset
                ));
            }
            if member.base_type.is_nested_definition()
                && member.nested_definition_id != 0
                && existing.nested_definition_id != 0
            {
                describe_definition_changes(
                    game,
                    editor,
                    &game.definitions[&member.nested_definition_id],
                    &editor.definitions[&existing.nested_definition_id],
                    seen,
                    output,
                );
            }
            continue;
        }

        let replacement = current.members.iter().find(|existing| {
            !matched.contains(&existing.index)
                && existing.offset == member.offset
                && existing.same_storage_shape(member)
                && is_generated_placeholder(&existing.name)
        });
        if let Some(existing) = replacement {
            matched.insert(existing.index);
            output.push(format!(
                "Replace placeholder `{}` with `{}` at 0x{:X}.",
                existing.name, member.name, member.offset
            ));
        } else {
            let macro_line = write_macro(member)
                .map(|value| format!(" `{value}`"))
                .unwrap_or_default();
            output.push(format!(
                "Add `{}` at 0x{:X}.{}",
                member.name, member.offset, macro_line
            ));
        }
    }

    for member in &current.members {
        if !matched.contains(&member.index) && !wanted.members.iter().any(|v| v.name == member.name)
        {
            output.push(format!(
                "Remove `{}` at 0x{:X}.",
                member.name, member.offset
            ));
        }
    }

    if !moved.is_empty() {
        let examples = moved.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
        let suffix = if moved.len() > 3 {
            format!(" and {} more", moved.len() - 3)
        } else {
            String::new()
        };
        output.push(format!(
            "{} existing member offsets move: {examples}{suffix}.",
            moved.len()
        ));
    }
}

fn is_generated_placeholder(name: &str) -> bool {
    // keep placeholders from older ShaderDriftCheck builds
    name.starts_with("oatmealf") || name.starts_with("ShaderDrift")
}

fn member_description(member: &MemberRecord) -> String {
    format!(
        "{} r{}c{} n{} at 0x{:X}",
        member.base_type.dump_name(),
        member.rows,
        member.columns,
        member.elements,
        member.offset
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Manifest<'a> {
    format: u32,
    generated_at: String,
    game_dump: String,
    editor_dump: String,
    game_layouts_verified: usize,
    editor_layouts_verified: usize,
    result: &'static str,
    ready: bool,
    source_files: &'a [String],
    layouts: Vec<ManifestLayout<'a>>,
    missing_layouts: Vec<ManifestMissingLayout<'a>>,
    unresolved: &'a [String],
    review: &'a [String],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ManifestLayout<'a> {
    name: &'a str,
    status: &'a str,
    game_rhi_hash: String,
    editor_rhi_hash: String,
    game_metadata_hash: String,
    editor_metadata_hash: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ManifestMissingLayout<'a> {
    name: &'a str,
    type_name: &'a str,
    plugin: Option<&'a str>,
}

fn write_manifest(
    output_directory: &Path,
    game_layouts_verified: usize,
    editor_layouts_verified: usize,
    planner: &SourcePlanner<'_>,
    source_files: &[String],
) -> Result<()> {
    let layouts = planner
        .outcomes
        .iter()
        .map(|outcome| ManifestLayout {
            name: &outcome.name,
            status: &outcome.status,
            game_rhi_hash: format!("{:08X}", outcome.game_rhi_hash),
            editor_rhi_hash: format!("{:08X}", outcome.editor_rhi_hash),
            game_metadata_hash: format!("{:08X}", outcome.game_metadata_hash),
            editor_metadata_hash: format!("{:08X}", outcome.editor_metadata_hash),
        })
        .collect();
    let manifest = Manifest {
        format: 1,
        generated_at: Local::now().to_rfc3339_opts(SecondsFormat::AutoSi, false),
        game_dump: GAME_EXPORT_PATH.to_string(),
        editor_dump: EDITOR_EXPORT_PATH.to_string(),
        game_layouts_verified,
        editor_layouts_verified,
        result: run_result(planner).manifest_value(),
        ready: run_result(planner) == RunResult::Ready,
        source_files,
        layouts,
        missing_layouts: planner
            .missing_layouts
            .iter()
            .map(|layout| ManifestMissingLayout {
                name: &layout.name,
                type_name: &layout.type_name,
                plugin: layout.plugin.as_deref(),
            })
            .collect(),
        unresolved: &planner.unresolved,
        review: &planner.review,
    };
    fs::write(
        output_directory.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)? + "\n",
    )?;
    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    Ok(std::path::absolute(path)?)
}

fn print_packing_summary(label: &str, dump: &LayoutDump) {
    let (exact, failures) = simulate::verify_dump(dump);
    println!(
        "Packing model reproduced {exact}/{} {label} definitions",
        exact + failures.len()
    );
    for failure in failures.iter().take(3) {
        println!("  {failure}");
    }
}

fn format_layout_count(compared: usize, total: usize, filtered: bool) -> String {
    if filtered {
        format!("{compared} of {total} game layouts selected")
    } else {
        format!("{compared} game layouts checked")
    }
}

fn run_result(planner: &SourcePlanner<'_>) -> RunResult {
    if planner.has_unresolved() || !planner.review.is_empty() {
        RunResult::ReviewNeeded
    } else if !planner.missing_layouts.is_empty() {
        RunResult::MissingLayouts
    } else {
        RunResult::Ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
    }

    fn unique_output(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("oatmealf-{label}-{unique}"))
    }

    #[test]
    fn placeholder_names_are_binding_compatible() {
        let root = fixture_root();
        let output = unique_output("output");
        let result = generate_with_minimum(
            &root.join("game-layouts.json"),
            &root.join("editor-layouts.json"),
            &root,
            &output,
            &["Test".to_string()],
            1,
            false,
        )
        .unwrap();
        assert!(!result.needs_review());
        for relative in [
            "report.md",
            "manifest.json",
            "exports/game-layouts.json",
            "exports/editor-layouts.json",
        ] {
            assert!(output.join(relative).is_file(), "missing {relative}");
        }
        assert!(!output.join("evidence").exists());
        let report = fs::read_to_string(output.join("report.md")).unwrap();
        assert!(report.contains("**Result: READY**"));
        assert!(report.contains("1 of 1 game layouts selected"));
        assert!(report.contains("0 changed layouts compared"));
        let manifest: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(output.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["ready"], true);
        assert_eq!(manifest["result"], "ready");
        assert_eq!(manifest["missingLayouts"], serde_json::json!([]));
        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn filtered_layout_count_distinguishes_selection_from_capture() {
        assert_eq!(
            format_layout_count(2, 153, true),
            "2 of 153 game layouts selected"
        );
        assert_eq!(
            format_layout_count(153, 153, false),
            "153 game layouts checked"
        );
    }

    #[test]
    fn modeled_result_distinguishes_current_and_generated_hashes() {
        let outcome = LayoutOutcome {
            name: "InstancedView".to_string(),
            status: "modeled exact".to_string(),
            detail: String::new(),
            game_rhi_hash: 0x538A_3501,
            editor_rhi_hash: 0x6DCA_3501,
            game_metadata_hash: 0,
            editor_metadata_hash: 0,
        };
        let mut report = String::new();
        write_layout_result(&mut report, &outcome, true);
        assert!(report.contains("Game hash:             0x538A3501"));
        assert!(report.contains("Current editor hash:   0x6DCA3501"));
        assert!(report.contains("Generated source hash: 0x538A3501"));
        assert!(report.contains("Result:                generated source matches the game"));
        assert!(!report.contains("modeled exact"));
    }

    #[test]
    fn missing_layout_does_not_print_a_zero_editor_hash() {
        let outcome = LayoutOutcome {
            name: "Missing".to_string(),
            status: "unresolved".to_string(),
            detail: String::new(),
            game_rhi_hash: 0x1234_5678,
            editor_rhi_hash: 0,
            game_metadata_hash: 0,
            editor_metadata_hash: 0,
        };
        let mut report = String::new();
        write_layout_result(&mut report, &outcome, false);
        assert!(report.contains("Current editor hash:   not available"));
        assert!(report.contains("Result:                layout was not loaded in the editor"));
        assert!(!report.contains("0x00000000"));
    }

    #[test]
    fn missing_layout_does_not_report_a_successful_comparison() {
        let root = fixture_root();
        let output = unique_output("missing-layout");
        let result = generate_with_minimum(
            &root.join("game-layouts.json"),
            &root.join("editor-layouts.json"),
            &root,
            &output,
            &["MissingLayout".to_string()],
            1,
            false,
        )
        .unwrap();
        assert!(result.needs_review());
        let report = fs::read_to_string(output.join("report.md")).unwrap();
        assert!(report.contains("0 of 1 game layouts selected"));
        assert!(report.contains("No selected layouts were compared."));
        assert!(!report.contains("already match the game"));
        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn existing_output_is_rejected_before_inputs_are_read() {
        let output = unique_output("existing");
        fs::create_dir_all(&output).unwrap();
        let error = generate_with_minimum(
            Path::new("missing-game"),
            Path::new("missing-editor"),
            Path::new("missing-engine"),
            &output,
            &[],
            1,
            false,
        )
        .unwrap_err();
        assert!(error.to_string().contains("already exists"));
        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn manifest_includes_review_reasons() {
        let root = fixture_root();
        let game = dump::read(&root.join("game-layouts.json"), 1).unwrap();
        let editor = dump::read(&root.join("editor-layouts.json"), 1).unwrap();
        let mut planner = SourcePlanner::new(&game, &editor, &root).unwrap();
        planner.plan(&[]);
        planner
            .review
            .push("FTestParameters::Texture needs a runtime value".to_string());

        let output = unique_output("manifest-review");
        fs::create_dir_all(&output).unwrap();
        write_manifest(&output, 1, 1, &planner, &[]).unwrap();
        let manifest: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(output.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["ready"], false);
        assert_eq!(manifest["result"], "review_needed");
        assert_eq!(manifest["gameDump"], GAME_EXPORT_PATH);
        assert_eq!(manifest["editorDump"], EDITOR_EXPORT_PATH);
        assert_eq!(
            manifest["review"][0],
            "FTestParameters::Texture needs a runtime value"
        );
        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn missing_layouts_have_their_own_result() {
        let root = fixture_root();
        let game = dump::read(&root.join("game-layouts.json"), 1).unwrap();
        let editor = dump::read(&root.join("editor-layouts.json"), 1).unwrap();
        let mut planner = SourcePlanner::new(&game, &editor, &root).unwrap();
        planner.outcomes.push(LayoutOutcome {
            name: "PluginOnlyVF".to_string(),
            status: "not loaded".to_string(),
            detail: "layout was not loaded in the editor (Example plugin)".to_string(),
            game_rhi_hash: 0x66D7_C8C6,
            editor_rhi_hash: 0,
            game_metadata_hash: 0,
            editor_metadata_hash: 0,
        });
        planner.missing_layouts.push(crate::planner::MissingLayout {
            name: "PluginOnlyVF".to_string(),
            type_name: "FWaterVertexFactoryParameters".to_string(),
            plugin: Some("Example".to_string()),
        });

        let output = unique_output("missing-result");
        fs::create_dir_all(&output).unwrap();
        write_report(&output, &game, &editor, &planner, &[], false, false).unwrap();
        write_manifest(&output, 1, 1, &planner, &[]).unwrap();

        let report = fs::read_to_string(output.join("report.md")).unwrap();
        assert!(report.contains("**Result: MISSING LAYOUTS**"));
        assert!(report.contains("- 1 layout not loaded in the editor"));
        assert!(report.contains("- `PluginOnlyVF`: Example plugin"));
        assert!(!report.contains("## Needs review"));

        let manifest: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(output.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["result"], "missing_layouts");
        assert_eq!(manifest["ready"], false);
        assert_eq!(manifest["missingLayouts"][0]["name"], "PluginOnlyVF");
        assert_eq!(manifest["missingLayouts"][0]["plugin"], "Example");
        assert_eq!(manifest["unresolved"], serde_json::json!([]));
        assert_eq!(run_result(&planner), RunResult::MissingLayouts);
        planner
            .review
            .push("FExample::Texture needs a renderer assignment".to_string());
        assert_eq!(run_result(&planner), RunResult::ReviewNeeded);
        fs::remove_dir_all(output).unwrap();
    }
}
