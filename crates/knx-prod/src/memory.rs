//! Memory-image builder (M5, download direction).
//!
//! Overlays parameter values onto the knxprod `<Code>` segment templates:
//!
//! - `<AbsoluteSegment>` / `<RelativeSegment>` carry a base64 `<Data>` byte
//!   template (the manufacturer defaults, possibly with holes).
//! - each device-level `<Parameter>` may carry a `<Memory CodeSegment Offset
//!   BitOffset>` that places its value inside one of those segments.
//!
//! The encoder packs values **MSB-first within each byte** and **big-endian
//! across bytes**. This matches the bit numbering used across the KNX specs
//! (bit 7 = msb … bit 0 = lsb) but is not yet confirmed byte-for-byte
//! against the ETS5 oracle — [`check_template`] surfaces exactly the
//! placements where the knxprod `Value` default disagrees with the shipped
//! segment template, which is the M5 reverse-engineering work list.
//!
//! Module parameters (those with a `Memory/@BaseOffset`) are not instantiated
//! here yet — that needs the module argument substitution (next M5 step).

use std::collections::HashMap;

use anyhow::Result;

use crate::{
    ApplicationProgram, ModuleDef, ModuleInstance, ParamNode, Parameter, ParameterType, Static,
    UiNode,
};

/// A built memory image: one byte buffer per code segment, plus the
/// placements that were written into it.
#[derive(Debug, Clone)]
pub struct MemoryImage {
    pub segments: Vec<SegmentImage>,
    pub placements: Vec<Placement>,
}

#[derive(Debug, Clone)]
pub struct SegmentImage {
    pub id: String,
    pub address: String,
    pub size: usize,
    pub memory_type: String,
    pub data: Vec<u8>,
}

/// One parameter value written into a segment.
#[derive(Debug, Clone)]
pub struct Placement {
    pub segment_id: String,
    pub param_id: String,
    /// Byte offset within the segment.
    pub offset: usize,
    /// Bit offset within the first byte (0 = msb, 7 = lsb).
    pub bit_offset: usize,
    /// Width in bits.
    pub bit_len: usize,
    pub value: String,
}

/// Build the memory image for a device: start from the decoded segment
/// templates and write every device-level parameter value into it.
///
/// `values` maps parameter id → value string; missing entries fall back to
/// the knxprod default (`Parameter.Value`).
pub fn build_parameter_image(
    ap: &ApplicationProgram,
    values: &HashMap<String, String>,
) -> Result<MemoryImage> {
    let static_ = ap
        .static_
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("application program has no <Static>"))?;

    let types: HashMap<&str, &ParameterType> = static_
        .parameter_types
        .as_ref()
        .map(|pt| pt.items.iter().map(|t| (t.id.as_str(), t)).collect())
        .unwrap_or_default();

    let templates = segment_templates(static_);
    let mut segments: Vec<SegmentImage> = templates
        .iter()
        .map(|(id, data)| {
            let meta = static_
                .code
                .as_ref()
                .and_then(|c| c.by_id(id));
            SegmentImage {
                id: id.clone(),
                address: meta.map(|s| s.address.clone()).unwrap_or_default(),
                size: data.len(),
                memory_type: meta.map(|s| s.memory_type.clone()).unwrap_or_default(),
                data: data.clone(),
            }
        })
        .collect();
    segments.sort_by(|a, b| {
        let ka = a.address.parse::<u32>().unwrap_or(0);
        let kb = b.address.parse::<u32>().unwrap_or(0);
        ka.cmp(&kb).then_with(|| a.id.cmp(&b.id))
    });

    let mut placements = Vec::new();

    // Device-level parameters: absolute offsets (no BaseOffset).
    for param in device_parameters(static_) {
        write_parameter(param, 0, values, &types, &mut segments, &mut placements);
    }

    // Module parameters: offset relative to the module instance's base.
    let modules: HashMap<&str, &ModuleDef> = ap
        .module_defs
        .as_ref()
        .map(|m| m.items.iter().map(|m| (m.id.as_str(), m)).collect())
        .unwrap_or_default();
    if let Some(dynamic) = &ap.dynamic {
        let mut instances = Vec::new();
        collect_module_instances(&dynamic.nodes, &modules, "", &mut instances);
        for located in instances {
            write_module_parameters(
                located.instance,
                located.module,
                &located.prefix,
                values,
                &types,
                &mut segments,
                &mut placements,
            );
        }
    }

    Ok(MemoryImage { segments, placements })
}

