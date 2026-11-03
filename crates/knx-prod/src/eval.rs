//! Parameter evaluation: the one place that decides what a device's
//! configuration *is* for a given set of user values.
//!
//! The form, the memory image and the group-object tables all need the same
//! answers, so they all go through [`Evaluation`]:
//!
//! - **effective values** — user value, else the default of the active
//!   `ParameterRef` (`ParameterRef/@Value`), else `Parameter/@Value`; `Assign`
//!   targets and `ParameterCalculation` results override both;
//! - **active branches** — every `when` whose `test` matches is active (not
//!   just the first one), `default` only when none matches; tests are value
//!   lists (`"0 1 2"`) or one comparison (`"<5"`, `">=12"`, `"!=0"`);
//! - **active elements** — the `ParameterRef`s, `ComObjectRef`s and module
//!   instances reached through active branches;
//! - **union members** — the member written into a `<Union>` is the active
//!   one, else the one marked `DefaultUnionParameter="true"`.
//!
//! Values feed back into visibility (a default can depend on which ref is
//! active, an `Assign` can switch another `choose`), so the evaluation is
//! iterated until it reaches a fixed point.
//!
//! ## Keys
//!
//! Device parameters are keyed by their `Parameter/@Id`; module parameters by
//! `<module instance id>/<parameter id>` (nested instances are
//! slash-separated). [`short_key`] / [`full_key`] convert to the
//! application-independent form stored in projects (the
//! `M-XXXX_A-XXXX-XX-XXXX_` prefix removed), so a project survives an
//! application upgrade that keeps the parameter numbering.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::calc;
use crate::{
    ApplicationProgram, Choose, ComObjectRef, Dynamic, ModuleDef, ModuleInstance, Parameter,
    ParameterRef, Static, UiNode, Union, When,
};

/// Upper bound for the value/visibility fixed-point iteration. Real
/// applications settle in two or three passes.
const MAX_PASSES: usize = 16;

/// One parameter namespace: the device itself, or one module instance.
#[derive(Debug)]
pub struct Scope<'a> {
    /// Key prefix (`""` for the device, else the instance id path).
    pub prefix: String,
    pub static_: Option<&'a Static>,
    pub dynamic: Option<&'a Dynamic>,
    pub instance: Option<&'a ModuleInstance>,
    pub module: Option<&'a ModuleDef>,
    /// Parameter id → parameter (plain parameters and union members).
    params: HashMap<&'a str, &'a Parameter>,
    /// ParameterRef id → ref.
    refs: HashMap<&'a str, &'a ParameterRef>,
    /// Parameter id → its refs, in document order.
    refs_of: HashMap<&'a str, Vec<&'a ParameterRef>>,
    com_refs: HashMap<&'a str, &'a ComObjectRef>,
}

impl<'a> Scope<'a> {
    fn new(
        prefix: String,
        static_: Option<&'a Static>,
        dynamic: Option<&'a Dynamic>,
        instance: Option<&'a ModuleInstance>,
        module: Option<&'a ModuleDef>,
    ) -> Self {
        let mut scope = Scope {
            prefix,
            static_,
            dynamic,
            instance,
            module,
            params: HashMap::new(),
            refs: HashMap::new(),
            refs_of: HashMap::new(),
            com_refs: HashMap::new(),
        };
        if let Some(s) = static_ {
            if let Some(p) = &s.parameters {
                for param in p.parameters() {
                    scope.params.insert(param.id.as_str(), param);
                }
                for u in p.unions() {
                    for param in &u.parameters {
                        scope.params.insert(param.id.as_str(), param);
                    }
                }
            }
            if let Some(pr) = &s.parameter_refs {
                for r in &pr.items {
                    scope.refs.insert(r.id.as_str(), r);
                    scope.refs_of.entry(r.ref_id.as_str()).or_default().push(r);
                }
            }
            if let Some(cr) = &s.com_object_refs {
                for r in &cr.items {
                    scope.com_refs.insert(r.id.as_str(), r);
                }
            }
        }
        scope
    }

    /// Instance key of a parameter in this scope.
    pub fn key(&self, param_id: &str) -> String {
        if self.prefix.is_empty() {
            param_id.to_string()
        } else {
            format!("{}/{param_id}", self.prefix)
        }
    }

