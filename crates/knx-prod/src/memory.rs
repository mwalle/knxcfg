//! Memory-image builder (M5, download direction).
//!
//! Overlays parameter values onto the knxprod `<Code>` segment templates:
//!
//! - `<AbsoluteSegment>` / `<RelativeSegment>` carry a base64 `<Data>` byte
//!   template (the manufacturer's default image, possibly with holes).
//! - a `<Parameter>` (or a `<Union>`) may carry a `<Memory CodeSegment
//!   Offset BitOffset [BaseOffset]>` that places its value inside one of those
//!   segments. Module parameters are relative to the instance argument named
//!   by `BaseOffset`.
//! - only the **active member** of a `<Union>` is written (see
//!   [`Evaluation::union_member`]); its own `Offset`/`BitOffset` are relative
//!   to the union's `<Memory>`.
//! - mutually exclusive alternatives (e.g. the module instances of different
//!   channel modes) may share memory. Every memory-backed parameter is
//!   written, in increasing priority: parameters of inactive module
//!   instances, then hidden parameters of active scopes, then active
//!   parameters — so the active configuration always wins. Placements that
//!   were overwritten are listed in [`MemoryImage::shadowed`].
//!
//! Values are taken from an [`Evaluation`], so `ParameterRef` defaults,
//! `Assign`s and calculations are already applied. Every value is validated
//! against its parameter type (enumeration, range, field width); a value that
//! cannot be encoded is an error, never a silent 0 or a truncated number.
//!
//! Bits are packed MSB-first within each byte. Multi-octet numbers are
//! big-endian unless the application declares
//! `<Options ParameterByteOrder="LittleEndian">`.

use std::collections::HashMap;

use anyhow::{anyhow, bail, Result};

use crate::eval::Evaluation;
use crate::{ApplicationProgram, Memory, ParamNode, Parameter, ParameterType, Static};

/// A built memory image: one byte buffer per code segment, plus the
/// placements that were written into it.
#[derive(Debug, Clone)]
pub struct MemoryImage {
    pub segments: Vec<SegmentImage>,
    pub placements: Vec<Placement>,
    /// Keys whose bits were (partly) overwritten by a higher-priority
    /// parameter sharing the same memory.
    pub shadowed: std::collections::HashSet<String>,
}