/// Compare the freshly-encoded knxprod defaults against the shipped segment
/// template. Returns one entry per placement whose default differs from the
/// template bytes it overwrites — the concrete list of placements to verify
/// against the ETS5 oracle (value encoding, `Assign` computation, scaling,
/// or a stale template in the knxprod).
pub fn check_template(ap: &ApplicationProgram) -> Result<Vec<String>> {
    let defaults = HashMap::new();
    let image = build_parameter_image(ap, &defaults)?;
    let templates = segment_templates(
        ap.static_
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no <Static>"))?,
    );

    let mut out = Vec::new();
    for p in &image.placements {
        let Some(template) = templates.get(&p.segment_id) else {
            continue;
        };
        let Some(seg) = image.segments.iter().find(|s| s.id == p.segment_id) else {
            continue;
        };
        if bits_differ(&seg.data, template, p.offset * 8 + p.bit_offset, p.bit_len) {
            out.push(format!(
                "{} @ byte {}+{}b ({} bit) default {:?} differs from template",
                p.param_id, p.offset, p.bit_offset, p.bit_len, p.value
            ));
        }
    }
    Ok(out)
}

/// Read the parameter values back out of a built memory image.
///
/// This is the inverse of [`build_parameter_image`]: it walks exactly the same
/// device-level and module-parameter placements (all module instances, because
/// memory allocation is static) and decodes the current bits into value
/// strings using the knxprod parameter types.
pub fn decode_parameter_image(
    ap: &ApplicationProgram,
    segments: &[SegmentImage],
) -> Result<HashMap<String, String>> {
    let static_ = ap
        .static_
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("application program has no <Static>"))?;

    let types: HashMap<&str, &ParameterType> = static_
        .parameter_types
        .as_ref()
        .map(|pt| pt.items.iter().map(|t| (t.id.as_str(), t)).collect())
        .unwrap_or_default();

    let segment_data: HashMap<&str, &[u8]> = segments
        .iter()
        .map(|s| (s.id.as_str(), s.data.as_slice()))
        .collect();

    let mut values = HashMap::new();

    for param in device_parameters(static_) {
        if let Some(mem) = &param.memory {
            let Some(data) = segment_data.get(mem.code_segment.as_str()) else {
                continue;
            };
            if let Some(value) = read_parameter_value(param, &types, 0, data) {
                values.insert(param.id.clone(), value);
            }
        }
    }

    let modules: HashMap<&str, &ModuleDef> = ap
        .module_defs
        .as_ref()
        .map(|m| m.items.iter().map(|m| (m.id.as_str(), m)).collect())
        .unwrap_or_default();
    if let Some(dynamic) = &ap.dynamic {
        let mut instances = Vec::new();
        collect_module_instances(&dynamic.nodes, &modules, "", &mut instances);
        for located in instances {
            read_module_parameter_values(
                located.instance,
                located.module,
                &located.prefix,
                &types,
                &segment_data,
                &mut values,
            );
        }
    }

    Ok(values)
}

/// Enumerate every parameter instance and its knxprod default value.
///
/// Keys match the form/TUI ids: device parameters use the bare `param.id`,
/// module parameters use `module-instance-id/param-id` (nested modules are
/// slash-separated). This is used to keep the project YAML minimal: a value
/// that equals this default is redundant and is not persisted.
pub fn parameter_defaults(ap: &ApplicationProgram) -> HashMap<String, String> {
    let mut out = HashMap::new();

    if let Some(static_) = &ap.static_ {
        push_static_defaults(static_, "", &mut out);
    }

    let modules: HashMap<&str, &ModuleDef> = ap
        .module_defs
        .as_ref()
        .map(|m| m.items.iter().map(|m| (m.id.as_str(), m)).collect())
        .unwrap_or_default();
    if let Some(dynamic) = &ap.dynamic {
        let mut instances = Vec::new();
        collect_module_instances(&dynamic.nodes, &modules, "", &mut instances);
        for located in instances {
            if let Some(static_) = &located.module.static_ {
                push_static_defaults(static_, &located.prefix, &mut out);
            }
        }
    }

    out
}

