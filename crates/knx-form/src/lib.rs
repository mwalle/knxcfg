//! Form engine: knxprod + current parameter values → neutral form model.
//!
//! knxprod's `Dynamic` tree describes the *pages, order and visibility* of
//! the parameters in the ETS dialog (`docs/knxprod-format.md`):
//!
//! - top-level `ParameterBlock` / `Channel` nodes → **pages** (ETS panes)
//! - nested `ParameterBlock` / `ParameterSeparator` → **section titles**
//! - `ParameterRefRef` → a parameter field
//! - `choose ParamRefId` + `when test=…` → conditional blocks; which branches
//!   are active, and every value shown, comes from [`knx_prod::Evaluation`]
//!   (the same evaluation the memory image and the tables use)
//! - `Module` → a per-channel group of parameters (resolved against its
//!   `ModuleDef`)
//!
//! Display text is resolved through the knxprod `<Languages>` translation
//! table, falling back to the raw `Text`/`Name` attributes (a `ParameterRef`
//! `Text`/`SuffixText` overrides the parameter's). Parameters with
//! `Access="None"` are hidden; `Access="ReadOnly"` fields and fields set by
//! an `Assign` are shown but not editable.

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use knx_prod::{ApplicationProgram, Arg, Evaluation, ModuleDef, ParameterType, UiNode};