#[derive(Debug, Clone)]
pub struct SegmentImage {
    pub id: String,
    /// Absolute segments: device address (decimal string). Empty for
    /// relative segments.
    pub address: String,
    pub size: usize,
    /// `RAM`, `EEPROM`, or empty (flash/unspecified).
    pub memory_type: String,
    /// Relative segments: the load state machine (interface object index)
    /// the segment belongs to.
    pub load_state_machine: Option<u8>,
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

/// Build the memory image for a device from user values (keys as in
/// [`crate::eval`]).
pub fn build_parameter_image(
    ap: &ApplicationProgram,
    values: &HashMap<String, String>,
) -> Result<MemoryImage> {
    build_parameter_image_eval(&Evaluation::new(ap, values))
}

/// Build the memory image for an evaluated configuration.
pub fn build_parameter_image_eval(ev: &Evaluation) -> Result<MemoryImage> {
    let ap = ev.ap;
    let static_ = ap
        .static_
        .as_ref()
        .ok_or_else(|| anyhow!("application program has no <Static>"))?;
    let codec = Codec::new(static_);

    let mut segments: Vec<SegmentImage> = static_
        .code
        .as_ref()
        .map(|c| {
            c.absolute
                .iter()
                .map(|s| (s, None))
                .chain(c.relative.iter().map(|s| (s, s.load_state_machine.parse().ok())))
                .map(|(s, lsm)| SegmentImage {
                    id: s.id.clone(),
                    address: s.address.clone(),
                    size: s.size_bytes(),
                    memory_type: s.memory_type.clone(),
                    load_state_machine: lsm,
                    data: s.decoded_data(),
                })
                .collect()
        })
        .unwrap_or_default();
    segments.sort_by(|a, b| {
        let ka = a.address.parse::<u32>().unwrap_or(u32::MAX);
        let kb = b.address.parse::<u32>().unwrap_or(u32::MAX);
        ka.cmp(&kb).then_with(|| a.id.cmp(&b.id))
    });

    let mut placements: Vec<Placement> = Vec::new();
    let mut errors = Vec::new();
    let mut owner: HashMap<(&str, usize), usize> = HashMap::new();
    let mut shadowed = std::collections::HashSet::new();
    let mut slots = memory_slots(ev);
    slots.sort_by_key(|s| s.rank);
    for slot in &slots {
        let value = ev.value(&slot.key).unwrap_or(&slot.param.value).to_string();
        let encoded = match codec.encode(slot.param, &value, slot.width_hint) {
            Ok(Some(e)) => e,
            Ok(None) => continue,
            Err(e) => {
                errors.push(format!("{} = {value:?}: {e}", slot.key));
                continue;
            }
        };
        let Some(segment) = segments.iter_mut().find(|s| s.id == slot.segment) else {
            errors.push(format!(
                "{}: memory segment {} does not exist",
                slot.key, slot.segment
            ));
            continue;
        };
        let abs_bit = slot.offset * 8 + slot.bit_offset;
        let bit_len = encoded.bit_len();
        if abs_bit + bit_len > segment.data.len() * 8 {
            errors.push(format!(
                "{}: {bit_len} bit at byte {} do not fit into segment {} ({} bytes)",
                slot.key,
                slot.offset,
                slot.segment,
                segment.data.len()
            ));
            continue;
        }
        encoded.write(&mut segment.data, abs_bit, codec.little_endian);
        for bit in abs_bit..abs_bit + bit_len {
            if let Some(prev) = owner.insert((slot.segment.as_str(), bit), placements.len()) {
                let prev: &Placement = &placements[prev];
                if slot.rank == Rank::Active && ev.is_param_active(&prev.param_id) {
                    errors.push(format!(
                        "{} and {} are both active and share memory at {}+{}",
                        prev.param_id, slot.key, slot.segment, bit / 8
                    ));
                }
                shadowed.insert(prev.param_id.clone());
            }
        }
        placements.push(Placement {
            segment_id: slot.segment.clone(),
            param_id: slot.key.clone(),
            offset: slot.offset,
            bit_offset: slot.bit_offset,
            bit_len,
            value,
        });
    }
    if !errors.is_empty() {
        errors.dedup();
        bail!("invalid parameter values:\n  {}", errors.join("\n  "));
    }

    Ok(MemoryImage { segments, placements, shadowed })
}

/// Compare the encoded default configuration against the shipped segment
/// templates.
///
/// The template is the manufacturer's default image; ETS overwrites every
/// parameter position, so a difference is not necessarily an error (legacy
/// templates are often stale, and `Assign`/calculations change values). The
/// list is informational: it shows where the default download differs from
/// the factory image.
pub fn check_template(ap: &ApplicationProgram) -> Result<Vec<String>> {
    let image = build_parameter_image(ap, &HashMap::new())?;
    let static_ = ap
        .static_
        .as_ref()
        .ok_or_else(|| anyhow!("no <Static>"))?;
    let templates: HashMap<&str, Vec<u8>> = static_
        .code
        .as_ref()
        .map(|c| c.segments().map(|s| (s.id.as_str(), s.decoded_data())).collect())
        .unwrap_or_default();

    let mut out = Vec::new();
    for p in &image.placements {
        let Some(template) = templates.get(p.segment_id.as_str()) else {
            continue;
        };
        let Some(seg) = image.segments.iter().find(|s| s.id == p.segment_id) else {
            continue;
        };
        if image.shadowed.contains(&p.param_id) {
            continue;
        }
        if bits_differ(&seg.data, template, p.offset * 8 + p.bit_offset, p.bit_len) {
            out.push(format!(
                "{} @ byte {}+{}b ({} bit) default {:?} differs from template",
                crate::eval::short_key(ap, &p.param_id),
                p.offset,
                p.bit_offset,
                p.bit_len,
                p.value
            ));
        }
    }
    Ok(out)
}

/// Read the parameter values back out of a memory image: the inverse of
/// [`build_parameter_image_eval`] for the same evaluation (same union
/// members, same placements).
pub fn decode_parameter_image(
    ev: &Evaluation,
    segments: &[SegmentImage],
) -> Result<HashMap<String, String>> {
    let static_ = ev
        .ap
        .static_
        .as_ref()
        .ok_or_else(|| anyhow!("application program has no <Static>"))?;
    let codec = Codec::new(static_);
    let data: HashMap<&str, &[u8]> = segments
        .iter()
        .map(|s| (s.id.as_str(), s.data.as_slice()))
        .collect();

    let mut values = HashMap::new();
    let mut slots = memory_slots(ev);
    slots.sort_by_key(|s| s.rank);
    for slot in slots {
        let Some(buf) = data.get(slot.segment.as_str()) else {
            continue;
        };
        let abs_bit = slot.offset * 8 + slot.bit_offset;
        if let Some(v) = codec.decode(slot.param, buf, abs_bit, slot.width_hint) {
            values.insert(slot.key, v);
        }
    }
    Ok(values)
}

/// Write priority of a memory slot (later wins).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    /// A parameter of a module instance that is not reached.
    InactiveScope,
    /// A hidden parameter of an active scope.
    Hidden,
    /// A parameter with an active `ParameterRef`.
    Active,
}