    pub fn parameter(&self, param_id: &str) -> Option<&'a Parameter> {
        self.params.get(param_id).copied()
    }

    pub fn parameter_ref(&self, ref_id: &str) -> Option<&'a ParameterRef> {
        self.refs.get(ref_id).copied()
    }

    pub fn com_object_ref(&self, ref_id: &str) -> Option<&'a ComObjectRef> {
        self.com_refs.get(ref_id).copied()
    }

    /// All parameters of this scope (plain and union members).
    pub fn parameters(&self) -> impl Iterator<Item = &'a Parameter> + '_ {
        self.static_
            .and_then(|s| s.parameters.as_ref())
            .into_iter()
            .flat_map(|p| {
                p.parameters()
                    .chain(p.unions().flat_map(|u| u.parameters.iter()))
            })
    }

    /// Value of a module argument (`TextArg`/`NumericArg`) by argument id.
    pub fn arg_value(&self, arg_id: &str) -> Option<&'a str> {
        self.instance.and_then(|i| i.arg_value(arg_id))
    }
}

/// The result of evaluating an application program against user values.
#[derive(Debug)]
pub struct Evaluation<'a> {
    pub ap: &'a ApplicationProgram,
    /// Index 0 is the device scope; module instances follow in document order.
    pub scopes: Vec<Scope<'a>>,
    /// Effective value per parameter key (every parameter of every scope).
    values: HashMap<String, String>,
    /// What each key would be without a user value (ref/param default).
    defaults: HashMap<String, String>,
    /// Keys forced by `Assign` or a calculation (user values are ignored).
    forced: HashSet<String>,
    /// Active `ParameterRef`s as `(scope, ref id)`.
    active_refs: HashSet<(usize, String)>,
    /// Parameter keys with at least one active ref.
    active_params: HashSet<String>,
    /// Active `ComObjectRef`s as `(scope, ref id)`, in tree order.
    active_com_refs: Vec<(usize, String)>,
    /// Scopes reached through active branches (always includes 0).
    active_scopes: HashSet<usize>,
    /// User keys that do not name any parameter.
    pub unknown_keys: Vec<String>,
    /// Things the evaluator could not do exactly like ETS.
    pub warnings: Vec<String>,
}

impl<'a> Evaluation<'a> {
    /// Evaluate `ap` for the given user values (keys as described in the
    /// module docs; short keys are accepted too).
    pub fn new(ap: &'a ApplicationProgram, user: &HashMap<String, String>) -> Self {
        let scopes = build_scopes(ap);
        let mut ev = Evaluation {
            ap,
            scopes,
            values: HashMap::new(),
            defaults: HashMap::new(),
            forced: HashSet::new(),
            active_refs: HashSet::new(),
            active_params: HashSet::new(),
            active_com_refs: Vec::new(),
            active_scopes: HashSet::new(),
            unknown_keys: Vec::new(),
            warnings: Vec::new(),
        };

        let user: HashMap<String, String> = user
            .iter()
            .map(|(k, v)| (full_key(ap, k), v.clone()))
            .collect();
        let known: HashSet<String> = ev
            .scopes
            .iter()
            .flat_map(|s| s.parameters().map(|p| s.key(&p.id)))
            .collect();
        ev.unknown_keys = user.keys().filter(|k| !known.contains(*k)).cloned().collect();
        ev.unknown_keys.sort();

        ev.check_unsupported();

        let mut forced: BTreeMap<String, String> = BTreeMap::new();
        let mut warnings = Vec::new();
        for pass in 0..MAX_PASSES {
            ev.compute_values(&user, &forced);
            let walk = ev.walk();
            let mut next_forced = walk.assigns;
            warnings = walk.warnings;
            ev.active_refs = walk.active_refs;
            ev.active_com_refs = walk.active_com_refs;
            ev.active_scopes = walk.active_scopes;
            ev.active_params = ev
                .active_refs
                .iter()
                .filter_map(|(si, rid)| {
                    let scope = &ev.scopes[*si];
                    scope.parameter_ref(rid).map(|r| scope.key(&r.ref_id))
                })
                .collect();
            for (k, v) in ev.run_calculations(&user, &mut warnings) {
                next_forced.insert(k, v);
            }
            if next_forced == forced {
                break;
            }
            if pass + 1 == MAX_PASSES {
                warnings.push(format!(
                    "parameter evaluation did not settle after {MAX_PASSES} passes \
                     (cyclic Assign/visibility dependency)"
                ));
            }
            forced = next_forced;
        }
        ev.compute_values(&user, &forced);
        ev.forced = forced.into_keys().collect();
        ev.warnings.extend(warnings);
        ev
    }

