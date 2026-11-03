//! Dynamic UI tree: channels, parameter blocks, visibility conditions.
//!
//! The `Dynamic` element describes the *order and visibility* of parameters
//! (and module instances) in the ETS parameter dialog:
//!
//! - `ParameterBlock` groups parameters (title = `Text`).
//! - `ParameterRefRef` references a `ParameterRef` (→ `Parameter`).
//! - `ParameterSeparator` is a headline/group separator.
//! - `choose ParamRefId=…` with `when test=…` children: every `when` whose
//!   test matches the referenced parameter's value is shown; a
//!   `when default="true"` is shown only if no test matches (see
//!   [`crate::eval`]).
//! - `Module` instantiates a `ModuleDef` with `TextArg`/`NumericArg`
//!   arguments (channel label, base memory offset, …).
//! - `Assign` sets another parameter's value while its branch is active
//!   (evaluated by [`crate::eval`]).
//! - `ComObjectRefRef` shows a group object entry.

use serde::Deserialize;

#[derive(Debug, Deserialize, Default, Clone)]
pub struct Dynamic {
    #[serde(rename = "$value", default)]
    pub nodes: Vec<UiNode>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ChannelIndependentBlock {
    #[serde(rename = "$value", default)]
    pub nodes: Vec<UiNode>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct Channel {
    #[serde(rename = "@Id", default)]
    pub id: String,
    #[serde(rename = "@Name", default)]
    pub name: String,
    #[serde(rename = "@Text", default)]
    pub text: String,
    #[serde(rename = "@Number", default)]
    pub number: String,
    #[serde(rename = "@TextParameterRefId", default)]
    pub text_parameter_ref_id: String,
    #[serde(rename = "$value", default)]
    pub nodes: Vec<UiNode>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ParameterBlock {
    #[serde(rename = "@Id", default)]
    pub id: String,
    #[serde(rename = "@Name", default)]
    pub name: String,
    #[serde(rename = "@Text", default)]
    pub text: String,
    #[serde(rename = "@TextParameterRefId", default)]
    pub text_parameter_ref_id: String,
    #[serde(rename = "@Access", default)]
    pub access: String,
    #[serde(rename = "$value", default)]
    pub nodes: Vec<UiNode>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ParameterRefRef {
    #[serde(rename = "@RefId", default)]
    pub ref_id: String,
    #[serde(rename = "@AliasName", default)]
    pub alias_name: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ParameterSeparator {
    #[serde(rename = "@Id", default)]
    pub id: String,
    #[serde(rename = "@Text", default)]
    pub text: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct Choose {
    #[serde(rename = "@ParamRefId", default)]
    pub param_ref_id: String,
    #[serde(rename = "$value", default)]
    pub whens: Vec<When>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct When {
    #[serde(rename = "@test", default)]
    pub test: String,
    #[serde(rename = "@default", default)]
    pub default: String,
    #[serde(rename = "$value", default)]
    pub nodes: Vec<UiNode>,
}

impl When {
    pub fn is_default(&self) -> bool {
        self.default == "true"
    }
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ModuleInstance {
    #[serde(rename = "@Id", default)]
    pub id: String,
    #[serde(rename = "@RefId", default)]
    pub ref_id: String,
    #[serde(rename = "@Name", default)]
    pub name: String,
    #[serde(rename = "$value", default)]
    pub args: Vec<Arg>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ComObjectRefRef {
    #[serde(rename = "@RefId", default)]
    pub ref_id: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct Assign {
    #[serde(rename = "@TargetParamRefRef", default)]
    pub target_param_ref_ref: String,
    #[serde(rename = "@Value", default)]
    pub value: String,
    #[serde(rename = "@SourceParamRefRef", default)]
    pub source_param_ref_ref: String,
}

#[derive(Debug, Deserialize, Clone)]
pub enum Arg {
    #[serde(rename = "TextArg")]
    Text(TextArg),
    #[serde(rename = "NumericArg")]
    Numeric(NumericArg),
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct TextArg {
    #[serde(rename = "@RefId", default)]
    pub ref_id: String,
    #[serde(rename = "@Value", default)]
    pub value: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct NumericArg {
    #[serde(rename = "@RefId", default)]
    pub ref_id: String,
    #[serde(rename = "@Value", default)]
    pub value: String,
    /// Dynamic allocation (`Value` is computed by ETS from the allocator).
    #[serde(rename = "@AllocatorRefId", default)]
    pub allocator_ref_id: String,
}

/// A node in the `Dynamic` tree. Unknown element types are ignored.
#[derive(Debug, Deserialize, Clone)]
pub enum UiNode {
    #[serde(rename = "ChannelIndependentBlock")]
    ChannelIndependentBlock(ChannelIndependentBlock),
    #[serde(rename = "Channel")]
    Channel(Channel),
    #[serde(rename = "ParameterBlock")]
    ParameterBlock(ParameterBlock),
    #[serde(rename = "ParameterRefRef")]
    ParameterRefRef(ParameterRefRef),
    #[serde(rename = "ParameterSeparator")]
    ParameterSeparator(ParameterSeparator),
    #[serde(rename = "choose")]
    Choose(Choose),
    #[serde(rename = "Module")]
    Module(ModuleInstance),
    #[serde(rename = "ComObjectRefRef")]
    ComObjectRefRef(ComObjectRefRef),
    #[serde(rename = "Assign")]
    Assign(Assign),
    #[serde(other)]
    Other,
}

impl ModuleInstance {
    /// Look up one argument value by its argument-definition id, from either
    /// a `TextArg` or a `NumericArg`.
    pub fn arg_value(&self, arg_id: &str) -> Option<&str> {
        self.args
            .iter()
            .find_map(|a| match a {
                Arg::Text(t) if t.ref_id == arg_id => Some(t.value.as_str()),
                Arg::Numeric(n) if n.ref_id == arg_id => Some(n.value.as_str()),
                _ => None,
            })
    }
}

impl UiNode {
    /// Direct child `UiNode`s of this node, if it is a container.
    pub fn children(&self) -> Option<&[UiNode]> {
        match self {
            UiNode::ChannelIndependentBlock(b) => Some(&b.nodes),
            UiNode::Channel(c) => Some(&c.nodes),
            UiNode::ParameterBlock(p) => Some(&p.nodes),
            _ => None,
        }
    }

    pub fn as_choose(&self) -> Option<&Choose> {
        match self {
            UiNode::Choose(c) => Some(c),
            _ => None,
        }
    }

    pub fn as_module(&self) -> Option<&ModuleInstance> {
        match self {
            UiNode::Module(m) => Some(m),
            _ => None,
        }
    }
}

