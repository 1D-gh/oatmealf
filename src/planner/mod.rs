use crate::cancel;
use crate::hash;
use crate::model::{
    DefinitionRecord, LayoutDump, LayoutRecord, MAX_DEFINITION_DEPTH, MemberRecord, ResourceEntry,
    ShaderParameterBaseType,
};
use crate::progress;
use crate::simulate;
use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use regex::{Captures, Regex};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use walkdir::WalkDir;

static LINE_SPLIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\r\n|\n").expect("valid line regex"));
static IDENTIFIER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").expect("valid identifier regex"));

#[derive(Clone, Debug)]
pub struct LayoutOutcome {
    pub name: String,
    pub status: String,
    pub detail: String,
    pub game_rhi_hash: u32,
    pub editor_rhi_hash: u32,
    pub game_metadata_hash: u32,
    pub editor_metadata_hash: u32,
}

#[derive(Clone, Debug)]
pub struct MissingLayout {
    pub name: String,
    pub type_name: String,
    pub plugin: Option<String>,
}

#[derive(Debug)]
struct FilePlan {
    source_path: PathBuf,
    relative_path: PathBuf,
    original_text: String,
    newline: &'static str,
    utf8_bom: bool,
    replacements: BTreeMap<usize, Option<String>>,
    insert_before: BTreeMap<usize, Vec<String>>,
    renames: BTreeMap<usize, BTreeMap<String, String>>,
}

impl FilePlan {
    fn original_lines(&self) -> Vec<&str> {
        LINE_SPLIT.split(&self.original_text).collect()
    }

    fn replace_line(&mut self, line_number: usize, value: Option<String>) -> Result<()> {
        if let Some(existing) = self.replacements.get(&line_number) {
            if existing != &value {
                bail!(
                    "conflicting replacements for {}:{line_number}",
                    self.relative_path.display()
                );
            }
        }
        self.replacements.insert(line_number, value);
        Ok(())
    }

    fn delete_line(&mut self, line_number: usize) -> Result<()> {
        self.replace_line(line_number, None)
    }

    fn add_before<I>(&mut self, line_number: usize, values: I)
    where
        I: IntoIterator<Item = String>,
    {
        self.insert_before
            .entry(line_number)
            .or_default()
            .extend(values);
    }

    fn has_edits(&self) -> bool {
        !self.replacements.is_empty() || !self.insert_before.is_empty() || !self.renames.is_empty()
    }

    fn render(&self) -> Result<String> {
        let lines: Vec<&str> = LINE_SPLIT.split(&self.original_text).collect();
        let mut output = Vec::with_capacity(
            lines.len() + self.insert_before.values().map(Vec::len).sum::<usize>(),
        );
        for (index, line) in lines.into_iter().enumerate() {
            let line_number = index + 1;
            if let Some(insertions) = self.insert_before.get(&line_number) {
                output.extend(insertions.iter().cloned());
            }
            match self.replacements.get(&line_number) {
                Some(Some(replacement)) => output.push(replacement.clone()),
                Some(None) => {}
                None => {
                    if let Some(renames) = self.renames.get(&line_number) {
                        let alternatives = renames
                            .keys()
                            .map(|value| regex::escape(value))
                            .collect::<Vec<_>>()
                            .join("|");
                        let pattern = Regex::new(&format!(r"\b(?:{alternatives})\b"))?;
                        output.push(
                            pattern
                                .replace_all(line, |captures: &Captures<'_>| {
                                    renames[&captures[0]].as_str()
                                })
                                .into_owned(),
                        );
                    } else {
                        output.push(line.to_string());
                    }
                }
            }
        }
        Ok(output.join(self.newline))
    }
}

#[derive(Debug)]
struct TargetLeaf<'a> {
    member: &'a MemberRecord,
    offset: u32,
    access_path: String,
}

mod bindings;
#[cfg(test)]
mod tests;

use bindings::*;

pub struct SourcePlanner<'a> {
    game: &'a LayoutDump,
    editor: &'a LayoutDump,
    engine_top: PathBuf,
    files: HashMap<PathBuf, FilePlan>,
    global_renames: BTreeMap<String, String>,
    removed_declarations: HashMap<String, HashSet<String>>,
    generated_target_types: HashMap<String, PathBuf>,
    source_definition_index: Option<HashMap<String, Vec<PathBuf>>>,
    source_file_cache: Option<Vec<PathBuf>>,
    definition_layout_owners: HashMap<u32, BTreeSet<String>>,
    selected_layouts: HashSet<String>,
    planned_definition_targets: HashMap<u32, u32>,
    planned_pairs: HashSet<(u32, u32)>,
    planned_definition_mutations: HashSet<u32>,
    planned_blocks: HashMap<(PathBuf, usize, usize), String>,
    planned_shared_insertions: HashSet<(PathBuf, usize, String)>,
    planned_layout_constant_growth: HashMap<String, u32>,
    layout_notes: HashMap<String, String>,
    pending_runtime_members: Vec<PendingRuntimeMember>,
    emit_blocked: bool,
    pub outcomes: Vec<LayoutOutcome>,
    pub missing_layouts: Vec<MissingLayout>,
    pub unresolved: Vec<String>,
    pub review: Vec<String>,
    pub runtime_bindings: Vec<String>,
}

impl<'a> SourcePlanner<'a> {
    pub fn new(game: &'a LayoutDump, editor: &'a LayoutDump, engine_root: &Path) -> Result<Self> {
        let engine_top = normalize_engine_root(engine_root)?;
        let definition_layout_owners = collect_definition_layout_owners(editor)?;
        Ok(Self {
            game,
            editor,
            engine_top,
            files: HashMap::new(),
            global_renames: BTreeMap::new(),
            removed_declarations: HashMap::new(),
            generated_target_types: HashMap::new(),
            source_definition_index: None,
            source_file_cache: None,
            definition_layout_owners,
            selected_layouts: HashSet::new(),
            planned_definition_targets: HashMap::new(),
            planned_pairs: HashSet::new(),
            planned_definition_mutations: HashSet::new(),
            planned_blocks: HashMap::new(),
            planned_shared_insertions: HashSet::new(),
            planned_layout_constant_growth: HashMap::new(),
            layout_notes: HashMap::new(),
            pending_runtime_members: Vec::new(),
            emit_blocked: false,
            outcomes: Vec::new(),
            missing_layouts: Vec::new(),
            unresolved: Vec::new(),
            review: Vec::new(),
            runtime_bindings: Vec::new(),
        })
    }

    pub fn has_unresolved(&self) -> bool {
        !self.unresolved.is_empty()
    }

    pub fn can_emit(&self) -> bool {
        !self.emit_blocked
    }

    fn reserve_definition_mutation(
        &mut self,
        definition_id: u32,
        target_definition_id: Option<u32>,
        key: &Path,
        block_begin: usize,
        block_end: usize,
        type_name: &str,
    ) -> Result<()> {
        // nested definitions affect every owning layout
        let owners = self
            .definition_layout_owners
            .get(&definition_id)
            .cloned()
            .unwrap_or_default();
        if owners.is_empty() {
            bail!("{type_name} is not owned by a registered editor layout");
        }
        if owners.len() > 1 {
            let Some(target_definition_id) = target_definition_id else {
                let names = owners.into_iter().collect::<Vec<_>>().join(", ");
                bail!(
                    "{type_name} is shared by multiple registered layouts ({names}); placeholder padding cannot account for the owning layouts together"
                );
            };
            self.verify_shared_definition_targets(definition_id, target_definition_id, &owners)?;
        }
        if !self.planned_definition_mutations.insert(definition_id) {
            bail!("multiple automatic source edits target {type_name}");
        }
        let block = (key.to_path_buf(), block_begin, block_end);
        if let Some(existing) = self.planned_blocks.get(&block) {
            self.planned_definition_mutations.remove(&definition_id);
            bail!("{type_name} shares a source block already being edited for {existing}");
        }
        self.planned_blocks.insert(block, type_name.to_string());
        Ok(())
    }

    pub fn plan(&mut self, requested_layouts: &[String]) {
        if requested_layouts
            .iter()
            .any(|value| value.trim().is_empty())
        {
            self.emit_blocked = true;
            self.unresolved
                .push("layout selection contains an empty name".to_string());
            return;
        }
        let requested = if requested_layouts.is_empty() {
            None
        } else {
            let values: HashSet<String> = requested_layouts
                .iter()
                .map(|value| value.trim().to_string())
                .collect();
            for name in &values {
                if !self.game.layouts.contains_key(name) {
                    self.unresolved
                        .push(format!("{name}: layout is not present in the game capture"));
                }
            }
            Some(values)
        };

        self.selected_layouts = self
            .game
            .layouts
            .keys()
            .filter(|name| {
                requested
                    .as_ref()
                    .is_none_or(|values| values.contains(name.as_str()))
            })
            .cloned()
            .collect();

        let mut layout_names: Vec<String> = self.game.layouts.keys().cloned().collect();
        layout_names.sort_by_key(|name| {
            let game_layout = &self.game.layouts[name];
            // member edits run first so padding accounts for shared insertions
            let is_simple_padding = self.editor.layouts.get(name).is_some_and(|editor_layout| {
                same_resource_arrays(game_layout, editor_layout)
                    && game_layout.constant_buffer_size > editor_layout.constant_buffer_size
                    && self.padding_is_layout_compatible(game_layout, editor_layout)
            });
            (is_simple_padding, name.to_ascii_lowercase())
        });

        for name in layout_names {
            if cancel::is_cancelled() {
                break;
            }
            let game_layout = self.game.layouts[&name].clone();
            if requested
                .as_ref()
                .is_some_and(|values| !values.contains(&game_layout.name))
            {
                continue;
            }
            let Some(editor_layout) = self.editor.layouts.get(&game_layout.name).cloned() else {
                let type_name = self
                    .game
                    .definitions
                    .get(&game_layout.root_definition_id)
                    .map(|definition| definition.type_name.clone())
                    .unwrap_or_else(|| game_layout.type_name.clone());
                let plugin = self
                    .plugin_for_shader_definition(&type_name)
                    .unwrap_or_default();
                let detail = plugin.as_ref().map_or_else(
                    || "layout was not loaded in the editor".to_string(),
                    |name| format!("layout was not loaded in the editor ({name} plugin)"),
                );
                self.add_outcome(&game_layout, None, "not loaded", &detail);
                self.missing_layouts.push(MissingLayout {
                    name: game_layout.name.clone(),
                    type_name,
                    plugin,
                });
                continue;
            };

            if self.layout_exact(&game_layout, &editor_layout) {
                self.add_outcome(
                    &game_layout,
                    Some(&editor_layout),
                    "already exact",
                    "cooked binding layout already matches",
                );
                continue;
            }

            match self.plan_layout(&game_layout, &editor_layout) {
                Ok(()) => {
                    let detail = self
                        .layout_notes
                        .get(&game_layout.name)
                        .cloned()
                        .unwrap_or_else(|| {
                            "source edits match the cooked game binding layout".to_string()
                        });
                    self.add_outcome(&game_layout, Some(&editor_layout), "modeled exact", &detail)
                }
                Err(error) => {
                    self.emit_blocked = true;
                    let detail = format!("{error:#}");
                    self.add_outcome(&game_layout, Some(&editor_layout), "unresolved", &detail);
                    self.unresolved
                        .push(format!("{}: {detail}", game_layout.name));
                }
            }
        }
        self.outcomes
            .sort_by_key(|outcome| outcome.name.to_ascii_lowercase());
        self.missing_layouts
            .sort_by_key(|layout| layout.name.to_ascii_lowercase());
    }