    /// Effective value of a parameter instance.
    pub fn value(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    /// All effective values.
    pub fn values(&self) -> &HashMap<String, String> {
        &self.values
    }

    /// The value a key would have without a user value.
    pub fn default_of(&self, key: &str) -> Option<&str> {
        self.defaults.get(key).map(String::as_str)
    }

    /// `true` if the value is set by an `Assign` or a calculation (the user
    /// value, if any, has no effect).
    pub fn is_forced(&self, key: &str) -> bool {
        self.forced.contains(key)
    }

    /// `true` if one of the parameter's refs is reached through active
    /// branches.
    pub fn is_param_active(&self, key: &str) -> bool {
        self.active_params.contains(key)
    }

    pub fn is_ref_active(&self, scope: usize, ref_id: &str) -> bool {
        self.active_refs.contains(&(scope, ref_id.to_string()))
    }

    pub fn is_scope_active(&self, scope: usize) -> bool {
        self.active_scopes.contains(&scope)
    }

    /// Active `ComObjectRef`s as `(scope index, ref)`.
    pub fn active_com_object_refs(&self) -> impl Iterator<Item = (usize, &'a ComObjectRef)> + '_ {
        self.active_com_refs.iter().filter_map(|(si, rid)| {
            self.scopes[*si].com_object_ref(rid).map(|r| (*si, r))
        })
    }

    /// Index of the scope for a key prefix (`""` = device).
    pub fn scope_index(&self, prefix: &str) -> Option<usize> {
        self.scopes.iter().position(|s| s.prefix == prefix)
    }

    /// Effective value of the parameter behind a `ParameterRef` of a scope.
    pub fn ref_value(&self, scope: usize, ref_id: &str) -> Option<&str> {
        let s = &self.scopes[scope];
        let r = s.parameter_ref(ref_id)?;
        self.value(&s.key(&r.ref_id))
    }

    /// The `when` branches of a `choose` that are active for the current
    /// values (all matching tests; `default` only if none matches).
    pub fn active_whens<'c>(&self, scope: usize, choose: &'c Choose) -> Vec<&'c When> {
        let current = self.ref_value(scope, &choose.param_ref_id).unwrap_or("");
        active_whens(choose, current)
    }

