use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

pub const MAX_DEFINITION_DEPTH: usize = 256;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[repr(u8)]
pub enum ShaderParameterBaseType {
    Invalid = 0,
    Bool = 1,
    Int32 = 2,
    Uint32 = 3,
    Float32 = 4,
    Texture = 5,
    Srv = 6,
    Uav = 7,
    Sampler = 8,
    RdgTexture = 9,
    RdgTextureAccess = 10,
    RdgTextureAccessArray = 11,
    RdgTextureSrv = 12,
    RdgTextureNonPixelSrv = 13,
    RdgTextureUav = 14,
    RdgBufferAccess = 15,
    RdgBufferAccessArray = 16,
    RdgBufferSrv = 17,
    RdgBufferUav = 18,
    RdgUniformBuffer = 19,
    NestedStruct = 20,
    IncludedStruct = 21,
    ReferencedStruct = 22,
    RenderTargetBindingSlots = 23,
    ResourceCollection = 24,
}

impl ShaderParameterBaseType {
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Invalid,
            1 => Self::Bool,
            2 => Self::Int32,
            3 => Self::Uint32,
            4 => Self::Float32,
            5 => Self::Texture,
            6 => Self::Srv,
            7 => Self::Uav,
            8 => Self::Sampler,
            9 => Self::RdgTexture,
            10 => Self::RdgTextureAccess,
            11 => Self::RdgTextureAccessArray,
            12 => Self::RdgTextureSrv,
            13 => Self::RdgTextureNonPixelSrv,
            14 => Self::RdgTextureUav,
            15 => Self::RdgBufferAccess,
            16 => Self::RdgBufferAccessArray,
            17 => Self::RdgBufferSrv,
            18 => Self::RdgBufferUav,
            19 => Self::RdgUniformBuffer,
            20 => Self::NestedStruct,
            21 => Self::IncludedStruct,
            22 => Self::ReferencedStruct,
            23 => Self::RenderTargetBindingSlots,
            24 => Self::ResourceCollection,
            _ => return None,
        })
    }

    pub fn dump_name(self) -> &'static str {
        match self {
            Self::Invalid => "INVALID",
            Self::Bool => "BOOL",
            Self::Int32 => "INT32",
            Self::Uint32 => "UINT32",
            Self::Float32 => "FLOAT32",
            Self::Texture => "TEXTURE",
            Self::Srv => "SRV",
            Self::Uav => "UAV",
            Self::Sampler => "SAMPLER",
            Self::RdgTexture => "RDG_TEXTURE",
            Self::RdgTextureAccess => "RDG_TEX_ACCESS",
            Self::RdgTextureAccessArray => "RDG_TEX_ACCESS_ARR",
            Self::RdgTextureSrv => "RDG_TEXTURE_SRV",
            Self::RdgTextureNonPixelSrv => "RDG_TEX_NONPIX_SRV",
            Self::RdgTextureUav => "RDG_TEXTURE_UAV",
            Self::RdgBufferAccess => "RDG_BUF_ACCESS",
            Self::RdgBufferAccessArray => "RDG_BUF_ACCESS_ARR",
            Self::RdgBufferSrv => "RDG_BUFFER_SRV",
            Self::RdgBufferUav => "RDG_BUFFER_UAV",
            Self::RdgUniformBuffer => "RDG_UNIFORM_BUFFER",
            Self::NestedStruct => "NESTED_STRUCT",
            Self::IncludedStruct => "INCLUDED_STRUCT",
            Self::ReferencedStruct => "REFERENCED_STRUCT",
            Self::RenderTargetBindingSlots => "RT_BINDING_SLOTS",
            Self::ResourceCollection => "RESOURCE_COLLECTION",
        }
    }

    pub fn is_nested_definition(self) -> bool {
        matches!(self, Self::NestedStruct | Self::IncludedStruct)
    }

    pub fn is_struct(self) -> bool {
        matches!(
            self,
            Self::NestedStruct | Self::IncludedStruct | Self::ReferencedStruct
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ResourceEntry {
    pub offset: u16,
    #[serde(rename = "type")]
    pub resource_type: ShaderParameterBaseType,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LayoutRecord {
    pub name: String,
    pub type_name: String,
    pub layout_name: String,
    pub root_definition_id: u32,
    #[serde(with = "hex_u32")]
    pub metadata_hash: u32,
    #[serde(with = "hex_u32")]
    pub rhi_hash: u32,
    pub constant_buffer_size: u32,
    pub render_targets_offset: u16,
    pub static_slot: u8,
    pub binding_flags: u8,
    pub flags: u8,
    #[serde(rename = "resourceTables")]
    pub arrays: BTreeMap<String, Vec<ResourceEntry>>,
}

impl LayoutRecord {
    pub fn new(name: String, type_name: String) -> Self {
        Self {
            name,
            type_name,
            layout_name: String::new(),
            root_definition_id: 0,
            metadata_hash: 0,
            rhi_hash: 0,
            constant_buffer_size: 0,
            render_targets_offset: 0,
            static_slot: 0,
            binding_flags: 0,
            flags: 0,
            arrays: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MemberRecord {
    pub index: usize,
    pub name: String,
    pub offset: u32,
    #[serde(rename = "type")]
    pub base_type: ShaderParameterBaseType,
    pub rows: u32,
    pub columns: u32,
    pub elements: u32,
    pub precision: u8,
    pub shader_type: String,
    pub struct_type: String,
    pub source_line: usize,
    pub nested_definition_id: u32,
}

impl MemberRecord {
    pub fn same_shape(&self, other: &Self) -> bool {
        self.base_type == other.base_type
            && self.rows == other.rows
            && self.columns == other.columns
            && self.elements == other.elements
            && self.precision == other.precision
            && self.shader_type == other.shader_type
            && self.struct_type == other.struct_type
    }

    pub fn same_storage_shape(&self, other: &Self) -> bool {
        self.base_type == other.base_type
            && self.rows == other.rows
            && self.columns == other.columns
            && self.elements == other.elements
            && self.precision == other.precision
    }

    pub fn exact(&self, other: &Self) -> bool {
        self.same_shape(other) && self.offset == other.offset && self.name == other.name
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DefinitionRecord {
    pub id: u32,
    pub type_name: String,
    pub layout_name: String,
    pub shader_variable_name: String,
    pub source_file: String,
    pub source_line: usize,
    pub size: u32,
    pub use_case: u8,
    pub binding_flags: u8,
    pub usage_flags: u8,
    pub members: Vec<MemberRecord>,
}

#[derive(Clone, Debug)]
pub struct LayoutDump {
    pub path: PathBuf,
    pub layouts: BTreeMap<String, LayoutRecord>,
    pub definitions: HashMap<u32, DefinitionRecord>,
}

pub const RESOURCE_ARRAY_NAMES: [&str; 6] = [
    "Resources",
    "GraphResources",
    "GraphTextures",
    "GraphBuffers",
    "GraphUniformBuffers",
    "UniformBuffers",
];

mod hex_u32 {
    use super::*;

    pub fn serialize<S>(value: &u32, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&format!("0x{value:08X}"))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<u32, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        let digits = value
            .strip_prefix("0x")
            .or_else(|| value.strip_prefix("0X"))
            .ok_or_else(|| {
                serde::de::Error::custom("expected a hexadecimal value beginning with 0x")
            })?;
        if digits.len() != 8 {
            return Err(serde::de::Error::custom(
                "expected exactly eight hexadecimal digits",
            ));
        }
        u32::from_str_radix(digits, 16).map_err(serde::de::Error::custom)
    }
}