#[derive(Debug, Clone, PartialEq)]
pub struct FormPage {
    pub title: String,
    pub sections: Vec<FormSection>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FormSection {
    pub title: Option<String>,
    pub fields: Vec<FormField>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FormField {
    /// Unique key. Device params use the knxprod parameter id; module params
    /// are prefixed with the module instance id.
    pub id: String,
    pub label: String,
    pub kind: FieldKind,
    pub value: String,
    /// `false` for `Access="ReadOnly"` parameters and for values set by an
    /// `Assign`.
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FieldKind {
    Checkbox { off: EnumOption, on: EnumOption },
    Enum { options: Vec<EnumOption> },
    Number { min: Option<String>, max: Option<String>, unit: Option<String> },
    Text,
}

/// One choice of an enumerated parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumOption {
    /// Value stored in the parameter memory / project (knxprod `Value`).
    pub value: String,
    /// Human-readable label (localized).
    pub label: String,
}

type Translations = HashMap<String, HashMap<String, String>>;

/// Evaluate the knxprod parameter tree for one device.
///
/// `values` maps parameter keys to user values; missing entries fall back to
/// the effective knxprod default. Device parameters are keyed by parameter
/// id, module parameters by `"{module_instance_id}/{param_id}"` (see
/// [`knx_prod::eval`]).
pub fn evaluate(
    ap: &ApplicationProgram,
    values: &HashMap<String, String>,
    lang: &str,
) -> Vec<FormPage> {
    evaluate_with(&Evaluation::new(ap, values), lang)
}

/// Build the form for an existing evaluation.
pub fn evaluate_with(ev: &Evaluation, lang: &str) -> Vec<FormPage> {
    let ap = ev.ap;
    let modules: HashMap<String, &ModuleDef> = ap
        .module_defs
        .as_ref()
        .map(|m| m.items.iter().map(|m| (m.id.clone(), m)).collect())
        .unwrap_or_default();
    let ctx = Ctx {
        ev,
        types: collect_types(ap),
        translations: ap.translations(lang),
        modules,
    };

    let mut out = Vec::new();
    if let Some(dynamic) = &ap.dynamic {
        walk_pages(&dynamic.nodes, &ctx, 0, "", &mut out);
    }
    out
}

/// Check a user value for a form field id before storing it.
pub fn validate(ap: &ApplicationProgram, id: &str, value: &str) -> Result<()> {
    let ev = Evaluation::new(ap, &HashMap::new());
    let (prefix, param_id) = match id.rsplit_once('/') {
        Some((p, id)) => (p, id),
        None => ("", id),
    };
    let scope = ev
        .scope_index(prefix)
        .ok_or_else(|| anyhow!("no module instance {prefix}"))?;
    let param = ev.scopes[scope]
        .parameter(param_id)
        .ok_or_else(|| anyhow!("no parameter {param_id}"))?;
    knx_prod::validate_value(ap, param, value)
}

/// Whether a parameter can be changed by the user: at least one of its refs
/// (or the parameter itself, for refs without `Access`) is neither `None`
/// nor `ReadOnly`.
pub fn is_editable(ap: &ApplicationProgram, id: &str) -> bool {
    let ev = Evaluation::new(ap, &HashMap::new());
    let (prefix, param_id) = id.rsplit_once('/').unwrap_or(("", id));
    let Some(scope) = ev.scope_index(prefix).map(|i| &ev.scopes[i]) else {
        return false;
    };
    let Some(param) = scope.parameter(param_id) else {
        return false;
    };
    let ok = |a: &str| a != "None" && a != "ReadOnly";
    let refs: Vec<_> = scope
        .static_
        .and_then(|s| s.parameter_refs.as_ref())
        .map(|r| r.items.iter().filter(|r| r.ref_id == param.id).collect())
        .unwrap_or_default();
    refs.iter()
        .any(|r| ok(if r.access.is_empty() { &param.access } else { &r.access }))
}

/// Lookups shared by the whole walk.
struct Ctx<'a> {
    ev: &'a Evaluation<'a>,
    types: HashMap<String, &'a ParameterType>,
    translations: Translations,
    modules: HashMap<String, &'a ModuleDef>,
}

fn collect_types(ap: &ApplicationProgram) -> HashMap<String, &ParameterType> {
    let mut types = HashMap::new();
    if let Some(pt) = ap.static_.as_ref().and_then(|s| s.parameter_types.as_ref()) {
        for t in &pt.items {
            types.insert(t.id.clone(), t);
        }
    }
    types
}

/// Walk at *page* level: top-level `ParameterBlock`/`Channel`/`Module`
/// become pages (ETS panes).
fn walk_pages(nodes: &[UiNode], ctx: &Ctx, scope: usize, chan_label: &str, out: &mut Vec<FormPage>) {
    for node in nodes {
        match node {
            UiNode::ChannelIndependentBlock(block) => {
                walk_pages(&block.nodes, ctx, scope, chan_label, out);
            }
            UiNode::Channel(ch) => {
                let title = channel_title(ch, ctx, scope, chan_label);
                let mut sections = Vec::new();
                walk_sections(&ch.nodes, ctx, scope, chan_label, &mut sections);
                out.push(FormPage { title, sections });
            }
            UiNode::ParameterBlock(pb) => {
                let title = block_title(pb, ctx, scope, chan_label);
                let mut sections = Vec::new();
                walk_sections(&pb.nodes, ctx, scope, chan_label, &mut sections);
                out.push(FormPage { title, sections });
            }
            UiNode::Choose(choose) => {
                for when in ctx.ev.active_whens(scope, choose) {
                    walk_pages(&when.nodes, ctx, scope, chan_label, out);
                }
            }
            UiNode::Module(instance) => {
                if let Some((module, inner)) = module_scope(ctx, scope, instance) {
                    let title = module_title(instance, module, ctx, inner);
                    let label = text_arg_value(instance);
                    let mut sections = Vec::new();
                    if let Some(dynamic) = &module.dynamic {
                        walk_sections(module_content(dynamic), ctx, inner, &label, &mut sections);
                    }
                    out.push(FormPage { title, sections });
                }
            }
            _ => {}
        }
    }
}

/// The definition and evaluation scope of a module instance.
fn module_scope<'a>(
    ctx: &Ctx<'a>,
    scope: usize,
    instance: &knx_prod::ModuleInstance,
) -> Option<(&'a ModuleDef, usize)> {
    let module = ctx.modules.get(instance.ref_id.as_str()).copied()?;
    let prefix = ctx.ev.scopes[scope].key(&instance.id);
    Some((module, ctx.ev.scope_index(&prefix)?))
}

