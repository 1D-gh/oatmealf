use super::*;

#[derive(Clone, Debug)]
pub(super) struct SiblingInfo {
    pub(super) name: String,
    pub(super) base_type: ShaderParameterBaseType,
    pub(super) shader_type: String,
}

#[derive(Clone, Debug)]
pub(super) struct PendingRuntimeMember {
    pub(super) container_type: String,
    pub(super) siblings: Vec<SiblingInfo>,
    pub(super) access_path: String,
    pub(super) member: MemberRecord,
}

#[derive(Clone, Debug)]
pub(super) struct AssignmentEntry {
    pub(super) line: usize,
    pub(super) indent: String,
    pub(super) receiver: String,
    pub(super) sibling: SiblingInfo,
    pub(super) expression: String,
    pub(super) function_id: usize,
    pub(super) function_header: String,
    pub(super) scope_path: Vec<usize>,
    pub(super) conditional_depth: usize,
    pub(super) graph_builder: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) struct BindingSite {
    pub(super) path: PathBuf,
    pub(super) receiver: String,
    pub(super) anchor_line: usize,
    pub(super) anchor_indent: String,
    pub(super) entries: Vec<AssignmentEntry>,
    pub(super) function_id: usize,
    pub(super) scope_path: Vec<usize>,
    pub(super) conditional_depth: usize,
    pub(super) graph_builder: Option<String>,
    pub(super) is_constructor: bool,
}

#[derive(Clone, Debug)]
pub(super) struct TypeMention {
    pub(super) line: usize,
    pub(super) text: String,
    pub(super) function_id: Option<usize>,
    pub(super) scope_path: Vec<usize>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct CppLineContext {
    pub(super) function_id: Option<usize>,
    pub(super) function_header: String,
    pub(super) scope_path: Vec<usize>,
    pub(super) conditional_depth: usize,
    pub(super) graph_builder: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) enum CppScopeKind {
    Function {
        header: String,
        graph_builder: Option<String>,
    },
    Conditional,
    Other,
}

#[derive(Clone, Debug)]
pub(super) struct CppScope {
    pub(super) id: usize,
    pub(super) kind: CppScopeKind,
}

impl<'a> SourcePlanner<'a> {
    pub(super) fn push_pending_runtime_member(
        &mut self,
        container: &DefinitionRecord,
        access_path: &str,
        member: &MemberRecord,
    ) {
        if self.pending_runtime_members.iter().any(|entry| {
            entry.container_type == container.type_name && entry.access_path == access_path
        }) {
            return;
        }
        let siblings = container
            .members
            .iter()
            .filter(|sibling| !sibling.name.is_empty() && !is_generated_placeholder(&sibling.name))
            .map(|sibling| SiblingInfo {
                name: sibling.name.clone(),
                base_type: sibling.base_type,
                shader_type: sibling.shader_type.clone(),
            })
            .collect();
        self.pending_runtime_members.push(PendingRuntimeMember {
            container_type: container.type_name.clone(),
            siblings,
            access_path: access_path.to_string(),
            member: member.clone(),
        });
    }

    pub(super) fn push_unbound_note(&mut self, entry: &PendingRuntimeMember) {
        let issue = format!(
            "{}::{} needs a renderer assignment because no safe placeholder binding was found",
            entry.container_type, entry.access_path
        );
        if !self.review.contains(&issue) {
            self.review.push(issue);
        }
    }