    /// The member of a union that is written to memory: the active member,
    /// else the `DefaultUnionParameter`, else none.
    pub fn union_member(&self, scope: usize, union: &'a Union) -> Option<&'a Parameter> {
        let s = &self.scopes[scope];
        union
            .parameters
            .iter()
            .find(|p| self.is_param_active(&s.key(&p.id)))
            .or_else(|| {
                union
                    .parameters
                    .iter()
                    .find(|p| p.default_union_parameter == "true")
            })
    }

    fn compute_values(&mut self, user: &HashMap<String, String>, forced: &BTreeMap<String, String>) {
        let mut values = HashMap::new();
        let mut defaults = HashMap::new();
        for (si, scope) in self.scopes.iter().enumerate() {
            for param in scope.parameters() {
                let key = scope.key(&param.id);
                let default = self.default_for(si, param);
                let value = forced
                    .get(&key)
                    .or_else(|| user.get(&key))
                    .cloned()
                    .unwrap_or_else(|| default.clone());
                defaults.insert(key.clone(), default);
                values.insert(key, value);
            }
        }
        self.values = values;
        self.defaults = defaults;
    }

    /// Default of a parameter: the `@Value` of its first active ref that has
    /// one, else of its first ref that has one when no ref is active, else
    /// `Parameter/@Value`.
    fn default_for(&self, scope: usize, param: &Parameter) -> String {
        let s = &self.scopes[scope];
        let refs = s.refs_of.get(param.id.as_str()).map(Vec::as_slice).unwrap_or(&[]);
        let active = refs
            .iter()
            .find(|r| r.value.is_some() && self.is_ref_active(scope, &r.id));
        let any_active = refs.iter().any(|r| self.is_ref_active(scope, &r.id));
        let chosen = active.or_else(|| {
            if any_active {
                None
            } else {
                refs.iter().find(|r| r.value.is_some())
            }
        });
        chosen
            .and_then(|r| r.value.clone())
            .unwrap_or_else(|| param.value.clone())
    }

    fn walk(&self) -> Walk {
        let mut w = Walk::default();
        w.active_scopes.insert(0);
        if let Some(d) = self.scopes[0].dynamic {
            self.walk_nodes(0, &d.nodes, &mut w);
        }
        w
    }

    fn walk_nodes(&self, scope: usize, nodes: &[UiNode], w: &mut Walk) {
        for node in nodes {
            match node {
                UiNode::ParameterRefRef(r) => {
                    w.active_refs.insert((scope, r.ref_id.clone()));
                }
                UiNode::ComObjectRefRef(r) => {
                    w.active_com_refs.push((scope, r.ref_id.clone()));
                }
                UiNode::Choose(choose) => {
                    for when in self.active_whens(scope, choose) {
                        self.walk_nodes(scope, &when.nodes, w);
                    }
                }
                UiNode::Assign(a) => {
                    let s = &self.scopes[scope];
                    let Some(target) = s.parameter_ref(&a.target_param_ref_ref) else {
                        w.warnings.push(format!(
                            "Assign target {} not found",
                            a.target_param_ref_ref
                        ));
                        continue;
                    };
                    let value = if !a.source_param_ref_ref.is_empty() {
                        match self.ref_value(scope, &a.source_param_ref_ref) {
                            Some(v) => v.to_string(),
                            None => {
                                w.warnings.push(format!(
                                    "Assign source {} not found",
                                    a.source_param_ref_ref
                                ));
                                continue;
                            }
                        }
                    } else {
                        a.value.clone()
                    };
                    w.assigns.insert(s.key(&target.ref_id), value);
                }
                UiNode::Module(instance) => {
                    let prefix = self.scopes[scope].key(&instance.id);
                    if let Some(si) = self.scope_index(&prefix) {
                        w.active_scopes.insert(si);
                        if let Some(d) = self.scopes[si].dynamic {
                            self.walk_nodes(si, &d.nodes, w);
                        }
                    }
                }
                _ => {
                    if let Some(children) = node.children() {
                        self.walk_nodes(scope, children, w);
                    }
                }
            }
        }
    }

    /// Run the `LRTransformation` of every calculation in an active scope
    /// whose `LParameters` include a user-set value (ETS runs a calculation
    /// when one of its inputs changes; with all inputs at their defaults the
    /// outputs keep their defaults).
    fn run_calculations(
        &self,
        user: &HashMap<String, String>,
        warnings: &mut Vec<String>,
    ) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (si, scope) in self.scopes.iter().enumerate() {
            if !self.is_scope_active(si) {
                continue;
            }
            let Some(calcs) = scope.static_.and_then(|s| s.parameter_calculations.as_ref()) else {
                continue;
            };
            for c in &calcs.items {
                let inputs = c.l_parameters.as_ref().map(|p| p.items.as_slice()).unwrap_or(&[]);
                let outputs = c.r_parameters.as_ref().map(|p| p.items.as_slice()).unwrap_or(&[]);
                let key_of = |alias_ref: &str| {
                    scope
                        .parameter_ref(alias_ref)
                        .map(|r| scope.key(&r.ref_id))
                };
                let touched = inputs.iter().any(|i| {
                    key_of(&i.ref_id)
                        .map(|k| user.contains_key(&k))
                        .unwrap_or(false)
                });
                if !touched {
                    continue;
                }
                if c.language != "JavaScript" {
                    warnings.push(format!("calculation {}: unsupported language {:?}", c.name, c.language));
                    continue;
                }
                let mut vars = HashMap::new();
                for p in inputs.iter().chain(outputs.iter()) {
                    if let Some(v) = key_of(&p.ref_id).and_then(|k| self.value(&k).map(str::to_string)) {
                        vars.insert(p.alias_name.clone(), v);
                    }
                }
                match calc::run(&c.lr_transformation, &mut vars) {
                    Ok(()) => {
                        for p in outputs {
                            let Some(k) = key_of(&p.ref_id) else { continue };
                            let Some(v) = vars.get(&p.alias_name) else { continue };
                            let integer = scope
                                .parameter_ref(&p.ref_id)
                                .and_then(|r| scope.parameter(&r.ref_id))
                                .map(|param| self.is_integer_type(param))
                                .unwrap_or(false);
                            // JavaScript numbers are doubles; integer
                            // parameters receive the value truncated
                            // toward zero (not verified against ETS).
                            let v = match v.parse::<f64>() {
                                Ok(f) if integer => format!("{}", f.trunc() as i64),
                                _ => v.clone(),
                            };
                            out.push((k, v));
                        }
                    }
                    Err(e) => warnings.push(format!(
                        "calculation {} ({}) not evaluated: {e}; its outputs keep their \
                         previous values",
                        c.name, c.id
                    )),
                }
            }
        }
        out
    }

