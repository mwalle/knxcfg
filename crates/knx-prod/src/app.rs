//! Application program parsing (parameters, types, group objects).
//!
//! Uses quick-xml's serde deserializer; unknown elements/attributes are
//! ignored, so we only model the parts we need.

use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::collections::HashMap;

use crate::dynamic::Dynamic;

#[derive(Debug, Deserialize)]
pub(crate) struct AppDoc {
    #[serde(rename = "ManufacturerData")]
    pub manufacturer_data: AppManufacturerData,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AppManufacturerData {
    #[serde(rename = "Manufacturer")]
    pub manufacturer: AppManufacturer,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AppManufacturer {
    #[serde(rename = "ApplicationPrograms", default)]
    pub application_programs: Option<ApplicationPrograms>,
    #[serde(rename = "Languages", default)]
    pub languages: Option<Languages>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct ApplicationPrograms {
    #[serde(rename = "ApplicationProgram", default)]
    pub items: Vec<ApplicationProgram>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ApplicationProgram {
    #[serde(rename = "@Id", default)]
    pub id: String,
    #[serde(rename = "@Name", default)]
    pub name: String,
    #[serde(rename = "@ApplicationNumber", default)]
    pub application_number: String,
    #[serde(rename = "@ApplicationVersion", default)]
    pub application_version: String,
    #[serde(rename = "@ProgramType", default)]
    pub program_type: String,
    #[serde(rename = "@MaskVersion", default)]
    pub mask_version: String,
    #[serde(rename = "@LoadProcedureStyle", default)]
    pub load_procedure_style: String,
    #[serde(rename = "@DefaultLanguage", default)]
    pub default_language: String,
    /// PEI type expected by the application (`LdCtrlTaskSegment` event data).
    #[serde(rename = "@PeiType", default)]
    pub pei_type: String,
    /// Application versions this one can be upgraded from.
    #[serde(rename = "@ReplacesVersions", default)]
    pub replaces_versions: String,
    /// KNX Data Secure capable (`IsSecureEnabled="true"`).
    #[serde(rename = "@IsSecureEnabled", default)]
    pub is_secure_enabled: String,
    #[serde(rename = "Static", default)]
    pub static_: Option<Static>,
    #[serde(rename = "Dynamic", default)]
    pub dynamic: Option<Dynamic>,
    #[serde(rename = "ModuleDefs", default)]
    pub module_defs: Option<ModuleDefs>,
    /// Parsed from the sibling `<Languages>` element (not part of
    /// `<ApplicationProgram>` itself).
    #[serde(skip)]
    pub languages: Languages,
}

impl ApplicationProgram {
    /// Translation table for one language: `refId -> (attribute -> text)`.
    pub fn translations(&self, lang: &str) -> HashMap<String, HashMap<String, String>> {
        let mut out = HashMap::new();
        for language in &self.languages.languages {
            if language.identifier != lang {
                continue;
            }
            for unit in &language.units {
                for element in &unit.elements {
                    let mut attrs = HashMap::new();
                    for t in &element.translations {
                        attrs.insert(t.attribute_name.clone(), t.text.clone());
                    }
                    if !attrs.is_empty() {
                        out.insert(element.ref_id.clone(), attrs);
                    }
                }
            }
        }
        out
    }
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct Languages {
    #[serde(rename = "Language", default)]
    pub languages: Vec<Language>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct Language {
    #[serde(rename = "@Identifier", default)]
    pub identifier: String,
    #[serde(rename = "TranslationUnit", default)]
    pub units: Vec<TranslationUnit>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct TranslationUnit {
    #[serde(rename = "@RefId", default)]
    pub ref_id: String,
    #[serde(rename = "TranslationElement", default)]
    pub elements: Vec<TranslationElement>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct TranslationElement {
    #[serde(rename = "@RefId", default)]
    pub ref_id: String,
    #[serde(rename = "Translation", default)]
    pub translations: Vec<Translation>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct Translation {
    #[serde(rename = "@AttributeName", default)]
    pub attribute_name: String,
    #[serde(rename = "@Text", default)]
    pub text: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct Static {
    #[serde(rename = "ParameterTypes", default)]
    pub parameter_types: Option<ParameterTypes>,
    #[serde(rename = "Parameters", default)]
    pub parameters: Option<Parameters>,
    #[serde(rename = "ParameterRefs", default)]
    pub parameter_refs: Option<ParameterRefs>,
    #[serde(rename = "ComObjectTable", default)]
    pub com_object_table: Option<ComObjectTable>,
    /// Some programs (module definitions) use `<ComObjects>` instead of
    /// `<ComObjectTable>`.
    #[serde(rename = "ComObjects", default)]
    pub com_objects: Option<ComObjects>,
    /// Per-context group-object overrides (ObjectSize/DPT/flags). Referenced
    /// from the `Dynamic` tree by `ComObjectRefRef`.
    #[serde(rename = "ComObjectRefs", default)]
    pub com_object_refs: Option<ComObjectRefs>,
    /// The `<Code>` element: memory segments (application code, parameter
    /// image, group-object/address/association tables).
    #[serde(rename = "Code", default)]
    pub code: Option<Code>,
    /// Header of the group-address table (entries are written at download).
    #[serde(rename = "AddressTable", default)]
    pub address_table: Option<Table>,
    /// Header of the association table (entries are written at download).
    #[serde(rename = "AssociationTable", default)]
    pub association_table: Option<Table>,
    /// The device's load procedure(s): the exact download sequence (unload,
    /// load, absolute segments, task segments, load-completed, restart).
    #[serde(rename = "LoadProcedures", default)]
    pub load_procedures: Option<LoadProcedures>,
    /// JavaScript-backed parameter transformations (`L` → `R` parameters).
    #[serde(rename = "ParameterCalculations", default)]
    pub parameter_calculations: Option<ParameterCalculations>,
    /// Named counters for `NumericArg AllocatorRefId` (dynamic module
    /// allocation). Parsed so their use can be reported; not evaluated.
    #[serde(rename = "Allocators", default)]
    pub allocators: Option<Allocators>,
    #[serde(rename = "Options", default)]
    pub options: Option<AppOptions>,
}

impl Static {
    /// `true` when multi-octet parameter values are stored little-endian
    /// (`<Options ParameterByteOrder="LittleEndian">`, used by System B
    /// applications). The default is big-endian.
    pub fn little_endian_parameters(&self) -> bool {
        self.options
            .as_ref()
            .map(|o| o.parameter_byte_order == "LittleEndian")
            .unwrap_or(false)
    }
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct AppOptions {
    #[serde(rename = "@ParameterByteOrder", default)]
    pub parameter_byte_order: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct Allocators {
    #[serde(rename = "Allocator", default)]
    pub items: Vec<Allocator>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct Allocator {
    #[serde(rename = "@Id", default)]
    pub id: String,
    #[serde(rename = "@Name", default)]
    pub name: String,
    #[serde(rename = "@Start", default)]
    pub start: String,
    #[serde(rename = "@maxInclusive", default)]
    pub max_inclusive: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ParameterCalculations {
    #[serde(rename = "ParameterCalculation", default)]
    pub items: Vec<ParameterCalculation>,
}

/// A `<ParameterCalculation>`: ETS runs `LRTransformation` whenever one of
/// the `LParameters` changes and writes the results to the `RParameters`
/// (the aliases name the script variables).
#[derive(Debug, Deserialize, Default, Clone)]
pub struct ParameterCalculation {
    #[serde(rename = "@Id", default)]
    pub id: String,
    #[serde(rename = "@Name", default)]
    pub name: String,
    #[serde(rename = "@Language", default)]
    pub language: String,
    #[serde(rename = "LRTransformation", default)]
    pub lr_transformation: String,
    #[serde(rename = "RLTransformation", default)]
    pub rl_transformation: String,
    #[serde(rename = "LParameters", default)]
    pub l_parameters: Option<CalculationParameters>,
    #[serde(rename = "RParameters", default)]
    pub r_parameters: Option<CalculationParameters>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct CalculationParameters {
    #[serde(rename = "ParameterRefRef", default)]
    pub items: Vec<crate::dynamic::ParameterRefRef>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ParameterRefs {
    #[serde(rename = "ParameterRef", default)]
    pub items: Vec<ParameterRef>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ParameterRef {
    #[serde(rename = "@Id", default)]
    pub id: String,
    #[serde(rename = "@RefId", default)]
    pub ref_id: String,
    #[serde(rename = "@Access", default)]
    pub access: String,
    /// Default value override for the referenced parameter (empty = use
    /// `Parameter/@Value`).
    #[serde(rename = "@Value", default)]
    pub value: Option<String>,
    /// Label override (empty = use `Parameter/@Text`).
    #[serde(rename = "@Text", default)]
    pub text: String,
    #[serde(rename = "@SuffixText", default)]
    pub suffix_text: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ComObjects {
    #[serde(rename = "ComObject", default)]
    pub items: Vec<ComObject>,
}

/// A list of `ComObjectRef` overrides.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct ComObjectRefs {
    #[serde(rename = "ComObjectRef", default)]
    pub items: Vec<ComObjectRef>,
}

/// A context-specific override for a `ComObject` (e.g. the same object can be
/// 1 Bit in one channel mode and 4 Bytes in another).
#[derive(Debug, Deserialize, Default, Clone)]
pub struct ComObjectRef {
    #[serde(rename = "@Id", default)]
    pub id: String,
    #[serde(rename = "@RefId", default)]
    pub ref_id: String,
    #[serde(rename = "@Text", default)]
    pub text: String,
    #[serde(rename = "@FunctionText", default)]
    pub function_text: String,
    #[serde(rename = "@ObjectSize", default)]
    pub object_size: String,
    #[serde(rename = "@ReadFlag", default)]
    pub read_flag: String,
    #[serde(rename = "@WriteFlag", default)]
    pub write_flag: String,
    #[serde(rename = "@CommunicationFlag", default)]
    pub communication_flag: String,
    #[serde(rename = "@TransmitFlag", default)]
    pub transmit_flag: String,
    #[serde(rename = "@UpdateFlag", default)]
    pub update_flag: String,
    #[serde(rename = "@ReadOnInitFlag", default)]
    pub read_on_init_flag: String,
    #[serde(rename = "@DatapointType", default)]
    pub datapoint_type: String,
    #[serde(rename = "@Priority", default)]
    pub priority: String,
}

/// Reusable parameter/GO definitions, instantiated per channel.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct ModuleDefs {
    #[serde(rename = "ModuleDef", default)]
    pub items: Vec<ModuleDef>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ModuleDef {
    #[serde(rename = "@Id", default)]
    pub id: String,
    #[serde(rename = "@Name", default)]
    pub name: String,
    #[serde(rename = "Arguments", default)]
    pub arguments: Option<Arguments>,
    #[serde(rename = "Static", default)]
    pub static_: Option<Static>,
    #[serde(rename = "Dynamic", default)]
    pub dynamic: Option<Dynamic>,
}

/// The `<Arguments>` block of a `ModuleDef`: names the values a `Module`
/// instance supplies via `TextArg`/`NumericArg` (matched by `RefId`).
#[derive(Debug, Deserialize, Default, Clone)]
pub struct Arguments {
    #[serde(rename = "Argument", default)]
    pub items: Vec<Argument>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct Argument {
    #[serde(rename = "@Id", default)]
    pub id: String,
    #[serde(rename = "@Name", default)]
    pub name: String,
    #[serde(rename = "@Type", default)]
    pub arg_type: String,
    /// Size in bytes that this argument allocates for the module instance
    /// (the stride between consecutive instances' base offsets).
    #[serde(rename = "@Allocates", default)]
    pub allocates: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ParameterTypes {
    #[serde(rename = "ParameterType", default)]
    pub items: Vec<ParameterType>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ParameterType {
    #[serde(rename = "@Id", default)]
    pub id: String,
    #[serde(rename = "@Name", default)]
    pub name: String,
    #[serde(rename = "TypeNumber", default)]
    pub number: Option<TypeNumber>,
    #[serde(rename = "TypeRestriction", default)]
    pub restriction: Option<TypeRestriction>,
    #[serde(rename = "TypeText", default)]
    pub text: Option<TypeText>,
    #[serde(rename = "TypeIPAddress", default)]
    pub ip_address: Option<TypeIpAddress>,
    #[serde(rename = "TypeFloat", default)]
    pub float: Option<TypeFloat>,
    #[serde(rename = "TypeTime", default)]
    pub time: Option<TypeTime>,
    #[serde(rename = "TypeColor", default)]
    pub color: Option<TypeColor>,
    /// Display-only types without a memory representation.
    #[serde(rename = "TypePicture", default)]
    pub picture: Option<serde::de::IgnoredAny>,
    #[serde(rename = "TypeNone", default)]
    pub none: Option<serde::de::IgnoredAny>,
}

impl ParameterType {
    /// Human-readable kind for display.
    pub fn kind(&self) -> &'static str {
        if self.number.is_some() {
            "number"
        } else if self.restriction.is_some() {
            "enum"
        } else if self.text.is_some() {
            "text"
        } else if self.ip_address.is_some() {
            "ip"
        } else if self.float.is_some() {
            "float"
        } else if self.time.is_some() {
            "time"
        } else if self.color.is_some() {
            "color"
        } else if self.picture.is_some() || self.none.is_some() {
            "none"
        } else {
            "unknown"
        }
    }

    pub fn size_in_bit(&self) -> Option<u32> {
        self.number
            .as_ref()
            .and_then(|n| n.size_in_bit.parse().ok())
            .or_else(|| {
                self.restriction
                    .as_ref()
                    .and_then(|r| r.size_in_bit.parse().ok())
            })
            .or_else(|| self.text.as_ref().and_then(|t| t.size_in_bit.parse().ok()))
            .or_else(|| self.time.as_ref().and_then(|t| t.size_in_bit.parse().ok()))
            .or_else(|| self.float.as_ref().and_then(TypeFloat::size_in_bit))
            .or_else(|| self.color.as_ref().map(|_| 24))
            .or_else(|| self.ip_address.as_ref().map(|_| 32))
    }
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct TypeNumber {
    #[serde(rename = "@SizeInBit", default)]
    pub size_in_bit: String,
    #[serde(rename = "@Type", default)]
    pub type_: String,
    #[serde(rename = "@minInclusive", default)]
    pub min_inclusive: String,
    #[serde(rename = "@maxInclusive", default)]
    pub max_inclusive: String,
    /// `CheckBox` for boolean checkboxes.
    #[serde(rename = "@UIHint", default)]
    pub ui_hint: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct TypeFloat {
    /// `DPT 9` (KNX 2-octet float), `IEEE-754 Single` or `IEEE-754 Double`.
    #[serde(rename = "@Encoding", default)]
    pub encoding: String,
    #[serde(rename = "@minInclusive", default)]
    pub min_inclusive: String,
    #[serde(rename = "@maxInclusive", default)]
    pub max_inclusive: String,
}

impl TypeFloat {
    pub fn size_in_bit(&self) -> Option<u32> {
        match self.encoding.as_str() {
            "DPT 9" => Some(16),
            "IEEE-754 Single" => Some(32),
            "IEEE-754 Double" => Some(64),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct TypeTime {
    #[serde(rename = "@SizeInBit", default)]
    pub size_in_bit: String,
    #[serde(rename = "@Unit", default)]
    pub unit: String,
    #[serde(rename = "@minInclusive", default)]
    pub min_inclusive: String,
    #[serde(rename = "@maxInclusive", default)]
    pub max_inclusive: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct TypeColor {
    /// `RGB` or `HSV`; both are stored as three octets (`#RRGGBB`).
    #[serde(rename = "@Space", default)]
    pub space: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct TypeRestriction {
    #[serde(rename = "@Base", default)]
    pub base: String,
    #[serde(rename = "@SizeInBit", default)]
    pub size_in_bit: String,
    #[serde(rename = "Enumeration", default)]
    pub enumerations: Vec<Enumeration>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct Enumeration {
    #[serde(rename = "@Id", default)]
    pub id: String,
    #[serde(rename = "@Text", default)]
    pub text: String,
    #[serde(rename = "@Value", default)]
    pub value: String,
    /// `Base="BinaryValue"` restrictions: base64 octets stored for this
    /// choice.
    #[serde(rename = "@BinaryValue", default)]
    pub binary_value: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct TypeText {
    #[serde(rename = "@SizeInBit", default)]
    pub size_in_bit: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct TypeIpAddress {
    #[serde(rename = "@SizeInBit", default)]
    pub size_in_bit: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct Parameters {
    /// Children of `<Parameters>` are a heterogeneous, interleaved sequence
    /// of `<Parameter>` and `<Union>` elements.
    #[serde(rename = "$value", default)]
    pub nodes: Vec<ParamNode>,
}

#[derive(Debug, Deserialize, Clone)]
pub enum ParamNode {
    #[serde(rename = "Parameter")]
    Parameter(Parameter),
    #[serde(rename = "Union")]
    Union(Union),
}

impl Parameters {
    pub fn parameters(&self) -> impl Iterator<Item = &Parameter> {
        self.nodes.iter().filter_map(|n| match n {
            ParamNode::Parameter(p) => Some(p),
            ParamNode::Union(_) => None,
        })
    }

    pub fn unions(&self) -> impl Iterator<Item = &Union> {
        self.nodes.iter().filter_map(|n| match n {
            ParamNode::Parameter(_) => None,
            ParamNode::Union(u) => Some(u),
        })
    }
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct Parameter {
    #[serde(rename = "@Id", default)]
    pub id: String,
    #[serde(rename = "@Name", default)]
    pub name: String,
    #[serde(rename = "@ParameterType", default)]
    pub parameter_type: String,
    #[serde(rename = "@Text", default)]
    pub text: String,
    #[serde(rename = "@SuffixText", default)]
    pub suffix_text: String,
    #[serde(rename = "@Value", default)]
    pub value: String,
    #[serde(rename = "@Access", default)]
    pub access: String,
    #[serde(rename = "Memory", default)]
    pub memory: Option<Memory>,
    /// `<Property>`-backed parameter (stored in an interface object property
    /// instead of memory).
    #[serde(rename = "Property", default)]
    pub property: Option<PropertyLocation>,
    /// Union members only: byte offset relative to the union's `<Memory>`.
    #[serde(rename = "@Offset", default)]
    pub offset: String,
    /// Union members only: bit offset relative to the union's `<Memory>`.
    #[serde(rename = "@BitOffset", default)]
    pub bit_offset: String,
    /// Union members only: the member that is written when no member of the
    /// union is visible.
    #[serde(rename = "@DefaultUnionParameter", default)]
    pub default_union_parameter: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct PropertyLocation {
    #[serde(rename = "@ObjectIndex", default)]
    pub object_index: String,
    #[serde(rename = "@ObjectType", default)]
    pub object_type: String,
    #[serde(rename = "@PropertyId", default)]
    pub property_id: String,
    #[serde(rename = "@Offset", default)]
    pub offset: String,
    #[serde(rename = "@BitOffset", default)]
    pub bit_offset: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct Union {
    #[serde(rename = "@SizeInBit", default)]
    pub size_in_bit: String,
    #[serde(rename = "Memory", default)]
    pub memory: Option<Memory>,
    #[serde(rename = "Property", default)]
    pub property: Option<PropertyLocation>,
    #[serde(rename = "Parameter", default)]
    pub parameters: Vec<Parameter>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct Memory {
    #[serde(rename = "@CodeSegment", default)]
    pub code_segment: String,
    #[serde(rename = "@Offset", default)]
    pub offset: String,
    #[serde(rename = "@BitOffset", default)]
    pub bit_offset: String,
    /// Module parameters are relative to a module argument (`BaseOffset`);
    /// device parameters have no `BaseOffset`.
    #[serde(rename = "@BaseOffset", default)]
    pub base_offset: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ComObjectTable {
    #[serde(rename = "@CodeSegment", default)]
    pub code_segment: String,
    #[serde(rename = "@Offset", default)]
    pub offset: String,
    #[serde(rename = "ComObject", default)]
    pub items: Vec<ComObject>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ComObject {
    #[serde(rename = "@Id", default)]
    pub id: String,
    #[serde(rename = "@Name", default)]
    pub name: String,
    #[serde(rename = "@Text", default)]
    pub text: String,
    #[serde(rename = "@Number", default)]
    pub number: String,
    #[serde(rename = "@FunctionText", default)]
    pub function_text: String,
    #[serde(rename = "@ObjectSize", default)]
    pub object_size: String,
    #[serde(rename = "@ReadFlag", default)]
    pub read_flag: String,
    #[serde(rename = "@WriteFlag", default)]
    pub write_flag: String,
    #[serde(rename = "@CommunicationFlag", default)]
    pub communication_flag: String,
    #[serde(rename = "@TransmitFlag", default)]
    pub transmit_flag: String,
    #[serde(rename = "@UpdateFlag", default)]
    pub update_flag: String,
    #[serde(rename = "@ReadOnInitFlag", default)]
    pub read_on_init_flag: String,
    #[serde(rename = "@DatapointType", default)]
    pub datapoint_type: String,
    /// Module group objects only: names the argument (in the enclosing
    /// `ModuleDef`) supplying the base group-object number; the object's
    /// final ASAP is `BaseNumber value + Number`.
    #[serde(rename = "@BaseNumber", default)]
    pub base_number: String,
    /// `Low` (default), `High` (= normal) or `Alert` (= urgent).
    #[serde(rename = "@Priority", default)]
    pub priority: String,
}

/// The `<Code>` element: a set of memory segments. Each segment is either an
/// `AbsoluteSegment` (fixed device address) or a `RelativeSegment` (relative
/// to a load state machine); both carry an optional base64 `<Data>` template.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct Code {
    #[serde(rename = "AbsoluteSegment", default)]
    pub absolute: Vec<Segment>,
    #[serde(rename = "RelativeSegment", default)]
    pub relative: Vec<Segment>,
}

impl Code {
    /// All segments, absolute first.
    pub fn segments(&self) -> impl Iterator<Item = &Segment> {
        self.absolute.iter().chain(self.relative.iter())
    }

    pub fn by_id(&self, id: &str) -> Option<&Segment> {
        self.segments().find(|s| s.id == id)
    }
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct Segment {
    #[serde(rename = "@Id", default)]
    pub id: String,
    #[serde(rename = "@Size", default)]
    pub size: String,
    /// Absolute segment only: fixed device address.
    #[serde(rename = "@Address", default)]
    pub address: String,
    /// Absolute segment only: e.g. `EEPROM`, `RAM`, or absent (flash).
    #[serde(rename = "@MemoryType", default)]
    pub memory_type: String,
    /// Relative segment only.
    #[serde(rename = "@LoadStateMachine", default)]
    pub load_state_machine: String,
    /// Relative segment only.
    #[serde(rename = "@Offset", default)]
    pub offset: String,
    /// base64 template; empty when the segment has no `<Data>`.
    #[serde(rename = "Data", default)]
    pub data: Option<String>,
    /// base64 bit mask over `<Data>`: set bits mark the octets that are
    /// significant when an image is compared with a device read-back (e.g.
    /// the individual address in a GrAT is masked out). Not used for
    /// download.
    #[serde(rename = "Mask", default)]
    pub mask: Option<String>,
}

impl Segment {
    pub fn size_bytes(&self) -> usize {
        self.size.parse().unwrap_or(0)
    }

    /// Decode the base64 `<Data>`, zero-filling to `@Size` if needed.
    pub fn decoded_data(&self) -> Vec<u8> {
        use base64::Engine;
        let mut bytes = self
            .data
            .as_deref()
            .map(|s| {
                let compact: String = s.chars().filter(|c| !c.is_whitespace()).collect();
                base64::engine::general_purpose::STANDARD
                    .decode(compact)
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        bytes.resize(self.size_bytes(), 0);
        bytes
    }
}

/// Header of the group-address table or the association table. The entries
/// themselves are empty in knxprod and are filled at download time.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct Table {
    #[serde(rename = "@CodeSegment", default)]
    pub code_segment: String,
    #[serde(rename = "@Offset", default)]
    pub offset: String,
    #[serde(rename = "@MaxEntries", default)]
    pub max_entries: String,
}

/// The `<LoadProcedures>` element: one or more load procedures.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct LoadProcedures {
    #[serde(rename = "LoadProcedure", default)]
    pub items: Vec<LoadProcedure>,
}

/// One `<LoadProcedure>`: a heterogeneous sequence of load-control steps.
///
/// With `LoadProcedureStyle="ProductProcedure"` the application carries the
/// complete procedure. With `MergedProcedure` each `<LoadProcedure MergeId>`
/// is a fragment that is spliced into the mask's procedure from
/// `knx_master.xml` at the matching `<LdCtrlMerge MergeId>`.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct LoadProcedure {
    #[serde(rename = "@MergeId", default)]
    pub merge_id: String,
    #[serde(rename = "$value", default)]
    pub steps: Vec<LoadStep>,
}

/// One step of a load procedure.
#[derive(Debug, Deserialize, Clone)]
pub enum LoadStep {
    #[serde(rename = "LdCtrlConnect")]
    Connect,
    #[serde(rename = "LdCtrlDisconnect")]
    Disconnect,
    #[serde(rename = "LdCtrlRestart")]
    Restart,
    #[serde(rename = "LdCtrlUnload")]
    Unload(LdCtrlUnload),
    #[serde(rename = "LdCtrlLoad")]
    Load(LdCtrlLoad),
    #[serde(rename = "LdCtrlAbsSegment")]
    AbsSegment(LdCtrlAbsSegment),
    #[serde(rename = "LdCtrlTaskSegment")]
    TaskSegment(LdCtrlTaskSegment),
    #[serde(rename = "LdCtrlLoadCompleted")]
    LoadCompleted(LdCtrlLoadCompleted),
    #[serde(rename = "LdCtrlCompareProp")]
    CompareProp(LdCtrlCompareProp),
    #[serde(rename = "LdCtrlRelSegment")]
    RelSegment(LdCtrlRelSegment),
    #[serde(rename = "LdCtrlWriteRelMem")]
    WriteRelMem(LdCtrlWriteRelMem),
    #[serde(rename = "LdCtrlWriteProp")]
    WriteProp(LdCtrlWriteProp),
    #[serde(rename = "LdCtrlLoadImageProp")]
    LoadImageProp(LdCtrlLoadImageProp),
    #[serde(rename = "LdCtrlDelay")]
    Delay(LdCtrlDelay),
    #[serde(rename = "LdCtrlMerge")]
    Merge(LdCtrlMerge),
    #[serde(rename = "LdCtrlMapError")]
    MapError(LdCtrlMapError),
    /// Parameter-dependent steps (`<choose ParamRefId><when test>`), with the
    /// same semantics as in the `Dynamic` tree.
    #[serde(rename = "choose")]
    Choose(LoadChoose),
    /// Any other load control. Kept by name so the download planner can
    /// refuse to build a plan it cannot execute.
    #[serde(other)]
    Other,
}

impl LoadStep {
    /// The `AppliesTo` filter (`full`, `par`, `full,par`, …); empty = all.
    pub fn applies_to(&self) -> &str {
        match self {
            LoadStep::RelSegment(s) => &s.applies_to,
            LoadStep::WriteRelMem(s) => &s.applies_to,
            LoadStep::WriteProp(s) => &s.applies_to,
            LoadStep::CompareProp(s) => &s.applies_to,
            LoadStep::LoadImageProp(s) => &s.applies_to,
            _ => "",
        }
    }
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct LdCtrlRelSegment {
    #[serde(rename = "@AppliesTo", default)]
    pub applies_to: String,
    #[serde(rename = "@LsmIdx", default)]
    pub lsm_idx: String,
    #[serde(rename = "@Size", default)]
    pub size: String,
    #[serde(rename = "@Mode", default)]
    pub mode: String,
    #[serde(rename = "@Fill", default)]
    pub fill: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct LdCtrlWriteRelMem {
    #[serde(rename = "@AppliesTo", default)]
    pub applies_to: String,
    #[serde(rename = "@ObjIdx", default)]
    pub obj_idx: String,
    #[serde(rename = "@Offset", default)]
    pub offset: String,
    #[serde(rename = "@Size", default)]
    pub size: String,
    #[serde(rename = "@Verify", default)]
    pub verify: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct LdCtrlWriteProp {
    #[serde(rename = "@AppliesTo", default)]
    pub applies_to: String,
    #[serde(rename = "@ObjIdx", default)]
    pub obj_idx: String,
    #[serde(rename = "@PropId", default)]
    pub prop_id: String,
    #[serde(rename = "@InlineData", default)]
    pub inline_data: String,
    #[serde(rename = "@Verify", default)]
    pub verify: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct LdCtrlLoadImageProp {
    #[serde(rename = "@AppliesTo", default)]
    pub applies_to: String,
    #[serde(rename = "@ObjIdx", default)]
    pub obj_idx: String,
    #[serde(rename = "@PropId", default)]
    pub prop_id: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct LdCtrlDelay {
    #[serde(rename = "@MilliSeconds", default)]
    pub milliseconds: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct LoadChoose {
    #[serde(rename = "@ParamRefId", default)]
    pub param_ref_id: String,
    #[serde(rename = "when", default)]
    pub whens: Vec<LoadWhen>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct LoadWhen {
    #[serde(rename = "@test", default)]
    pub test: String,
    #[serde(rename = "@default", default)]
    pub default: String,
    #[serde(rename = "$value", default)]
    pub steps: Vec<LoadStep>,
}

/// Error mapping for the following steps: a device error `OriginalError`
/// is treated as `MappedError` (0 = ignore).
#[derive(Debug, Deserialize, Default, Clone)]
pub struct LdCtrlMapError {
    #[serde(rename = "@OriginalError", default)]
    pub original_error: String,
    #[serde(rename = "@MappedError", default)]
    pub mapped_error: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct LdCtrlMerge {
    #[serde(rename = "@MergeId", default)]
    pub merge_id: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct LdCtrlUnload {
    #[serde(rename = "@LsmIdx", default)]
    pub lsm_idx: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct LdCtrlLoad {
    #[serde(rename = "@LsmIdx", default)]
    pub lsm_idx: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct LdCtrlLoadCompleted {
    #[serde(rename = "@LsmIdx", default)]
    pub lsm_idx: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct LdCtrlTaskSegment {
    #[serde(rename = "@LsmIdx", default)]
    pub lsm_idx: String,
    #[serde(rename = "@Address", default)]
    pub address: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct LdCtrlCompareProp {
    #[serde(rename = "@AppliesTo", default)]
    pub applies_to: String,
    #[serde(rename = "@InlineData", default)]
    pub inline_data: String,
    #[serde(rename = "@ObjIdx", default)]
    pub obj_idx: String,
    #[serde(rename = "@PropId", default)]
    pub prop_id: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct LdCtrlAbsSegment {
    #[serde(rename = "@LsmIdx", default)]
    pub lsm_idx: String,
    #[serde(rename = "@SegType", default)]
    pub seg_type: String,
    #[serde(rename = "@Address", default)]
    pub address: String,
    #[serde(rename = "@Size", default)]
    pub size: String,
    #[serde(rename = "@Access", default)]
    pub access: String,
    #[serde(rename = "@MemType", default)]
    pub mem_type: String,
    #[serde(rename = "@SegFlags", default)]
    pub seg_flags: String,
}

/// Strip a UTF-8 BOM if present (some knxprod files start with one).
fn strip_bom(s: &str) -> &str {
    s.strip_prefix('\u{feff}').unwrap_or(s)
}

/// Parse an application program XML file.
pub(crate) fn parse_application_program(xml: &str) -> Result<ApplicationProgram> {
    let doc: AppDoc =
        quick_xml::de::from_str(strip_bom(xml)).map_err(|e| anyhow!("application program: {e}"))?;
    let items = doc
        .manufacturer_data
        .manufacturer
        .application_programs
        .map(|p| p.items)
        .unwrap_or_default();

    let mut ap = items
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no ApplicationProgram element"))?;
    ap.languages = doc
        .manufacturer_data
        .manufacturer
        .languages
        .unwrap_or_default();
    Ok(ap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameters_handles_interleaved_parameter_and_union() {
        let xml = r#"
        <Parameters>
            <Parameter Id="P-1" Name="a" ParameterType="PT-1" Text="t" Value="1"/>
            <Union SizeInBit="16">
                <Memory CodeSegment="RS" Offset="0" BitOffset="0"/>
                <Parameter Id="U-1" Name="u1" ParameterType="PT-2" Value="2"/>
                <Parameter Id="U-2" Name="u2" ParameterType="PT-3" Value="3"/>
            </Union>
            <Parameter Id="P-2" Name="b" ParameterType="PT-1" Value="4"/>
        </Parameters>"#;
        let p: Parameters = quick_xml::de::from_str(xml).unwrap();
        assert_eq!(p.nodes.len(), 3);
        assert_eq!(p.parameters().count(), 2);
        assert_eq!(p.unions().count(), 1);
        let u = p.unions().next().unwrap();
        assert_eq!(u.parameters.len(), 2);
    }

    #[test]
    fn load_procedure_parses_heterogeneous_steps() {
        let xml = r#"
        <LoadProcedures>
          <LoadProcedure>
            <LdCtrlConnect />
            <LdCtrlUnload LsmIdx="1" />
            <LdCtrlLoad LsmIdx="1" />
            <LdCtrlAbsSegment LsmIdx="1" SegType="0" Address="16384" Size="513" Access="255" MemType="3" SegFlags="128" />
            <LdCtrlTaskSegment LsmIdx="1" Address="16384" />
            <LdCtrlLoadCompleted LsmIdx="1" />
            <LdCtrlRestart />
            <LdCtrlDisconnect />
          </LoadProcedure>
        </LoadProcedures>"#;
        let p: LoadProcedures = quick_xml::de::from_str(xml).unwrap();
        assert_eq!(p.items.len(), 1);
        let steps = &p.items[0].steps;
        assert_eq!(steps.len(), 8);
        assert!(matches!(steps[0], LoadStep::Connect));
        assert!(matches!(steps[1], LoadStep::Unload(_)));
        assert!(matches!(steps[3], LoadStep::AbsSegment(_)));
        assert!(matches!(steps[7], LoadStep::Disconnect));
    }
}