    fn verify_shared_definition_targets(
        &self,
        current_definition_id: u32,
        target_definition_id: u32,
        owners: &BTreeSet<String>,
    ) -> Result<()> {
        let current = &self.editor.definitions[&current_definition_id];
        let target_bytes = hash::serialize_definition(self.game, target_definition_id)?;

        for owner in owners {
            if !self.selected_layouts.contains(owner) {
                bail!(
                    "{} is shared with {owner}, which is outside the selected layouts",
                    current.type_name
                );
            }
            let target_layout = self.game.layouts.get(owner).with_context(|| {
                format!("shared owner {owner} is missing from the game capture")
            })?;
            let matching = reachable_definitions_by_type(
                self.game,
                target_layout.root_definition_id,
                &current.type_name,
            )?;
            if matching.is_empty() {
                bail!(
                    "{} is owned by {owner} in the editor but has no matching game definition",
                    current.type_name
                );
            }

            let mut bodies = BTreeSet::new();
            for definition_id in matching {
                bodies.insert(hash::serialize_definition(self.game, definition_id)?);
            }
            if bodies.len() != 1 || !bodies.contains(&target_bytes) {
                bail!(
                    "{} has conflicting captured definitions across its owning layouts",
                    current.type_name
                );
            }
        }
        Ok(())
    }

    pub fn add_rename_references(&mut self) -> Result<()> {
        if self.global_renames.is_empty() && self.removed_declarations.is_empty() {
            self.validate_edit_boundaries();
            return Ok(());
        }
        let mut identifiers: Vec<String> = self
            .global_renames
            .keys()
            .chain(self.removed_declarations.keys())
            .cloned()
            .collect();
        identifiers.sort();
        identifiers.dedup();
        identifiers.sort_by_key(|value| std::cmp::Reverse(value.len()));
        let pattern = Regex::new(&format!(
            r"\b(?:{})\b",
            identifiers
                .iter()
                .map(|value| regex::escape(value))
                .collect::<Vec<_>>()
                .join("|")
        ))?;

        let mut dangling = HashSet::new();
        let mut unsafe_renames = HashSet::new();
        let mut paths = Vec::new();
        let source_files = self.source_files()?;
        let total = source_files.len();
        progress::step("Checking generated identifiers across the source tree...");
        for (index, path) in source_files.iter().enumerate() {
            if (index + 1) % 2048 == 0 {
                progress::transient(&format!("  checked {} of {total} files", index + 1));
            }
            let text = fs::read_to_string(path)
                .with_context(|| format!("could not scan {}", path.display()))?;
            if pattern.is_match(&text) {
                paths.push(path.clone());
            }
        }
        progress::finish_transient();
        for path in paths {
            let key = self.ensure_file(&path)?;
            let original_text = self.files[&key].original_text.clone();
            let original_lines: Vec<&str> = LINE_SPLIT.split(&original_text).collect();
            let matches: Vec<(String, usize)> = pattern
                .find_iter(&original_text)
                .map(|value| (value.as_str().to_string(), value.start()))
                .collect();
            if matches.is_empty() {
                if !self.files[&key].has_edits() {
                    self.files.remove(&key);
                }
                continue;
            }

            for (identifier, position) in matches {
                let line = line_number_at(&original_text, position);
                let location = declaration_location(&path, line);
                if let Some(replacement) = self.global_renames.get(&identifier) {
                    let source_line = original_lines.get(line - 1).copied().unwrap_or_default();
                    let is_reserved_declaration = self
                        .removed_declarations
                        .get(&identifier)
                        .is_some_and(|locations| locations.contains(&location));
                    if is_parameter_declaration_line(source_line, &identifier)
                        && !is_reserved_declaration
                    {
                        let relative = path_relative_slash(&self.engine_top, &path)?;
                        unsafe_renames.insert(format!("{identifier} in {relative}:{line}"));
                        continue;
                    }
                    self.files
                        .get_mut(&key)
                        .expect("file plan exists")
                        .renames
                        .entry(line)
                        .or_default()
                        .insert(identifier, replacement.clone());
                    continue;
                }
                if !self.removed_declarations[&identifier].contains(&location) {
                    let relative = path_relative_slash(&self.engine_top, &path)?;
                    dangling.insert(format!("{identifier} in {relative}:{line}"));
                }
            }
            if !self.files[&key].has_edits() {
                self.files.remove(&key);
            }
        }

        let mut dangling: Vec<String> = dangling.into_iter().collect();
        dangling.sort_by_key(|value| value.to_ascii_lowercase());
        for reference in dangling {
            self.emit_blocked = true;
            self.unresolved.push(format!(
                "removed placeholder is still referenced: {reference}"
            ));
        }
        let mut unsafe_renames: Vec<String> = unsafe_renames.into_iter().collect();
        unsafe_renames.sort_by_key(|value| value.to_ascii_lowercase());
        for declaration in unsafe_renames {
            self.emit_blocked = true;
            self.unresolved.push(format!(
                "rename source is also declared outside its reserved edit: {declaration}"
            ));
        }
        self.validate_edit_boundaries();
        Ok(())
    }

    fn validate_edit_boundaries(&mut self) {
        // block structural edits outside the reserved definition
        let mut issues = Vec::new();
        for (path, plan) in &self.files {
            let reserved = |line: usize| {
                self.planned_blocks.keys().any(|(block_path, begin, end)| {
                    block_path == path && line >= *begin && line <= *end
                })
            };
            for line in plan.replacements.keys().chain(plan.insert_before.keys()) {
                if !reserved(*line) {
                    issues.push(format!(
                        "unreserved structural source edit in {}:{line}",
                        plan.relative_path.display()
                    ));
                }
            }
        }
        issues.sort_by_key(|value| value.to_ascii_lowercase());
        issues.dedup();
        if !issues.is_empty() {
            self.emit_blocked = true;
            self.unresolved.extend(issues);
        }
    }

    pub fn emit(&self, output_directory: &Path) -> Result<Vec<String>> {
        let mut plans: Vec<&FilePlan> = self.files.values().collect();
        plans.sort_by_key(|plan| plan.relative_path.to_string_lossy().to_ascii_lowercase());
        let mut written = Vec::new();
        for plan in plans {
            let rendered = plan.render()?;
            if rendered == plan.original_text {
                continue;
            }
            let output_path = output_directory.join(&plan.relative_path);
            if let Some(parent) = output_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut bytes = Vec::with_capacity(rendered.len() + usize::from(plan.utf8_bom) * 3);
            if plan.utf8_bom {
                bytes.extend_from_slice(&[0xef, 0xbb, 0xbf]);
            }
            bytes.extend_from_slice(rendered.as_bytes());
            fs::write(&output_path, bytes)?;
            written.push(slash_path(&plan.relative_path));
        }
        Ok(written)
    }

    fn plan_layout(&mut self, target: &LayoutRecord, current: &LayoutRecord) -> Result<()> {
        if target.type_name != current.type_name || target.layout_name != current.layout_name {
            bail!("layout or C++ type name changed");
        }
        if (target.static_slot != 255) != (current.static_slot != 255) {
            bail!("static-slot presence changed");
        }
        if target.binding_flags != current.binding_flags || target.flags != current.flags {
            bail!("binding or RHI flags changed; member declarations cannot infer that edit");
        }
        if same_resource_arrays(target, current)
            && target.constant_buffer_size > current.constant_buffer_size
            && self.padding_is_layout_compatible(target, current)
        {
            return self.plan_constant_padding(target, current);
        }
        self.plan_definition(target.root_definition_id, current.root_definition_id)
    }