/// One memory location to write: a parameter (or the active union member)
/// at an absolute offset within a segment.
struct Slot<'a> {
    rank: Rank,
    key: String,
    param: &'a Parameter,
    segment: String,
    offset: usize,
    bit_offset: usize,
    /// Field width from the enclosing union (for types without their own
    /// `SizeInBit`).
    width_hint: Option<usize>,
}

/// Every memory-backed parameter of every scope (all module instances:
/// memory allocation is static, so inactive instances are written with their
/// effective values too).
fn memory_slots<'a>(ev: &Evaluation<'a>) -> Vec<Slot<'a>> {
    let mut out = Vec::new();
    for (si, scope) in ev.scopes.iter().enumerate() {
        let Some(params) = scope.static_.and_then(|s| s.parameters.as_ref()) else {
            continue;
        };
        let base_of = |mem: &Memory| -> Option<usize> {
            if mem.base_offset.is_empty() {
                Some(0)
            } else {
                scope.arg_value(&mem.base_offset)?.trim().parse().ok()
            }
        };
        for node in &params.nodes {
            match node {
                ParamNode::Parameter(param) => {
                    let Some(mem) = &param.memory else { continue };
                    let Some(base) = base_of(mem) else { continue };
                    let key = scope.key(&param.id);
                    out.push(Slot {
                        rank: rank(ev, si, &key),
                        key,
                        param,
                        segment: mem.code_segment.clone(),
                        offset: base + num(&mem.offset),
                        bit_offset: num(&mem.bit_offset),
                        width_hint: None,
                    });
                }
                ParamNode::Union(union) => {
                    let Some(mem) = &union.memory else { continue };
                    let Some(base) = base_of(mem) else { continue };
                    let Some(member) = ev.union_member(si, union) else { continue };
                    let bit = num(&mem.bit_offset) + num(&member.bit_offset);
                    let key = scope.key(&member.id);
                    out.push(Slot {
                        rank: rank(ev, si, &key),
                        key,
                        param: member,
                        segment: mem.code_segment.clone(),
                        offset: base + num(&mem.offset) + num(&member.offset) + bit / 8,
                        bit_offset: bit % 8,
                        width_hint: union.size_in_bit.parse().ok(),
                    });
                }
            }
        }
    }
    out
}

fn rank(ev: &Evaluation, scope: usize, key: &str) -> Rank {
    if ev.is_param_active(key) {
        Rank::Active
    } else if ev.is_scope_active(scope) {
        Rank::Hidden
    } else {
        Rank::InactiveScope
    }
}

fn num(s: &str) -> usize {
    s.trim().parse().unwrap_or(0)
}

/// Value encoder/decoder for one application's parameter types.
struct Codec<'a> {
    types: HashMap<&'a str, &'a ParameterType>,
    little_endian: bool,
}

/// An encoded parameter value.
#[derive(Debug, PartialEq)]
enum Encoded {
    /// An unsigned bit field of the given width.
    Bits(u64, usize),
    /// Raw octets, written in order (text, colors, binary values).
    Bytes(Vec<u8>),
    /// A multi-octet number, subject to the parameter byte order.
    Number(Vec<u8>),
}

impl Encoded {
    fn bit_len(&self) -> usize {
        match self {
            Encoded::Bits(_, n) => *n,
            Encoded::Bytes(b) | Encoded::Number(b) => b.len() * 8,
        }
    }

    fn write(&self, buf: &mut [u8], abs_bit: usize, little_endian: bool) {
        match self {
            Encoded::Bits(v, n) => {
                if little_endian && *n > 8 && n % 8 == 0 {
                    let be = v.to_be_bytes();
                    let bytes: Vec<u8> = be[8 - n / 8..].iter().rev().copied().collect();
                    write_bytes(buf, &bytes, abs_bit);
                } else {
                    write_bits(buf, *v, abs_bit, *n);
                }
            }
            Encoded::Bytes(b) => write_bytes(buf, b, abs_bit),
            Encoded::Number(b) => {
                if little_endian {
                    let rev: Vec<u8> = b.iter().rev().copied().collect();
                    write_bytes(buf, &rev, abs_bit);
                } else {
                    write_bytes(buf, b, abs_bit);
                }
            }
        }
    }
}