    pub fn plan_runtime_assignments(&mut self) -> Result<()> {
        const MIN_SIBLING_ANCHORS: usize = 2;
        const MAX_CLUSTER_GAP: usize = 50;

        let pending = std::mem::take(&mut self.pending_runtime_members);
        if pending.is_empty() {
            return Ok(());
        }
        if self.emit_blocked {
            for entry in &pending {
                self.push_unbound_note(entry);
            }
            return Ok(());
        }

        let mut groups: BTreeMap<String, Vec<PendingRuntimeMember>> = BTreeMap::new();
        for entry in pending {
            groups
                .entry(entry.container_type.clone())
                .or_default()
                .push(entry);
        }
        let mut sibling_owners: HashMap<String, Vec<String>> = HashMap::new();
        let mut sibling_meta: HashMap<String, HashMap<String, SiblingInfo>> = HashMap::new();
        for (container, members) in &groups {
            let meta = sibling_meta.entry(container.clone()).or_default();
            for sibling in &members[0].siblings {
                let owners = sibling_owners.entry(sibling.name.clone()).or_default();
                if !owners.contains(container) {
                    owners.push(container.clone());
                }
                meta.entry(sibling.name.clone())
                    .or_insert_with(|| sibling.clone());
            }
        }
        if sibling_owners.is_empty() {
            for members in groups.values() {
                for entry in members {
                    self.push_unbound_note(entry);
                }
            }
            return Ok(());
        }
        let mut names: Vec<&String> = sibling_owners.keys().collect();
        names.sort();
        let pattern = Regex::new(&format!(
            r"\b(?:{})\b",
            names
                .iter()
                .map(|value| regex::escape(value))
                .collect::<Vec<_>>()
                .join("|")
        ))?;

        // scan once so receiver types and sibling writes stay in the same scope
        let files = self.source_files()?.to_vec();
        progress::step("Binding placeholder resources...");
        let total = files.len();
        let mut matches: BTreeMap<String, BTreeMap<PathBuf, Vec<AssignmentEntry>>> =
            BTreeMap::new();
        let mut type_mentions: HashMap<(String, PathBuf), Vec<TypeMention>> = HashMap::new();
        let mut function_returns: HashMap<(PathBuf, usize), HashSet<String>> = HashMap::new();
        let mut source_texts: HashMap<PathBuf, String> = HashMap::new();
        for (file_index, path) in files.iter().enumerate() {
            cancel::check()?;
            if (file_index + 1) % 2048 == 0 {
                progress::transient(&format!(
                    "  scanned {} of {total} files for binding sites",
                    file_index + 1
                ));
            }
            let Ok(bytes) = fs::read(path) else { continue };
            let Ok(text) = std::str::from_utf8(&bytes) else {
                continue;
            };
            if !pattern.is_match(text) {
                continue;
            }
            let contexts = cpp_line_contexts(text);
            for (index, line) in LINE_SPLIT.split(text).enumerate() {
                let context = contexts.get(index).cloned().unwrap_or_default();
                for container in groups.keys() {
                    if contains_word(line, container) {
                        type_mentions
                            .entry((container.clone(), path.clone()))
                            .or_default()
                            .push(TypeMention {
                                line: index + 1,
                                text: line.to_string(),
                                function_id: context.function_id,
                                scope_path: context.scope_path.clone(),
                            });
                    }
                }
                if let (Some(function_id), Some(returned)) =
                    (context.function_id, returned_identifier(line))
                {
                    function_returns
                        .entry((path.clone(), function_id))
                        .or_default()
                        .insert(returned.to_string());
                }
            }
            for (index, line) in LINE_SPLIT.split(text).enumerate() {
                let Some((indent, receiver, name, expression)) =
                    parse_sibling_assignment(line, &pattern)
                else {
                    continue;
                };
                let Some(context) = contexts.get(index) else {
                    continue;
                };
                let Some(function_id) = context.function_id else {
                    continue;
                };
                let Some(owners) = sibling_owners.get(name) else {
                    continue;
                };
                for container in owners {
                    let sibling = sibling_meta[container][name].clone();
                    matches
                        .entry(container.clone())
                        .or_default()
                        .entry(path.clone())
                        .or_default()
                        .push(AssignmentEntry {
                            line: index + 1,
                            indent: indent.to_string(),
                            receiver: receiver.to_string(),
                            sibling,
                            expression: expression.to_string(),
                            function_id,
                            function_header: context.function_header.clone(),
                            scope_path: context.scope_path.clone(),
                            conditional_depth: context.conditional_depth,
                            graph_builder: context.graph_builder.clone(),
                        });
                }
            }
            source_texts.insert(path.clone(), text.to_string());
        }
        progress::finish_transient();

        let mut sites_by_container: BTreeMap<String, Vec<BindingSite>> = BTreeMap::new();
        for (container, by_path) in &matches {
            for (path, entries) in by_path {
                let mut by_receiver_and_scope: BTreeMap<
                    (String, usize, Vec<usize>),
                    Vec<&AssignmentEntry>,
                > = BTreeMap::new();
                for entry in entries {
                    by_receiver_and_scope
                        .entry((
                            entry.receiver.clone(),
                            entry.function_id,
                            entry.scope_path.clone(),
                        ))
                        .or_default()
                        .push(entry);
                }
                for ((receiver, function_id, scope_path), mut group) in by_receiver_and_scope {
                    group.sort_by_key(|entry| entry.line);
                    let mut start = 0;
                    for index in 0..=group.len() {
                        let split = index == group.len()
                            || (index > start
                                && group[index].line - group[index - 1].line > MAX_CLUSTER_GAP);
                        if !split {
                            continue;
                        }
                        let cluster = &group[start..index];
                        start = index;
                        let distinct: HashSet<&str> = cluster
                            .iter()
                            .map(|entry| entry.sibling.name.as_str())
                            .collect();
                        let context = cluster[0];
                        let receiver_head = receiver
                            .split(['.', '['])
                            .next()
                            .unwrap_or_default()
                            .to_string();
                        // bare writes need a constructor, named receivers need a matching type
                        let is_constructor = receiver_head.is_empty()
                            && is_constructor_header(&context.function_header, container);
                        let receiver_is_declared = if is_constructor {
                            true
                        } else if receiver_head.is_empty() {
                            false
                        } else {
                            let declaration = Regex::new(&format!(
                                r"\b{}\b[\s&*]*\b{}\b",
                                regex::escape(container),
                                regex::escape(&receiver_head)
                            ))?;
                            declaration.is_match(&context.function_header)
                                || type_mentions
                                    .get(&(container.clone(), path.clone()))
                                    .is_some_and(|mentions| {
                                        mentions.iter().any(|mention| {
                                            mention.function_id == Some(function_id)
                                                && mention.line <= context.line
                                                && scope_is_prefix(&mention.scope_path, &scope_path)
                                                && declaration.is_match(&mention.text)
                                        })
                                    })
                        };
                        if !receiver_is_declared {
                            continue;
                        }
                        let is_output_parameter = !receiver_head.is_empty()
                            && is_output_parameter(
                                &context.function_header,
                                container,
                                &receiver_head,
                            )?;
                        let is_returned = function_returns
                            .get(&(path.clone(), function_id))
                            .is_some_and(|values| values.contains(&receiver_head));
                        if !is_constructor && !is_output_parameter && !is_returned {
                            continue;
                        }
                        // returned locals need one sibling, output parameters need two
                        if distinct.len() < MIN_SIBLING_ANCHORS && !is_returned {
                            continue;
                        }
                        let anchor = cluster.last().expect("cluster is not empty");
                        sites_by_container
                            .entry(container.clone())
                            .or_default()
                            .push(BindingSite {
                                path: path.clone(),
                                receiver: receiver.to_string(),
                                anchor_line: anchor.line,
                                anchor_indent: anchor.indent.clone(),
                                entries: cluster.iter().map(|entry| (*entry).clone()).collect(),
                                function_id,
                                scope_path: scope_path.clone(),
                                conditional_depth: context.conditional_depth,
                                graph_builder: context.graph_builder.clone(),
                                is_constructor,
                            });
                    }
                }
            }
        }

        // prefer one constructor or one complete site per producer function
        for (container, members) in groups {
            let candidates = sites_by_container.remove(&container).unwrap_or_default();
            let sites = select_binding_sites(&members, candidates, &source_texts);
            if sites.is_empty() {
                for entry in &members {
                    self.push_unbound_note(entry);
                }
                continue;
            }
            for entry in &members {
                let mut bound = false;
                for site in &sites {
                    let Some(source) = source_texts.get(&site.path) else {
                        continue;
                    };
                    let Some(expression) =
                        runtime_placeholder_expression(site, &entry.member, source)
                    else {
                        continue;
                    };
                    let key = self.ensure_file(&site.path)?;
                    let insert_line = site.anchor_line + 1;
                    let statement = format!(
                        "{}{}{} = {}; // generated placeholder; game runtime data is unavailable in editor",
                        site.anchor_indent,
                        if site.receiver.is_empty() {
                            String::new()
                        } else {
                            format!("{}.", site.receiver)
                        },
                        entry.access_path,
                        expression
                    );
                    self.files
                        .get_mut(&key)
                        .expect("file plan exists")
                        .add_before(insert_line, vec![statement]);
                    self.planned_blocks
                        .entry((key.clone(), site.anchor_line, insert_line))
                        .or_insert_with(|| format!("placeholder bindings for {container}"));
                    let relative = path_relative_slash(&self.engine_top, &key)?;
                    self.runtime_bindings.push(format!(
                        "`{}::{}` = `{}` in `{}:{}`",
                        container, entry.access_path, expression, relative, insert_line
                    ));
                    bound = true;
                }
                if !bound {
                    self.push_unbound_note(entry);
                }
            }
        }
        Ok(())
    }
}

pub(super) fn cpp_line_contexts(text: &str) -> Vec<CppLineContext> {
    let lines: Vec<&str> = LINE_SPLIT.split(text).collect();
    let mut contexts = Vec::with_capacity(lines.len());
    let mut scopes: Vec<CppScope> = Vec::new();
    let mut next_scope_id = 1;
    let mut pending_header = String::new();
    let mut parenthesis_depth = 0usize;
    let mut in_block_comment = false;

    for line in lines {
        contexts.push(cpp_context_from_scopes(&scopes));

        let code = sanitize_cpp_line(line, &mut in_block_comment);
        if code.trim_start().starts_with('#') && parenthesis_depth == 0 {
            pending_header.clear();
            continue;
        }

        for value in code.chars() {
            match value {
                '(' => {
                    parenthesis_depth = parenthesis_depth.saturating_add(1);
                    pending_header.push(value);
                }
                ')' => {
                    parenthesis_depth = parenthesis_depth.saturating_sub(1);
                    pending_header.push(value);
                }
                ';' if parenthesis_depth == 0 => pending_header.clear(),
                '{' if parenthesis_depth == 0 => {
                    let header = normalize_cpp_header(&pending_header);
                    let kind = classify_cpp_scope(&header, &scopes);
                    scopes.push(CppScope {
                        id: next_scope_id,
                        kind,
                    });
                    next_scope_id += 1;
                    pending_header.clear();
                }
                '}' if parenthesis_depth == 0 => {
                    scopes.pop();
                    pending_header.clear();
                }
                _ => pending_header.push(value),
            }
        }
        if !pending_header.is_empty() {
            pending_header.push(' ');
        }
    }
    contexts
}

pub(super) fn cpp_context_from_scopes(scopes: &[CppScope]) -> CppLineContext {
    let Some(function_index) = scopes
        .iter()
        .rposition(|scope| matches!(scope.kind, CppScopeKind::Function { .. }))
    else {
        return CppLineContext::default();
    };
    let CppScopeKind::Function {
        header,
        graph_builder,
    } = &scopes[function_index].kind
    else {
        unreachable!();
    };
    CppLineContext {
        function_id: Some(scopes[function_index].id),
        function_header: header.clone(),
        scope_path: scopes[function_index..]
            .iter()
            .map(|scope| scope.id)
            .collect(),
        conditional_depth: scopes[function_index + 1..]
            .iter()
            .filter(|scope| matches!(scope.kind, CppScopeKind::Conditional))
            .count(),
        graph_builder: graph_builder.clone(),
    }
}

pub(super) fn classify_cpp_scope(header: &str, scopes: &[CppScope]) -> CppScopeKind {
    let inside_function = scopes
        .iter()
        .any(|scope| matches!(scope.kind, CppScopeKind::Function { .. }));
    if inside_function && is_cpp_conditional_header(header) {
        return CppScopeKind::Conditional;
    }
    if !inside_function && looks_like_cpp_function_header(header) {
        return CppScopeKind::Function {
            header: header.to_string(),
            graph_builder: cpp_graph_builder_name(header),
        };
    }
    CppScopeKind::Other
}

pub(super) fn is_cpp_conditional_header(header: &str) -> bool {
    let header = header.trim_start();
    ["if", "else", "for", "while", "switch", "do", "try", "catch"]
        .iter()
        .any(|keyword| {
            header == *keyword
                || header.strip_prefix(keyword).is_some_and(|rest| {
                    rest.starts_with(|value: char| value.is_ascii_whitespace() || value == '(')
                })
        })
}

pub(super) fn looks_like_cpp_function_header(header: &str) -> bool {
    let header = header.trim();
    if !header.contains('(') || !header.contains(')') {
        return false;
    }
    ![
        "class ",
        "struct ",
        "enum ",
        "namespace ",
        "if ",
        "if(",
        "for ",
        "for(",
        "while ",
        "while(",
        "switch ",
        "switch(",
        "catch ",
        "catch(",
    ]
    .iter()
    .any(|prefix| header.starts_with(prefix))
}

pub(super) fn cpp_graph_builder_name(header: &str) -> Option<String> {
    static GRAPH_BUILDER: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\b(?:const\s+)?FRDGBuilder\s*&\s*([A-Za-z_][A-Za-z0-9_]*)")
            .expect("valid graph builder regex")
    });
    GRAPH_BUILDER
        .captures(header)
        .map(|captures| captures[1].to_string())
}