fn push_static_defaults(static_: &Static, prefix: &str, out: &mut HashMap<String, String>) {
    let Some(parameters) = &static_.parameters else {
        return;
    };
    for param in parameters.parameters() {
        let id = if prefix.is_empty() {
            param.id.clone()
        } else {
            format!("{prefix}/{}", param.id)
        };
        out.insert(id, param.value.clone());
    }
    for union in parameters.unions() {
        for param in &union.parameters {
            let id = if prefix.is_empty() {
                param.id.clone()
            } else {
                format!("{prefix}/{}", param.id)
            };
            out.insert(id, param.value.clone());
        }
    }
}

/// Decoded template bytes per segment id.
fn segment_templates(static_: &Static) -> HashMap<String, Vec<u8>> {
    static_
        .code
        .as_ref()
        .map(|c| {
            c.segments()
                .map(|s| (s.id.clone(), s.decoded_data()))
                .collect()
        })
        .unwrap_or_default()
}

/// Device-level parameters (direct `<Static>/<Parameters>` children) that
/// carry a `<Memory>` without a `BaseOffset`.
fn device_parameters(static_: &Static) -> Vec<&Parameter> {
    static_
        .parameters
        .as_ref()
        .map(|p| {
            p.parameters()
                .filter(|p| p.memory.as_ref().map(|m| m.base_offset.is_empty()).unwrap_or(false))
                .collect()
        })
        .unwrap_or_default()
}

/// Write one parameter's value into its memory segment.
///
/// `base` is the absolute byte offset of the containing region (0 for
/// device-level parameters; the module instance's base offset for module
/// parameters). The final absolute offset is `base + Memory.Offset`.
#[allow(clippy::too_many_arguments)]
fn write_parameter(
    param: &Parameter,
    base: usize,
    values: &HashMap<String, String>,
    types: &HashMap<&str, &ParameterType>,
    segments: &mut [SegmentImage],
    placements: &mut Vec<Placement>,
) {
    let Some(mem) = &param.memory else {
        return;
    };
    let rel_offset: usize = mem.offset.parse().unwrap_or(0);
    let bit_offset: usize = mem.bit_offset.parse().unwrap_or(0);
    let offset = base + rel_offset;
    let value = values
        .get(&param.id)
        .cloned()
        .unwrap_or_else(|| param.value.clone());

    let type_ = types.get(param.parameter_type.as_str()).copied();
    let Ok(encoded) = encode(type_, &value) else {
        return;
    };

    let Some(segment) = segments.iter_mut().find(|s| s.id == mem.code_segment) else {
        return;
    };

    match encoded {
        Encoded::Bits(v, bit_len) => {
            write_bits(&mut segment.data, v, offset * 8 + bit_offset, bit_len);
            placements.push(Placement {
                segment_id: mem.code_segment.clone(),
                param_id: param.id.clone(),
                offset,
                bit_offset,
                bit_len,
                value,
            });
        }
        Encoded::Bytes(bytes) => {
            for (i, b) in bytes.iter().enumerate() {
                write_bits(&mut segment.data, *b as u64, offset * 8 + bit_offset + i * 8, 8);
            }
            placements.push(Placement {
                segment_id: mem.code_segment.clone(),
                param_id: param.id.clone(),
                offset,
                bit_offset,
                bit_len: bytes.len() * 8,
                value,
            });
        }
    }
}

/// A located module instance, ready for parameter overlay.
struct LocatedModule<'a> {
    instance: &'a ModuleInstance,
    module: &'a ModuleDef,
    /// `module_instance_id/` prefix used to key per-instance values.
    prefix: String,
}