impl<'a> Codec<'a> {
    fn new(static_: &'a Static) -> Self {
        Self {
            types: static_
                .parameter_types
                .as_ref()
                .map(|pt| pt.items.iter().map(|t| (t.id.as_str(), t)).collect())
                .unwrap_or_default(),
            little_endian: static_.little_endian_parameters(),
        }
    }

    fn type_of(&self, param: &Parameter) -> Result<&'a ParameterType> {
        self.types
            .get(param.parameter_type.as_str())
            .copied()
            .ok_or_else(|| anyhow!("unknown parameter type {}", param.parameter_type))
    }

    /// Encode a value. `Ok(None)` for types without a memory representation.
    fn encode(&self, param: &Parameter, value: &str, width_hint: Option<usize>) -> Result<Option<Encoded>> {
        let t = self.type_of(param)?;
        let value = value.trim();
        if let Some(r) = &t.restriction {
            let bits = size(&r.size_in_bit, width_hint)?;
            let e = r
                .enumerations
                .iter()
                .find(|e| e.value == value)
                .ok_or_else(|| {
                    let allowed: Vec<&str> = r.enumerations.iter().map(|e| e.value.as_str()).collect();
                    anyhow!("not one of the allowed values [{}]", allowed.join(", "))
                })?;
            if r.base == "BinaryValue" {
                use base64::Engine;
                let mut bytes = base64::engine::general_purpose::STANDARD
                    .decode(e.binary_value.trim())
                    .map_err(|err| anyhow!("bad BinaryValue {:?}: {err}", e.binary_value))?;
                bytes.resize(bits / 8, 0);
                return Ok(Some(Encoded::Bytes(bytes)));
            }
            let v: u64 = value.parse().map_err(|_| anyhow!("enumeration value is not a number"))?;
            return Ok(Some(Encoded::Bits(check_width(v as i128, bits, false)?, bits)));
        }
        if let Some(n) = &t.number {
            let bits = size(&n.size_in_bit, width_hint)?;
            let v = parse_int(value)?;
            check_range(v as f64, &n.min_inclusive, &n.max_inclusive)?;
            let signed = n.type_ == "signedInt";
            return Ok(Some(Encoded::Bits(check_width(v, bits, signed)?, bits)));
        }
        if let Some(tt) = &t.time {
            let bits = size(&tt.size_in_bit, width_hint)?;
            let v = parse_int(value)?;
            check_range(v as f64, &tt.min_inclusive, &tt.max_inclusive)?;
            return Ok(Some(Encoded::Bits(check_width(v, bits, false)?, bits)));
        }
        if let Some(f) = &t.float {
            let v: f64 = value.parse().map_err(|_| anyhow!("not a number"))?;
            check_range(v, &f.min_inclusive, &f.max_inclusive)?;
            return Ok(Some(Encoded::Number(match f.encoding.as_str() {
                "DPT 9" => encode_dpt9(v)?.to_vec(),
                "IEEE-754 Single" => (v as f32).to_be_bytes().to_vec(),
                "IEEE-754 Double" => v.to_be_bytes().to_vec(),
                other => bail!("unsupported float encoding {other:?}"),
            })));
        }
        if let Some(txt) = &t.text {
            let bits = size(&txt.size_in_bit, width_hint)?;
            let mut bytes = value.as_bytes().to_vec();
            if bytes.len() > bits / 8 {
                bail!("text is {} bytes, the field holds {}", bytes.len(), bits / 8);
            }
            bytes.resize(bits / 8, 0);
            return Ok(Some(Encoded::Bytes(bytes)));
        }
        if t.color.is_some() {
            let hex = value.strip_prefix('#').unwrap_or(value);
            if hex.len() != 6 {
                bail!("expected a color #RRGGBB");
            }
            let v = u32::from_str_radix(hex, 16).map_err(|_| anyhow!("expected a color #RRGGBB"))?;
            return Ok(Some(Encoded::Bytes(v.to_be_bytes()[1..].to_vec())));
        }
        if t.ip_address.is_some() {
            let addr: std::net::Ipv4Addr = value.parse().map_err(|_| anyhow!("not an IPv4 address"))?;
            return Ok(Some(Encoded::Bytes(addr.octets().to_vec())));
        }
        if t.picture.is_some() || t.none.is_some() {
            return Ok(None);
        }
        bail!("parameter type {} has no supported encoding", t.id)
    }

    fn decode(&self, param: &Parameter, data: &[u8], abs_bit: usize, width_hint: Option<usize>) -> Option<String> {
        let t = self.type_of(param).ok()?;
        let le = self.little_endian;
        let number = |bits: usize| -> u64 {
            if le && bits > 8 && bits % 8 == 0 {
                let mut b = read_bytes(data, abs_bit, bits / 8);
                b.reverse();
                b.iter().fold(0u64, |acc, x| (acc << 8) | *x as u64)
            } else {
                read_bits(data, abs_bit, bits)
            }
        };
        if let Some(r) = &t.restriction {
            let bits = size(&r.size_in_bit, width_hint).ok()?;
            if r.base == "BinaryValue" {
                use base64::Engine;
                let bytes = read_bytes(data, abs_bit, bits / 8);
                return r
                    .enumerations
                    .iter()
                    .find(|e| {
                        base64::engine::general_purpose::STANDARD
                            .decode(e.binary_value.trim())
                            .map(|mut b| {
                                b.resize(bits / 8, 0);
                                b == bytes
                            })
                            .unwrap_or(false)
                    })
                    .map(|e| e.value.clone());
            }
            return Some(number(bits).to_string());
        }
        if let Some(n) = &t.number {
            let bits = size(&n.size_in_bit, width_hint).ok()?;
            let raw = number(bits);
            if n.type_ == "signedInt" {
                return Some(sign_extend(raw, bits).to_string());
            }
            return Some(raw.to_string());
        }
        if let Some(tt) = &t.time {
            let bits = size(&tt.size_in_bit, width_hint).ok()?;
            return Some(number(bits).to_string());
        }
        if let Some(f) = &t.float {
            let len = f.size_in_bit()? as usize / 8;
            let mut b = read_bytes(data, abs_bit, len);
            if le {
                b.reverse();
            }
            let v = match f.encoding.as_str() {
                "DPT 9" => decode_dpt9([b[0], b[1]]),
                "IEEE-754 Single" => f32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64,
                _ => f64::from_be_bytes(b.try_into().ok()?),
            };
            return Some(format!("{v}"));
        }
        if let Some(txt) = &t.text {
            let bits = size(&txt.size_in_bit, width_hint).ok()?;
            let bytes = read_bytes(data, abs_bit, bits / 8);
            let end = bytes.iter().rposition(|b| *b != 0).map(|i| i + 1).unwrap_or(0);
            return Some(String::from_utf8_lossy(&bytes[..end]).into_owned());
        }
        if t.color.is_some() {
            let b = read_bytes(data, abs_bit, 3);
            return Some(format!("#{:02X}{:02X}{:02X}", b[0], b[1], b[2]));
        }
        if t.ip_address.is_some() {
            let b = read_bytes(data, abs_bit, 4);
            return Some(format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3]));
        }
        None
    }
}