pub(super) fn normalize_cpp_header(header: &str) -> String {
    header.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn sanitize_cpp_line(line: &str, in_block_comment: &mut bool) -> String {
    let values: Vec<char> = line.chars().collect();
    let mut output = String::with_capacity(values.len());
    let mut index = 0;
    let mut quote = None;
    let mut escaped = false;
    while index < values.len() {
        let value = values[index];
        let next = values.get(index + 1).copied();
        if *in_block_comment {
            output.push(' ');
            if value == '*' && next == Some('/') {
                output.push(' ');
                index += 2;
                *in_block_comment = false;
            } else {
                index += 1;
            }
            continue;
        }
        if let Some(delimiter) = quote {
            output.push(' ');
            if escaped {
                escaped = false;
            } else if value == '\\' {
                escaped = true;
            } else if value == delimiter {
                quote = None;
            }
            index += 1;
            continue;
        }
        if value == '/' && next == Some('/') {
            output.extend(std::iter::repeat_n(' ', values.len() - index));
            break;
        }
        if value == '/' && next == Some('*') {
            output.push(' ');
            output.push(' ');
            index += 2;
            *in_block_comment = true;
            continue;
        }
        if value == '"' || value == '\'' {
            quote = Some(value);
            output.push(' ');
            index += 1;
            continue;
        }
        output.push(value);
        index += 1;
    }
    output
}

pub(super) fn scope_is_prefix(prefix: &[usize], value: &[usize]) -> bool {
    prefix.len() <= value.len() && prefix.iter().zip(value).all(|(left, right)| left == right)
}

pub(super) fn returned_identifier(line: &str) -> Option<&str> {
    static RETURN_VALUE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\s*return\s+([A-Za-z_][A-Za-z0-9_]*)\s*;")
            .expect("valid return statement regex")
    });
    RETURN_VALUE
        .captures(line)
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str())
}