/// Recursively collect every `Module` instance in a `Dynamic` tree.
///
/// Memory allocation is static: every module instance occupies its own
/// pre-allocated region (its `BaseOffset` argument), so *all* `when` branches
/// are materialized regardless of UI visibility. Nested module instances are
/// descended into and their parameter ids are prefixed exactly like the form
/// engine (`parent-instance-id/child-instance-id/param-id`).
fn collect_module_instances<'a>(
    nodes: &'a [UiNode],
    modules: &HashMap<&str, &'a ModuleDef>,
    prefix: &str,
    out: &mut Vec<LocatedModule<'a>>,
) {
    for node in nodes {
        match node {
            UiNode::Module(instance) => {
                if let Some(module) = modules.get(instance.ref_id.as_str()) {
                    let new_prefix = join_prefix(prefix, &instance.id);
                    out.push(LocatedModule {
                        instance,
                        module,
                        prefix: new_prefix.clone(),
                    });
                    if let Some(dynamic) = &module.dynamic {
                        collect_module_instances(&dynamic.nodes, modules, &new_prefix, out);
                    }
                }
            }
            UiNode::Choose(choose) => {
                for when in &choose.whens {
                    collect_module_instances(&when.nodes, modules, prefix, out);
                }
            }
            _ => {
                if let Some(children) = node.children() {
                    collect_module_instances(children, modules, prefix, out);
                }
            }
        }
    }
}

fn join_prefix(prefix: &str, id: &str) -> String {
    if prefix.is_empty() {
        id.to_string()
    } else {
        format!("{prefix}/{id}")
    }
}

/// Write every memory-backed parameter of a module instance, applying the
/// instance's argument value as the base offset.
fn write_module_parameters(
    instance: &ModuleInstance,
    module: &ModuleDef,
    prefix: &str,
    values: &HashMap<String, String>,
    types: &HashMap<&str, &ParameterType>,
    segments: &mut [SegmentImage],
    placements: &mut Vec<Placement>,
) {
    let Some(static_) = &module.static_ else {
        return;
    };
    let Some(parameters) = &static_.parameters else {
        return;
    };

    for node in &parameters.nodes {
        match node {
            ParamNode::Parameter(param) => {
                if let Some(mem) = &param.memory {
                    if mem.base_offset.is_empty() {
                        continue;
                    }
                    let Some(base) = instance.arg_value(&mem.base_offset).and_then(|v| v.parse::<usize>().ok())
                    else {
                        continue;
                    };
                    let key = format!("{prefix}/{}", param.id);
                    write_parameter_scoped(param, &key, base, values, types, segments, placements);
                }
            }
            ParamNode::Union(union) => {
                let mem = union.memory.as_ref();
                let base = mem
                    .filter(|m| !m.base_offset.is_empty())
                    .and_then(|m| instance.arg_value(&m.base_offset))
                    .and_then(|v| v.parse::<usize>().ok());
                let Some(base) = base else {
                    continue;
                };
                for param in &union.parameters {
                    // Union children may carry their own Memory, or inherit
                    // the union's; in the observed knxprod they inherit via
                    // the union-level Memory + per-param Offset/BitOffset.
                    let mut p = param.clone();
                    if p.memory.is_none() {
                        p.memory = mem.cloned();
                    }
                    let key = format!("{prefix}/{}", param.id);
                    write_parameter_scoped(&p, &key, base, values, types, segments, placements);
                }
            }
        }
    }
}