/// Encode a value as octets (for property-backed parameters, which are
/// compared or written as a whole property value).
pub fn encode_value_bytes(ap: &ApplicationProgram, param: &Parameter, value: &str) -> Result<Vec<u8>> {
    let static_ = ap
        .static_
        .as_ref()
        .ok_or_else(|| anyhow!("application program has no <Static>"))?;
    let codec = Codec::new(static_);
    let Some(encoded) = codec.encode(param, value, None)? else {
        return Ok(Vec::new());
    };
    let mut buf = vec![0u8; encoded.bit_len().div_ceil(8)];
    let pad = buf.len() * 8 - encoded.bit_len();
    encoded.write(&mut buf, pad, codec.little_endian);
    Ok(buf)
}

/// Validate a value against a parameter type without encoding it into an
/// image. Used by the CLI and the TUI to reject input early.
pub fn validate_value(ap: &ApplicationProgram, param: &Parameter, value: &str) -> Result<()> {
    let static_ = ap
        .static_
        .as_ref()
        .ok_or_else(|| anyhow!("application program has no <Static>"))?;
    Codec::new(static_).encode(param, value, None).map(|_| ())
}

fn size(s: &str, hint: Option<usize>) -> Result<usize> {
    s.trim()
        .parse()
        .ok()
        .or(hint)
        .filter(|n| *n > 0 && *n <= 64 * 8)
        .ok_or_else(|| anyhow!("parameter type has no size"))
}

