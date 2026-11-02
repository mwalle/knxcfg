//! Form engine: knxprod + current parameter values → neutral form model.
//!
//! knxprod's `Dynamic` tree describes the *pages, order and visibility* of
//! the parameters in the ETS dialog (`docs/knxprod-format.md`):
//!
//! - top-level `ParameterBlock` / `Channel` nodes → **pages** (ETS panes)
//! - nested `ParameterBlock` / `ParameterSeparator` → **section titles**
//! - `ParameterRefRef` → a parameter field
//! - `choose ParamRefId` + `when test=…` → conditional blocks: a branch is
//!   visible when the referenced parameter's value equals `test`
//! - `Module` → a per-channel group of parameters (resolved against its
//!   `ModuleDef`)
//!
//! Display text is resolved through the knxprod `<Languages>` translation
//! table, falling back to the raw `Text`/`Name` attributes. Parameters with
//! `Access="None"` are hidden; `Access="ReadOnly"` fields are shown but not
//! editable.

use std::collections::HashMap;

use knx_prod::{
    ApplicationProgram, Arg, ModuleDef, Parameter, ParameterRef, ParameterType, Static, UiNode,
};

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
    /// `false` for `Access="ReadOnly"` parameters.
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FieldKind {
    Checkbox { off: String, on: String },
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
/// `values` maps parameter ids to current values; missing entries fall back
/// to the knxprod default. For module parameters the key is
/// `"{module_instance_id}/{param_id}"` (falling back to `param_id`).
pub fn evaluate(
    ap: &ApplicationProgram,
    values: &HashMap<String, String>,
    lang: &str,
) -> Vec<FormPage> {
    let modules: HashMap<String, &ModuleDef> = ap
        .module_defs
        .as_ref()
        .map(|m| m.items.iter().map(|m| (m.id.clone(), m)).collect())
        .unwrap_or_default();

    let global_types = collect_types(ap.static_.as_ref());
    let translations = ap.translations(lang);

    let mut out = Vec::new();
    if let Some(dynamic) = &ap.dynamic {
        let ctx = build_ctx(ap.static_.as_ref());
        walk_pages(
            &dynamic.nodes,
            &ctx,
            &global_types,
            &translations,
            &modules,
            values,
            "",
            "",
            &mut out,
        );
    }
    out
}

/// Indexed lookups over one `Static` block (device or module definition).
struct Ctx<'a> {
    params: HashMap<String, &'a Parameter>,
    refs: HashMap<String, &'a ParameterRef>,
}

fn build_ctx<'a>(static_: Option<&'a Static>) -> Ctx<'a> {
    let mut ctx = Ctx {
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

fn collect_types<'a>(static_: Option<&'a Static>) -> HashMap<String, &'a ParameterType> {
    let mut types = HashMap::new();
    if let Some(s) = static_ {
        if let Some(pt) = &s.parameter_types {
            for t in &pt.items {
                types.insert(t.id.clone(), t);
            }
        }
    }
    types
}

/// Walk at *page* level: top-level `ParameterBlock`/`Channel`/`Module`
/// become pages (ETS panes).
#[allow(clippy::too_many_arguments)]
fn walk_pages(
    nodes: &[UiNode],
    ctx: &Ctx,
    global_types: &HashMap<String, &ParameterType>,
    translations: &Translations,
    modules: &HashMap<String, &ModuleDef>,
    values: &HashMap<String, String>,
    prefix: &str,
    chan_label: &str,
    out: &mut Vec<FormPage>,
) {
    for node in nodes {
        match node {
            UiNode::ChannelIndependentBlock(block) => {
                walk_pages(&block.nodes, ctx, global_types, translations, modules, values, prefix, chan_label, out);
            }
            UiNode::Channel(ch) => {
                let title = translated(ch.id.as_str(), ch.text.as_str(), translations);
                let mut sections = Vec::new();
                walk_sections(&ch.nodes, ctx, global_types, translations, modules, values, prefix, chan_label, &mut sections);
                out.push(FormPage { title, sections });
            }
            UiNode::ParameterBlock(pb) => {
                let title = block_title(pb, ctx, translations, values, prefix, chan_label);
                let mut sections = Vec::new();
                walk_sections(&pb.nodes, ctx, global_types, translations, modules, values, prefix, chan_label, &mut sections);
                out.push(FormPage { title, sections });
            }
            UiNode::Choose(choose) => {
                if let Some(when) = pick_branch(choose, ctx, values, prefix) {
                    walk_pages(&when.nodes, ctx, global_types, translations, modules, values, prefix, chan_label, out);
                }
            }
            UiNode::Module(instance) => {
                if let Some(module) = modules.get(instance.ref_id.as_str()).copied() {
                    let module_ctx = build_ctx(module.static_.as_ref());
                    let new_prefix = join_prefix(prefix, &instance.id);
                    let title = module_title(instance, module, &module_ctx, translations, values, &new_prefix);
                    let label = text_arg_value(instance);
                    let mut sections = Vec::new();
                    if let Some(dynamic) = &module.dynamic {
                        walk_sections(module_content(dynamic), &module_ctx, global_types, translations, modules, values, &new_prefix, &label, &mut sections);
                    }
                    out.push(FormPage { title, sections });
                }
            }
            _ => {}
        }
    }
}