/// Like [`write_parameter`] but looks the value up under a scoped key
/// (`module_instance_id/param_id`) first, then the bare param id.
#[allow(clippy::too_many_arguments)]
fn write_parameter_scoped(
    param: &Parameter,
    key: &str,
    base: usize,
    values: &HashMap<String, String>,
    types: &HashMap<&str, &ParameterType>,
    segments: &mut [SegmentImage],
    placements: &mut Vec<Placement>,
) {
    let Some(mem) = &param.memory else {
        return;
    };
    let rel_offset: usize = mem.offset.parse().unwrap_or(0);
    let bit_offset: usize = mem.bit_offset.parse().unwrap_or(0);
    let offset = base + rel_offset;
    let value = values
        .get(key)
        .cloned()
        .or_else(|| values.get(&param.id).cloned())
        .unwrap_or_else(|| param.value.clone());

    let type_ = types.get(param.parameter_type.as_str()).copied();
    let Ok(encoded) = encode(type_, &value) else {
        return;
    };

    let Some(segment) = segments.iter_mut().find(|s| s.id == mem.code_segment) else {
        return;
    };

    let param_id = if key.is_empty() {
        param.id.clone()
    } else {
        key.to_string()
    };

    match encoded {
        Encoded::Bits(v, bit_len) => {
            write_bits(&mut segment.data, v, offset * 8 + bit_offset, bit_len);
            placements.push(Placement {
                segment_id: mem.code_segment.clone(),
                param_id: param_id.clone(),
                offset,
                bit_offset,
                bit_len,
                value,
            });
        }
        Encoded::Bytes(bytes) => {
            for (i, b) in bytes.iter().enumerate() {
                write_bits(&mut segment.data, *b as u64, offset * 8 + bit_offset + i * 8, 8);
            }
            placements.push(Placement {
                segment_id: mem.code_segment.clone(),
                param_id: param_id.clone(),
                offset,
                bit_offset,
                bit_len: bytes.len() * 8,
                value,
            });
        }
    }
}

enum Encoded {
    Bits(u64, usize),
    Bytes(Vec<u8>),
}

/// Encode a parameter value into its memory representation.
fn encode(type_: Option<&ParameterType>, value: &str) -> Result<Encoded> {
    let Some(t) = type_ else {
        return Ok(Encoded::Bits(0, 0));
    };

    if let Some(r) = &t.restriction {
        let bit_len: usize = r.size_in_bit.parse().unwrap_or(0);
        let v: u64 = value.parse().unwrap_or(0);
        return Ok(Encoded::Bits(v, bit_len));
    }
    if let Some(n) = &t.number {
        let bit_len: usize = n.size_in_bit.parse().unwrap_or(0);
        if n.type_ == "signedInt" {
            let v: i64 = value.parse().unwrap_or(0);
            let mask = if bit_len >= 64 { u64::MAX } else { (1u64 << bit_len) - 1 };
            return Ok(Encoded::Bits((v as u64) & mask, bit_len));
        }
        let v: u64 = value.parse().unwrap_or(0);
        return Ok(Encoded::Bits(v, bit_len));
    }
    if let Some(txt) = &t.text {
        let bit_len: usize = txt.size_in_bit.parse().unwrap_or(0);
        let mut bytes = value.as_bytes().to_vec();
        bytes.resize(bit_len / 8, 0);
        return Ok(Encoded::Bytes(bytes));
    }
    if t.ip_address.is_some() {
        let mut bytes = [0u8; 4];
        for (i, part) in value.split('.').take(4).enumerate() {
            bytes[i] = part.parse().unwrap_or(0);
        }
        return Ok(Encoded::Bytes(bytes.to_vec()));
    }

    Ok(Encoded::Bits(0, 0))
}

/// Write the low `bit_len` bits of `value` MSB-first at the absolute bit
/// position `abs_bit` (bit 7 of a byte is its most significant bit).
fn write_bits(buf: &mut [u8], value: u64, abs_bit: usize, bit_len: usize) {
    for i in 0..bit_len {
        let pos = abs_bit + i;
        let byte = pos / 8;
        let bit = 7 - (pos % 8);
        if byte >= buf.len() {
            return;
        }
        let b = (value >> (bit_len - 1 - i)) & 1;
        if b == 1 {
            buf[byte] |= 1 << bit;
        } else {
            buf[byte] &= !(1 << bit);
        }
    }
}

