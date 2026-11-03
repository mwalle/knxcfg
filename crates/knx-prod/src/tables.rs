//! Memory-image table builders (M5): Group Address Table, Association Table
//! and Group Object Table.
//!
//! These produce the byte content that ETS writes into the table memory
//! segments at download time (the knxprod `<AddressTable>`/
//! `<AssociationTable>`/`<ComObjectTable>` headers give the segment + offset;
//! the entries themselves are empty in knxprod).
//!
//! The entry formats follow the KNX System B realisations (source:
//! `03_05_01 §4.16/4.17/4.18`) and are **byte-confirmed against ETS5**
//! (`docs/ets5-findings.md`):
//!
//! - **Group Address Table** — a **1-octet** length (counting the individual
//!   address), a 2-octet IA, then the 16-bit group addresses (big-endian,
//!   ascending). TSAP = 1-based index into this list.
//! - **Association Table** — a **1-octet** length followed by 2-octet
//!   associations `TSAP` (high octet) `ASAP` (low octet). TSAP is 1-based;
//!   ASAP is the **0-based** group object number (ETS writes `00 06 17` for
//!   GO0/GO6/GO23).
//! - **Group Object Table** — **no length prefix**; one 4-octet descriptor per
//!   record `[data_pointer][flags][00][vft]`, produced by overlaying flags onto
//!   the knxprod template.
//!
//! All multi-octet integers are **big-endian**, matching the KNX spec's
//! `PDT_UNSIGNED_INT` / 16-bit field convention.

use std::collections::HashMap;

use anyhow::{bail, Result};

use crate::{
    ApplicationProgram, Choose, ModuleDef, ModuleInstance, Parameter, ParameterRef, Static, UiNode,
    When,
};

/// One entry of the Association Table (GrOAT format 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Association {
    /// Transport Service Access Point: 1-based index into the Group Address
    /// Table (0 = none / unused).
    pub tsap: u16,
    /// Application Service Access Point: 0-based group object number
    /// (0 = GO0).
    pub asap: u16,
}

/// A complete set of device tables ready for download.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceTables {
    /// Group Address Table bytes (GrAT Realisation Type 7).
    pub address: Vec<u8>,
    /// Association Table bytes (GrOAT format 0).
    pub association: Vec<u8>,
    /// Group Object Table bytes (Realisation Type 7).
    pub group_objects: Vec<u8>,
}

/// Build all three device tables from the group objects and the links.
///
/// `objects` are the device's group objects in ASAP order (index `i` →
/// ASAP `i + 1`). `links` map an ASAP (group object number) to a 16-bit group
/// address; any group object without a link is associated with TSAP 0
/// (unused). `ga_dpt` is ignored here — only the 16-bit address matters.
pub fn build_device_tables(objects: &[GroupObject], links: &[(u16, u16)]) -> DeviceTables {
    let (address, association) = build_address_and_association_tables(links);
    let group_objects = build_group_object_table(objects);
    DeviceTables { address, association, group_objects }
}

/// Build the Group Address Table and Association Table from the project links.
///
/// Each `link` is `(ASAP, group address)`; ASAP is the 0-based group object
/// number (the same numbering returned by [`group_objects`]), and the group
/// address is already in its raw 16-bit form. The GrAT carries the distinct
/// GAs in ascending order; the GrOAT entries reference them by TSAP
/// (1-based index into the GrAT).
pub fn build_address_and_association_tables(links: &[(u16, u16)]) -> (Vec<u8>, Vec<u8>) {
    let mut gas: Vec<u16> = links.iter().map(|&(_, ga)| ga).collect();
    gas.sort_unstable();
    gas.dedup();
    let tsap_of: HashMap<u16, u16> = gas
        .iter()
        .enumerate()
        .map(|(i, &ga)| (ga, i as u16 + 1))
        .collect();

    let address = build_group_address_table(gas);

    let associations: Vec<Association> = links
        .iter()
        .filter_map(|&(asap, ga)| {
            tsap_of.get(&ga).map(|&tsap| Association { tsap, asap })
        })
        .collect();
    let association = build_association_table(associations);

    (address, association)
}

/// Build the list of `GroupObject` descriptors (and their absolute ASAP
/// numbers) from an application program, expanding module group objects.
///
/// Device-level `<ComObjectTable>` objects keep their `Number` as the ASAP.
/// Module `<ComObjects>` objects use `BaseNumber (argument) + Number`, where
/// `BaseNumber` names the module-definition argument (e.g. `BaseObj`,
/// `BaseObjGroupt`, `ObjNumberBase`) whose per-instance value is the stride
/// base. `Allocates` on the argument is the per-instance stride.
///
/// NOTE: this enumerates **all** `Dynamic` module instances, including
/// mutually-exclusive channel modes (e.g. "single" vs "group" input). Those
/// modes overlap in ASAP space; the *active* mode is chosen by `choose`/`when`
/// visibility against the project's parameter values — a downstream concern
/// for the download builder (`docs/ets5-findings.md`).
pub fn group_objects(ap: &ApplicationProgram) -> Vec<(u16, GroupObject)> {
    group_objects_impl(ap, &HashMap::new(), false)
}

