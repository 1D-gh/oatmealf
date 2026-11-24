use crate::cancel;
use crate::dump;
use crate::model::{
    DefinitionRecord, LayoutDump, LayoutRecord, MAX_DEFINITION_DEPTH, MemberRecord,
    RESOURCE_ARRAY_NAMES, ResourceEntry, ShaderParameterBaseType,
};
use crate::process::{MemoryRegion, ProcessInfo, ProcessMemory};
use crate::progress;
use anyhow::{Context, Result, bail};
use chrono::Local;
use memchr::memmem::Finder;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

const ANCHOR_SHADER_NAME: &str = "View";
const ANCHOR_TYPE_NAME: &str = "FViewUniformShaderParameters";

const POINTER_SIZE: usize = 8;
const ARRAY_DATA: usize = 0x00;
const ARRAY_COUNT: usize = 0x08;
const ARRAY_CAPACITY: usize = 0x0c;
const ARRAY_SIZE: usize = 0x10;

const METADATA_LAYOUT_NAME_POINTER: usize = 0x08;
const METADATA_STRUCT_TYPE_NAME_POINTER: usize = 0x10;
const METADATA_SHADER_VARIABLE_NAME_POINTER: usize = 0x18;
const METADATA_HEADER_SIZE: usize = 0xb0;
const METADATA_FIELD_SCAN_START: usize = 0x40;
const METADATA_LIST_NODE_SCAN_START: usize = 0x18;
const METADATA_LIST_NODE_SCAN_END: usize = 0x300;
const METADATA_FILE_NAME_POINTERS: [usize; 2] = [0x30, 0x38];

const LIST_NEXT_LINK: u64 = 0x00;
const LIST_PREVIOUS_LINK: u64 = 0x08;
const LIST_ELEMENT: u64 = 0x10;
const METADATA_LAYOUT_HASH_FROM_LIST_NODE: i64 = 0x18;

const MEMBER_SIZE: usize = 0x30;
const MEMBER_NAME_POINTER: usize = 0x00;
const MEMBER_SHADER_TYPE_POINTER: usize = 0x08;
const MEMBER_SOURCE_LINE: usize = 0x10;
const MEMBER_BYTE_OFFSET: usize = 0x14;
const MEMBER_BASE_TYPE: usize = 0x18;
const MEMBER_PRECISION: usize = 0x19;
const MEMBER_ROWS: usize = 0x1c;
const MEMBER_COLUMNS: usize = 0x20;
const MEMBER_ELEMENTS: usize = 0x24;
const MEMBER_NESTED_METADATA: usize = 0x28;

const RHI_LAYOUT_READ_SIZE: usize = 0x120;
const RHI_NAME_PROBE_SIZE: usize = 0x60;
const RHI_NAME_SEARCH_SIZE: usize = 0x50;
const RHI_RESOURCE_ARRAYS_FROM_NAME: usize = 0x10;
const RHI_RESOURCE_ARRAY_STRIDE: usize = 0x10;
const RHI_HASH_FROM_NAME: usize = 0x70;
const RHI_CONSTANT_BUFFER_SIZE_FROM_NAME: usize = 0x74;
const RHI_RENDER_TARGETS_OFFSET_FROM_NAME: usize = 0x78;
const RHI_STATIC_SLOT_FROM_NAME: usize = 0x7a;
const RHI_BINDING_FLAGS_FROM_NAME: usize = 0x7b;
const RHI_FLAGS_FROM_NAME: usize = 0x7c;
const RHI_RESOURCE_ENTRY_SIZE: usize = 4;

#[derive(Clone, Debug)]
struct MetadataSnapshot {
    layout_name: String,
    struct_type_name: String,
    shader_variable_name: String,
    source_file: String,
    source_line: i32,
    size: u32,
    use_case: u8,
    binding_flags: u8,
    usage_flags: u8,
    layout: u64,
    members: Vec<u8>,
    member_count: usize,
}

struct Capturer {
    memory: ProcessMemory,
    regions: Vec<MemoryRegion>,
    global_list_node_offset: i64,
    definition_ids: HashMap<u64, u32>,
    definitions: HashMap<u64, MetadataSnapshot>,
    definition_order: Vec<u64>,
}

pub fn run(process: &ProcessInfo, output_path: &Path) -> Result<usize> {
    run_with_verbosity(process, output_path, false)
}

pub fn run_with_verbosity(
    process: &ProcessInfo,
    output_path: &Path,
    verbose: bool,
) -> Result<usize> {
    if usize::BITS != 64 {
        bail!("run oatmealf as a 64-bit process");
    }
    let memory = ProcessMemory::open(process.pid)?;
    if verbose {
        println!("Attached {} ({})", process.process_name(), process.pid);
    }
    run_with_memory(
        memory,
        &process.process_name(),
        process.pid,
        output_path,
        verbose,
    )
}