fn bits_differ(a: &[u8], b: &[u8], abs_bit: usize, bit_len: usize) -> bool {
    for i in 0..bit_len {
        let pos = abs_bit + i;
        let byte = pos / 8;
        let bit = 7 - (pos % 8);
        let va = a.get(byte).map(|x| (x >> bit) & 1).unwrap_or(0);
        let vb = b.get(byte).map(|x| (x >> bit) & 1).unwrap_or(0);
        if va != vb {
            return true;
        }
    }
    false
}

fn read_parameter_value(
    param: &Parameter,
    types: &HashMap<&str, &ParameterType>,
    base: usize,
    data: &[u8],
) -> Option<String> {
    let mem = param.memory.as_ref()?;
    let rel_offset: usize = mem.offset.parse().unwrap_or(0);
    let bit_offset: usize = mem.bit_offset.parse().unwrap_or(0);
    let abs_bit = (base + rel_offset) * 8 + bit_offset;
    let type_ = types.get(param.parameter_type.as_str()).copied();
    decode_value(type_, data, abs_bit)
}

#[allow(clippy::too_many_arguments)]
fn read_module_parameter_values(
    instance: &ModuleInstance,
    module: &ModuleDef,
    prefix: &str,
    types: &HashMap<&str, &ParameterType>,
    segment_data: &HashMap<&str, &[u8]>,
    values: &mut HashMap<String, String>,
) {
    let Some(static_) = &module.static_ else {
        return;
    };
    let Some(parameters) = &static_.parameters else {
        return;
    };

    for node in &parameters.nodes {
        match node {
            ParamNode::Parameter(param) => {
                let Some(mem) = &param.memory else {
                    continue;
                };
                if mem.base_offset.is_empty() {
                    continue;
                }
                let Some(base) = instance
                    .arg_value(&mem.base_offset)
                    .and_then(|v| v.parse::<usize>().ok())
                else {
                    continue;
                };
                let Some(data) = segment_data.get(mem.code_segment.as_str()) else {
                    continue;
                };
                let key = format!("{prefix}/{}", param.id);
                if let Some(value) = read_parameter_value(param, types, base, data) {
                    values.insert(key, value);
                }
            }
            ParamNode::Union(union) => {
                let mem = union.memory.as_ref();
                let base = mem
                    .filter(|m| !m.base_offset.is_empty())
                    .and_then(|m| instance.arg_value(&m.base_offset))
                    .and_then(|v| v.parse::<usize>().ok());
                let Some(base) = base else {
                    continue;
                };
                for param in &union.parameters {
                    let mut p = param.clone();
                    if p.memory.is_none() {
                        p.memory = mem.cloned();
                    }
                    let Some(mem2) = &p.memory else {
                        continue;
                    };
                    let Some(data) = segment_data.get(mem2.code_segment.as_str()) else {
                        continue;
                    };
                    let key = format!("{prefix}/{}", param.id);
                    if let Some(value) = read_parameter_value(&p, types, base, data) {
                        values.insert(key, value);
                    }
                }
            }
        }
    }
}

fn decode_value(type_: Option<&ParameterType>, data: &[u8], abs_bit: usize) -> Option<String> {
    let t = type_?;

    if let Some(r) = &t.restriction {
        let bit_len: usize = r.size_in_bit.parse().unwrap_or(0);
        return Some(read_bits(data, abs_bit, bit_len).to_string());
    }
    if let Some(n) = &t.number {
        let bit_len: usize = n.size_in_bit.parse().unwrap_or(0);
        let raw = read_bits(data, abs_bit, bit_len);
        if n.type_ == "signedInt" {
            return Some(sign_extend(raw, bit_len).to_string());
        }
        return Some(raw.to_string());
    }
    if let Some(txt) = &t.text {
        let bit_len: usize = txt.size_in_bit.parse().unwrap_or(0);
        let bytes = read_bytes(data, abs_bit, bit_len / 8);
        let mut end = bytes.len();
        while end > 0 && bytes[end - 1] == 0 {
            end -= 1;
        }
        return Some(String::from_utf8_lossy(&bytes[..end]).into_owned());
    }
    if t.ip_address.is_some() {
        let bytes = read_bytes(data, abs_bit, 4);
        return Some(format!("{}.{}.{}.{}", bytes[0], bytes[1], bytes[2], bytes[3]));
    }

    None
}