/// Walk at *section* level: nested blocks become section titles, refs become
/// fields.
fn walk_sections(nodes: &[UiNode], ctx: &Ctx, scope: usize, chan_label: &str, out: &mut Vec<FormSection>) {
    for node in nodes {
        match node {
            UiNode::ChannelIndependentBlock(block) => {
                walk_sections(&block.nodes, ctx, scope, chan_label, out);
            }
            UiNode::Channel(ch) => {
                push_title(out, channel_title(ch, ctx, scope, chan_label));
                walk_sections(&ch.nodes, ctx, scope, chan_label, out);
            }
            UiNode::ParameterBlock(pb) => {
                push_title(out, block_title(pb, ctx, scope, chan_label));
                walk_sections(&pb.nodes, ctx, scope, chan_label, out);
            }
            UiNode::ParameterRefRef(r) => {
                if let Some(field) = resolve_field(&r.ref_id, &r.alias_name, ctx, scope) {
                    match out.last_mut() {
                        Some(section) => section.fields.push(field),
                        None => out.push(FormSection { title: None, fields: vec![field] }),
                    }
                }
            }
            UiNode::ParameterSeparator(sep) => {
                push_title(out, translated(sep.id.as_str(), sep.text.as_str(), &ctx.translations));
            }
            UiNode::Choose(choose) => {
                for when in ctx.ev.active_whens(scope, choose) {
                    walk_sections(&when.nodes, ctx, scope, chan_label, out);
                }
            }
            UiNode::Module(instance) => {
                if let Some((module, inner)) = module_scope(ctx, scope, instance) {
                    push_title(out, module_title(instance, module, ctx, inner));
                    if let Some(dynamic) = &module.dynamic {
                        let label = text_arg_value(instance);
                        walk_sections(module_content(dynamic), ctx, inner, &label, out);
                    }
                }
            }
            UiNode::ComObjectRefRef(_) | UiNode::Assign(_) | UiNode::Other => {}
        }
    }
}

/// A module definition's top-level `ParameterBlock` or `Channel` carries the
/// module title (e.g. `Text="Logic {{ChNo}}: {{0}}"`); its children are the
/// actual content, so unwrap it instead of rendering a redundant section
/// header.
fn module_content(dynamic: &knx_prod::Dynamic) -> &[UiNode] {
    if dynamic.nodes.len() == 1 {
        match &dynamic.nodes[0] {
            UiNode::ParameterBlock(pb) => return &pb.nodes,
            UiNode::Channel(ch) => return &ch.nodes,
            _ => {}
        }
    }
    &dynamic.nodes
}

fn block_title(pb: &knx_prod::ParameterBlock, ctx: &Ctx, scope: usize, chan_label: &str) -> String {
    let text = translated(pb.id.as_str(), pb.text.as_str(), &ctx.translations);
    fill_template(text, &pb.text_parameter_ref_id, ctx, scope, chan_label)
}

fn channel_title(ch: &knx_prod::Channel, ctx: &Ctx, scope: usize, chan_label: &str) -> String {
    let text = translated(ch.id.as_str(), ch.text.as_str(), &ctx.translations);
    fill_template(text, &ch.text_parameter_ref_id, ctx, scope, chan_label)
}

/// Substitute the `{{0}}` (value of the `TextParameterRefId` parameter) and
/// `{{ChNo}}` placeholders of a block/channel text.
///
/// "Channel {{ChNo}}: {{0}}" with an empty `{{0}}` → "Channel A", not
/// "Channel A:".
fn fill_template(text: String, text_ref: &str, ctx: &Ctx, scope: usize, chan_label: &str) -> String {
    if !text.contains("{{") {
        return text;
    }
    let v = if text_ref.is_empty() {
        ""
    } else {
        ctx.ev.ref_value(scope, text_ref).unwrap_or("")
    };
    let text = text.replace("{{0}}", v).replace("{{ChNo}}", chan_label);
    text.trim_end().trim_end_matches(':').trim_end().to_string()
}