pub(super) fn is_constructor_header(header: &str, container: &str) -> bool {
    Regex::new(&format!(
        r"\b{}::{}\s*\(",
        regex::escape(container),
        regex::escape(container)
    ))
    .expect("escaped constructor regex is valid")
    .is_match(header)
}

pub(super) fn is_output_parameter(header: &str, container: &str, receiver: &str) -> Result<bool> {
    Ok(Regex::new(&format!(
        r"\b{}\b\s*[&*]+\s*\b{}\b",
        regex::escape(container),
        regex::escape(receiver)
    ))?
    .is_match(header))
}

pub(super) fn parse_sibling_assignment<'a>(
    line: &'a str,
    pattern: &Regex,
) -> Option<(&'a str, &'a str, &'a str, &'a str)> {
    let trimmed_start = line.trim_start();
    if trimmed_start.starts_with("//") || trimmed_start.starts_with('*') {
        return None;
    }
    if !line.trim_end().ends_with(';') {
        return None;
    }
    let found = pattern.find(line)?;
    let after = line[found.end()..].trim_start();
    let mut operator = after.chars();
    if operator.next() != Some('=') || operator.next() == Some('=') {
        return None;
    }
    let expression = after[1..].trim().trim_end_matches(';').trim_end();
    if expression.is_empty() || expression.contains(';') {
        return None;
    }
    let indent = &line[..line.len() - trimmed_start.len()];
    let before = &line[..found.start()];
    let receiver = if let Some(receiver_part) = before.strip_suffix('.') {
        let receiver_start = receiver_part
            .rfind(|value: char| {
                !(value.is_ascii_alphanumeric()
                    || value == '_'
                    || value == '.'
                    || value == '['
                    || value == ']')
            })
            .map(|position| position + 1)
            .unwrap_or(0);
        let receiver = &receiver_part[receiver_start..];
        let starts_like_identifier = receiver
            .chars()
            .next()
            .is_some_and(|value| value.is_ascii_alphabetic() || value == '_');
        if !starts_like_identifier {
            return None;
        }
        receiver
    } else if before.trim().is_empty() {
        ""
    } else {
        return None;
    };
    Some((indent, receiver, found.as_str(), expression))
}