fn read_bits(buf: &[u8], abs_bit: usize, bit_len: usize) -> u64 {
    let mut value = 0u64;
    for i in 0..bit_len {
        let pos = abs_bit + i;
        let byte = pos / 8;
        let bit = 7 - (pos % 8);
        let b = buf.get(byte).map(|x| (x >> bit) & 1).unwrap_or(0);
        value = (value << 1) | (b as u64);
    }
    value
}

fn read_bytes(buf: &[u8], abs_bit: usize, len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        out.push(read_bits(buf, abs_bit + i * 8, 8) as u8);
    }
    out
}

fn sign_extend(raw: u64, bit_len: usize) -> i64 {
    if bit_len == 0 {
        return 0;
    }
    if bit_len >= 64 {
        return raw as i64;
    }
    let mask = (1u64 << bit_len) - 1;
    let value = raw & mask;
    if value & (1u64 << (bit_len - 1)) != 0 {
        (value as i64) - (1i64 << bit_len)
    } else {
        value as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_bits_is_msb_first_and_big_endian() {
        let mut buf = vec![0u8; 4];
        write_bits(&mut buf, 0x02, 0, 8); // 8-bit: 0x02
        assert_eq!(buf[0], 0x02);
        write_bits(&mut buf, 0x1234, 8, 16); // 16-bit big-endian at byte 1
        assert_eq!(&buf[1..3], &[0x12, 0x34]);
        let mut b = vec![0u8; 1];
        write_bits(&mut b, 1, 1, 1); // 1 bit at bit offset 1 → bit 6
        assert_eq!(b[0], 0b0100_0000);
    }

    #[test]
    fn bits_differ_compares_ranges() {
        let a = [0b0000_0010u8];
        let b = [0b0000_0000u8];
        assert!(bits_differ(&a, &b, 6, 1)); // bit 6 (msb-indexed 1) differs
        assert!(!bits_differ(&a, &b, 0, 6));
    }

    #[test]
    fn segment_decodes_base64_to_size() {
        let s = crate::Segment {
            id: "S".into(),
            size: "4".into(),
            data: Some("AQID".into()), // 0x01 0x02 0x03
            ..Default::default()
        };
        assert_eq!(s.decoded_data(), vec![1, 2, 3, 0]);
    }

    #[test]
    fn module_base_offset_applies_argument_value() {
        use crate::{Argument, Arguments, Dynamic, Memory, ModuleDefs, Parameters, Segment, Static, TextArg};

        // One 8-bit parameter type `PT-1`.
        let pt = ParameterType {
            id: "PT-1".into(),
            number: Some(crate::TypeNumber {
                size_in_bit: "8".into(),
                ..Default::default()
            }),
            ..Default::default()
        };

        // Module def with an argument that supplies the base offset, and a
        // parameter placed at `Offset=432` relative to it.
        let module = crate::ModuleDef {
            id: "MD-1".into(),
            name: "M".into(),
            arguments: Some(Arguments {
                items: vec![Argument {
                    id: "MD-1_A-1".into(),
                    name: "ParamOffs".into(),
                    allocates: "54".into(),
                    ..Default::default()
                }],
            }),
            static_: Some(Static {
                parameters: Some(Parameters {
                    nodes: vec![ParamNode::Parameter(Parameter {
                        id: "MD-1_P-1".into(),
                        parameter_type: "PT-1".into(),
                        value: "42".into(),
                        memory: Some(Memory {
                            code_segment: "AS".into(),
                            offset: "432".into(),
                            base_offset: "MD-1_A-1".into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    })],
                }),
                ..Default::default()
            }),
            ..Default::default()
        };

        // Instance supplies base 16.
        let instance = crate::ModuleInstance {
            id: "MD-1_M-1".into(),
            ref_id: "MD-1".into(),
            args: vec![crate::Arg::Text(TextArg {
                ref_id: "MD-1_A-1".into(),
                value: "16".into(),
            })],
            ..Default::default()
        };

        let ap = ApplicationProgram {
            static_: Some(Static {
                parameter_types: Some(crate::ParameterTypes { items: vec![pt] }),
                code: Some(crate::Code {
                    absolute: vec![Segment {
                        id: "AS".into(),
                        size: "600".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            }),
            dynamic: Some(Dynamic {
                nodes: vec![UiNode::Module(instance)],
            }),
            module_defs: Some(ModuleDefs { items: vec![module] }),
            ..Default::default()
        };

        let image = build_parameter_image(&ap, &HashMap::new()).unwrap();
        let seg = image.segments.iter().find(|s| s.id == "AS").unwrap();
        // Value 42 (= 0x2a) written at byte 432 + 16 = 448.
        assert_eq!(seg.data[448], 0x2a);

        let p = &image.placements[0];
        assert_eq!(p.offset, 448);
        assert_eq!(p.param_id, "MD-1_M-1/MD-1_P-1");
    }

    #[test]
    fn decode_parameter_image_roundtrips_device_and_module() {
        use crate::{Argument, Arguments, Dynamic, Memory, ModuleDefs, Parameters, Segment, Static, TextArg};

        let pt = ParameterType {
            id: "PT-1".into(),
            number: Some(crate::TypeNumber {
                size_in_bit: "8".into(),
                ..Default::default()
            }),
            ..Default::default()
        };

        let module = crate::ModuleDef {
            id: "MD-1".into(),
            name: "M".into(),
            arguments: Some(Arguments {
                items: vec![Argument {
                    id: "MD-1_A-1".into(),
                    name: "ParamOffs".into(),
                    allocates: "54".into(),
                    ..Default::default()
                }],
            }),
            static_: Some(Static {
                parameters: Some(Parameters {
                    nodes: vec![ParamNode::Parameter(Parameter {
                        id: "MD-1_P-1".into(),
                        parameter_type: "PT-1".into(),
                        value: "42".into(),
                        memory: Some(Memory {
                            code_segment: "AS".into(),
                            offset: "432".into(),
                            base_offset: "MD-1_A-1".into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    })],
                }),
                ..Default::default()
            }),
            ..Default::default()
        };

        let instance = crate::ModuleInstance {
            id: "MD-1_M-1".into(),
            ref_id: "MD-1".into(),
            args: vec![crate::Arg::Text(TextArg {
                ref_id: "MD-1_A-1".into(),
                value: "16".into(),
            })],
            ..Default::default()
        };

        let ap = ApplicationProgram {
            static_: Some(Static {
                parameter_types: Some(crate::ParameterTypes { items: vec![pt] }),
                parameters: Some(Parameters {
                    nodes: vec![ParamNode::Parameter(Parameter {
                        id: "P-1".into(),
                        parameter_type: "PT-1".into(),
                        value: "1".into(),
                        memory: Some(Memory {
                            code_segment: "AS".into(),
                            offset: "0".into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    })],
                }),
                code: Some(crate::Code {
                    absolute: vec![Segment {
                        id: "AS".into(),
                        size: "600".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            }),
            dynamic: Some(Dynamic {
                nodes: vec![UiNode::Module(instance)],
            }),
            module_defs: Some(ModuleDefs { items: vec![module] }),
            ..Default::default()
        };

        let mut values = HashMap::new();
        values.insert("P-1".to_string(), "9".to_string());
        values.insert("MD-1_M-1/MD-1_P-1".to_string(), "7".to_string());
        let image = build_parameter_image(&ap, &values).unwrap();

        let decoded = decode_parameter_image(&ap, &image.segments).unwrap();
        assert_eq!(decoded.get("P-1").map(String::as_str), Some("9"));
        assert_eq!(
            decoded.get("MD-1_M-1/MD-1_P-1").map(String::as_str),
            Some("7")
        );

        let defaults = parameter_defaults(&ap);
        assert_eq!(defaults.get("P-1").map(String::as_str), Some("1"));
        assert_eq!(
            defaults.get("MD-1_M-1/MD-1_P-1").map(String::as_str),
            Some("42")
        );
    }
}