    // padding is safe only when shared members keep the same offset and shape
    fn padding_is_layout_compatible(&self, target: &LayoutRecord, current: &LayoutRecord) -> bool {
        if target.render_targets_offset != current.render_targets_offset {
            return false;
        }
        let (Ok(game_leaves), Ok(editor_leaves)) = (
            collect_offset_leaves(&self.game.definitions, target.root_definition_id),
            collect_offset_leaves(&self.editor.definitions, current.root_definition_id),
        ) else {
            return false;
        };
        let mut game_by_path: HashMap<&str, &OffsetLeaf<'_>> =
            HashMap::with_capacity(game_leaves.len());
        for leaf in &game_leaves {
            if let Some(previous) = game_by_path.insert(leaf.path.as_str(), leaf) {
                if previous.offset != leaf.offset || !previous.member.same_shape(leaf.member) {
                    return false;
                }
            }
        }
        let mut editor_paths = HashSet::with_capacity(editor_leaves.len());
        for leaf in &editor_leaves {
            editor_paths.insert(leaf.path.as_str());
            match game_by_path.get(leaf.path.as_str()) {
                Some(game_leaf) => {
                    if game_leaf.offset != leaf.offset || !game_leaf.member.same_shape(leaf.member)
                    {
                        return false;
                    }
                }
                None => {
                    if !is_scalar_constant(leaf.member.base_type) {
                        return false;
                    }
                }
            }
        }
        game_leaves
            .iter()
            .filter(|leaf| !editor_paths.contains(leaf.path.as_str()))
            .all(|leaf| is_scalar_constant(leaf.member.base_type))
    }

    fn plan_constant_padding(
        &mut self,
        target: &LayoutRecord,
        current: &LayoutRecord,
    ) -> Result<()> {
        let current_definition = self.editor.definitions[&current.root_definition_id].clone();
        if current_definition.source_file.is_empty() {
            bail!(
                "editor metadata has no source path for {}",
                current_definition.type_name
            );
        }
        let source_path = self.resolve_source_path(&current_definition.source_file)?;
        let key = self.ensure_file(&source_path)?;
        let (block_begin, block_end) =
            find_definition_block(&self.files[&key], &current_definition)?;
        simulate::verify_definition(&self.editor.definitions, current.root_definition_id)
            .with_context(|| {
                format!(
                    "packing model cannot reproduce the captured editor layout of {}",
                    current_definition.type_name
                )
            })?;
        let prior_growth = self
            .planned_layout_constant_growth
            .get(&target.name)
            .copied()
            .unwrap_or(0);
        let effective_definition_size = current_definition
            .size
            .checked_add(prior_growth)
            .context("planned constant growth overflowed the struct size")?;
        let effective_buffer_size = current
            .constant_buffer_size
            .checked_add(prior_growth)
            .context("planned constant growth overflowed the constant buffer size")?;
        if effective_definition_size != effective_buffer_size {
            bail!(
                "the {} struct size {} does not equal its constant buffer size {}; appended padding cannot grow the constant buffer exactly",
                current_definition.type_name,
                effective_definition_size,
                effective_buffer_size
            );
        }
        let delta = target
            .constant_buffer_size
            .checked_sub(effective_buffer_size)
            .context("earlier coordinated edits already exceed the game constant buffer size")?;
        if delta % 16 != 0 {
            bail!("constant-buffer growth is not representable as 16-byte placeholder padding");
        }

        let mut candidate = current.clone();
        candidate.constant_buffer_size = target.constant_buffer_size;
        if hash::compute_rhi_hash(&candidate) != target.rhi_hash {
            bail!("placeholder padding does not reproduce the game RHI hash");
        }
        if delta == 0 {
            self.layout_notes.insert(
                target.name.clone(),
                "coordinated shared source edits reproduce the game layout".to_string(),
            );
            return Ok(());
        }
        let identifier = target
            .name
            .chars()
            .map(|value| {
                if value.is_ascii_alphanumeric() || value == '_' {
                    value
                } else {
                    '_'
                }
            })
            .collect::<String>();
        let placeholder_name = format!("oatmealfPadding_{identifier}");
        if self.files[&key].original_lines()[block_begin..block_end - 1]
            .iter()
            .any(|line| is_parameter_declaration_line(line, &placeholder_name))
        {
            bail!(
                "{placeholder_name} is already present in source while the editor layout still differs; rebuild and restart the editor before running the tool again"
            );
        }
        self.reserve_definition_mutation(
            current.root_definition_id,
            None,
            &key,
            block_begin,
            block_end,
            &current_definition.type_name,
        )?;

        let mut lines = vec![format!(
            "\t// {} bytes of placeholder constants recovered from game layout",
            delta
        )];
        let vectors = delta / 16;
        lines.push(format!(
            "\tSHADER_PARAMETER_ARRAY(FVector4f, {placeholder_name}, [{vectors}])"
        ));
        self.files
            .get_mut(&key)
            .expect("file plan exists")
            .add_before(block_end, lines);
        self.layout_notes.insert(
            target.name.clone(),
            format!("added {delta} bytes of placeholder constants"),
        );
        Ok(())
    }

    fn plan_definition(&mut self, target_id: u32, current_id: u32) -> Result<()> {
        let pair = (target_id, current_id);
        if !self.planned_pairs.insert(pair) {
            return Ok(());
        }
        let result = self.plan_definition_inner(target_id, current_id);
        if result.is_err() {
            // let another layout retry this pair
            self.planned_pairs.remove(&pair);
        }
        result
    }

    fn plan_definition_inner(&mut self, target_id: u32, current_id: u32) -> Result<()> {
        if let Some(existing_target) = self.planned_definition_targets.get(&current_id) {
            if *existing_target != target_id {
                bail!("one editor definition maps to conflicting game definitions");
            }
        }
        self.planned_definition_targets
            .insert(current_id, target_id);

        let target = self.game.definitions[&target_id].clone();
        let current = self.editor.definitions[&current_id].clone();
        if target.type_name != current.type_name
            || target.use_case != current.use_case
            || target.binding_flags != current.binding_flags
            || target.usage_flags != current.usage_flags
        {
            bail!("definition contract changed for {}", current.type_name);
        }
        simulate::verify_definition(&self.game.definitions, target_id).with_context(|| {
            format!(
                "packing model cannot reproduce the captured game layout of {}",
                target.type_name
            )
        })?;
        simulate::verify_definition(&self.editor.definitions, current_id).with_context(|| {
            format!(
                "packing model cannot reproduce the captured editor layout of {}",
                current.type_name
            )
        })?;
        if current.source_file.is_empty() {
            bail!(
                "editor metadata has no source path for {}",
                current.type_name
            );
        }

        let source_path = self.resolve_source_path(&current.source_file)?;
        let key = self.ensure_file(&source_path)?;
        let (block_begin, block_end) = {
            let plan = &self.files[&key];
            find_definition_block(plan, &current)?
        };
        let shared_parameter_table =
            shared_parameter_table_in_block(&self.files[&key], block_begin, block_end);
        let mapping = align_members(&target, &current)?;
        let mut previous_current_index = None;
        for current_index in mapping.values() {
            if previous_current_index.is_some_and(|previous| *current_index <= previous) {
                bail!(
                    "member order changed in {}; moving existing declarations is not supported",
                    current.type_name
                );
            }
            previous_current_index = Some(*current_index);
        }

        let used_current: HashSet<usize> = mapping.values().copied().collect();
        let extras: Vec<MemberRecord> = current
            .members
            .iter()
            .enumerate()
            .filter(|(index, _)| !used_current.contains(index))
            .map(|(_, member)| member.clone())
            .collect();
        let unsafe_extras: Vec<&str> = extras
            .iter()
            .filter(|member| !is_generated_placeholder(&member.name))
            .map(|member| member.name.as_str())
            .collect();
        if !unsafe_extras.is_empty() {
            bail!(
                "non-placeholder editor members would need removal from {}: {}",
                current.type_name,
                unsafe_extras.join(", ")
            );
        }
        let has_shape_changes = mapping.iter().any(|(target_index, current_index)| {
            !target.members[*target_index].same_shape(&current.members[*current_index])
        });
        let has_insertions = mapping.len() != target.members.len();
        if !extras.is_empty() || has_shape_changes || has_insertions {
            self.reserve_definition_mutation(
                current_id,
                Some(target_id),
                &key,
                block_begin,
                block_end,
                &current.type_name,
            )?;
        }
        for extra in &extras {
            if let Some(access_path) = self.find_nested_leaf_access_definition(&target, extra)? {
                self.register_rename(&extra.name, &access_path)?;
            }
            self.remove_placeholder(&key, &current, extra)?;
        }

        for (&target_index, &current_index) in &mapping {
            let wanted = target.members[target_index].clone();
            let existing = current.members[current_index].clone();
            let wanted_struct = wanted.base_type.is_struct();
            let existing_struct = existing.base_type.is_struct();
            if wanted_struct && !wanted.same_shape(&existing) {
                self.ensure_target_struct_available(
                    &wanted,
                    &key,
                    block_begin,
                    &current,
                    &wanted.name,
                )?;
            }

            if wanted.name != existing.name {
                if !is_generated_placeholder(&existing.name) {
                    bail!("cannot safely rename {} to {}", existing.name, wanted.name);
                }
                if wanted.same_shape(&existing) {
                    // names are not part of the RHI layout hash
                } else if wanted_struct && !existing_struct {
                    if let Some(access_path) =
                        self.find_nested_leaf_access_member(&wanted, &existing)?
                    {
                        self.register_rename(&existing.name, &access_path)?;
                    }
                } else {
                    self.register_rename(&existing.name, &wanted.name)?;
                }
            }

            if !wanted.same_shape(&existing) {
                if let Some(macro_name) = &shared_parameter_table {
                    bail!(
                        "shape changes in {} require editing shared parameter table {macro_name}, which is not supported",
                        current.type_name
                    );
                }
                let (line, old_line) = {
                    let plan = &self.files[&key];
                    let line = find_member_line(plan, &current, &existing)?;
                    if !(line > block_begin && line < block_end) {
                        if let Some(macro_name) =
                            shared_macro_for_anchor(plan, line, block_begin, block_end)
                        {
                            bail!(
                                "shape changes inside shared parameter table {macro_name} are not supported"
                            );
                        }
                        bail!(
                            "shape change for {}::{} is outside its declaration block",
                            current.type_name,
                            existing.name
                        );
                    }
                    (line, plan.original_lines()[line - 1].to_string())
                };
                let indentation_len = old_line.len() - old_line.trim_start().len();
                let indentation = &old_line[..indentation_len];
                let continuation = if old_line.trim_end().ends_with('\\') {
                    " \\"
                } else {
                    ""
                };
                let replacement = format!("{indentation}{}{continuation}", write_macro(&wanted)?);
                self.files
                    .get_mut(&key)
                    .expect("file plan exists")
                    .replace_line(line, Some(replacement))?;
                if wanted.name != existing.name {
                    self.register_removed(&existing.name, &key, line)?;
                }
            }

            if wanted.base_type.is_nested_definition()
                && existing_struct
                && wanted.nested_definition_id != 0
                && existing.nested_definition_id != 0
            {
                self.plan_definition(wanted.nested_definition_id, existing.nested_definition_id)?;
            }
        }
        self.plan_insertions(&key, &target, &current, &mapping, block_begin, block_end)
    }