/// Like [`group_objects`], but only materializes the `Dynamic` module
/// instances that are actually visible for the given parameter `values`
/// (following `choose`/`when` branches).
///
/// This is the enumeration the download builder must use: mutually-exclusive
/// channel modes share ASAP space, and only the active mode's objects are
/// downloaded (`docs/ets5-findings.md`).
pub fn active_group_objects(
    ap: &ApplicationProgram,
    values: &HashMap<String, String>,
) -> Vec<(u16, GroupObject)> {
    group_objects_impl(ap, values, true)
}

fn group_objects_impl(
    ap: &ApplicationProgram,
    values: &HashMap<String, String>,
    visible_only: bool,
) -> Vec<(u16, GroupObject)> {
    let mut out = Vec::new();

    // Device-level group objects. In visible-only mode they are emitted only
    // when their `ComObjectRef` is selected by the device `Dynamic` tree.
    if let Some(static_) = &ap.static_ {
        if let Some(t) = &static_.com_object_table {
            if visible_only {
                let ref_by_base =
                    visible_refs_for_static(static_, ap.dynamic.as_ref(), values, "");
                let has_refs = static_
                    .com_object_refs
                    .as_ref()
                    .map(|r| !r.items.is_empty())
                    .unwrap_or(false);
                for co in &t.items {
                    if !has_refs {
                        let asap = co.number.parse::<u16>().unwrap_or(0);
                        out.push((asap, group_object_of(co)));
                    } else if let Some(com_ref) = ref_by_base.get(co.id.as_str()) {
                        out.push(group_object_of_ref(co, com_ref));
                    }
                }
            } else {
                for co in &t.items {
                    let asap = co.number.parse::<u16>().unwrap_or(0);
                    out.push((asap, group_object_of(co)));
                }
            }
        }
    }

    // Module group objects, instantiated per module instance.
    let modules: HashMap<&str, &ModuleDef> = ap
        .module_defs
        .as_ref()
        .map(|m| m.items.iter().map(|m| (m.id.as_str(), m)).collect())
        .unwrap_or_default();
    if let Some(dynamic) = &ap.dynamic {
        if visible_only {
            let ctx = build_table_ctx(ap.static_.as_ref());
            let mut instances = Vec::new();
            collect_visible_module_instances(
                &dynamic.nodes,
                &ctx,
                &modules,
                values,
                "",
                &mut instances,
            );
            push_visible_module_group_objects(&instances, values, &mut out);
        } else {
            let mut instances = Vec::new();
            collect_module_instances(&dynamic.nodes, &modules, &mut instances);
            push_module_group_objects(&instances, &mut out);
        }
    }

    out.sort_by_key(|(asap, _)| *asap);
    if visible_only {
        // Visible modes should not overlap in ASAP space; the all-objects
        // enumeration intentionally keeps every possible object, including
        // mutually-exclusive modes that share the same ASAP.
        out.dedup_by_key(|(asap, _)| *asap);
    }
    out
}

fn push_module_group_objects(
    instances: &[(&ModuleInstance, &ModuleDef, String)],
    out: &mut Vec<(u16, GroupObject)>,
) {
    for (instance, module, _prefix) in instances {
        let Some(static_) = &module.static_ else {
            continue;
        };
        let Some(com_objs) = &static_.com_objects else {
            continue;
        };
        for co in &com_objs.items {
            out.push(module_group_object(instance, co));
        }
    }
}

/// Push the group objects for the *visible* module instances, resolving each
/// object through the `ComObjectRef` selected by the module's `Dynamic` tree
/// (the `ComObjectRefRef` elements). The `ComObjectRef` may override the base
/// `ComObject` ObjectSize and flags per channel mode.
fn push_visible_module_group_objects(
    instances: &[(&ModuleInstance, &ModuleDef, String)],
    values: &HashMap<String, String>,
    out: &mut Vec<(u16, GroupObject)>,
) {
    for (instance, module, prefix) in instances {
        let Some(static_) = &module.static_ else {
            continue;
        };
        let Some(com_objs) = &static_.com_objects else {
            continue;
        };

        // Resolve the visible `ComObjectRef` for each base `ComObject` (when
        // the module has a Dynamic tree). When the module defines no
        // `ComObjectRef`s at all, every base object is always visible;
        // otherwise only the objects selected by a `ComObjectRefRef` are
        // visible, and the rest keep the template's default flags.
        let ref_by_base = visible_refs_for_static(static_, module.dynamic.as_ref(), values, prefix);
        let has_refs = static_
            .com_object_refs
            .as_ref()
            .map(|r| !r.items.is_empty())
            .unwrap_or(false);

        for co in &com_objs.items {
            if !has_refs {
                out.push(module_group_object(instance, co));
            } else if let Some(com_ref) = ref_by_base.get(co.id.as_str()) {
                out.push(module_group_object_ref(instance, co, com_ref));
            }
        }
    }
}