/// Capture the uniform-buffer registry from a Windows minidump instead of a live process.
pub fn run_from_dump(dump_path: &Path, output_path: &Path, verbose: bool) -> Result<usize> {
    let dump = crate::minidump::DumpMemory::open(dump_path)?;
    let name = dump
        .main_module()
        .map(|module| {
            Path::new(&module.name.replace('\\', "/"))
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or_else(|| "minidump".to_string());
    if verbose {
        println!(
            "Opened minidump {}: {} regions, {:.2} GiB, {} modules",
            dump_path.display(),
            dump.region_count(),
            dump.total_memory() as f64 / (1u64 << 30) as f64,
            dump.modules.len()
        );
        if let Some(module) = dump.main_module() {
            println!(
                "Main module {} base 0x{:X} size 0x{:X} file version {}.{}.{}.{}",
                module.name,
                module.base,
                module.size,
                module.file_version.0,
                module.file_version.1,
                module.file_version.2,
                module.file_version.3
            );
        }
    }
    let process_id = dump.process_id.max(1);
    run_with_memory(
        ProcessMemory::Dump(dump),
        &name,
        process_id,
        output_path,
        verbose,
    )
}

fn run_with_memory(
    memory: ProcessMemory,
    process_name: &str,
    process_id: u32,
    output_path: &Path,
    verbose: bool,
) -> Result<usize> {
    let mut capturer = Capturer {
        regions: memory.readable_regions(),
        memory,
        global_list_node_offset: 0,
        definition_ids: HashMap::new(),
        definitions: HashMap::new(),
        definition_order: Vec::new(),
    };
    let anchor_metadata = capturer.find_anchor_metadata();
    cancel::check()?;
    if anchor_metadata == 0 {
        bail!("could not find the View uniform-buffer metadata anchor");
    }
    let anchor_node = capturer.find_list_node(anchor_metadata);
    cancel::check()?;
    if anchor_node == 0 {
        bail!("could not locate the uniform-buffer global-list node");
    }
    capturer.global_list_node_offset = anchor_node as i64 - anchor_metadata as i64;
    let list_head = capturer.find_list_head(anchor_node)?;
    let metadata_objects = capturer.walk_list(list_head)?;
    if verbose {
        println!(
            "Found {} registered uniform buffers",
            metadata_objects.len()
        );
    }

    let mut layouts = BTreeMap::new();
    for (index, metadata) in metadata_objects.iter().enumerate() {
        cancel::check()?;
        progress::transient(&format!(
            "  reading layout {} of {}",
            index + 1,
            metadata_objects.len()
        ));
        let layout = capturer.capture_one(*metadata)?;
        let name = layout.name.clone();
        if layouts.insert(name.clone(), layout).is_some() {
            bail!("duplicate registered uniform buffer '{name}'");
        }
    }
    progress::finish_transient();
    let definitions = capturer.capture_definitions()?;
    let captured_at = Local::now().to_rfc3339();
    let capture = LayoutDump {
        path: output_path.to_path_buf(),
        layouts,
        definitions,
    };
    dump::write(
        output_path,
        process_name,
        process_id,
        &captured_at,
        &capture,
    )?;
    progress::step(&format!(
        "  captured {} layouts and {} definitions",
        metadata_objects.len(),
        capturer.definition_order.len()
    ));
    if verbose {
        println!(
            "Captured {} uniform buffers and {} metadata definitions",
            metadata_objects.len(),
            capturer.definition_order.len()
        );
    }
    Ok(metadata_objects.len())
}

impl Capturer {
    fn read_wide_string(&self, address: u64, max_length: usize) -> Option<String> {
        if address < 0x1_0000 {
            return None;
        }
        let bytes = self.memory.read_bytes(address, max_length * 2)?;
        let mut value = String::new();
        for pair in bytes.chunks_exact(2) {
            let character = u16::from_le_bytes([pair[0], pair[1]]);
            if character == 0 {
                return Some(value);
            }
            if !(0x20..=0x7e).contains(&character) {
                return None;
            }
            value.push(char::from_u32(u32::from(character))?);
        }
        None
    }

    fn read_ansi_string(&self, address: u64, max_length: usize) -> Option<String> {
        if address < 0x1_0000 {
            return None;
        }
        let bytes = self.memory.read_bytes(address, max_length)?;
        let length = bytes.iter().position(|value| *value == 0)?;
        if bytes[..length]
            .iter()
            .any(|value| !(0x20..=0x7e).contains(value))
        {
            return None;
        }
        String::from_utf8(bytes[..length].to_vec()).ok()
    }

    fn scan_total(&self, image_only: bool) -> u64 {
        self.regions
            .iter()
            .filter(|region| !image_only || region.image)
            .map(|region| region.size)
            .sum()
    }

    fn find_wide_strings(&self, value: &str, image_only: bool, label: &str) -> HashSet<u64> {
        let mut pattern = Vec::with_capacity((value.len() + 1) * 2);
        for character in value.encode_utf16().chain(std::iter::once(0)) {
            pattern.extend_from_slice(&character.to_le_bytes());
        }
        let finder = Finder::new(&pattern);
        let mut hits = HashSet::new();
        const CHUNK_SIZE: u64 = 8 << 20;
        // overlap reads so split UTF-16 anchors are still found
        const OVERLAP: u64 = 256;
        let total = self.scan_total(image_only);
        let mut scanned = 0u64;
        for region in &self.regions {
            if cancel::is_cancelled() {
                return hits;
            }
            if image_only && !region.image {
                continue;
            }
            let mut offset = 0u64;
            while offset < region.size {
                if cancel::is_cancelled() {
                    return hits;
                }
                let length = CHUNK_SIZE.min(region.size - offset) as usize;
                if let Some(bytes) = self.memory.read_bytes(region.base + offset, length) {
                    for index in aligned_pattern_offsets(&bytes, &finder, 2) {
                        hits.insert(region.base + offset + index as u64);
                    }
                }
                offset = offset.saturating_add(CHUNK_SIZE - OVERLAP);
                scanned = scanned.saturating_add(length as u64);
                progress::transient(&format!(
                    "{label}... {}%",
                    progress::percent(scanned.min(total), total)
                ));
            }
        }
        progress::finish_transient();
        hits
    }

    fn find_pointers(&self, targets: &HashSet<u64>, image_only: bool, label: &str) -> Vec<u64> {
        let mut hits = Vec::new();
        const CHUNK_SIZE: u64 = 8 << 20;
        let total = self.scan_total(image_only);
        let mut scanned = 0u64;
        for region in &self.regions {
            if cancel::is_cancelled() {
                return hits;
            }
            if image_only && !region.image {
                continue;
            }
            let mut offset = 0u64;
            while offset < region.size {
                if cancel::is_cancelled() {
                    return hits;
                }
                let length = CHUNK_SIZE.min(region.size - offset) as usize;
                if length >= 8 {
                    if let Some(bytes) = self.memory.read_bytes(region.base + offset, length) {
                        for index in (0..=length.saturating_sub(8)).step_by(8) {
                            if targets.contains(&u64_at(&bytes, index)) {
                                hits.push(region.base + offset + index as u64);
                            }
                        }
                    }
                }
                offset = offset.saturating_add(CHUNK_SIZE);
                scanned = scanned.saturating_add(length as u64);
                progress::transient(&format!(
                    "{label}... {}%",
                    progress::percent(scanned.min(total), total)
                ));
            }
        }
        progress::finish_transient();
        hits
    }

    fn is_valid_metadata(&self, metadata: u64) -> Result<bool> {
        if metadata < 0x1_0000 {
            return Ok(false);
        }
        let layout_name_pointer = self
            .memory
            .read_pointer(metadata + METADATA_LAYOUT_NAME_POINTER as u64)
            .with_context(|| format!("could not read layout name pointer at 0x{metadata:X}"))?;
        let struct_type_name_pointer = self
            .memory
            .read_pointer(metadata + METADATA_STRUCT_TYPE_NAME_POINTER as u64)
            .with_context(|| format!("could not read struct type pointer at 0x{metadata:X}"))?;
        let shader_variable_name_pointer = self
            .memory
            .read_pointer(metadata + METADATA_SHADER_VARIABLE_NAME_POINTER as u64)
            .with_context(|| format!("could not read shader variable pointer at 0x{metadata:X}"))?;
        let layout_name = self
            .read_wide_string(layout_name_pointer, 128)
            .with_context(|| format!("could not read layout name at 0x{layout_name_pointer:X}"))?;
        let struct_type_name = self
            .read_wide_string(struct_type_name_pointer, 128)
            .with_context(|| {
                format!("could not read struct type name at 0x{struct_type_name_pointer:X}")
            })?;
        let shader_variable_name = self
            .read_wide_string(shader_variable_name_pointer, 128)
            .with_context(|| {
                format!("could not read shader variable name at 0x{shader_variable_name_pointer:X}")
            })?;
        Ok(is_type_name(&layout_name)
            && is_type_name(&struct_type_name)
            && !shader_variable_name.is_empty())
    }

    fn find_anchor_metadata(&self) -> u64 {
        self.find_anchor_metadata_in_regions(true)
            .or_else(|| self.find_anchor_metadata_in_regions(false))
            .unwrap_or(0)
    }

    fn find_anchor_metadata_in_regions(&self, image_only: bool) -> Option<u64> {
        let scope = if image_only {
            "executable memory"
        } else {
            "full memory"
        };
        let type_names = self.find_wide_strings(
            ANCHOR_TYPE_NAME,
            image_only,
            &format!("  searching {scope} for the View metadata anchor"),
        );
        if type_names.is_empty() {
            return None;
        }
        for pointer_address in self.find_pointers(
            &type_names,
            image_only,
            &format!("  searching {scope} for metadata references"),
        ) {
            if pointer_address < 8 {
                continue;
            }
            let metadata = pointer_address - 8;
            let Some(layout_name_pointer) = self
                .memory
                .read_pointer(metadata + METADATA_LAYOUT_NAME_POINTER as u64)
            else {
                continue;
            };
            let Some(struct_type_name_pointer) = self
                .memory
                .read_pointer(metadata + METADATA_STRUCT_TYPE_NAME_POINTER as u64)
            else {
                continue;
            };
            let Some(shader_variable_name_pointer) = self
                .memory
                .read_pointer(metadata + METADATA_SHADER_VARIABLE_NAME_POINTER as u64)
            else {
                continue;
            };
            if type_names.contains(&layout_name_pointer)
                && type_names.contains(&struct_type_name_pointer)
                && self.read_wide_string(shader_variable_name_pointer, 128)
                    == Some(ANCHOR_SHADER_NAME.to_string())
                && self.is_valid_metadata(metadata).unwrap_or(false)
            {
                return Some(metadata);
            }
        }
        None
    }

    fn find_list_node(&self, metadata: u64) -> u64 {
        for offset in
            (METADATA_LIST_NODE_SCAN_START..=METADATA_LIST_NODE_SCAN_END).step_by(POINTER_SIZE)
        {
            if self.memory.read_pointer(metadata + offset as u64) != Some(metadata) {
                continue;
            }
            let node = metadata + offset as u64 - LIST_ELEMENT;
            let Some(next_node) = self.memory.read_pointer(node + LIST_NEXT_LINK) else {
                continue;
            };
            let next_is_valid = next_node == 0
                || self
                    .memory
                    .read_pointer(next_node + LIST_ELEMENT)
                    .is_some_and(|value| self.is_valid_metadata(value).unwrap_or(false));
            if next_is_valid
                && self
                    .memory
                    .read_pointer(node + LIST_PREVIOUS_LINK)
                    .is_some_and(|value| value >= 0x1_0000)
            {
                return node;
            }
        }
        0
    }

    fn find_list_head(&self, mut node: u64) -> Result<u64> {
        for _ in 0..16_384 {
            cancel::check()?;
            let previous = self
                .memory
                .read_pointer(node + LIST_PREVIOUS_LINK)
                .with_context(|| format!("could not read previous list link at 0x{node:X}"))?;
            if previous < 0x1_0000 {
                return Ok(node);
            }
            // the head points to a global sentinel, so a failed probe ends the walk
            let Some(previous_next) = self.memory.read_pointer(previous) else {
                return Ok(node);
            };
            if previous_next != node {
                return Ok(node);
            }
            let Some(previous_metadata) = self.memory.read_pointer(previous + LIST_ELEMENT) else {
                return Ok(node);
            };
            match self.is_valid_metadata(previous_metadata) {
                Ok(true)
                    if previous as i64 - previous_metadata as i64
                        == self.global_list_node_offset => {}
                Ok(_) | Err(_) => return Ok(node),
            }
            node = previous;
        }
        bail!("uniform-buffer list walk exceeded its guard while finding the head")
    }

    fn walk_list(&self, head: u64) -> Result<Vec<u64>> {
        let mut metadata_objects = Vec::new();
        let mut seen_nodes = HashSet::new();
        let mut node = head;
        while node != 0 {
            cancel::check()?;
            if node < 0x1_0000 {
                bail!("invalid uniform-buffer next link at 0x{node:X}");
            }
            if !seen_nodes.insert(node) {
                bail!("cycle detected in the uniform-buffer global list");
            }
            if metadata_objects.len() >= 16_384 {
                bail!("uniform-buffer global-list walk exceeded its guard");
            }
            let metadata = self
                .memory
                .read_pointer(node + LIST_ELEMENT)
                .with_context(|| format!("could not read metadata pointer at 0x{node:X}"))?;
            if !self
                .is_valid_metadata(metadata)
                .with_context(|| format!("could not validate metadata at 0x{metadata:X}"))?
                || node as i64 - metadata as i64 != self.global_list_node_offset
            {
                bail!("invalid uniform-buffer list node at 0x{node:X}");
            }
            metadata_objects.push(metadata);
            node = self
                .memory
                .read_pointer(node + LIST_NEXT_LINK)
                .with_context(|| format!("could not read next list link at 0x{node:X}"))?;
        }
        Ok(metadata_objects)
    }

    fn is_valid_member(&self, bytes: &[u8], offset: usize) -> bool {
        if offset + MEMBER_SIZE > bytes.len() {
            return false;
        }
        let name_pointer = u64_at(bytes, offset + MEMBER_NAME_POINTER);
        let source_line = i32_at(bytes, offset + MEMBER_SOURCE_LINE);
        let member_offset = u32_at(bytes, offset + MEMBER_BYTE_OFFSET);
        let base_type_value = bytes[offset + MEMBER_BASE_TYPE];
        let rows = u32_at(bytes, offset + MEMBER_ROWS);
        let columns = u32_at(bytes, offset + MEMBER_COLUMNS);
        let elements = u32_at(bytes, offset + MEMBER_ELEMENTS);
        (0..=200_000).contains(&source_line)
            && member_offset <= 0x4_0000
            && ShaderParameterBaseType::from_u8(base_type_value)
                .is_some_and(|value| value != ShaderParameterBaseType::Invalid)
            && rows <= 4
            && columns <= 4
            && elements <= 8192
            && name_pointer >= 0x1_0000
            && self
                .read_wide_string(name_pointer, 128)
                .is_some_and(|value| !value.is_empty())
    }

    fn read_metadata_snapshot(&self, metadata: u64) -> Result<MetadataSnapshot> {
        let header = self
            .memory
            .read_bytes(metadata, METADATA_HEADER_SIZE)
            .with_context(|| format!("metadata object is unreadable at 0x{metadata:X}"))?;
        let layout_name = self
            .read_wide_string(u64_at(&header, METADATA_LAYOUT_NAME_POINTER), 128)
            .unwrap_or_default();
        let struct_type_name = self
            .read_wide_string(u64_at(&header, METADATA_STRUCT_TYPE_NAME_POINTER), 128)
            .unwrap_or_default();
        let shader_variable_name = self
            .read_wide_string(u64_at(&header, METADATA_SHADER_VARIABLE_NAME_POINTER), 128)
            .unwrap_or_default();

        let mut source_file = String::new();
        let mut source_line = 0i32;
        let mut size = 0u32;
        let mut use_case = 0u8;
        let mut binding_flags = 0u8;
        let mut usage_flags = 0u8;
        let mut found_profile = false;
        let mut diagnostics = String::new();
        // editor metadata has one extra pointer, so try both UE 5.6 headers
        for (profile, file_name_offset) in METADATA_FILE_NAME_POINTERS.iter().enumerate() {
            let file_line_offset = file_name_offset + 0x08;
            let size_offset = file_line_offset + 0x04;
            let flags_offset = size_offset + 0x04;
            let candidate_source = self
                .read_ansi_string(u64_at(&header, *file_name_offset), 2048)
                .unwrap_or_default();
            let candidate_line = i32_at(&header, file_line_offset);
            let candidate_size = u32_at(&header, size_offset);
            let candidate_use_case = header[flags_offset];
            let candidate_binding_flags = header[flags_offset + 1];
            let candidate_usage_flags = header[flags_offset + 2];
            let valid = is_type_name(&layout_name)
                && is_type_name(&struct_type_name)
                && (0..=200_000).contains(&candidate_line)
                && candidate_size <= 0x4_0000
                && candidate_use_case <= 2
                && candidate_binding_flags <= 3
                && candidate_usage_flags <= 15;
            diagnostics = format!(
                "profile={} source='{}' line={} size={}",
                if profile == 0 { "shipping" } else { "editor" },
                candidate_source,
                candidate_line,
                candidate_size
            );
            if !valid {
                continue;
            }
            source_file = candidate_source;
            source_line = candidate_line;
            size = candidate_size;
            use_case = candidate_use_case;
            binding_flags = candidate_binding_flags;
            usage_flags = candidate_usage_flags;
            found_profile = true;
            break;
        }
        if !found_profile {
            bail!(
                "invalid shader metadata at 0x{metadata:X} layout='{layout_name}' type='{struct_type_name}' {diagnostics}"
            );
        }

        let mut layout = 0u64;
        let mut layout_field_offset = None;
        let mut member_data = 0u64;
        let mut member_count = None;
        for offset in (METADATA_FIELD_SCAN_START..=header.len() - ARRAY_SIZE).step_by(POINTER_SIZE)
        {
            let candidate = u64_at(&header, offset);
            if candidate < 0x1_0000 {
                continue;
            }
            if layout == 0
                && self
                    .memory
                    .read_bytes(candidate, RHI_NAME_PROBE_SIZE)
                    .is_some_and(|bytes| self.find_name_offset(&bytes, &layout_name).is_some())
            {
                layout = candidate;
                layout_field_offset = Some(offset);
            }
            if member_data == 0 {
                let count = i32_at(&header, offset + ARRAY_COUNT);
                let capacity = i32_at(&header, offset + ARRAY_CAPACITY);
                if (1..=4096).contains(&count) && capacity >= count && capacity <= 8192 {
                    let sample_count = count.min(4) as usize;
                    if let Some(sample) = self
                        .memory
                        .read_bytes(candidate, sample_count * MEMBER_SIZE)
                    {
                        if (0..sample_count)
                            .all(|index| self.is_valid_member(&sample, index * MEMBER_SIZE))
                        {
                            member_data = candidate;
                            member_count = Some(count as usize);
                        }
                    }
                }
            }
        }
        if member_data == 0 {
            if let Some(layout_offset) = layout_field_offset {
                if layout_offset + ARRAY_SIZE + POINTER_SIZE <= header.len() {
                    let members_offset = layout_offset + POINTER_SIZE;
                    let candidate = u64_at(&header, members_offset);
                    let count = i32_at(&header, members_offset + ARRAY_COUNT);
                    let capacity = i32_at(&header, members_offset + ARRAY_CAPACITY);
                    if count == 0 && capacity == 0 {
                        member_data = candidate;
                        member_count = Some(0);
                    }
                }
            }
        }
        let Some(member_count) = member_count else {
            bail!(
                "could not locate layout or members for metadata at 0x{metadata:X} type='{struct_type_name}' {diagnostics}"
            );
        };
        if layout == 0 {
            bail!(
                "could not locate layout or members for metadata at 0x{metadata:X} type='{struct_type_name}' {diagnostics}"
            );
        }
        let members = if member_count == 0 {
            Vec::new()
        } else {
            self.memory
                .read_bytes(member_data, member_count * MEMBER_SIZE)
                .with_context(|| format!("member array is unreadable for {struct_type_name}"))?
        };
        for index in 0..member_count {
            if !self.is_valid_member(&members, index * MEMBER_SIZE) {
                bail!("invalid member {index} in {struct_type_name}");
            }
        }
        Ok(MetadataSnapshot {
            layout_name,
            struct_type_name,
            shader_variable_name,
            source_file,
            source_line,
            size,
            use_case,
            binding_flags,
            usage_flags,
            layout,
            members,
            member_count,
        })
    }

    fn register_definition(&mut self, metadata: u64) -> Result<u32> {
        self.register_definition_at_depth(metadata, 0)
    }

    fn register_definition_at_depth(&mut self, metadata: u64, depth: usize) -> Result<u32> {
        cancel::check()?;
        if let Some(id) = self.definition_ids.get(&metadata) {
            return Ok(*id);
        }
        if depth >= MAX_DEFINITION_DEPTH {
            bail!("shader metadata nesting exceeded {MAX_DEFINITION_DEPTH} definitions");
        }
        let snapshot = self.read_metadata_snapshot(metadata)?;
        let id = self.definition_order.len() as u32 + 1;
        let nested_metadata: Vec<u64> = (0..snapshot.member_count)
            .filter_map(|index| {
                let offset = index * MEMBER_SIZE;
                let base_type =
                    ShaderParameterBaseType::from_u8(snapshot.members[offset + MEMBER_BASE_TYPE])?;
                if base_type.is_nested_definition() {
                    let nested = u64_at(&snapshot.members, offset + MEMBER_NESTED_METADATA);
                    (nested >= 0x1_0000).then_some(nested)
                } else {
                    None
                }
            })
            .collect();
        self.definition_ids.insert(metadata, id);
        self.definitions.insert(metadata, snapshot);
        self.definition_order.push(metadata);
        for nested in nested_metadata {
            self.register_definition_at_depth(nested, depth + 1)?;
        }
        Ok(id)
    }

    fn capture_member(&self, snapshot: &MetadataSnapshot, index: usize) -> Result<MemberRecord> {
        let offset = index * MEMBER_SIZE;
        let name = self
            .read_wide_string(u64_at(&snapshot.members, offset + MEMBER_NAME_POINTER), 128)
            .unwrap_or_default();
        let shader_type = self
            .read_wide_string(
                u64_at(&snapshot.members, offset + MEMBER_SHADER_TYPE_POINTER),
                128,
            )
            .unwrap_or_default();
        let source_line = i32_at(&snapshot.members, offset + MEMBER_SOURCE_LINE);
        let member_offset = u32_at(&snapshot.members, offset + MEMBER_BYTE_OFFSET);
        let base_type_value = snapshot.members[offset + MEMBER_BASE_TYPE];
        let base_type = ShaderParameterBaseType::from_u8(base_type_value)
            .with_context(|| format!("unknown member base type {base_type_value}"))?;
        let precision = snapshot.members[offset + MEMBER_PRECISION];
        let rows = u32_at(&snapshot.members, offset + MEMBER_ROWS);
        let columns = u32_at(&snapshot.members, offset + MEMBER_COLUMNS);
        let elements = u32_at(&snapshot.members, offset + MEMBER_ELEMENTS);
        let nested_metadata = u64_at(&snapshot.members, offset + MEMBER_NESTED_METADATA);
        let mut nested_id = 0u32;
        let mut nested_type = String::new();
        if base_type.is_nested_definition() && nested_metadata >= 0x1_0000 {
            nested_id = self.definition_ids[&nested_metadata];
            nested_type = self.definitions[&nested_metadata].struct_type_name.clone();
        } else if matches!(
            base_type,
            ShaderParameterBaseType::RdgUniformBuffer | ShaderParameterBaseType::ReferencedStruct
        ) {
            nested_type = shader_type.clone();
        }
        Ok(MemberRecord {
            index,
            name,
            offset: member_offset,
            base_type,
            rows,
            columns,
            elements,
            precision,
            shader_type,
            struct_type: nested_type,
            source_line: usize::try_from(source_line).context("negative member source line")?,
            nested_definition_id: nested_id,
        })
    }

    fn capture_definitions(&self) -> Result<HashMap<u32, DefinitionRecord>> {
        let mut definitions = HashMap::new();
        for address in &self.definition_order {
            cancel::check()?;
            let snapshot = &self.definitions[address];
            let id = self.definition_ids[address];
            let members = (0..snapshot.member_count)
                .map(|index| self.capture_member(snapshot, index))
                .collect::<Result<Vec<_>>>()?;
            let definition = DefinitionRecord {
                id,
                type_name: snapshot.struct_type_name.clone(),
                layout_name: snapshot.layout_name.clone(),
                shader_variable_name: snapshot.shader_variable_name.clone(),
                source_file: snapshot.source_file.clone(),
                source_line: usize::try_from(snapshot.source_line)
                    .context("negative definition source line")?,
                size: snapshot.size,
                use_case: snapshot.use_case,
                binding_flags: snapshot.binding_flags,
                usage_flags: snapshot.usage_flags,
                members,
            };
            if definitions.insert(id, definition).is_some() {
                bail!("duplicate captured definition id {id}");
            }
        }
        Ok(definitions)
    }

    fn find_name_offset(&self, bytes: &[u8], layout_type_name: &str) -> Option<usize> {
        (0..=RHI_NAME_SEARCH_SIZE - ARRAY_SIZE)
            .step_by(POINTER_SIZE)
            .find(|offset| {
                let data = u64_at(bytes, *offset + ARRAY_DATA);
                let length = i32_at(bytes, *offset + ARRAY_COUNT);
                length == layout_type_name.len() as i32 + 1
                    && self.read_wide_string(data, 128).as_deref() == Some(layout_type_name)
            })
    }

    fn capture_resource_array(
        &self,
        data: u64,
        count: usize,
        name: &str,
    ) -> Result<Vec<ResourceEntry>> {
        if count == 0 {
            return Ok(Vec::new());
        }
        let bytes = self
            .memory
            .read_bytes(data, count * RHI_RESOURCE_ENTRY_SIZE)
            .with_context(|| format!("{name} array is unreadable"))?;
        (0..count)
            .map(|index| {
                let offset = index * RHI_RESOURCE_ENTRY_SIZE;
                let type_value = bytes[offset + 2];
                let resource_type = ShaderParameterBaseType::from_u8(type_value)
                    .filter(|value| *value != ShaderParameterBaseType::Invalid)
                    .with_context(|| format!("{name} has invalid resource type {type_value}"))?;
                Ok(ResourceEntry {
                    offset: u16_at(&bytes, offset),
                    resource_type,
                })
            })
            .collect()
    }

    fn capture_rhi_layout(
        &self,
        layout: u64,
        layout_type_name: &str,
        record: &mut LayoutRecord,
    ) -> Result<()> {
        let bytes = self
            .memory
            .read_bytes(layout, RHI_LAYOUT_READ_SIZE)
            .context("uniform-buffer RHI layout is unreadable")?;
        let name_offset = self
            .find_name_offset(&bytes, layout_type_name)
            .context("could not locate the RHI layout name")?;
        let hash = u32_at(&bytes, name_offset + RHI_HASH_FROM_NAME);
        let constant_buffer_size = u32_at(&bytes, name_offset + RHI_CONSTANT_BUFFER_SIZE_FROM_NAME);
        let render_targets_offset =
            u16_at(&bytes, name_offset + RHI_RENDER_TARGETS_OFFSET_FROM_NAME);
        let static_slot = bytes[name_offset + RHI_STATIC_SLOT_FROM_NAME];
        let binding_flags = bytes[name_offset + RHI_BINDING_FLAGS_FROM_NAME];
        let flags = bytes[name_offset + RHI_FLAGS_FROM_NAME];
        record.rhi_hash = hash;
        record.constant_buffer_size = constant_buffer_size;
        record.render_targets_offset = render_targets_offset;
        record.static_slot = static_slot;
        record.binding_flags = binding_flags;
        record.flags = flags;
        for (array_index, name) in RESOURCE_ARRAY_NAMES.iter().enumerate() {
            let array_offset = name_offset
                + RHI_RESOURCE_ARRAYS_FROM_NAME
                + array_index * RHI_RESOURCE_ARRAY_STRIDE;
            let data = u64_at(&bytes, array_offset + ARRAY_DATA);
            let count = i32_at(&bytes, array_offset + ARRAY_COUNT);
            let capacity = i32_at(&bytes, array_offset + ARRAY_CAPACITY);
            if !(0..=2048).contains(&count)
                || capacity < count
                || capacity > 4096
                || (count > 0 && data < 0x1_0000)
            {
                bail!("{name} array metadata is invalid");
            }
            record.arrays.insert(
                (*name).to_string(),
                self.capture_resource_array(data, count as usize, name)?,
            );
        }
        Ok(())
    }

    fn capture_one(&mut self, metadata: u64) -> Result<LayoutRecord> {
        let root_definition_id = self.register_definition(metadata)?;
        let snapshot = self.definitions[&metadata].clone();
        let hash_address = (metadata as i64
            + self.global_list_node_offset
            + METADATA_LAYOUT_HASH_FROM_LIST_NODE) as u64;
        let metadata_layout_hash = self.memory.read_u32(hash_address)?;
        let mut record = LayoutRecord::new(
            snapshot.shader_variable_name.clone(),
            snapshot.struct_type_name.clone(),
        );
        record.layout_name = snapshot.layout_name.clone();
        record.root_definition_id = root_definition_id;
        record.metadata_hash = metadata_layout_hash;
        self.capture_rhi_layout(snapshot.layout, &snapshot.layout_name, &mut record)?;
        Ok(record)
    }
}

fn aligned_pattern_offsets<'a, 'n: 'a>(
    haystack: &'a [u8],
    finder: &'a Finder<'n>,
    alignment: usize,
) -> impl Iterator<Item = usize> + 'a {
    let mut start = 0;
    std::iter::from_fn(move || {
        loop {
            if alignment == 0 || start + finder.needle().len() > haystack.len() {
                return None;
            }
            let relative = finder.find(&haystack[start..])?;
            let index = start + relative;
            start = index + 1;
            if index % alignment == 0 {
                return Some(index);
            }
        }
    })
}

fn is_type_name(value: &str) -> bool {
    value.len() > 1 && value.as_bytes()[0].is_ascii_uppercase()
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn i32_at(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligned_pattern_search_matches_the_previous_scan_rules() {
        let finder = Finder::new(b"aa");
        let matches = aligned_pattern_offsets(b"aaaaa", &finder, 2).collect::<Vec<_>>();
        assert_eq!(matches, [0, 2]);
    }

    #[test]
    fn aligned_pattern_search_ignores_unaligned_matches() {
        let needle = b"anchor";
        let finder = Finder::new(needle);
        let mut haystack = vec![0u8; 40];
        haystack[2..2 + needle.len()].copy_from_slice(needle);
        haystack[17..17 + needle.len()].copy_from_slice(needle);
        let matches = aligned_pattern_offsets(&haystack, &finder, 2).collect::<Vec<_>>();
        assert_eq!(matches, [2]);
    }
}