/// Walk at *section* level: nested blocks become section titles, refs become
/// fields.
#[allow(clippy::too_many_arguments)]
fn walk_sections(
    nodes: &[UiNode],
    ctx: &Ctx,
    global_types: &HashMap<String, &ParameterType>,
    translations: &Translations,
    modules: &HashMap<String, &ModuleDef>,
    values: &HashMap<String, String>,
    prefix: &str,
    chan_label: &str,
    out: &mut Vec<FormSection>,
) {
    for node in nodes {
        match node {
            UiNode::ChannelIndependentBlock(block) => {
                walk_sections(&block.nodes, ctx, global_types, translations, modules, values, prefix, chan_label, out);
            }
            UiNode::Channel(ch) => {
                push_title(out, translated(ch.id.as_str(), ch.text.as_str(), translations));
                walk_sections(&ch.nodes, ctx, global_types, translations, modules, values, prefix, chan_label, out);
            }
            UiNode::ParameterBlock(pb) => {
                push_title(out, block_title(pb, ctx, translations, values, prefix, chan_label));
                walk_sections(&pb.nodes, ctx, global_types, translations, modules, values, prefix, chan_label, out);
            }
            UiNode::ParameterRefRef(r) => {
                if let Some(field) =
                    resolve_field(&r.ref_id, &r.alias_name, ctx, global_types, translations, values, prefix)
                {
                    match out.last_mut() {
                        Some(section) => section.fields.push(field),
                        None => out.push(FormSection { title: None, fields: vec![field] }),
                    }
                }
            }
            UiNode::ParameterSeparator(sep) => {
                push_title(out, translated(sep.id.as_str(), sep.text.as_str(), translations));
            }
            UiNode::Choose(choose) => {
                if let Some(when) = pick_branch(choose, ctx, values, prefix) {
                    walk_sections(&when.nodes, ctx, global_types, translations, modules, values, prefix, chan_label, out);
                }
            }
            UiNode::Module(instance) => {
                if let Some(module) = modules.get(instance.ref_id.as_str()).copied() {
                    let module_ctx = build_ctx(module.static_.as_ref());
                    let new_prefix = join_prefix(prefix, &instance.id);
                    push_title(out, module_title(instance, module, &module_ctx, translations, values, &new_prefix));
                    if let Some(dynamic) = &module.dynamic {
                        let label = text_arg_value(instance);
                        walk_sections(module_content(dynamic), &module_ctx, global_types, translations, modules, values, &new_prefix, &label, out);
                    }
                }
            }
            UiNode::ComObjectRefRef(_) | UiNode::Assign(_) | UiNode::Other => {}
        }
    }
}

/// A module definition's top-level `ParameterBlock` carries the module title
/// (e.g. `Text="Logic {{ChNo}}: {{0}}"`); its children are the actual
/// content, so unwrap it instead of rendering a redundant section header.
fn module_content(dynamic: &knx_prod::Dynamic) -> &[UiNode] {
    if dynamic.nodes.len() == 1 {
        if let UiNode::ParameterBlock(pb) = &dynamic.nodes[0] {
            return &pb.nodes;
        }
    }
    &dynamic.nodes
}

fn join_prefix(prefix: &str, id: &str) -> String {
    if prefix.is_empty() {
        id.to_string()
    } else {
        format!("{prefix}/{id}")
    }
}

fn pick_branch<'a>(
    choose: &'a knx_prod::Choose,
    ctx: &Ctx,
    values: &HashMap<String, String>,
    prefix: &str,
) -> Option<&'a knx_prod::When> {
    let current = current_value(ctx, &choose.param_ref_id, values, prefix);
    choose
        .whens
        .iter()
        .find(|w| !w.is_default() && w.test == current)
        .or_else(|| choose.whens.iter().find(|w| w.is_default()))
}