    fn is_integer_type(&self, param: &Parameter) -> bool {
        self.ap
            .static_
            .as_ref()
            .and_then(|s| s.parameter_types.as_ref())
            .and_then(|t| t.items.iter().find(|t| t.id == param.parameter_type))
            .map(|t| t.number.is_some() || t.restriction.is_some() || t.time.is_some())
            .unwrap_or(false)
    }

    fn check_unsupported(&mut self) {
        for scope in &self.scopes {
            if let Some(inst) = scope.instance {
                for a in &inst.args {
                    if let crate::Arg::Numeric(n) = a {
                        if !n.allocator_ref_id.is_empty() {
                            self.warnings.push(format!(
                                "module {} uses allocator {} for argument {}; allocators are \
                                 not evaluated",
                                inst.id, n.allocator_ref_id, n.ref_id
                            ));
                        }
                    }
                }
            }
            for p in scope.parameters() {
                if p.property.as_ref().is_some_and(|loc| !is_device_check(loc)) {
                    self.warnings.push(format!(
                        "parameter {} is stored in an interface object property; property \
                         parameters are not downloaded",
                        scope.key(&p.id)
                    ));
                }
            }
        }
    }
}

/// Device object (index 0) `PID_HARDWARE_TYPE` (78) / `PID_ORDER_INFO`
/// (15): ETS compares these with the product data before a download
/// (`03_05_03 §3.9.3.4`); the download plan turns them into `CompareProp`.
pub fn is_device_check(loc: &crate::PropertyLocation) -> bool {
    loc.object_index.trim() == "0"
        && matches!(loc.property_id.trim(), "78" | "15")
        && loc.offset.trim().parse::<u32>().unwrap_or(0) == 0
        && loc.bit_offset.trim().parse::<u32>().unwrap_or(0) == 0
}

#[derive(Default)]
struct Walk {
    active_refs: HashSet<(usize, String)>,
    active_com_refs: Vec<(usize, String)>,
    active_scopes: HashSet<usize>,
    assigns: BTreeMap<String, String>,
    warnings: Vec<String>,
}

/// Build the device scope and one scope per module instance (all branches;
/// memory for a module instance exists whether or not it is visible).
fn build_scopes(ap: &ApplicationProgram) -> Vec<Scope<'_>> {
    let modules: HashMap<&str, &ModuleDef> = ap
        .module_defs
        .as_ref()
        .map(|m| m.items.iter().map(|m| (m.id.as_str(), m)).collect())
        .unwrap_or_default();
    let mut scopes = vec![Scope::new(
        String::new(),
        ap.static_.as_ref(),
        ap.dynamic.as_ref(),
        None,
        None,
    )];
    if let Some(d) = &ap.dynamic {
        collect_instances(&d.nodes, &modules, "", &mut scopes);
    }
    scopes
}