fn module_group_object(instance: &ModuleInstance, co: &crate::ComObject) -> (u16, GroupObject) {
    let number = co.number.parse::<u16>().unwrap_or(0);
    let base = module_go_base(instance, co);
    (base + number, group_object_of(co))
}

fn module_group_object_ref(
    instance: &ModuleInstance,
    co: &crate::ComObject,
    com_ref: &crate::ComObjectRef,
) -> (u16, GroupObject) {
    let merged = merged_com_object(co, com_ref);
    let number = merged.number.parse::<u16>().unwrap_or(0);
    let base = module_go_base(instance, &merged);
    (base + number, group_object_of(&merged))
}

/// A device-level group object resolved through its `ComObjectRef` override.
fn group_object_of_ref(
    co: &crate::ComObject,
    com_ref: &crate::ComObjectRef,
) -> (u16, GroupObject) {
    let merged = merged_com_object(co, com_ref);
    let asap = merged.number.parse::<u16>().unwrap_or(0);
    (asap, group_object_of(&merged))
}

/// Merge a `ComObjectRef`'s non-empty overrides onto its base `ComObject`.
fn merged_com_object(
    co: &crate::ComObject,
    com_ref: &crate::ComObjectRef,
) -> crate::ComObject {
    let mut merged = co.clone();
    if !com_ref.object_size.is_empty() {
        merged.object_size = com_ref.object_size.clone();
    }
    if !com_ref.read_flag.is_empty() {
        merged.read_flag = com_ref.read_flag.clone();
    }
    if !com_ref.write_flag.is_empty() {
        merged.write_flag = com_ref.write_flag.clone();
    }
    if !com_ref.communication_flag.is_empty() {
        merged.communication_flag = com_ref.communication_flag.clone();
    }
    if !com_ref.transmit_flag.is_empty() {
        merged.transmit_flag = com_ref.transmit_flag.clone();
    }
    if !com_ref.update_flag.is_empty() {
        merged.update_flag = com_ref.update_flag.clone();
    }
    if !com_ref.read_on_init_flag.is_empty() {
        merged.read_on_init_flag = com_ref.read_on_init_flag.clone();
    }
    merged
}

fn module_go_base(instance: &ModuleInstance, co: &crate::ComObject) -> u16 {
    if co.base_number.is_empty() {
        0
    } else {
        instance
            .arg_value(&co.base_number)
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(0)
    }
}

/// Resolve the `ComObjectRef`s selected by the *visible* branches of a
/// `Dynamic` tree (device-level or a module definition), keyed by their base
/// `ComObject` id. Objects without an entry are not visible.
fn visible_refs_for_static<'a>(
    static_: &'a crate::Static,
    dynamic: Option<&crate::Dynamic>,
    values: &HashMap<String, String>,
    prefix: &str,
) -> HashMap<&'a str, &'a crate::ComObjectRef> {
    let mut ref_by_base = HashMap::new();
    let Some(dynamic) = dynamic else {
        return ref_by_base;
    };
    let ctx = build_table_ctx(Some(static_));
    let mut ref_ids = Vec::new();
    collect_visible_com_object_refs(&dynamic.nodes, &ctx, values, prefix, &mut ref_ids);
    if let Some(com_refs) = &static_.com_object_refs {
        for ref_id in ref_ids {
            if let Some(com_ref) = com_refs.items.iter().find(|r| r.id == ref_id) {
                ref_by_base.insert(com_ref.ref_id.as_str(), com_ref);
            }
        }
    }
    ref_by_base
}

/// Collect the `ComObjectRefRef` ids in the *visible* branches of a module's
/// `Dynamic` tree, mirroring the form engine's `choose`/`when` rules.
fn collect_visible_com_object_refs(
    nodes: &[UiNode],
    ctx: &TableCtx,
    values: &HashMap<String, String>,
    prefix: &str,
    out: &mut Vec<String>,
) {
    for node in nodes {
        match node {
            UiNode::ComObjectRefRef(r) => out.push(r.ref_id.clone()),
            UiNode::Choose(choose) => {
                if let Some(when) = pick_visible_when(choose, ctx, values, prefix) {
                    collect_visible_com_object_refs(&when.nodes, ctx, values, prefix, out);
                }
            }
            _ => {
                if let Some(children) = node.children() {
                    collect_visible_com_object_refs(children, ctx, values, prefix, out);
                }
            }
        }
    }
}

/// Collect every module instance from a `Dynamic` tree, descending into all
/// `when` branches (memory allocation is static, so every instance exists in
/// memory even when it is not shown in the ETS dialog).
fn collect_module_instances<'a>(
    nodes: &'a [UiNode],
    modules: &HashMap<&str, &'a ModuleDef>,
    out: &mut Vec<(&'a ModuleInstance, &'a ModuleDef, String)>,
) {
    for node in nodes {
        match node {
            UiNode::Module(instance) => {
                if let Some(module) = modules.get(instance.ref_id.as_str()) {
                    out.push((instance, module, instance.id.clone()));
                }
            }
            UiNode::Choose(choose) => {
                for when in &choose.whens {
                    collect_module_instances(&when.nodes, modules, out);
                }
            }
            _ => {
                if let Some(children) = node.children() {
                    collect_module_instances(children, modules, out);
                }
            }
        }
    }
}