fn translated(id: &str, raw: &str, translations: &Translations) -> String {
    translations
        .get(id)
        .and_then(|a| a.get("Text"))
        .cloned()
        .unwrap_or_else(|| raw.to_string())
}

fn push_title(out: &mut Vec<FormSection>, title: String) {
    if !title.is_empty() {
        out.push(FormSection { title: Some(title), fields: Vec::new() });
    }
}

/// Title of a module instance.
///
/// ETS takes the instance's `Name` attribute when present. Instances without
/// a `Name` (e.g. MDT's `Logic` modules) get their title from the module
/// definition's top-level `ParameterBlock` (or `Channel`, e.g. MDT's
/// actuator channels) `Text` template, with the `{{ChNo}}` and `{{0}}`
/// placeholders substituted:
///
/// ```xml
/// <ModuleDef Name="ModuleDefLogic">
///   <Arguments><Argument Name="ChNo" …/></Arguments>
///   <Dynamic>
///     <ParameterBlock Text="Logic {{ChNo}}: {{0}}" TextParameterRefId="…P-3…"/>
/// ```
///
/// → "Logic 1", "Logic 2", … (`{{ChNo}}` = the instance's `ChNo` argument,
/// `{{0}}` = the referenced free-text parameter, empty by default).
fn module_title(instance: &knx_prod::ModuleInstance, module: &ModuleDef, ctx: &Ctx, scope: usize) -> String {
    if !instance.name.is_empty() {
        let text_arg = text_arg_value(instance);
        if !text_arg.is_empty() {
            format!("{} ({text_arg})", instance.name)
        } else {
            instance.name.clone()
        }
    } else if let Some(dynamic) = &module.dynamic {
        let chno = module_arg_value(instance, module, "ChNo");
        let title = dynamic.nodes.iter().find_map(|n| match n {
            UiNode::ParameterBlock(pb) => Some(block_title(pb, ctx, scope, &chno)),
            UiNode::Channel(ch) => Some(channel_title(ch, ctx, scope, &chno)),
            _ => None,
        });
        match title {
            Some(t) if !t.is_empty() => t,
            _ => module.name.clone(),
        }
    } else {
        module.name.clone()
    }
}