pub(super) fn clone_placeholder_expression(
    site: &BindingSite,
    member: &MemberRecord,
) -> Option<String> {
    for entry in &site.entries {
        if entry.sibling.base_type == member.base_type
            && entry.sibling.shader_type == member.shader_type
            && is_placeholder_expression(&entry.expression)
        {
            return Some(entry.expression.clone());
        }
    }
    None
}

pub(super) fn runtime_placeholder_expression(
    site: &BindingSite,
    member: &MemberRecord,
    source: &str,
) -> Option<String> {
    clone_placeholder_expression(site, member)
        .or_else(|| placeholder_table_expression(site, member, source))
}

pub(super) fn select_binding_sites(
    members: &[PendingRuntimeMember],
    candidates: Vec<BindingSite>,
    source_texts: &HashMap<PathBuf, String>,
) -> Vec<BindingSite> {
    let mut complete: Vec<BindingSite> = candidates
        .into_iter()
        .filter(|site| {
            source_texts.get(&site.path).is_some_and(|source| {
                members.iter().all(|entry| {
                    runtime_placeholder_expression(site, &entry.member, source).is_some()
                })
            })
        })
        .collect();
    if complete.is_empty() {
        return Vec::new();
    }

    let mut constructors: Vec<BindingSite> = complete
        .iter()
        .filter(|site| site.is_constructor)
        .cloned()
        .collect();
    if !constructors.is_empty() {
        constructors.sort_by(compare_binding_sites);
        return vec![constructors.remove(0)];
    }

    let mut selected: BTreeMap<(PathBuf, usize, String), BindingSite> = BTreeMap::new();
    for site in complete.drain(..) {
        let key = (site.path.clone(), site.function_id, site.receiver.clone());
        match selected.entry(key) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(site);
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                if compare_binding_sites(&site, entry.get()).is_lt() {
                    entry.insert(site);
                }
            }
        }
    }
    selected.into_values().collect()
}