fn parse_int(s: &str) -> Result<i128> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return i128::from_str_radix(hex, 16).map_err(|_| anyhow!("not an integer"));
    }
    if let Ok(v) = s.parse::<i128>() {
        return Ok(v);
    }
    // knxprod defaults are sometimes written as "1.000000000000000E+001".
    match s.parse::<f64>() {
        Ok(f) if f.fract() == 0.0 && f.abs() < 1e18 => Ok(f as i128),
        _ => bail!("not an integer"),
    }
}

fn check_range(v: f64, min: &str, max: &str) -> Result<()> {
    let lo = min.trim().parse::<f64>().ok();
    let hi = max.trim().parse::<f64>().ok();
    if lo.is_some_and(|lo| v < lo) || hi.is_some_and(|hi| v > hi) {
        bail!(
            "out of range {}..{}",
            if min.is_empty() { "" } else { min },
            if max.is_empty() { "" } else { max }
        );
    }
    Ok(())
}

/// Check that `v` fits into `bits` and return its bit pattern.
fn check_width(v: i128, bits: usize, signed: bool) -> Result<u64> {
    let bits = bits.min(64);
    let (lo, hi) = if signed {
        (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
    } else {
        (0, (1i128 << bits) - 1)
    };
    if v < lo || v > hi {
        bail!("does not fit into {bits} bit ({lo}..{hi})");
    }
    let mask = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
    Ok((v as i64 as u64) & mask)
}

/// KNX 2-octet float (DPT 9): `value = 0.01 · M · 2^E`, `MEEEEMMM MMMMMMMM`
/// with a 12-bit two's-complement mantissa (`03_07_02 §3.10`).
pub fn encode_dpt9(v: f64) -> Result<[u8; 2]> {
    if !(-671_088.64..=670_760.96).contains(&v) {
        bail!("out of the DPT 9 range -671088.64..670760.96");
    }
    let mut m = (v * 100.0).round() as i64;
    let mut e = 0u16;
    while !(-2048..=2047).contains(&m) {
        m = (m as f64 / 2.0).round() as i64;
        e += 1;
    }
    let m = (m as i16 as u16) & 0x0FFF;
    let raw = ((m & 0x0800) << 4) | (e << 11) | (m & 0x07FF);
    Ok(raw.to_be_bytes())
}

pub fn decode_dpt9(b: [u8; 2]) -> f64 {
    let raw = u16::from_be_bytes(b);
    let e = ((raw >> 11) & 0x0F) as i32;
    let mut m = (raw & 0x07FF) as i32;
    if raw & 0x8000 != 0 {
        m -= 2048;
    }
    0.01 * m as f64 * 2f64.powi(e)
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

fn write_bytes(buf: &mut [u8], bytes: &[u8], abs_bit: usize) {
    for (i, b) in bytes.iter().enumerate() {
        write_bits(buf, *b as u64, abs_bit + i * 8, 8);
    }
}

fn bits_differ(a: &[u8], b: &[u8], abs_bit: usize, bit_len: usize) -> bool {
    (0..bit_len).any(|i| {
        let pos = abs_bit + i;
        let bit = 7 - (pos % 8);
        let va = a.get(pos / 8).map(|x| (x >> bit) & 1).unwrap_or(0);
        let vb = b.get(pos / 8).map(|x| (x >> bit) & 1).unwrap_or(0);
        va != vb
    })
}

fn read_bits(buf: &[u8], abs_bit: usize, bit_len: usize) -> u64 {
    let mut value = 0u64;
    for i in 0..bit_len {
        let pos = abs_bit + i;
        let bit = 7 - (pos % 8);
        let b = buf.get(pos / 8).map(|x| (x >> bit) & 1).unwrap_or(0);
        value = (value << 1) | (b as u64);
    }
    value
}

fn read_bytes(buf: &[u8], abs_bit: usize, len: usize) -> Vec<u8> {
    (0..len).map(|i| read_bits(buf, abs_bit + i * 8, 8) as u8).collect()
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
    use crate::{
        Argument, Arguments, Code, Dynamic, Enumeration, ModuleDef, ModuleDefs, ModuleInstance,
        ParameterTypes, Parameters, Segment, TextArg, TypeNumber, TypeRestriction, UiNode, Union,
    };

    #[test]
    fn write_bits_is_msb_first_and_big_endian() {
        let mut buf = vec![0u8; 4];
        write_bits(&mut buf, 0x02, 0, 8);
        assert_eq!(buf[0], 0x02);
        write_bits(&mut buf, 0x1234, 8, 16);
        assert_eq!(&buf[1..3], &[0x12, 0x34]);
        let mut b = vec![0u8; 1];
        write_bits(&mut b, 1, 1, 1); // bit offset 1 → bit 6
        assert_eq!(b[0], 0b0100_0000);
    }

    #[test]
    fn bits_differ_compares_ranges() {
        let a = [0b0000_0010u8];
        let b = [0b0000_0000u8];
        assert!(bits_differ(&a, &b, 6, 1));
        assert!(!bits_differ(&a, &b, 0, 6));
    }

    #[test]
    fn segment_decodes_base64_to_size() {
        let s = Segment {
            id: "S".into(),
            size: "4".into(),
            data: Some("AQID".into()),
            ..Default::default()
        };
        assert_eq!(s.decoded_data(), vec![1, 2, 3, 0]);
    }

    #[test]
    fn dpt9_roundtrip() {
        assert_eq!(encode_dpt9(0.0).unwrap(), [0x00, 0x00]);
        assert_eq!(encode_dpt9(20.48).unwrap(), [0x0C, 0x00]);
        assert_eq!(encode_dpt9(-1.0).unwrap(), [0x87, 0x9C]);
        for v in [21.5, -30.0, 1000.0, 15.0, 35000.0] {
            let back = decode_dpt9(encode_dpt9(v).unwrap());
            assert!((back - v).abs() <= v.abs() * 0.01 + 0.01, "{v} -> {back}");
        }
    }

    fn number_type(id: &str, bits: &str, min: &str, max: &str) -> ParameterType {
        ParameterType {
            id: id.into(),
            number: Some(TypeNumber {
                size_in_bit: bits.into(),
                type_: "unsignedInt".into(),
                min_inclusive: min.into(),
                max_inclusive: max.into(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn mem(seg: &str, offset: &str, base: &str) -> Option<Memory> {
        Some(Memory {
            code_segment: seg.into(),
            offset: offset.into(),
            base_offset: base.into(),
            ..Default::default()
        })
    }

    fn ap_with(params: Vec<ParamNode>, types: Vec<ParameterType>, le: bool) -> ApplicationProgram {
        ApplicationProgram {
            static_: Some(Static {
                parameter_types: Some(ParameterTypes { items: types }),
                parameters: Some(Parameters { nodes: params }),
                code: Some(Code {
                    absolute: vec![Segment {
                        id: "AS".into(),
                        size: "16".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                options: le.then(|| crate::AppOptions {
                    parameter_byte_order: "LittleEndian".into(),
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn invalid_values_are_errors() {
        let ap = ap_with(
            vec![ParamNode::Parameter(Parameter {
                id: "P-3".into(),
                parameter_type: "PT-N".into(),
                value: "2".into(),
                memory: mem("AS", "0", ""),
                ..Default::default()
            })],
            vec![number_type("PT-N", "8", "2", "240")],
            false,
        );
        let mut v = HashMap::new();
        v.insert("P-3".to_string(), "abc".to_string());
        assert!(build_parameter_image(&ap, &v).is_err());
        v.insert("P-3".to_string(), "70000".to_string());
        let err = build_parameter_image(&ap, &v).unwrap_err().to_string();
        assert!(err.contains("out of range"), "{err}");
        v.insert("P-3".to_string(), "240".to_string());
        let image = build_parameter_image(&ap, &v).unwrap();
        assert_eq!(image.segments[0].data[0], 240);
    }

    #[test]
    fn enumeration_values_are_checked() {
        let ap = ap_with(
            vec![ParamNode::Parameter(Parameter {
                id: "P-1".into(),
                parameter_type: "PT-E".into(),
                value: "1".into(),
                memory: mem("AS", "0", ""),
                ..Default::default()
            })],
            vec![ParameterType {
                id: "PT-E".into(),
                restriction: Some(TypeRestriction {
                    size_in_bit: "2".into(),
                    enumerations: vec![
                        Enumeration { value: "0".into(), ..Default::default() },
                        Enumeration { value: "1".into(), ..Default::default() },
                    ],
                    ..Default::default()
                }),
                ..Default::default()
            }],
            false,
        );
        let mut v = HashMap::new();
        v.insert("P-1".to_string(), "3".to_string());
        assert!(build_parameter_image(&ap, &v).is_err());
    }

    #[test]
    fn little_endian_numbers() {
        let ap = ap_with(
            vec![ParamNode::Parameter(Parameter {
                id: "P-1".into(),
                parameter_type: "PT-16".into(),
                value: "4660".into(),
                memory: mem("AS", "2", ""),
                ..Default::default()
            })],
            vec![number_type("PT-16", "16", "", "")],
            true,
        );
        let image = build_parameter_image(&ap, &HashMap::new()).unwrap();
        assert_eq!(&image.segments[0].data[2..4], &[0x34, 0x12]);
        let ev = Evaluation::new(&ap, &HashMap::new());
        let back = decode_parameter_image(&ev, &image.segments).unwrap();
        assert_eq!(back["P-1"], "4660");
    }

    #[test]
    fn only_the_union_default_member_is_written() {
        let union = Union {
            size_in_bit: "16".into(),
            memory: mem("AS", "4", ""),
            parameters: vec![
                Parameter {
                    id: "UP-1".into(),
                    parameter_type: "PT-8".into(),
                    value: "1".into(),
                    offset: "0".into(),
                    ..Default::default()
                },
                Parameter {
                    id: "UP-2".into(),
                    parameter_type: "PT-8".into(),
                    value: "2".into(),
                    offset: "1".into(),
                    default_union_parameter: "true".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let ap = ap_with(
            vec![ParamNode::Union(union)],
            vec![number_type("PT-8", "8", "", "")],
            false,
        );
        let image = build_parameter_image(&ap, &HashMap::new()).unwrap();
        // UP-2 (the default member) at union offset 4 + member offset 1.
        assert_eq!(image.placements.len(), 1);
        assert_eq!(image.placements[0].param_id, "UP-2");
        assert_eq!(&image.segments[0].data[4..6], &[0x00, 0x02]);
    }

    fn module_ap(value: &str) -> ApplicationProgram {
        let module = ModuleDef {
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
                        value: value.into(),
                        memory: mem("AS", "432", "MD-1_A-1"),
                        ..Default::default()
                    })],
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let instance = ModuleInstance {
            id: "MD-1_M-1".into(),
            ref_id: "MD-1".into(),
            args: vec![crate::Arg::Text(TextArg {
                ref_id: "MD-1_A-1".into(),
                value: "16".into(),
            })],
            ..Default::default()
        };
        ApplicationProgram {
            static_: Some(Static {
                parameter_types: Some(ParameterTypes {
                    items: vec![number_type("PT-1", "8", "", "")],
                }),
                parameters: Some(Parameters {
                    nodes: vec![ParamNode::Parameter(Parameter {
                        id: "P-1".into(),
                        parameter_type: "PT-1".into(),
                        value: "1".into(),
                        memory: mem("AS", "0", ""),
                        ..Default::default()
                    })],
                }),
                code: Some(Code {
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
        }
    }

    #[test]
    fn module_base_offset_applies_argument_value() {
        let ap = module_ap("42");
        let image = build_parameter_image(&ap, &HashMap::new()).unwrap();
        let seg = &image.segments[0];
        assert_eq!(seg.data[448], 0x2a); // 432 + 16
        let p = image.placements.iter().find(|p| p.param_id == "MD-1_M-1/MD-1_P-1").unwrap();
        assert_eq!(p.offset, 448);
    }

    #[test]
    fn module_values_need_the_instance_key() {
        let ap = module_ap("42");
        let mut values = HashMap::new();
        values.insert("P-1".to_string(), "9".to_string());
        values.insert("MD-1_M-1/MD-1_P-1".to_string(), "7".to_string());
        let image = build_parameter_image(&ap, &values).unwrap();
        let ev = Evaluation::new(&ap, &values);
        let decoded = decode_parameter_image(&ev, &image.segments).unwrap();
        assert_eq!(decoded["P-1"], "9");
        assert_eq!(decoded["MD-1_M-1/MD-1_P-1"], "7");

        // A bare module parameter id no longer applies to every instance.
        let mut bare = HashMap::new();
        bare.insert("MD-1_P-1".to_string(), "7".to_string());
        let ev = Evaluation::new(&ap, &bare);
        assert_eq!(ev.unknown_keys, vec!["MD-1_P-1".to_string()]);
        assert_eq!(ev.value("MD-1_M-1/MD-1_P-1"), Some("42"));
    }
}