    fn plan_insertions(
        &mut self,
        key: &Path,
        target: &DefinitionRecord,
        current: &DefinitionRecord,
        mapping: &BTreeMap<usize, usize>,
        block_begin: usize,
        block_end: usize,
    ) -> Result<()> {
        let mut target_index = 0;
        while target_index < target.members.len() {
            if mapping.contains_key(&target_index) {
                target_index += 1;
                continue;
            }
            let first = target_index;
            while target_index < target.members.len() && !mapping.contains_key(&target_index) {
                target_index += 1;
            }
            let after = target_index;
            let mut insert_line = None;
            let mut shared_anchor = None;
            if after < target.members.len() {
                let anchor = &current.members[mapping[&after]];
                let anchor_line = find_member_line(&self.files[key], current, anchor)?;
                if anchor_line > block_begin && anchor_line < block_end {
                    insert_line = Some(anchor_line);
                } else if let Some(macro_name) =
                    shared_macro_for_anchor(&self.files[key], anchor_line, block_begin, block_end)
                {
                    shared_anchor = Some((macro_name, anchor_line, mapping[&after]));
                }
            }
            if let Some((macro_name, anchor_line, current_anchor_index)) = shared_anchor {
                self.plan_shared_table_insertion(
                    key,
                    &macro_name,
                    current,
                    &target.members[first..after],
                    anchor_line,
                    current_anchor_index,
                )?;
                continue;
            }
            if insert_line.is_none() && after == target.members.len() {
                // append after the shared table so only this consumer changes
                insert_line = Some(block_end);
            }
            let Some(insert_line) = insert_line else {
                bail!(
                    "cannot place inserted members in {} without editing a shared macro table",
                    current.type_name
                );
            };

            let mut lines = vec!["\t// generated from captured shader metadata".to_string()];
            for member in &target.members[first..after] {
                if member.base_type.is_struct() {
                    self.ensure_target_struct_available(
                        member,
                        key,
                        block_begin,
                        current,
                        &member.name,
                    )?;
                }
                if member_needs_runtime_value(member) {
                    self.push_pending_runtime_member(current, &member.name, member);
                }
                lines.push(format!("\t{}", write_macro(member)?));
            }
            self.files
                .get_mut(key)
                .expect("file plan exists")
                .add_before(insert_line, lines);
        }
        Ok(())
    }

    fn plan_shared_table_insertion(
        &mut self,
        key: &Path,
        macro_name: &str,
        current: &DefinitionRecord,
        inserted: &[MemberRecord],
        anchor_line: usize,
        current_anchor_index: usize,
    ) -> Result<()> {
        if inserted.is_empty() {
            return Ok(());
        }
        let (macro_begin, macro_end, anchor_macro) =
            shared_macro_block(&self.files[key], anchor_line)
                .with_context(|| format!("could not locate shared parameter table {macro_name}"))?;
        let signature = inserted
            .iter()
            .map(|member| member.name.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let insertion_key = (key.to_path_buf(), anchor_line, signature);
        if self.planned_shared_insertions.contains(&insertion_key) {
            return Ok(());
        }

        let consumer_ids: Vec<u32> = self
            .editor
            .definitions
            .values()
            .filter(|definition| definition.source_file == current.source_file)
            .filter_map(|definition| {
                let block = find_definition_block(&self.files[key], definition).ok()?;
                (shared_parameter_table_in_block(&self.files[key], block.0, block.1)
                    == Some(macro_name.to_string()))
                .then_some(definition.id)
            })
            .collect();
        if consumer_ids.len() < 2 {
            bail!("shared parameter table {macro_name} does not have multiple captured consumers");
        }

        let mut consumer_growth = Vec::new();
        for consumer_id in consumer_ids {
            let consumer = &self.editor.definitions[&consumer_id];
            let owners = self
                .definition_layout_owners
                .get(&consumer.id)
                .cloned()
                .unwrap_or_default();
            for owner in &owners {
                if !self.selected_layouts.contains(owner) {
                    bail!(
                        "shared parameter table {macro_name} also changes {owner}, which is outside the selected layouts"
                    );
                }
            }
            let target_ids: Vec<u32> = self
                .game
                .definitions
                .values()
                .filter(|definition| definition.type_name == consumer.type_name)
                .map(|definition| definition.id)
                .collect();
            if target_ids.len() != 1 {
                bail!(
                    "shared parameter table {macro_name} has no unique game definition for {}",
                    consumer.type_name
                );
            }
            let consumer_target = &self.game.definitions[&target_ids[0]];
            simulate::verify_definition(&self.game.definitions, consumer_target.id)?;
            simulate::verify_definition(&self.editor.definitions, consumer.id)?;

            let anchor_name = &current.members[current_anchor_index].name;
            let current_anchor = consumer
                .members
                .iter()
                .position(|member| member.name == *anchor_name)
                .with_context(|| {
                    format!(
                        "{} does not contain the shared-table anchor {anchor_name}",
                        consumer.type_name
                    )
                })?;
            let target_anchor = consumer_target
                .members
                .iter()
                .position(|member| member.name == *anchor_name)
                .with_context(|| {
                    format!(
                        "game definition {} does not contain the shared-table anchor {anchor_name}",
                        consumer.type_name
                    )
                })?;
            if target_anchor < inserted.len() {
                bail!("shared parameter table insertion has no valid target position");
            }
            let wanted = &consumer_target.members[target_anchor - inserted.len()..target_anchor];
            if wanted
                .iter()
                .zip(inserted)
                .any(|(left, right)| left.name != right.name || !left.same_shape(right))
            {
                bail!(
                    "shared parameter table {macro_name} does not have the same captured insertion in {}",
                    consumer.type_name
                );
            }
            if inserted.iter().any(|wanted| {
                consumer
                    .members
                    .iter()
                    .any(|existing| existing.name == wanted.name)
            }) {
                bail!(
                    "shared parameter table insertion is already present in {}",
                    consumer.type_name
                );
            }

            let mut projected = consumer.clone();
            let mut projected_members = inserted.to_vec();
            projected
                .members
                .splice(current_anchor..current_anchor, projected_members.drain(..));
            for (index, member) in projected.members.iter_mut().enumerate() {
                member.index = index;
            }
            let mut projected_definitions = self.editor.definitions.clone();
            projected_definitions.insert(projected.id, projected.clone());
            let simulated = simulate::simulate_definition(&projected_definitions, projected.id)?;
            let simulated_anchor = simulated.member_offsets[current_anchor + inserted.len()];
            if simulated_anchor != consumer_target.members[target_anchor].offset {
                bail!(
                    "shared parameter table {macro_name} does not reproduce the captured anchor offset in {}",
                    consumer.type_name
                );
            }
            let growth = simulated
                .size
                .checked_sub(consumer.size)
                .context("shared parameter table unexpectedly shrinks a consumer")?;

            for layout_name in owners {
                consumer_growth.push((layout_name, growth));
            }
        }

        let block = (key.to_path_buf(), macro_begin, macro_end);
        if let Some(existing) = self.planned_blocks.get(&block) {
            bail!("shared parameter table {macro_name} is already being edited for {existing}");
        }
        self.planned_blocks
            .insert(block, format!("shared parameter table {macro_name}"));
        self.planned_shared_insertions.insert(insertion_key);
        for (layout_name, growth) in consumer_growth {
            let entry = self
                .planned_layout_constant_growth
                .entry(layout_name)
                .or_default();
            *entry = entry
                .checked_add(growth)
                .context("planned shared-table growth overflowed")?;
        }

        let original = self.files[key].original_lines()[anchor_line - 1];
        let indentation = &original[..original.len() - original.trim_start().len()];
        let mut lines = Vec::with_capacity(inserted.len() + 1);
        lines.push(format!(
            "{indentation}/* Constants recovered from every captured {macro_name} consumer. */ \\",
        ));
        for member in inserted {
            lines.push(format!(
                "{indentation}{} \\",
                write_shared_member_macro(member, &anchor_macro)?
            ));
        }
        self.files
            .get_mut(key)
            .expect("file plan exists")
            .add_before(anchor_line, lines);

        self.layout_notes.insert(
            current.shader_variable_name.clone(),
            format!(
                "added {} constants through shared parameter table {macro_name}",
                inserted.len()
            ),
        );
        Ok(())
    }

    fn ensure_target_struct_available(
        &mut self,
        member: &MemberRecord,
        parent_key: &Path,
        insert_line: usize,
        container: &DefinitionRecord,
        path_prefix: &str,
    ) -> Result<()> {
        if !member.base_type.is_struct() {
            return Ok(());
        }
        if member.struct_type.is_empty() {
            bail!("struct member has no C++ type: {}", member.name);
        }
        if let Some(editor_definition) = self
            .editor
            .definitions
            .values()
            .find(|candidate| candidate.type_name == member.struct_type)
        {
            let editor_id = editor_definition.id;
            if member.nested_definition_id != 0 {
                self.plan_definition(member.nested_definition_id, editor_id)?;
            }
            return Ok(());
        }
        if let Some(existing_parent) = self.generated_target_types.get(&member.struct_type) {
            if existing_parent == parent_key {
                return Ok(());
            }
            bail!(
                "generated struct {} is required by multiple source files; choose a shared declaration location manually",
                member.struct_type
            );
        }
        if member.nested_definition_id == 0 {
            bail!(
                "captured metadata has no definition for {}",
                member.struct_type
            );
        }
        if self.source_contains_shader_definition(&member.struct_type)? {
            bail!(
                "{} is declared in the source tree but was not present in the editor capture",
                member.struct_type
            );
        }

        let target_definition = self.game.definitions[&member.nested_definition_id].clone();
        if target_definition.use_case != 0 {
            bail!(
                "cannot synthesize non-parameter struct {}",
                target_definition.type_name
            );
        }
        self.generated_target_types
            .insert(member.struct_type.clone(), parent_key.to_path_buf());
        for child in target_definition
            .members
            .iter()
            .filter(|child| child.base_type.is_struct())
        {
            self.ensure_target_struct_available(
                child,
                parent_key,
                insert_line,
                container,
                &format!("{path_prefix}.{}", child.name),
            )?;
        }

        let mut lines = vec![
            "// generated from captured shader metadata".to_string(),
            format!(
                "BEGIN_SHADER_PARAMETER_STRUCT({}, )",
                target_definition.type_name
            ),
        ];
        for child in &target_definition.members {
            if member_needs_runtime_value(child) {
                self.push_pending_runtime_member(
                    container,
                    &format!("{path_prefix}.{}", child.name),
                    child,
                );
            }
            lines.push(format!("\t{}", write_macro(child)?));
        }
        lines.push("END_SHADER_PARAMETER_STRUCT()".to_string());
        lines.push(String::new());
        self.files
            .get_mut(parent_key)
            .expect("parent file exists")
            .add_before(insert_line, lines);
        Ok(())
    }

    fn find_nested_leaf_access_definition(
        &self,
        target: &DefinitionRecord,
        current: &MemberRecord,
    ) -> Result<Option<String>> {
        let mut leaves = Vec::new();
        for member in target
            .members
            .iter()
            .filter(|member| member.base_type.is_struct())
        {
            self.collect_target_leaves(member, member.offset, member.name.clone(), &mut leaves, 0)?;
        }
        Ok(select_leaf_access(&leaves, current))
    }

    fn find_nested_leaf_access_member(
        &self,
        target: &MemberRecord,
        current: &MemberRecord,
    ) -> Result<Option<String>> {
        let mut leaves = Vec::new();
        self.collect_target_leaves(target, target.offset, target.name.clone(), &mut leaves, 0)?;
        Ok(select_leaf_access(&leaves, current))
    }

    fn collect_target_leaves<'b>(
        &'b self,
        member: &'b MemberRecord,
        absolute_offset: u32,
        access_path: String,
        leaves: &mut Vec<TargetLeaf<'b>>,
        depth: usize,
    ) -> Result<()> {
        const MAX_EXPANDED_LEAVES: usize = 65_536;

        if depth >= MAX_DEFINITION_DEPTH {
            bail!("shader metadata nesting exceeded {MAX_DEFINITION_DEPTH} definitions");
        }
        if member.base_type.is_nested_definition() && member.nested_definition_id != 0 {
            let definition = self
                .game
                .definitions
                .get(&member.nested_definition_id)
                .with_context(|| {
                    format!(
                        "nested member {} references missing definition {}",
                        member.name, member.nested_definition_id
                    )
                })?;
            let count = member.elements.max(1);
            if usize::try_from(count).unwrap_or(usize::MAX) > MAX_EXPANDED_LEAVES {
                bail!(
                    "nested member {} expands to too many array elements ({count})",
                    member.name
                );
            }
            for element in 0..count {
                let element_offset = element
                    .checked_mul(definition.size)
                    .and_then(|offset| absolute_offset.checked_add(offset))
                    .with_context(|| format!("nested member {} offset overflowed", member.name))?;
                let element_path = if member.elements == 0 {
                    access_path.clone()
                } else {
                    format!("{access_path}[{element}]")
                };
                for child in &definition.members {
                    let child_offset =
                        element_offset.checked_add(child.offset).with_context(|| {
                            format!("nested member {} child offset overflowed", child.name)
                        })?;
                    self.collect_target_leaves(
                        child,
                        child_offset,
                        format!("{element_path}.{}", child.name),
                        leaves,
                        depth + 1,
                    )?;
                }
            }
            return Ok(());
        }
        if leaves.len() >= MAX_EXPANDED_LEAVES {
            bail!("nested member expansion exceeded {MAX_EXPANDED_LEAVES} leaves");
        }
        leaves.push(TargetLeaf {
            member,
            offset: absolute_offset,
            access_path,
        });
        Ok(())
    }