fn block_title(
    pb: &knx_prod::ParameterBlock,
    ctx: &Ctx,
    translations: &Translations,
    values: &HashMap<String, String>,
    prefix: &str,
    chan_label: &str,
) -> String {
    let mut text = translated(pb.id.as_str(), pb.text.as_str(), translations);
    if !pb.text_parameter_ref_id.is_empty() {
        let v = current_value(ctx, &pb.text_parameter_ref_id, values, prefix);
        text = text.replace("{{0}}", &v);
    }
    text.replace("{{ChNo}}", chan_label)
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
/// definition's top-level `ParameterBlock` `Text` template, with the
/// `{{ChNo}}` and `{{0}}` placeholders substituted:
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
fn module_title(
    instance: &knx_prod::ModuleInstance,
    module: &ModuleDef,
    ctx: &Ctx,
    translations: &Translations,
    values: &HashMap<String, String>,
    prefix: &str,
) -> String {
    if !instance.name.is_empty() {
        let text_arg = text_arg_value(instance);
        if !text_arg.is_empty() {
            format!("{} ({text_arg})", instance.name)
        } else {
            instance.name.clone()
        }
    } else if let Some(dynamic) = &module.dynamic {
        if let Some(pb) = dynamic.nodes.iter().find_map(|n| match n {
            UiNode::ParameterBlock(pb) => Some(pb),
            _ => None,
        }) {
            let chno = module_arg_value(instance, module, "ChNo");
            let mut text = translated(pb.id.as_str(), pb.text.as_str(), translations);
            if !pb.text_parameter_ref_id.is_empty() {
                let v = current_value(ctx, &pb.text_parameter_ref_id, values, prefix);
                text = text.replace("{{0}}", &v);
            }
            let title = text.replace("{{ChNo}}", &chno);
            // "Logic {{ChNo}}: {{0}}" with an empty `{{0}}` → "Logic 1:".
            let trimmed = title.trim_end().trim_end_matches(':').trim_end().to_string();
            if !trimmed.is_empty() {
                return trimmed;
            }
        }
        module.name.clone()
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

#[allow(clippy::too_many_arguments)]
fn resolve_field(
    ref_id: &str,
    alias: &str,
    ctx: &Ctx,
    global_types: &HashMap<String, &ParameterType>,
    translations: &Translations,
    values: &HashMap<String, String>,
    prefix: &str,
) -> Option<FormField> {
    let pref = ctx.refs.get(ref_id)?;
    let param_id = pref.ref_id.clone();
    let param = ctx.params.get(&param_id)?;

    let access = if pref.access.is_empty() { &param.access } else { &pref.access };
    if access == "None" {
        return None; // hidden parameter
    }
    let enabled = access != "ReadOnly";

    let type_ = global_types.get(param.parameter_type.as_str()).copied();

    let id = if prefix.is_empty() {
        param_id.clone()
    } else {
        format!("{prefix}/{param_id}")
    };

    let value = values
        .get(&id)
        .cloned()
        .or_else(|| values.get(&param_id).cloned())
        .unwrap_or_else(|| param.value.clone());

    let label = if !alias.is_empty() {
        alias.to_string()
    } else if let Some(s) = translations.get(&param_id).and_then(|a| a.get("Text")) {
        s.clone()
    } else if !param.text.is_empty() {
        param.text.clone()
    } else {
        param.name.clone()
    };

    Some(FormField {
        id,
        label,
        kind: field_kind(type_, &param.suffix_text, translations),
        value,
        enabled,
    })
}

fn field_kind(
    type_: Option<&ParameterType>,
    suffix: &str,
    translations: &Translations,
) -> FieldKind {
    let Some(t) = type_ else {
        return FieldKind::Text;
    };
    if let Some(n) = &t.number {
        return FieldKind::Number {
            min: opt(n.min_inclusive.clone()),
            max: opt(n.max_inclusive.clone()),
            unit: opt(suffix.to_string()),
        };
    }
    if let Some(r) = &t.restriction {
        let options: Vec<EnumOption> = r
            .enumerations
            .iter()
            .map(|e| EnumOption {
                value: e.value.clone(),
                label: translated(e.id.as_str(), e.text.as_str(), translations),
            })
            .collect();
        if r.size_in_bit == "1" && options.len() == 2 {
            return FieldKind::Checkbox {
                off: options[0].value.clone(),
                on: options[1].value.clone(),
            };
        }
        return FieldKind::Enum { options };
    }
    FieldKind::Text
}

fn opt(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

fn current_value(ctx: &Ctx, ref_id: &str, values: &HashMap<String, String>, prefix: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_kind_checkbox_for_1bit_2option() {
        let pt = ParameterType {
            id: "PT-1".into(),
            name: "bool".into(),
            restriction: Some(knx_prod::TypeRestriction {
                size_in_bit: "1".into(),
                enumerations: vec![
                    knx_prod::Enumeration { id: "E-0".into(), text: "no".into(), value: "0".into() },
                    knx_prod::Enumeration { id: "E-1".into(), text: "yes".into(), value: "1".into() },
                ],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            field_kind(Some(&pt), "", &HashMap::new()),
            FieldKind::Checkbox { off: "0".into(), on: "1".into() }
        );
    }
}