/// Parameter/ref lookup for the `Dynamic` tree whose module instances we are
/// enumerating (device-level or a module definition).
struct TableCtx<'a> {
    params: HashMap<String, &'a Parameter>,
    refs: HashMap<String, &'a ParameterRef>,
}

fn build_table_ctx<'a>(static_: Option<&'a Static>) -> TableCtx<'a> {
    let mut ctx = TableCtx {
        params: HashMap::new(),
        refs: HashMap::new(),
    };
    let Some(s) = static_ else {
        return ctx;
    };
    if let Some(p) = &s.parameters {
        for param in p.parameters() {
            ctx.params.insert(param.id.clone(), param);
        }
        for u in p.unions() {
            for param in &u.parameters {
                ctx.params.insert(param.id.clone(), param);
            }
        }
    }
    if let Some(pr) = &s.parameter_refs {
        for r in &pr.items {
            ctx.refs.insert(r.id.clone(), r);
        }
    }
    ctx
}

/// Collect the module instances in the *visible* branch of each `choose`/
/// `when`, mirroring the form engine's visibility rules.
fn collect_visible_module_instances<'a>(
    nodes: &'a [UiNode],
    ctx: &TableCtx,
    modules: &HashMap<&str, &'a ModuleDef>,
    values: &HashMap<String, String>,
    prefix: &str,
    out: &mut Vec<(&'a ModuleInstance, &'a ModuleDef, String)>,
) {
    for node in nodes {
        match node {
            UiNode::Module(instance) => {
                if let Some(module) = modules.get(instance.ref_id.as_str()) {
                    let new_prefix = join_prefix(prefix, &instance.id);
                    out.push((instance, module, new_prefix.clone()));
                    if let Some(dynamic) = &module.dynamic {
                        let module_ctx = build_table_ctx(module.static_.as_ref());
                        collect_visible_module_instances(
                            &dynamic.nodes,
                            &module_ctx,
                            modules,
                            values,
                            &new_prefix,
                            out,
                        );
                    }
                }
            }
            UiNode::Choose(choose) => {
                if let Some(when) = pick_visible_when(choose, ctx, values, prefix) {
                    collect_visible_module_instances(
                        &when.nodes,
                        ctx,
                        modules,
                        values,
                        prefix,
                        out,
                    );
                }
            }
            _ => {
                if let Some(children) = node.children() {
                    collect_visible_module_instances(
                        children,
                        ctx,
                        modules,
                        values,
                        prefix,
                        out,
                    );
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

fn pick_visible_when<'a>(
    choose: &'a Choose,
    ctx: &TableCtx,
    values: &HashMap<String, String>,
    prefix: &str,
) -> Option<&'a When> {
    let current = current_param_value(ctx, &choose.param_ref_id, values, prefix);
    choose
        .whens
        .iter()
        .find(|w| !w.is_default() && test_matches(&w.test, &current))
        .or_else(|| choose.whens.iter().find(|w| w.is_default()))
}

fn test_matches(test: &str, current: &str) -> bool {
    test.split_whitespace().any(|t| t == current)
}

fn current_param_value(
    ctx: &TableCtx,
    ref_id: &str,
    values: &HashMap<String, String>,
    prefix: &str,
) -> String {
    let Some(param_id) = ctx.refs.get(ref_id).map(|r| r.ref_id.clone()) else {
        return String::new();
    };
    values
        .get(&format!("{prefix}/{param_id}"))
        .cloned()
        .or_else(|| values.get(&param_id).cloned())
        .or_else(|| ctx.params.get(&param_id).map(|p| p.value.clone()))
        .unwrap_or_default()
}

fn group_object_of(co: &crate::ComObject) -> GroupObject {
    GroupObject {
        // `data_pointer` (the first descriptor octet) is the GO's value
        // address/offset, computed from the knxprod `<Memory>` placements;
        // it is not simply the ASAP. We leave it 0 until the value-area
        // allocation is decoded; the ASAP lives in the tuple returned by
        // `group_objects` and feeds the Association Table.
        data_pointer: 0,
        flags: GroupObjectFlags::from_com_object(co),
        value_field_type: value_field_type_from_size(&co.object_size),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GroupObjectFlags {
    pub transmit: bool,
    pub communicate: bool,
    pub read: bool,
    pub write: bool,
    pub update: bool,
    pub read_on_init: bool,
}

impl GroupObjectFlags {
    /// Build from knxprod `<ComObject>` `*Flag` attributes (values `Enabled`
    /// / `Disabled`, empty = disabled).
    pub fn from_com_object(co: &crate::ComObject) -> Self {
        let on = |s: &str| s == "Enabled";
        Self {
            transmit: on(&co.transmit_flag),
            communicate: on(&co.communication_flag),
            read: on(&co.read_flag),
            write: on(&co.write_flag),
            update: on(&co.update_flag),
            read_on_init: on(&co.read_on_init_flag),
        }
    }
}

/// A group object description (the input to the Group Object Table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupObject {
    /// **Data Pointer** (1 octet): offset of the GO's value MSB in its data
    /// segment (RAM/EEPROM). The segment is chosen by the Segment Selector
    /// bit (bit 5) of `flags` (0→segment 00h, 1→segment 100h). Computed by the
    /// application from the knxprod `<Memory>` placements — see
    /// `docs/ets5-findings.md` (`03_05_01 §4.18.3.1.2`).
    pub data_pointer: u8,
    pub flags: GroupObjectFlags,
    /// Value field type: the object size in bits/octets.
    pub value_field_type: u8,
}

/// The observed 4-octet memory descriptor per group object (mask 0705).
///
/// `[data_pointer][flags][0x00][vft]` — the octet after `flags` (`0x00`) is
/// the low half of the 16-bit `flags`/`config`+`type` pair (see RT1/RT2 in
/// `docs/ets5-findings.md`); `vft` is the value field type.
fn go_descriptor(o: &GroupObject) -> [u8; 4] {
    [
        o.data_pointer,
        go_descriptor_high(o.flags),
        0x00,
        o.value_field_type,
    ]
}

/// Encode the "value field type" code (low octet of the GO descriptor) from
/// an `<ComObject ObjectSize="…">` string, e.g. `"1 Bit"` → 0, `"2 Bytes"` → 8.
///
/// `03_05_01 §4.18.6.2.4.1.4 Table 88`:
///   0..6 → 1..7 bits, then 7 → 1 octet, 8 → 2, 9 → 3, 10 → 4, 11 → 6,
///   12 → 8, 13 → 10, 14 → 14 octets (the remaining sizes are non-standard
///   and not used by `ObjectSize`).
pub fn value_field_type_from_size(size: &str) -> u8 {
    let s = size.trim();
    if let Some(bits) = s.strip_suffix(" Bit") {
        bits.trim().parse::<u8>().map(|n| n - 1).unwrap_or(0)
    } else if let Some(bytes) = s
        .strip_suffix(" Bytes")
        .or_else(|| s.strip_suffix(" Byte"))
    {
        let n: u8 = bytes.trim().parse().unwrap_or(0);
        match n {
            1 => 7,
            2 => 8,
            3 => 9,
            4 => 10,
            6 => 11,
            8 => 12,
            10 => 13,
            14 => 14,
            // Unknown sizes map to 7 (1 octet) by default rather than 0.
            _ => 7,
        }
    } else {
        0
    }
}

/// Build the Group Address Table (GrAT) bytes.
///
/// `gas` are the 16-bit group addresses used by the device, in ascending
/// order. The caller must sort them; we sort defensively here too.
///
/// Confirmed against ETS5 (mask 0705): a **1-octet length** (counted
/// including the individual address), a **2-octet individual address**
/// (`0.0.0` = `00 00`), then the 16-bit GAs (big-endian, ascending). ETS wrote
/// length `04` at 0x4000 and `00 0A 00 0B 00 0C` at 0x4003 for 3 GAs.
pub fn build_group_address_table(mut gas: Vec<u16>) -> Vec<u8> {
    gas.sort_unstable();
    gas.dedup();
    let mut out = Vec::with_capacity(3 + gas.len() * 2);
    // Length = 1 (individual address) + number of GAs.
    out.push((gas.len() + 1) as u8);
    out.extend_from_slice(&[0x00, 0x00]); // individual address 0.0.0
    for ga in gas {
        out.extend_from_slice(&ga.to_be_bytes());
    }
    out
}

/// Build the Association Table (GrOAT format 0) bytes.
///
/// Associations must be sorted by ascending ASAP (group object number); the
/// entries are 2 octets each: `TSAP` (high) then `ASAP` (low).
///
/// The current length is **1 octet** (confirmed against ETS5: `03 01 00 02 06
/// 03 17` for 3 associations).
pub fn build_association_table(mut assocs: Vec<Association>) -> Vec<u8> {
    assocs.sort_by_key(|a| a.asap);
    let mut out = Vec::with_capacity(1 + assocs.len() * 2);
    out.push(assocs.len() as u8);
    for a in assocs {
        out.push(a.tsap as u8);
        out.push(a.asap as u8);
    }
    out
}

/// Decode a Group Address Table byte slice into its 16-bit group addresses,
/// in TSAP order (index 0 → TSAP 1). The input starts at the table's length
/// octet and may include trailing bytes (they are ignored).
pub fn read_group_address_table(data: &[u8]) -> Result<Vec<u16>> {
    let Some(&count) = data.first() else {
        bail!("empty Group Address Table");
    };
    let count = count as usize;
    if count < 1 {
        bail!("Group Address Table length is zero");
    }
    let ga_count = count - 1; // length counts the individual address too.
    let needed = 3 + ga_count * 2;
    if data.len() < needed {
        bail!(
            "Group Address Table truncated: need {} bytes, have {}",
            needed,
            data.len()
        );
    }
    let mut gas = Vec::with_capacity(ga_count);
    for i in 0..ga_count {
        let off = 3 + i * 2;
        gas.push(u16::from_be_bytes([data[off], data[off + 1]]));
    }
    Ok(gas)
}

/// Decode an Association Table (format 0) byte slice. The input starts at the
/// table's length octet and may include trailing bytes.
pub fn read_association_table(data: &[u8]) -> Result<Vec<Association>> {
    let Some(&count) = data.first() else {
        bail!("empty Association Table");
    };
    let count = count as usize;
    let needed = 1 + count * 2;
    if data.len() < needed {
        bail!(
            "Association Table truncated: need {} bytes, have {}",
            needed,
            data.len()
        );
    }
    let mut assocs = Vec::with_capacity(count);
    for i in 0..count {
        let off = 1 + i * 2;
        assocs.push(Association {
            tsap: data[off] as u16,
            asap: data[off + 1] as u16,
        });
    }
    Ok(assocs)
}

/// Decode the project links `(ASAP, group address)` from a GrAT and a GrOAT
/// byte slice. Unused association entries (TSAP 0) are skipped.
pub fn read_links(address: &[u8], association: &[u8]) -> Result<Vec<(u16, u16)>> {
    let gas = read_group_address_table(address)?;
    let assocs = read_association_table(association)?;
    let mut links = Vec::with_capacity(assocs.len());
    for a in assocs {
        if a.tsap == 0 {
            continue;
        }
        let Some(&ga) = gas.get((a.tsap - 1) as usize) else {
            bail!("Association TSAP {} out of range", a.tsap);
        };
        links.push((a.asap, ga));
    }
    links.sort_by_key(|(asap, _)| *asap);
    Ok(links)
}

/// Build the Group Object Table (Realisation Type 7) bytes.
///
/// One 4-octet descriptor per group object, in ASAP order (index 0 → ASAP 0).
/// Confirmed against ETS5 (mask 0705): **no length prefix** — the segment is
/// exactly `26 × 4 = 104` octets. The memory-mapped format is
/// `[data_pointer][flags][00][vft]` (see `docs/ets5-findings.md`).
pub fn build_group_object_table(objects: &[GroupObject]) -> Vec<u8> {
    let mut out = Vec::with_capacity(objects.len() * 4);
    for o in objects {
        out.extend_from_slice(&go_descriptor(o));
    }
    out
}

/// Overlay the group object **flags** onto a knxprod `ComObjectTable`
/// template, keeping the template's `data_pointer` (and trailing octets).
///
/// The authoritative realisation of the mask-0705 GoT: the knxprod template
/// already carries each object's `data_pointer` (value address) in its first
/// octet; ETS only rewrites the **flags** octet.
///
/// The template's header record (first record) carries the entry count in its
/// first octet; the following records are the group-object slots in ASAP
/// order. `flags_by_asap` maps the 0-based ASAP to the final flags octet for
/// the *visible* objects. Slots not present in the map are hidden/inactive:
/// they keep the template's own slot flags with the communication bit cleared.
pub fn overlay_group_object_table(
    template: &[u8],
    flags_by_asap: &HashMap<u16, u8>,
) -> Vec<u8> {
    overlay_group_object_table_at(template, 0, flags_by_asap)
}

/// Like [`overlay_group_object_table`], but the GoT starts at `base_offset`
/// within `template` (the `ComObjectTable/@Offset`, usually 0).
pub fn overlay_group_object_table_at(
    template: &[u8],
    base_offset: usize,
    flags_by_asap: &HashMap<u16, u8>,
) -> Vec<u8> {
    let mut out = template.to_vec();
    let Some(entry_count) = group_object_table_entry_count(template, base_offset) else {
        return out;
    };
    let mut slot = base_offset + 4; // skip the header record
    for asap in 0..entry_count as u16 {
        if slot + 1 >= out.len() {
            break;
        }
        let flags = flags_by_asap
            .get(&asap)
            .copied()
            .unwrap_or(out[slot + 1] & !COMMUNICATION_FLAG);
        out[slot + 1] = flags;
        slot += 4;
    }
    out
}

/// Flag bits (bits 7..2) of the memory-mapped Group Object Descriptor
/// Config Octet (`03_05_01 §4.18.4.1`, used by mask 0705 System B):
///
/// | bit | name | meaning |
/// |----:|------|---------|
/// | 7   | Update enable | set → 1 |
/// | 6   | Transmit enable | set → 1 |
/// | 5   | Segment selector | 0 → segment 00h, 1 → segment 100h |
/// | 4   | Write enable | set → 1 |
/// | 3   | Read enable | set → 1 |
/// | 2   | Communication enable | set → 1 |
///
/// Bit 5 is the **segment selector** (always 0 = segment 00h here), *not* the
/// System-B “value read on init” flag — that feature is not supported by this
/// memory-mapped layout (ETS greys the checkbox out), so it is never encoded.
/// The transmission priority (bits 1..0) is **not** encoded here — it is
/// taken from the knxprod GoT template's slot octet (see
/// [`visible_group_object_flags`] and [`overlay_group_object_table_at`]).
pub fn go_descriptor_high(f: GroupObjectFlags) -> u8 {
    let mut b = 0u8;
    if f.update { b |= 0x80; }
    if f.transmit { b |= 0x40; }
    // bit 5 = segment selector = 0 (segment 00h).
    if f.write { b |= 0x10; }
    if f.read { b |= 0x08; }
    if f.communicate { b |= 0x04; }
    b
}

/// Table 87 bit 2: communication enable.
pub const COMMUNICATION_FLAG: u8 = 0x04;

/// Table 87 bits 1..0: transmission priority mask.
pub const PRIORITY_MASK: u8 = 0x03;

/// Compose the final GoT flags octet for a *visible* group object.
///
/// The flag bits (7..2) come from the resolved knxprod `ComObject`/
/// `ComObjectRef`; the transmission priority (1..0) is taken from the knxprod
/// GoT template's own slot octet. The communication bit is cleared for
/// objects that are not linked to a group address, because an unlinked object
/// cannot communicate.
pub fn visible_group_object_flags(
    flags: GroupObjectFlags,
    template_slot_flags: u8,
    linked: bool,
) -> u8 {
    let mut b = go_descriptor_high(flags) | (template_slot_flags & PRIORITY_MASK);
    if !linked {
        b &= !COMMUNICATION_FLAG;
    }
    b
}

/// The number of group-object slots in a GoT template.
///
/// The template's leading header record carries the entry count in its first
/// octet (`Current Size`, see `03_05_01 §4.18.3.1`). For the MDT mask-0705
/// oracle this is `0x19` = 25 slots in a 26-record template (header + 25 GO
/// slots).
pub fn group_object_table_entry_count(template: &[u8], base_offset: usize) -> Option<usize> {
    template.get(base_offset).copied().map(|n| n as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_address_table_sorted_deduped_with_be_length() {
        // Unsorted + a duplicate → sorted + deduped.
        let bytes = build_group_address_table(vec![0x0119, 0x0019, 0x0019]);
        // 1-octet length (1 + 2 GAs = 3), 2-octet IA 0.0.0, then GAs.
        assert_eq!(&bytes[0..3], &[0x03, 0x00, 0x00]);
        assert_eq!(&bytes[3..5], &[0x00, 0x19]);
        assert_eq!(&bytes[5..7], &[0x01, 0x19]);
        assert_eq!(bytes.len(), 7);
    }

    #[test]
    fn association_table_format0_tsap_high_asap_low() {
        let bytes = build_association_table(vec![
            Association { tsap: 1, asap: 2 },
            Association { tsap: 3, asap: 1 },
        ]);
        // sorted by ASAP: (asap=1, tsap=3) then (asap=2, tsap=1)
        assert_eq!(&bytes[0..1], &[0x02]); // length 2 (1 octet)
        assert_eq!(&bytes[1..3], &[0x03, 0x01]); // tsap 3, asap 1
        assert_eq!(&bytes[3..5], &[0x01, 0x02]); // tsap 1, asap 2
    }

    #[test]
    fn group_object_descriptor_flags() {
        let f = GroupObjectFlags {
            transmit: true,
            communicate: true,
            read: false,
            write: true,
            update: false,
            read_on_init: true,
        };
        // Config octet (RT2): update=0, transmit=1, segment-selector=0,
        // write=1, read=0, comm=1 → 0b0101_0100 = 0x54 (priority not included).
        assert_eq!(go_descriptor_high(f), 0x54);
    }

    #[test]
    fn overlay_group_object_table_keeps_data_pointer() {
        // Header record (entry count 2) + two GO slots (flags 0xdf).
        let template = [
            0x02, 0x07, 0x00, 0x07, // header
            0x1a, 0xdf, 0x00, 0x07, // ASAP 0
            0x22, 0xdf, 0x00, 0x07, // ASAP 1
        ];
        let mut map = std::collections::HashMap::new();
        map.insert(0u16, 0x4f);
        let out = overlay_group_object_table(&template, &map);
        // header record unchanged.
        assert_eq!(&out[0..4], &[0x02, 0x07, 0x00, 0x07]);
        // ASAP 0: visible flags 0x4f, data_pointer 0x1a kept.
        assert_eq!(&out[4..8], &[0x1a, 0x4f, 0x00, 0x07]);
        // ASAP 1: hidden → template slot flags 0xdf with comm cleared = 0xdb.
        assert_eq!(&out[8..12], &[0x22, 0xdb, 0x00, 0x07]);
    }

    #[test]
    fn value_field_type_mapping() {
        assert_eq!(value_field_type_from_size("1 Bit"), 0);
        assert_eq!(value_field_type_from_size("2 Bit"), 1);
        assert_eq!(value_field_type_from_size("1 Byte"), 7);
        assert_eq!(value_field_type_from_size("2 Bytes"), 8);
        assert_eq!(value_field_type_from_size("4 Bytes"), 10);
        assert_eq!(value_field_type_from_size("8 Bytes"), 12);
        assert_eq!(value_field_type_from_size("14 Bytes"), 14);
    }

    #[test]
    fn group_object_table_be_length() {
        let objs = vec![
            GroupObject { data_pointer: 0x19, flags: GroupObjectFlags { communicate: true, ..Default::default() }, value_field_type: 8 },
            GroupObject { data_pointer: 0x1a, flags: GroupObjectFlags::default(), value_field_type: 0 },
        ];
        let bytes = build_group_object_table(&objs);
        // no length prefix; first descriptor: [0x19][0x04][0x00][0x08]
        assert_eq!(&bytes[0..4], &[0x19, 0x04, 0x00, 0x08]);
        // second descriptor: [0x1a][0x00][0x00][0x00]
        assert_eq!(&bytes[4..8], &[0x1a, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn overlay_group_object_table_at_applies_base_offset() {
        let template = [
            0xAA, 0xAA, 0xAA, 0xAA, // prefix before GoT
            0x02, 0x07, 0x00, 0x07, // header (entry count 2)
            0x1a, 0xdf, 0x00, 0x07, // ASAP 0
            0x22, 0xdf, 0x00, 0x07, // ASAP 1
        ];
        let mut map = std::collections::HashMap::new();
        map.insert(1u16, 0x4f);
        let out = overlay_group_object_table_at(&template, 4, &map);
        assert_eq!(&out[0..4], &[0xAA, 0xAA, 0xAA, 0xAA]);
        assert_eq!(&out[4..8], &[0x02, 0x07, 0x00, 0x07]); // header unchanged
        assert_eq!(&out[8..12], &[0x1a, 0xdb, 0x00, 0x07]); // ASAP 0 hidden
        assert_eq!(&out[12..16], &[0x22, 0x4f, 0x00, 0x07]); // ASAP 1 visible
    }

    #[test]
    fn build_address_and_association_tables_matches_device_tables() {
        let links = vec![(0u16, 0x000Au16), (6, 0x000B), (23, 0x000C)];
        let (address, association) = build_address_and_association_tables(&links);
        let device = build_device_tables(&[], &links);
        assert_eq!(address, device.address);
        assert_eq!(association, device.association);
    }

    #[test]
    fn read_links_roundtrips() {
        let links = vec![(0u16, 0x000Au16), (6, 0x000B), (23, 0x000C)];
        let (address, association) = build_address_and_association_tables(&links);
        assert_eq!(read_links(&address, &association).unwrap(), links);
    }

    #[test]
    fn active_group_objects_respects_choose() {
        let device_static = crate::Static {
            parameters: Some(crate::Parameters {
                nodes: vec![crate::ParamNode::Parameter(crate::Parameter {
                    id: "P-1".into(),
                    value: "0".into(),
                    ..Default::default()
                })],
            }),
            parameter_refs: Some(crate::ParameterRefs {
                items: vec![crate::ParameterRef {
                    id: "PR-1".into(),
                    ref_id: "P-1".into(),
                    ..Default::default()
                }],
            }),
            ..Default::default()
        };

        let module_a = crate::ModuleDef {
            id: "MD-A".into(),
            static_: Some(crate::Static {
                com_objects: Some(crate::ComObjects {
                    items: vec![crate::ComObject { number: "0".into(), ..Default::default() }],
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let module_b = crate::ModuleDef {
            id: "MD-B".into(),
            static_: Some(crate::Static {
                com_objects: Some(crate::ComObjects {
                    items: vec![crate::ComObject { number: "0".into(), ..Default::default() }],
                }),
                ..Default::default()
            }),
            ..Default::default()
        };

        let ap = crate::ApplicationProgram {
            static_: Some(device_static),
            dynamic: Some(crate::Dynamic {
                nodes: vec![crate::UiNode::Choose(crate::Choose {
                    param_ref_id: "PR-1".into(),
                    whens: vec![
                        crate::When {
                            test: "0".into(),
                            nodes: vec![crate::UiNode::Module(crate::ModuleInstance {
                                id: "M-A".into(),
                                ref_id: "MD-A".into(),
                                ..Default::default()
                            })],
                            ..Default::default()
                        },
                        crate::When {
                            default: "true".into(),
                            nodes: vec![crate::UiNode::Module(crate::ModuleInstance {
                                id: "M-B".into(),
                                ref_id: "MD-B".into(),
                                ..Default::default()
                            })],
                            ..Default::default()
                        },
                    ],
                })],
            }),
            module_defs: Some(crate::ModuleDefs { items: vec![module_a, module_b] }),
            ..Default::default()
        };

        let mut values = std::collections::HashMap::new();
        values.insert("P-1".to_string(), "0".to_string());
        let active = active_group_objects(&ap, &values);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].0, 0);
    }
}