    fn remove_placeholder(
        &mut self,
        key: &Path,
        definition: &DefinitionRecord,
        member: &MemberRecord,
    ) -> Result<()> {
        let line = find_member_line(&self.files[key], definition, member)?;
        let (block_begin, block_end) = find_definition_block(&self.files[key], definition)?;
        if let Some(macro_name) =
            shared_macro_for_anchor(&self.files[key], line, block_begin, block_end)
        {
            bail!(
                "cannot remove {} from shared parameter table {macro_name}",
                member.name
            );
        }
        if !(line > block_begin && line < block_end) {
            bail!(
                "placeholder {}::{} is outside its declaration block",
                definition.type_name,
                member.name
            );
        }
        if !is_safe_macro_line_deletion(&self.files[key], line) {
            bail!(
                "cannot remove {} from the final line of a continued macro",
                member.name
            );
        }
        self.files
            .get_mut(key)
            .expect("file plan exists")
            .delete_line(line)?;
        self.register_removed(&member.name, key, line)
    }

    fn register_rename(&mut self, from: &str, to: &str) -> Result<()> {
        if let Some(prior) = self.global_renames.get(from) {
            if prior != to {
                bail!("conflicting rename for {from}: {prior} or {to}");
            }
        }
        self.global_renames.insert(from.to_string(), to.to_string());
        Ok(())
    }

    fn register_removed(&mut self, name: &str, key: &Path, line: usize) -> Result<()> {
        let source_path = self.files[key].source_path.clone();
        self.removed_declarations
            .entry(name.to_string())
            .or_default()
            .insert(declaration_location(&source_path, line));
        Ok(())
    }

    fn source_contains_shader_definition(&mut self, type_name: &str) -> Result<bool> {
        if self.source_definition_index.is_none() {
            self.source_definition_index = Some(self.build_source_definition_index()?);
        }
        Ok(self
            .source_definition_index
            .as_ref()
            .is_some_and(|definitions| definitions.contains_key(type_name)))
    }

    fn plugin_for_shader_definition(&mut self, type_name: &str) -> Result<Option<String>> {
        if self.source_definition_index.is_none() {
            self.source_definition_index = Some(self.build_source_definition_index()?);
        }
        let Some(paths) = self
            .source_definition_index
            .as_ref()
            .and_then(|definitions| definitions.get(type_name))
            .cloned()
        else {
            return Ok(None);
        };

        let plugins_root = self.engine_top.join("Engine/Plugins");
        let mut names = BTreeSet::new();
        for path in paths {
            let Some(name) = plugin_name_for_source(&path, &plugins_root) else {
                return Ok(None);
            };
            names.insert(name);
        }
        if names.len() == 1 {
            Ok(names.into_iter().next())
        } else {
            Ok(None)
        }
    }

    fn build_source_definition_index(&mut self) -> Result<HashMap<String, Vec<PathBuf>>> {
        let declaration = Regex::new(
            r"\bBEGIN_(?:GLOBAL_)?SHADER_PARAMETER_STRUCT(?:_WITH_CONSTRUCTOR)?\s*\(\s*([A-Za-z_][A-Za-z0-9_]*)\b",
        )?;
        let paths = self.source_files()?.to_vec();
        progress::step("Searching the source tree for shader struct declarations...");
        let total = paths.len();
        let scanned = std::sync::atomic::AtomicUsize::new(0);

        // build the fallback index lazily and in parallel
        let index = paths
            .par_iter()
            .try_fold(
                HashMap::<String, Vec<PathBuf>>::new,
                |mut definitions, path| -> Result<_> {
                    cancel::check()?;
                    let done = scanned.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    if done % 2048 == 0 {
                        progress::transient(&format!("  searched {done} of {total} files"));
                    }
                    let bytes = fs::read(path)
                        .with_context(|| format!("could not scan {}", path.display()))?;
                    if memchr::memmem::find(&bytes, b"SHADER_PARAMETER_STRUCT").is_none() {
                        return Ok(definitions);
                    }
                    let text = std::str::from_utf8(&bytes)
                        .with_context(|| format!("source file is not UTF-8: {}", path.display()))?;
                    for captures in declaration.captures_iter(text) {
                        definitions
                            .entry(captures[1].to_string())
                            .or_default()
                            .push(path.clone());
                    }
                    Ok(definitions)
                },
            )
            .try_reduce(
                HashMap::<String, Vec<PathBuf>>::new,
                |mut all, definitions| -> Result<_> {
                    for (name, mut paths) in definitions {
                        all.entry(name).or_default().append(&mut paths);
                    }
                    Ok(all)
                },
            );
        progress::finish_transient();
        let mut index = index?;
        for paths in index.values_mut() {
            paths.sort();
            paths.dedup();
        }
        Ok(index)
    }

    fn source_files(&mut self) -> Result<&[PathBuf]> {
        if self.source_file_cache.is_none() {
            self.source_file_cache = Some(source_files(&self.engine_top.join("Engine"))?);
        }
        Ok(self.source_file_cache.as_deref().unwrap_or_default())
    }

    fn layout_exact(&self, target: &LayoutRecord, current: &LayoutRecord) -> bool {
        if target.type_name != current.type_name
            || target.layout_name != current.layout_name
            || target.rhi_hash != current.rhi_hash
            || target.constant_buffer_size != current.constant_buffer_size
            || target.render_targets_offset != current.render_targets_offset
            || (target.static_slot != 255) != (current.static_slot != 255)
            || target.binding_flags != current.binding_flags
            || target.flags != current.flags
        {
            return false;
        }
        for (name, left) in &target.arrays {
            let Some(right) = current.arrays.get(name) else {
                return false;
            };
            if !resource_arrays_equal(left, right) {
                return false;
            }
        }
        true
    }