fn collect_instances<'a>(
    nodes: &'a [UiNode],
    modules: &HashMap<&str, &'a ModuleDef>,
    prefix: &str,
    out: &mut Vec<Scope<'a>>,
) {
    for node in nodes {
        match node {
            UiNode::Module(instance) => {
                let Some(module) = modules.get(instance.ref_id.as_str()).copied() else {
                    continue;
                };
                let new_prefix = if prefix.is_empty() {
                    instance.id.clone()
                } else {
                    format!("{prefix}/{}", instance.id)
                };
                if out.iter().any(|s| s.prefix == new_prefix) {
                    // The same instance listed in several branches.
                    continue;
                }
                out.push(Scope::new(
                    new_prefix.clone(),
                    module.static_.as_ref(),
                    module.dynamic.as_ref(),
                    Some(instance),
                    Some(module),
                ));
                if let Some(d) = &module.dynamic {
                    collect_instances(&d.nodes, modules, &new_prefix, out);
                }
            }
            UiNode::Choose(choose) => {
                for when in &choose.whens {
                    collect_instances(&when.nodes, modules, prefix, out);
                }
            }
            _ => {
                if let Some(children) = node.children() {
                    collect_instances(children, modules, prefix, out);
                }
            }
        }
    }
}

/// The active branches of a `choose` for a given value of its parameter.
pub fn active_whens<'c>(choose: &'c Choose, current: &str) -> Vec<&'c When> {
    let matching: Vec<&When> = choose
        .whens
        .iter()
        .filter(|w| !w.is_default() && test_matches(&w.test, current))
        .collect();
    if matching.is_empty() {
        choose.whens.iter().filter(|w| w.is_default()).collect()
    } else {
        matching
    }
}

/// Evaluate a `when/@test` against the current value.
///
/// A test is either a whitespace-separated list of values (any one matches)
/// or a single comparison `<v`, `<=v`, `>v`, `>=v`, `!=v`, `=v`. Values are
/// compared numerically when both sides are numbers, else as strings.
pub fn test_matches(test: &str, current: &str) -> bool {
    let test = test.trim();
    for op in ["<=", ">=", "!=", "<", ">", "="] {
        if let Some(rhs) = test.strip_prefix(op) {
            let rhs = rhs.trim();
            return match (parse_num(current), parse_num(rhs)) {
                (Some(a), Some(b)) => match op {
                    "<=" => a <= b,
                    ">=" => a >= b,
                    "!=" => a != b,
                    "<" => a < b,
                    ">" => a > b,
                    _ => a == b,
                },
                _ => match op {
                    "!=" => current != rhs,
                    "=" => current == rhs,
                    _ => false,
                },
            };
        }
    }
    test.split_whitespace().any(|t| {
        t == current
            || matches!((parse_num(t), parse_num(current)), (Some(a), Some(b)) if a == b)
    })
}

fn parse_num(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    s.parse::<f64>().ok()
}

/// The `M-XXXX_A-XXXX-XX-XXXX_` prefix shared by every id of an application.
fn id_prefix(ap: &ApplicationProgram) -> String {
    format!("{}_", ap.id)
}

/// Strip the application id from every component of a parameter key
/// (`M-0083_A-00B6-32-9642_MD-2_M-2/M-0083_A-00B6-32-9642_MD-2_P-26` →
/// `MD-2_M-2/MD-2_P-26`).
pub fn short_key(ap: &ApplicationProgram, key: &str) -> String {
    let prefix = id_prefix(ap);
    key.split('/')
        .map(|c| c.strip_prefix(prefix.as_str()).unwrap_or(c))
        .collect::<Vec<_>>()
        .join("/")
}

