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

use crate::{ApplicationProgram, Parameter, ParameterType, Static};

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
    for param in device_parameters(static_) {
        let Some(mem) = &param.memory else {
            continue;
        };
        let offset: usize = mem.offset.parse().unwrap_or(0);
        let bit_offset: usize = mem.bit_offset.parse().unwrap_or(0);
        let value = values
            .get(&param.id)
            .cloned()
            .unwrap_or_else(|| param.value.clone());

        let type_ = types.get(param.parameter_type.as_str()).copied();
        let encoded = encode(type_, &value)?;

        let Some(segment) = segments.iter_mut().find(|s| s.id == mem.code_segment) else {
            continue;
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
}