    fn add_outcome(
        &mut self,
        game: &LayoutRecord,
        editor: Option<&LayoutRecord>,
        status: &str,
        detail: &str,
    ) {
        self.outcomes.push(LayoutOutcome {
            name: game.name.clone(),
            status: status.to_string(),
            detail: detail.to_string(),
            game_rhi_hash: game.rhi_hash,
            editor_rhi_hash: editor.map_or(0, |value| value.rhi_hash),
            game_metadata_hash: game.metadata_hash,
            editor_metadata_hash: editor.map_or(0, |value| value.metadata_hash),
        });
    }

    fn ensure_file(&mut self, path: &Path) -> Result<PathBuf> {
        let path = path
            .canonicalize()
            .with_context(|| format!("source file not found: {}", path.display()))?;
        if self.files.contains_key(&path) {
            return Ok(path);
        }
        let bytes = fs::read(&path)?;
        if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
            bail!("UTF-16 source files are not supported: {}", path.display());
        }
        let utf8_bom = bytes.starts_with(&[0xef, 0xbb, 0xbf]);
        let text = String::from_utf8(bytes[usize::from(utf8_bom) * 3..].to_vec())
            .with_context(|| format!("source file is not UTF-8: {}", path.display()))?;
        let relative_path = path
            .strip_prefix(&self.engine_top)
            .with_context(|| {
                format!(
                    "source path is outside the supplied engine root: {}",
                    path.display()
                )
            })?
            .to_path_buf();
        let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
        self.files.insert(
            path.clone(),
            FilePlan {
                source_path: path.clone(),
                relative_path,
                original_text: text,
                newline,
                utf8_bom,
                replacements: BTreeMap::new(),
                insert_before: BTreeMap::new(),
                renames: BTreeMap::new(),
            },
        );
        Ok(path)
    }

    fn resolve_source_path(&mut self, recorded_path: &str) -> Result<PathBuf> {
        let normalized = recorded_path.replace('\\', "/");
        let lowercase = normalized.to_ascii_lowercase();
        let marker = lowercase
            .find("/engine/source/")
            .or_else(|| lowercase.find("/engine/plugins/"));
        let starts_at_engine =
            lowercase.starts_with("engine/source/") || lowercase.starts_with("engine/plugins/");
        if marker.is_some() || starts_at_engine {
            let relative = marker
                .map(|index| &normalized[index + 1..])
                .unwrap_or(&normalized);
            let candidate = self.engine_top.join(relative);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }

        let file_name = Path::new(&normalized)
            .file_name()
            .context("recorded source path has no file name")?
            .to_string_lossy();
        let mut candidates = Vec::new();
        for entry in self.source_files()? {
            if entry
                .file_name()
                .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case(&file_name))
            {
                candidates.push(entry.clone());
                if candidates.len() == 3 {
                    break;
                }
            }
        }
        if candidates.len() == 1 {
            return Ok(candidates.remove(0));
        }
        bail!("could not map recorded source path into the supplied engine tree: {recorded_path}")
    }
}

struct OffsetLeaf<'a> {
    path: String,
    offset: u32,
    member: &'a MemberRecord,
}

fn is_scalar_constant(base_type: ShaderParameterBaseType) -> bool {
    matches!(
        base_type,
        ShaderParameterBaseType::Bool
            | ShaderParameterBaseType::Int32
            | ShaderParameterBaseType::Uint32
            | ShaderParameterBaseType::Float32
    )
}

// flatten nested definitions into absolute offsets and access paths
fn collect_offset_leaves(
    definitions: &HashMap<u32, DefinitionRecord>,
    root_definition_id: u32,
) -> Result<Vec<OffsetLeaf<'_>>> {
    const MAX_EXPANDED_LEAVES: usize = 65_536;

    fn collect<'a>(
        definitions: &'a HashMap<u32, DefinitionRecord>,
        member: &'a MemberRecord,
        absolute_offset: u32,
        access_path: String,
        leaves: &mut Vec<OffsetLeaf<'a>>,
        depth: usize,
    ) -> Result<()> {
        if depth >= MAX_DEFINITION_DEPTH {
            bail!("shader metadata nesting exceeded {MAX_DEFINITION_DEPTH} definitions");
        }
        if member.base_type.is_nested_definition() && member.nested_definition_id != 0 {
            let definition = definitions
                .get(&member.nested_definition_id)
                .with_context(|| {
                    format!(
                        "nested member {} references missing definition {}",
                        member.name, member.nested_definition_id
                    )
                })?;
            let count = member.elements.max(1);
            if usize::try_from(count).unwrap_or(usize::MAX) > MAX_EXPANDED_LEAVES {
                bail!(
                    "nested member {} expands to too many array elements ({count})",
                    member.name
                );
            }
            for element in 0..count {
                let element_offset = element
                    .checked_mul(definition.size)
                    .and_then(|offset| absolute_offset.checked_add(offset))
                    .with_context(|| format!("nested member {} offset overflowed", member.name))?;
                let element_path = if member.elements == 0 {
                    access_path.clone()
                } else {
                    format!("{access_path}[{element}]")
                };
                for child in &definition.members {
                    let child_offset =
                        element_offset.checked_add(child.offset).with_context(|| {
                            format!("nested member {} child offset overflowed", child.name)
                        })?;
                    collect(
                        definitions,
                        child,
                        child_offset,
                        format!("{element_path}.{}", child.name),
                        leaves,
                        depth + 1,
                    )?;
                }
            }
            return Ok(());
        }
        if leaves.len() >= MAX_EXPANDED_LEAVES {
            bail!("nested member expansion exceeded {MAX_EXPANDED_LEAVES} leaves");
        }
        leaves.push(OffsetLeaf {
            path: access_path,
            offset: absolute_offset,
            member,
        });
        Ok(())
    }

    let root = definitions
        .get(&root_definition_id)
        .with_context(|| format!("missing definition {root_definition_id}"))?;
    let mut leaves = Vec::new();
    for member in &root.members {
        collect(
            definitions,
            member,
            member.offset,
            member.name.clone(),
            &mut leaves,
            0,
        )?;
    }
    Ok(leaves)
}

fn collect_definition_layout_owners(dump: &LayoutDump) -> Result<HashMap<u32, BTreeSet<String>>> {
    let mut owners: HashMap<u32, BTreeSet<String>> = HashMap::new();
    for layout in dump.layouts.values() {
        let mut pending = vec![(layout.root_definition_id, 0usize)];
        let mut visited = HashSet::new();
        while let Some((definition_id, depth)) = pending.pop() {
            if !visited.insert(definition_id) {
                continue;
            }
            if depth >= MAX_DEFINITION_DEPTH {
                bail!(
                    "shader metadata nesting exceeded {MAX_DEFINITION_DEPTH} definitions while mapping {}",
                    layout.name
                );
            }
            let definition = dump.definitions.get(&definition_id).with_context(|| {
                format!(
                    "layout {} references missing definition {definition_id}",
                    layout.name
                )
            })?;
            owners
                .entry(definition_id)
                .or_default()
                .insert(layout.name.clone());
            for member in &definition.members {
                if member.base_type.is_nested_definition() && member.nested_definition_id != 0 {
                    pending.push((member.nested_definition_id, depth + 1));
                }
            }
        }
    }
    Ok(owners)
}

fn reachable_definitions_by_type(
    dump: &LayoutDump,
    root_definition_id: u32,
    type_name: &str,
) -> Result<Vec<u32>> {
    let mut matches = Vec::new();
    let mut pending = vec![(root_definition_id, 0usize)];
    let mut visited = HashSet::new();
    while let Some((definition_id, depth)) = pending.pop() {
        if !visited.insert(definition_id) {
            continue;
        }
        if depth >= MAX_DEFINITION_DEPTH {
            bail!("shader metadata nesting exceeded {MAX_DEFINITION_DEPTH} definitions");
        }
        let definition = dump
            .definitions
            .get(&definition_id)
            .with_context(|| format!("missing definition {definition_id}"))?;
        if definition.type_name == type_name {
            matches.push(definition_id);
        }
        for member in &definition.members {
            if member.base_type.is_nested_definition() && member.nested_definition_id != 0 {
                pending.push((member.nested_definition_id, depth + 1));
            }
        }
    }
    matches.sort_unstable();
    Ok(matches)
}

fn align_members(
    target: &DefinitionRecord,
    current: &DefinitionRecord,
) -> Result<BTreeMap<usize, usize>> {
    let mut mapping = BTreeMap::new();
    let mut used = HashSet::new();
    let mut by_name = HashMap::new();
    for (index, member) in current.members.iter().enumerate() {
        if by_name.insert(member.name.clone(), index).is_some() {
            bail!("duplicate editor member name {}", member.name);
        }
    }
    for (target_index, member) in target.members.iter().enumerate() {
        if let Some(&current_index) = by_name.get(&member.name) {
            mapping.insert(target_index, current_index);
            used.insert(current_index);
        }
    }
    for (target_index, wanted) in target.members.iter().enumerate() {
        if mapping.contains_key(&target_index) {
            continue;
        }
        let mut candidates: Vec<usize> = current
            .members
            .iter()
            .enumerate()
            .filter(|(index, member)| {
                !used.contains(index)
                    && member.offset == wanted.offset
                    && is_generated_placeholder(&member.name)
            })
            .map(|(index, _)| index)
            .collect();
        let shape_matches: Vec<usize> = candidates
            .iter()
            .copied()
            .filter(|index| current.members[*index].same_shape(wanted))
            .collect();
        if shape_matches.len() == 1 {
            candidates = shape_matches;
        }
        if candidates.len() == 1 {
            mapping.insert(target_index, candidates[0]);
            used.insert(candidates[0]);
        }
    }
    Ok(mapping)
}