/// Value of one module-definition argument by its `Name` (e.g. `ChNo`),
/// whether supplied as a `TextArg` or a `NumericArg`.
fn module_arg_value(instance: &knx_prod::ModuleInstance, module: &ModuleDef, name: &str) -> String {
    let Some(arguments) = &module.arguments else {
        return String::new();
    };
    let Some(arg_def) = arguments.items.iter().find(|a| a.name == name) else {
        return String::new();
    };
    instance
        .args
        .iter()
        .find_map(|a| match a {
            Arg::Text(t) if t.ref_id == arg_def.id => Some(t.value.clone()),
            Arg::Numeric(n) if n.ref_id == arg_def.id => Some(n.value.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

fn text_arg_value(instance: &knx_prod::ModuleInstance) -> String {
    instance
        .args
        .iter()
        .find_map(|a| match a {
            Arg::Text(t) if !t.value.is_empty() => Some(t.value.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

fn resolve_field(ref_id: &str, alias: &str, ctx: &Ctx, scope: usize) -> Option<FormField> {
    let sc = &ctx.ev.scopes[scope];
    let pref = sc.parameter_ref(ref_id)?;
    let param = sc.parameter(&pref.ref_id)?;

    let access = if pref.access.is_empty() { &param.access } else { &pref.access };
    if access == "None" {
        return None; // hidden parameter
    }
    let id = sc.key(&param.id);
    let enabled = access != "ReadOnly" && !ctx.ev.is_forced(&id);

    let t = &ctx.translations;
    let attr = |id: &str, name: &str| t.get(id).and_then(|a| a.get(name)).cloned();
    let label = if !alias.is_empty() {
        alias.to_string()
    } else {
        attr(&pref.id, "Text")
            .or_else(|| (!pref.text.is_empty()).then(|| pref.text.clone()))
            .or_else(|| attr(&param.id, "Text"))
            .or_else(|| (!param.text.is_empty()).then(|| param.text.clone()))
            .unwrap_or_else(|| param.name.clone())
    };
    let suffix = attr(&pref.id, "SuffixText")
        .or_else(|| (!pref.suffix_text.is_empty()).then(|| pref.suffix_text.clone()))
        .or_else(|| attr(&param.id, "SuffixText"))
        .unwrap_or_else(|| param.suffix_text.clone());

    let type_ = ctx.types.get(param.parameter_type.as_str()).copied();
    Some(FormField {
        value: ctx.ev.value(&id).unwrap_or(&param.value).to_string(),
        id,
        label,
        kind: field_kind(type_, &suffix, t),
        enabled,
    })
}

fn field_kind(type_: Option<&ParameterType>, suffix: &str, translations: &Translations) -> FieldKind {
    let Some(t) = type_ else {
        return FieldKind::Text;
    };
    let unit = opt(suffix.to_string());
    if let Some(n) = &t.number {
        if n.ui_hint == "CheckBox" {
            return FieldKind::Checkbox {
                off: EnumOption { value: "0".into(), label: "off".into() },
                on: EnumOption { value: "1".into(), label: "on".into() },
            };
        }
        return FieldKind::Number {
            min: opt(n.min_inclusive.clone()),
            max: opt(n.max_inclusive.clone()),
            unit,
        };
    }
    if let Some(r) = &t.restriction {
        let options = r
            .enumerations
            .iter()
            .map(|e| EnumOption {
                value: e.value.clone(),
                label: translated(e.id.as_str(), e.text.as_str(), translations),
            })
            .collect();
        return FieldKind::Enum { options };
    }
    if let Some(f) = &t.float {
        return FieldKind::Number {
            min: opt(f.min_inclusive.clone()),
            max: opt(f.max_inclusive.clone()),
            unit,
        };
    }
    if let Some(tt) = &t.time {
        return FieldKind::Number {
            min: opt(tt.min_inclusive.clone()),
            max: opt(tt.max_inclusive.clone()),
            unit: unit.or_else(|| opt(tt.unit.to_lowercase())),
        };
    }
    FieldKind::Text
}

fn opt(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_value_enum_is_not_a_checkbox() {
        let pt = ParameterType {
            id: "PT-1".into(),
            name: "bool".into(),
            restriction: Some(knx_prod::TypeRestriction {
                size_in_bit: "1".into(),
                enumerations: vec![
                    knx_prod::Enumeration { id: "E-0".into(), text: "no".into(), value: "0".into(), ..Default::default() },
                    knx_prod::Enumeration { id: "E-1".into(), text: "yes".into(), value: "1".into(), ..Default::default() },
                ],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            field_kind(Some(&pt), "", &HashMap::new()),
            FieldKind::Enum {
                options: vec![
                    EnumOption { value: "0".into(), label: "no".into() },
                    EnumOption { value: "1".into(), label: "yes".into() },
                ],
            }
        );
    }

    #[test]
    fn checkbox_from_ui_hint() {
        let pt = ParameterType {
            id: "PT-1".into(),
            number: Some(knx_prod::TypeNumber {
                size_in_bit: "1".into(),
                ui_hint: "CheckBox".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(matches!(field_kind(Some(&pt), "", &HashMap::new()), FieldKind::Checkbox { .. }));
    }
}