/// Inverse of [`short_key`]; full keys pass through unchanged.
pub fn full_key(ap: &ApplicationProgram, key: &str) -> String {
    if ap.id.is_empty() {
        return key.to_string();
    }
    let prefix = id_prefix(ap);
    key.split('/')
        .map(|c| {
            if c.starts_with("M-") {
                c.to_string()
            } else {
                format!("{prefix}{c}")
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ParamNode, ParameterRefRef, ParameterRefs, Parameters};

    #[test]
    fn test_matching() {
        assert!(test_matches("0 1 2", "1"));
        assert!(!test_matches("0 1 2", "3"));
        assert!(test_matches("!=0", "5"));
        assert!(!test_matches("!=0", "0"));
        assert!(test_matches(">=12", "12"));
        assert!(!test_matches(">=12", "11"));
        assert!(test_matches("<5", "4"));
        assert!(test_matches(">0", "1"));
        assert!(test_matches("<=3", "3"));
        assert!(test_matches("1", "1.0"));
    }

    fn param(id: &str, value: &str) -> ParamNode {
        ParamNode::Parameter(Parameter {
            id: id.into(),
            value: value.into(),
            ..Default::default()
        })
    }

    fn pref(id: &str, param: &str, value: Option<&str>) -> ParameterRef {
        ParameterRef {
            id: id.into(),
            ref_id: param.into(),
            value: value.map(str::to_string),
            ..Default::default()
        }
    }

    fn rr(id: &str) -> UiNode {
        UiNode::ParameterRefRef(ParameterRefRef {
            ref_id: id.into(),
            ..Default::default()
        })
    }

    fn when(test: &str, nodes: Vec<UiNode>) -> When {
        When {
            test: test.into(),
            nodes,
            ..Default::default()
        }
    }

    /// P-1 selects branches; P-2 is shown with ref R-2a (default 7) or R-2b
    /// (default 9) depending on overlapping tests.
    fn sample() -> ApplicationProgram {
        ApplicationProgram {
            static_: Some(Static {
                parameters: Some(Parameters {
                    nodes: vec![param("P-1", "1"), param("P-2", "0"), param("P-3", "0")],
                }),
                parameter_refs: Some(ParameterRefs {
                    items: vec![
                        pref("R-1", "P-1", None),
                        pref("R-2a", "P-2", Some("7")),
                        pref("R-2b", "P-2", Some("9")),
                        pref("R-3", "P-3", None),
                    ],
                }),
                ..Default::default()
            }),
            dynamic: Some(Dynamic {
                nodes: vec![
                    rr("R-1"),
                    UiNode::Choose(Choose {
                        param_ref_id: "R-1".into(),
                        whens: vec![
                            when("0 1", vec![rr("R-2a")]),
                            when(">=1", vec![UiNode::Assign(crate::Assign {
                                target_param_ref_ref: "R-3".into(),
                                value: "42".into(),
                                ..Default::default()
                            })]),
                            When {
                                default: "true".into(),
                                nodes: vec![rr("R-2b")],
                                ..Default::default()
                            },
                        ],
                    }),
                ],
            }),
            ..Default::default()
        }
    }

    #[test]
    fn all_matching_whens_and_ref_defaults_and_assign() {
        let ap = sample();
        let ev = Evaluation::new(&ap, &HashMap::new());
        // P-1 = 1 matches both "0 1" and ">=1".
        assert!(ev.is_ref_active(0, "R-2a"));
        assert!(!ev.is_ref_active(0, "R-2b"));
        assert_eq!(ev.value("P-2"), Some("7"));
        assert_eq!(ev.value("P-3"), Some("42"));
        assert!(ev.is_forced("P-3"));

        let mut user = HashMap::new();
        user.insert("P-1".to_string(), "5".to_string());
        let ev = Evaluation::new(&ap, &user);
        // 5 matches only ">=1" — the default branch stays inactive.
        assert!(!ev.is_ref_active(0, "R-2a"));
        assert!(!ev.is_ref_active(0, "R-2b"));
        // No ref of P-2 is active: the first ref with a Value supplies it.
        assert_eq!(ev.value("P-2"), Some("7"));

        user.insert("P-1".to_string(), "-1".to_string());
        let ev = Evaluation::new(&ap, &user);
        assert!(ev.is_ref_active(0, "R-2b"));
        assert_eq!(ev.value("P-2"), Some("9"));
        assert_eq!(ev.value("P-3"), Some("0"));
    }

    #[test]
    fn keys_shorten_and_expand() {
        let ap = ApplicationProgram {
            id: "M-0083_A-00B6-32-9642".into(),
            ..Default::default()
        };
        let full = "M-0083_A-00B6-32-9642_MD-2_M-2/M-0083_A-00B6-32-9642_MD-2_P-26";
        assert_eq!(short_key(&ap, full), "MD-2_M-2/MD-2_P-26");
        assert_eq!(full_key(&ap, "MD-2_M-2/MD-2_P-26"), full);
        assert_eq!(full_key(&ap, full), full);
    }
}