fn select_leaf_access(leaves: &[TargetLeaf<'_>], current: &MemberRecord) -> Option<String> {
    let matches: Vec<&TargetLeaf<'_>> = leaves
        .iter()
        .filter(|leaf| leaf.offset == current.offset && leaf.member.same_storage_shape(current))
        .collect();
    (matches.len() == 1).then(|| matches[0].access_path.clone())
}

fn shared_macro_for_anchor(
    plan: &FilePlan,
    anchor_line: usize,
    block_begin: usize,
    block_end: usize,
) -> Option<String> {
    let lines = plan.original_lines();
    let mut define_line = None;
    for line_number in (1..=anchor_line).rev() {
        let line = lines.get(line_number - 1)?.trim();
        if let Some(rest) = line.strip_prefix("#define ") {
            let name = rest
                .split(|value: char| value.is_ascii_whitespace() || value == '(')
                .next()?
                .to_string();
            define_line = Some((line_number, name));
            break;
        }
        if line_number != anchor_line && !line.ends_with('\\') {
            return None;
        }
    }
    let (define_line, macro_name) = define_line?;
    if !(define_line..=anchor_line).all(|line| lines[line - 1].trim_end().ends_with('\\')) {
        return None;
    }
    let used_by_definition = lines[block_begin..block_end - 1]
        .iter()
        .any(|line| parameter_table_usage_name(line) == Some(macro_name.as_str()));
    used_by_definition.then_some(macro_name)
}

fn shared_macro_block(plan: &FilePlan, anchor_line: usize) -> Option<(usize, usize, String)> {
    let lines = plan.original_lines();
    let anchor = lines.get(anchor_line - 1)?.trim();
    let anchor_macro = anchor.split_once('(')?.0.trim().to_string();
    if !IDENTIFIER.is_match(&anchor_macro) {
        return None;
    }

    let mut begin = None;
    for line_number in (1..=anchor_line).rev() {
        let line = lines.get(line_number - 1)?.trim();
        if line.starts_with("#define ") {
            begin = Some(line_number);
            break;
        }
        if line_number != anchor_line && !line.ends_with('\\') {
            return None;
        }
    }
    let begin = begin?;
    let mut end = begin;
    while end < lines.len() && lines[end - 1].trim_end().ends_with('\\') {
        end += 1;
    }
    (anchor_line >= begin && anchor_line <= end).then_some((begin, end, anchor_macro))
}

fn shared_parameter_table_in_block(
    plan: &FilePlan,
    block_begin: usize,
    block_end: usize,
) -> Option<String> {
    let lines = plan.original_lines();
    for usage in &lines[block_begin..block_end - 1] {
        let Some(macro_name) = parameter_table_usage_name(usage) else {
            continue;
        };
        for (index, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            let Some(rest) = trimmed.strip_prefix("#define ") else {
                continue;
            };
            let Some(defined_name) = rest
                .split(|value: char| value.is_ascii_whitespace() || value == '(')
                .next()
            else {
                continue;
            };
            if defined_name != macro_name || !trimmed.trim_end().ends_with('\\') {
                continue;
            }
            let mut body_has_member = false;
            for body in &lines[index + 1..] {
                body_has_member |= body.contains("MEMBER");
                if !body.trim_end().ends_with('\\') {
                    break;
                }
            }
            if body_has_member {
                return Some(macro_name.to_string());
            }
        }
    }
    None
}

fn parameter_table_usage_name(line: &str) -> Option<&str> {
    let line = line.split_once("//").map_or(line, |(code, _)| code);
    let line = if let Some(comment) = line.find("/*") {
        if !line[comment..].trim_end().ends_with("*/") {
            return None;
        }
        &line[..comment]
    } else {
        line
    };
    let name = line.trim();
    IDENTIFIER.is_match(name).then_some(name)
}

fn write_shared_member_macro(member: &MemberRecord, anchor_macro: &str) -> Result<String> {
    let declaration = write_macro(member)?;
    let (macro_name, arguments) = declaration
        .split_once('(')
        .context("generated shader declaration has no argument list")?;
    if macro_name != "SHADER_PARAMETER" {
        bail!(
            "shared parameter table insertion requires {macro_name}, which cannot be derived safely from {anchor_macro}"
        );
    }
    if !anchor_macro.contains("MEMBER") || anchor_macro.ends_with("_EX") {
        bail!("shared parameter table macro {anchor_macro} is not a plain member declaration");
    }
    Ok(format!("{anchor_macro}({arguments}"))
}

fn is_generated_placeholder(name: &str) -> bool {
    // keep placeholders from older ShaderDriftCheck builds
    name.starts_with("oatmealf") || name.starts_with("ShaderDrift")
}

fn member_needs_runtime_value(member: &MemberRecord) -> bool {
    !matches!(
        member.base_type,
        ShaderParameterBaseType::Bool
            | ShaderParameterBaseType::Int32
            | ShaderParameterBaseType::Uint32
            | ShaderParameterBaseType::Float32
            | ShaderParameterBaseType::IncludedStruct
            | ShaderParameterBaseType::RenderTargetBindingSlots
    )
}

fn find_definition_block(plan: &FilePlan, definition: &DefinitionRecord) -> Result<(usize, usize)> {
    let lines = plan.original_lines();
    let candidates: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains("BEGIN_") && contains_word(line, &definition.type_name))
        .map(|(index, _)| index + 1)
        .collect();
    let Some(begin) = candidates
        .into_iter()
        .min_by_key(|line| line.abs_diff(definition.source_line))
    else {
        bail!(
            "could not find declaration block for {} in {}",
            definition.type_name,
            plan.relative_path.display()
        );
    };
    let end = ((begin + 1)..=lines.len()).find(|line| {
        lines[*line - 1].contains("END_")
            && (lines[*line - 1].contains("SHADER_PARAMETER_STRUCT")
                || lines[*line - 1].contains("UNIFORM_BUFFER_STRUCT"))
    });
    let Some(end) = end else {
        bail!(
            "could not find end of declaration block for {}",
            definition.type_name
        );
    };
    Ok((begin, end))
}

fn find_member_line(
    plan: &FilePlan,
    definition: &DefinitionRecord,
    member: &MemberRecord,
) -> Result<usize> {
    let lines = plan.original_lines();
    if member.source_line > 0
        && member.source_line <= lines.len()
        && is_parameter_declaration_line(lines[member.source_line - 1], &member.name)
    {
        return Ok(member.source_line);
    }
    let matches: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| is_parameter_declaration_line(line, &member.name))
        .map(|(index, _)| index + 1)
        .collect();
    if matches.len() == 1 {
        return Ok(matches[0]);
    }
    bail!(
        "could not uniquely locate member {}::{} in {}",
        definition.type_name,
        member.name,
        plan.relative_path.display()
    )
}

fn is_parameter_declaration_line(line: &str, member_name: &str) -> bool {
    let mut trimmed = line.trim_start();
    while let Some(comment) = trimmed.strip_prefix("/*") {
        let Some(end) = comment.find("*/") else {
            return false;
        };
        trimmed = comment[end + 2..].trim_start();
    }
    if trimmed.starts_with("//") || trimmed.starts_with('*') {
        return false;
    }
    let Some(open_parenthesis) = trimmed.find('(') else {
        return false;
    };
    let macro_name = trimmed[..open_parenthesis].trim();
    let macro_family = macro_name.contains("PARAMETER") || macro_name.contains("MEMBER");
    let macro_style = !macro_name.is_empty()
        && macro_name
            .chars()
            .all(|value| value.is_ascii_uppercase() || value.is_ascii_digit() || value == '_');
    macro_family && macro_style && contains_word(trimmed, member_name)
}

fn is_safe_macro_line_deletion(plan: &FilePlan, line_number: usize) -> bool {
    if line_number <= 1 {
        return true;
    }
    let lines = plan.original_lines();
    let Some(previous) = lines.get(line_number - 2) else {
        return false;
    };
    let Some(current) = lines.get(line_number - 1) else {
        return false;
    };
    !previous.trim_end().ends_with('\\') || current.trim_end().ends_with('\\')
}

fn contains_word(line: &str, word: &str) -> bool {
    line.match_indices(word).any(|(index, _)| {
        let before = line[..index].chars().next_back();
        let after = line[index + word.len()..].chars().next();
        !before.is_some_and(is_identifier_character) && !after.is_some_and(is_identifier_character)
    })
}

fn is_identifier_character(value: char) -> bool {
    value.is_ascii_alphanumeric() || value == '_'
}

fn line_number_at(text: &str, index: usize) -> usize {
    1 + text.as_bytes()[..index]
        .iter()
        .filter(|value| **value == b'\n')
        .count()
}

fn resource_arrays_equal(left: &[ResourceEntry], right: &[ResourceEntry]) -> bool {
    left == right
}

fn same_resource_arrays(left: &LayoutRecord, right: &LayoutRecord) -> bool {
    left.arrays.len() == right.arrays.len()
        && left.arrays.iter().all(|(name, entries)| {
            right
                .arrays
                .get(name)
                .is_some_and(|other| resource_arrays_equal(entries, other))
        })
}

fn declaration_location(path: &Path, line: usize) -> String {
    format!(
        "{}|{line}",
        path.canonicalize()
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .to_ascii_lowercase()
    )
}

fn source_files(root: &Path) -> Result<Vec<PathBuf>> {
    progress::step("Indexing engine source files...");
    let mut files = Vec::new();
    for entry in WalkDir::new(root).into_iter().filter_entry(|entry| {
        !(entry.file_type().is_dir()
            && [
                "Binaries",
                "Build",
                "Content",
                "DerivedDataCache",
                "Documentation",
                "Extras",
                "Intermediate",
                "Resources",
                "Saved",
                "Shaders",
                "ThirdParty",
            ]
            .iter()
            .any(|name| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(name)
            }))
    }) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let extension = entry
            .path()
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if ["h", "hpp", "cpp", "inl"]
            .iter()
            .any(|value| extension.eq_ignore_ascii_case(value))
        {
            files.push(entry.path().to_path_buf());
            if files.len() % 4096 == 0 {
                progress::transient(&format!("  found {} source files", files.len()));
            }
        }
    }
    files.sort_by(|left, right| {
        source_path_priority(root, left)
            .cmp(&source_path_priority(root, right))
            .then_with(|| left.cmp(right))
    });
    progress::step(&format!("  indexed {} source files", files.len()));
    Ok(files)
}