pub(super) fn compare_binding_sites(left: &BindingSite, right: &BindingSite) -> std::cmp::Ordering {
    let left_members = left
        .entries
        .iter()
        .map(|entry| entry.sibling.name.as_str())
        .collect::<HashSet<_>>()
        .len();
    let right_members = right
        .entries
        .iter()
        .map(|entry| entry.sibling.name.as_str())
        .collect::<HashSet<_>>()
        .len();
    left.conditional_depth
        .cmp(&right.conditional_depth)
        .then_with(|| left.scope_path.len().cmp(&right.scope_path.len()))
        .then_with(|| right_members.cmp(&left_members))
        .then_with(|| left.path.cmp(&right.path))
        .then_with(|| left.anchor_line.cmp(&right.anchor_line))
}

pub(super) fn is_placeholder_expression(expression: &str) -> bool {
    [
        "Dummy",
        "Black",
        "White",
        "Default",
        "TStaticSamplerState",
        "SystemTextures",
        "Identity",
        "Empty",
        "Null",
        "Zero",
    ]
    .iter()
    .any(|marker| expression.contains(marker))
}

pub(super) fn placeholder_table_expression(
    site: &BindingSite,
    member: &MemberRecord,
    source: &str,
) -> Option<String> {
    match member.base_type {
        ShaderParameterBaseType::Sampler if source.contains("TStaticSamplerState") => Some(
            "TStaticSamplerState<SF_Point, AM_Clamp, AM_Clamp, AM_Clamp>::GetRHI()".to_string(),
        ),
        ShaderParameterBaseType::Texture => {
            let shader_type = member.shader_type.as_str();
            let global = if shader_type.starts_with("Texture2DArray") {
                "GBlackArrayTexture"
            } else if shader_type.starts_with("TextureCubeArray") {
                "GBlackCubeArrayTexture"
            } else if shader_type.starts_with("TextureCube") {
                "GBlackTextureCube"
            } else if shader_type.starts_with("Texture3D") {
                "GBlackVolumeTexture"
            } else if shader_type.starts_with("Texture2D") {
                "GBlackTexture"
            } else {
                return None;
            };
            source
                .contains(global)
                .then(|| format!("{global}->TextureRHI"))
        }
        ShaderParameterBaseType::Srv => {
            let globals: &[&str] = if member.shader_type == "StructuredBuffer<float4>" {
                &[
                    "GBlackFloat4StructuredBufferWithSRV",
                    "GWhiteVertexBufferWithSRV",
                ]
            } else {
                &["GWhiteVertexBufferWithSRV"]
            };
            globals
                .iter()
                .find(|global| source.contains(**global))
                .map(|global| format!("{global}->ShaderResourceViewRHI"))
        }
        ShaderParameterBaseType::RdgTexture => {
            let graph_builder = site.graph_builder.as_deref()?;
            if !source.contains("FRDGSystemTextures::Get") {
                return None;
            }
            let texture = if member.shader_type.starts_with("Texture3D") {
                "VolumetricBlackAlphaOne"
            } else if member.shader_type.starts_with("Texture2D") {
                "Black"
            } else {
                return None;
            };
            Some(format!(
                "FRDGSystemTextures::Get({graph_builder}).{texture}"
            ))
        }
        ShaderParameterBaseType::RdgTextureSrv => {
            let graph_builder = site.graph_builder.as_deref()?;
            if !member.shader_type.starts_with("Texture2D")
                || !source.contains("FRDGSystemTextures::Get")
                || !source.contains("StencilDummySRV")
            {
                return None;
            }
            Some(format!(
                "FRDGSystemTextures::Get({graph_builder}).StencilDummySRV"
            ))
        }
        _ => None,
    }
}