fn plugin_name_for_source(path: &Path, plugins_root: &Path) -> Option<String> {
    let mut directory = path.parent()?;
    while directory.starts_with(plugins_root) && directory != plugins_root {
        let mut descriptors = fs::read_dir(directory)
            .ok()?
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("uplugin"))
            });
        let descriptor = descriptors.next();
        if descriptors.next().is_some() {
            return None;
        }
        if let Some(descriptor) = descriptor {
            return descriptor
                .path()
                .file_stem()
                .map(|name| name.to_string_lossy().into_owned());
        }
        directory = directory.parent()?;
    }
    None
}

fn source_path_priority(root: &Path, path: &Path) -> u8 {
    let first = path
        .strip_prefix(root)
        .ok()
        .and_then(|relative| relative.components().next())
        .map(|component| component.as_os_str().to_string_lossy());
    match first.as_deref() {
        Some(name) if name.eq_ignore_ascii_case("Source") => 0,
        Some(name) if name.eq_ignore_ascii_case("Plugins") => 1,
        _ => 2,
    }
}

fn slash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn path_relative_slash(root: &Path, path: &Path) -> Result<String> {
    Ok(slash_path(path.strip_prefix(root)?))
}

pub fn normalize_engine_root(value: &Path) -> Result<PathBuf> {
    let root = value
        .canonicalize()
        .with_context(|| format!("engine root does not exist: {}", value.display()))?;
    if root.join("Engine").join("Source").is_dir() {
        return Ok(root);
    }
    if root.join("Source").is_dir()
        && root
            .file_name()
            .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("Engine"))
    {
        return root
            .parent()
            .map(Path::to_path_buf)
            .context("Engine directory has no parent");
    }
    if root
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("Source"))
        && root.parent().is_some_and(|parent| {
            parent
                .file_name()
                .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("Engine"))
        })
    {
        return root
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .context("Engine\\Source directory has no engine root");
    }
    bail!(
        "expected an Unreal Engine root, Engine directory, or Engine\\Source directory: {}",
        root.display()
    )
}

pub fn write_macro(member: &MemberRecord) -> Result<String> {
    if !IDENTIFIER.is_match(&member.name) {
        bail!("member name is not a C++ identifier: {}", member.name);
    }
    if matches!(
        member.base_type,
        ShaderParameterBaseType::Int32
            | ShaderParameterBaseType::Uint32
            | ShaderParameterBaseType::Float32
    ) {
        let numeric_type = numeric_type(member)?;
        let precision = precision(member.precision)?;
        if member.elements > 0 {
            return Ok(if precision.is_empty() {
                format!(
                    "SHADER_PARAMETER_ARRAY({numeric_type}, {}, [{}])",
                    member.name, member.elements
                )
            } else {
                format!(
                    "SHADER_PARAMETER_ARRAY_EX({numeric_type}, {}, [{}], {precision})",
                    member.name, member.elements
                )
            });
        }
        return Ok(if precision.is_empty() {
            format!("SHADER_PARAMETER({numeric_type}, {})", member.name)
        } else {
            format!(
                "SHADER_PARAMETER_EX({numeric_type}, {}, {precision})",
                member.name
            )
        });
    }

    let macro_name = match member.base_type {
        ShaderParameterBaseType::Texture => Some("SHADER_PARAMETER_TEXTURE"),
        ShaderParameterBaseType::Srv => Some("SHADER_PARAMETER_SRV"),
        ShaderParameterBaseType::Uav => Some("SHADER_PARAMETER_UAV"),
        ShaderParameterBaseType::Sampler => Some("SHADER_PARAMETER_SAMPLER"),
        ShaderParameterBaseType::RdgTexture => Some("SHADER_PARAMETER_RDG_TEXTURE"),
        ShaderParameterBaseType::RdgTextureSrv => Some("SHADER_PARAMETER_RDG_TEXTURE_SRV"),
        ShaderParameterBaseType::RdgTextureNonPixelSrv => {
            Some("SHADER_PARAMETER_RDG_TEXTURE_NON_PIXEL_SRV")
        }
        ShaderParameterBaseType::RdgTextureUav => Some("SHADER_PARAMETER_RDG_TEXTURE_UAV"),
        ShaderParameterBaseType::RdgBufferSrv => Some("SHADER_PARAMETER_RDG_BUFFER_SRV"),
        ShaderParameterBaseType::RdgBufferUav => Some("SHADER_PARAMETER_RDG_BUFFER_UAV"),
        _ => None,
    };
    if let Some(mut macro_name) = macro_name.map(str::to_string) {
        if member.shader_type.is_empty() {
            bail!("resource member has no shader type: {}", member.name);
        }
        if member.elements > 0 {
            if member.base_type == ShaderParameterBaseType::Uav {
                bail!("RHI UAV arrays have no public shader-parameter macro");
            }
            macro_name.push_str("_ARRAY");
            return Ok(format!(
                "{macro_name}({}, {}, [{}])",
                member.shader_type, member.name, member.elements
            ));
        }
        return Ok(format!(
            "{macro_name}({}, {})",
            member.shader_type, member.name
        ));
    }

    Ok(match member.base_type {
        ShaderParameterBaseType::RdgTextureAccess => {
            format!("RDG_TEXTURE_ACCESS_DYNAMIC({})", member.name)
        }
        ShaderParameterBaseType::RdgTextureAccessArray => {
            format!("RDG_TEXTURE_ACCESS_ARRAY({})", member.name)
        }
        ShaderParameterBaseType::RdgBufferAccess => {
            format!("RDG_BUFFER_ACCESS_DYNAMIC({})", member.name)
        }
        ShaderParameterBaseType::RdgBufferAccessArray => {
            format!("RDG_BUFFER_ACCESS_ARRAY({})", member.name)
        }
        ShaderParameterBaseType::RdgUniformBuffer => format!(
            "SHADER_PARAMETER_RDG_UNIFORM_BUFFER({}, {})",
            required_struct_type(member)?,
            member.name
        ),
        ShaderParameterBaseType::NestedStruct => {
            if member.elements > 0 {
                format!(
                    "SHADER_PARAMETER_STRUCT_ARRAY({}, {}, [{}])",
                    required_struct_type(member)?,
                    member.name,
                    member.elements
                )
            } else {
                format!(
                    "SHADER_PARAMETER_STRUCT({}, {})",
                    required_struct_type(member)?,
                    member.name
                )
            }
        }
        ShaderParameterBaseType::IncludedStruct => {
            if member.elements > 0 {
                bail!("included struct arrays are not supported");
            }
            format!(
                "SHADER_PARAMETER_STRUCT_INCLUDE({}, {})",
                required_struct_type(member)?,
                member.name
            )
        }
        ShaderParameterBaseType::ReferencedStruct => format!(
            "SHADER_PARAMETER_STRUCT_REF({}, {})",
            required_struct_type(member)?,
            member.name
        ),
        ShaderParameterBaseType::RenderTargetBindingSlots => {
            if member.name != "RenderTargets" {
                bail!("render-target member must be named RenderTargets");
            }
            "RENDER_TARGET_BINDING_SLOTS()".to_string()
        }
        _ => bail!(
            "no source macro mapping for base type {} on member {}",
            member.base_type.dump_name(),
            member.name
        ),
    })
}

fn required_struct_type(member: &MemberRecord) -> Result<&str> {
    if !member.struct_type.is_empty() {
        Ok(&member.struct_type)
    } else if !member.shader_type.is_empty() {
        Ok(&member.shader_type)
    } else {
        bail!("struct member has no type: {}", member.name)
    }
}

fn precision(value: u8) -> Result<&'static str> {
    match value {
        0 => Ok(""),
        1 => Ok("EShaderPrecisionModifier::Half"),
        2 => Ok("EShaderPrecisionModifier::Fixed"),
        _ => bail!("unknown precision modifier {value}"),
    }
}

fn numeric_type(member: &MemberRecord) -> Result<&'static str> {
    if member.rows == 1 {
        let value = match (member.base_type, member.columns) {
            (ShaderParameterBaseType::Int32, 1) => Some("int32"),
            (ShaderParameterBaseType::Int32, 2) => Some("FIntPoint"),
            (ShaderParameterBaseType::Int32, 3) => Some("FIntVector"),
            (ShaderParameterBaseType::Int32, 4) => Some("FIntVector4"),
            (ShaderParameterBaseType::Uint32, 1) => Some("uint32"),
            (ShaderParameterBaseType::Uint32, 2) => Some("FUintVector2"),
            (ShaderParameterBaseType::Uint32, 3) => Some("FUintVector3"),
            (ShaderParameterBaseType::Uint32, 4) => Some("FUintVector4"),
            (ShaderParameterBaseType::Float32, 1) => Some("float"),
            (ShaderParameterBaseType::Float32, 2) => Some("FVector2f"),
            (ShaderParameterBaseType::Float32, 3) => Some("FVector3f"),
            (ShaderParameterBaseType::Float32, 4) => Some("FVector4f"),
            _ => None,
        };
        if let Some(value) = value {
            return Ok(value);
        }
    }
    if member.base_type == ShaderParameterBaseType::Float32
        && member.rows == 3
        && member.columns == 4
    {
        return Ok("FMatrix3x4");
    }
    if member.base_type == ShaderParameterBaseType::Float32
        && member.rows == 4
        && member.columns == 4
    {
        return Ok("FMatrix44f");
    }
    bail!(
        "no canonical C++ type for {} r{}c{}",
        member.base_type.dump_name(),
        member.rows,
        member.columns
    )
}
