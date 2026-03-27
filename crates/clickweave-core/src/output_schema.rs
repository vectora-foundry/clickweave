use serde::{Deserialize, Serialize};

/// The type of data an output field produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum OutputFieldType {
    Bool,
    Number,
    String,
    Array,
    Object,
    Any,
}

/// A declared output field on a node type (compile-time schema metadata).
#[derive(Debug, Clone)]
pub struct OutputField {
    pub name: &'static str,
    pub field_type: OutputFieldType,
    pub description: &'static str,
}

/// A declared input field that accepts variable references (compile-time schema metadata).
#[derive(Debug, Clone)]
pub struct InputField {
    pub name: &'static str,
    pub accepted_types: &'static [OutputFieldType],
    pub description: &'static str,
}

/// A reference to a specific output field of an upstream node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct OutputRef {
    /// auto_id of the source node (e.g. "find_text_1")
    pub node: String,
    /// Output field name (e.g. "coordinates")
    pub field: String,
}

/// Method used to verify an action node's effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum VerificationMethod {
    Vlm,
}

/// What kind of data a node produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum OutputRole {
    Query,
    Action,
    Ai,
    ControlFlow,
    Generic,
}

/// The execution context a node operates in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum NodeContext {
    Native,
    Cdp,
    Independent,
}

/// Right-hand side of a condition: either a literal or a reference to an upstream output.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "type")]
pub enum ConditionValue {
    Literal { value: crate::LiteralValue },
    Ref(OutputRef),
}

// --- Output schema registry ---

use crate::NodeType;

// Short aliases for OutputFieldType variants used in schema constants.
use OutputFieldType as T;

const FIND_TEXT_OUTPUTS: &[OutputField] = &[
    OutputField {
        name: "found",
        field_type: T::Bool,
        description: "Whether any matches were found",
    },
    OutputField {
        name: "count",
        field_type: T::Number,
        description: "Number of matches found",
    },
    OutputField {
        name: "text",
        field_type: T::String,
        description: "Text of the first match",
    },
    OutputField {
        name: "coordinates",
        field_type: T::Object,
        description: "Coordinates of the first match",
    },
];

const FIND_IMAGE_OUTPUTS: &[OutputField] = &[
    OutputField {
        name: "found",
        field_type: T::Bool,
        description: "Whether any matches were found",
    },
    OutputField {
        name: "count",
        field_type: T::Number,
        description: "Number of matches found",
    },
    OutputField {
        name: "coordinates",
        field_type: T::Object,
        description: "Coordinates of the first match",
    },
    OutputField {
        name: "confidence",
        field_type: T::Number,
        description: "Confidence score of the first match",
    },
];

const FIND_APP_OUTPUTS: &[OutputField] = &[
    OutputField {
        name: "found",
        field_type: T::Bool,
        description: "Whether the app is running",
    },
    OutputField {
        name: "name",
        field_type: T::String,
        description: "App name",
    },
    OutputField {
        name: "pid",
        field_type: T::Number,
        description: "Process ID",
    },
];

const TAKE_SCREENSHOT_OUTPUTS: &[OutputField] = &[OutputField {
    name: "result",
    field_type: T::String,
    description: "Screenshot data",
}];

const CDP_WAIT_OUTPUTS: &[OutputField] = &[OutputField {
    name: "found",
    field_type: T::Bool,
    description: "Whether the text appeared before timeout",
}];

const AI_STEP_OUTPUTS: &[OutputField] = &[OutputField {
    name: "result",
    field_type: T::String,
    description: "LLM response text",
}];

const GENERIC_OUTPUTS: &[OutputField] = &[OutputField {
    name: "result",
    field_type: T::Any,
    description: "Raw tool result",
}];

const EMPTY_OUTPUTS: &[OutputField] = &[];

const VERIFICATION_OUTPUTS: &[OutputField] = &[
    OutputField {
        name: "verified",
        field_type: T::Bool,
        description: "Whether the action had the intended effect",
    },
    OutputField {
        name: "verification_reasoning",
        field_type: T::String,
        description: "Explanation of the verification result",
    },
];

// --- Input schema registry ---

const CLICK_INPUTS: &[InputField] = &[InputField {
    name: "target_ref",
    accepted_types: &[T::Object],
    description: "Coordinates from FindText/FindImage",
}];
const HOVER_INPUTS: &[InputField] = &[InputField {
    name: "target_ref",
    accepted_types: &[T::Object],
    description: "Coordinates from FindText/FindImage",
}];
const DRAG_INPUTS: &[InputField] = &[
    InputField {
        name: "from_ref",
        accepted_types: &[T::Object],
        description: "Start coordinates",
    },
    InputField {
        name: "to_ref",
        accepted_types: &[T::Object],
        description: "End coordinates",
    },
];
const TYPE_TEXT_INPUTS: &[InputField] = &[InputField {
    name: "text_ref",
    accepted_types: &[T::String, T::Number, T::Bool],
    description: "Value to type",
}];
const FOCUS_WINDOW_INPUTS: &[InputField] = &[InputField {
    name: "value_ref",
    accepted_types: &[T::String, T::Number],
    description: "App name or PID",
}];
const AI_STEP_INPUTS: &[InputField] = &[InputField {
    name: "prompt_ref",
    accepted_types: &[T::String, T::Number, T::Bool],
    description: "Include upstream data in the prompt",
}];
const CDP_FILL_INPUTS: &[InputField] = &[InputField {
    name: "value_ref",
    accepted_types: &[T::String, T::Number, T::Bool],
    description: "Value to fill",
}];
const CDP_TYPE_INPUTS: &[InputField] = &[InputField {
    name: "text_ref",
    accepted_types: &[T::String, T::Number, T::Bool],
    description: "Text to type",
}];
const CDP_NAVIGATE_INPUTS: &[InputField] = &[InputField {
    name: "url_ref",
    accepted_types: &[T::String],
    description: "URL to navigate to",
}];
const CDP_NEW_PAGE_INPUTS: &[InputField] = &[InputField {
    name: "url_ref",
    accepted_types: &[T::String],
    description: "URL to open in new tab",
}];
const EMPTY_INPUTS: &[InputField] = &[];

impl NodeType {
    /// Returns the static output schema (without verification fields).
    pub fn output_schema(&self) -> &'static [OutputField] {
        match self {
            Self::FindText(_) => FIND_TEXT_OUTPUTS,
            Self::FindImage(_) => FIND_IMAGE_OUTPUTS,
            Self::FindApp(_) => FIND_APP_OUTPUTS,
            Self::TakeScreenshot(_) => TAKE_SCREENSHOT_OUTPUTS,
            Self::CdpWait(_) => CDP_WAIT_OUTPUTS,
            Self::AiStep(_) => AI_STEP_OUTPUTS,
            Self::McpToolCall(_) | Self::AppDebugKitOp(_) => GENERIC_OUTPUTS,
            _ => EMPTY_OUTPUTS,
        }
    }

    /// Returns the static input schema.
    pub fn input_schema(&self) -> &'static [InputField] {
        match self {
            Self::Click(_) => CLICK_INPUTS,
            Self::Hover(_) => HOVER_INPUTS,
            Self::Drag(_) => DRAG_INPUTS,
            Self::TypeText(_) => TYPE_TEXT_INPUTS,
            Self::FocusWindow(_) => FOCUS_WINDOW_INPUTS,
            Self::AiStep(_) => AI_STEP_INPUTS,
            Self::CdpFill(_) => CDP_FILL_INPUTS,
            Self::CdpType(_) => CDP_TYPE_INPUTS,
            Self::CdpNavigate(_) => CDP_NAVIGATE_INPUTS,
            Self::CdpNewPage(_) => CDP_NEW_PAGE_INPUTS,
            _ => EMPTY_INPUTS,
        }
    }
}

/// Full output schema including verification fields when enabled.
pub fn full_output_schema(node_type: &NodeType, has_verification: bool) -> Vec<OutputField> {
    let base = node_type.output_schema();
    if has_verification && node_type.output_role() == OutputRole::Action {
        let mut fields: Vec<OutputField> = base.to_vec();
        fields.extend_from_slice(VERIFICATION_OUTPUTS);
        fields
    } else {
        base.to_vec()
    }
}

/// Add the fixup_auto_ids method to Workflow.
impl crate::Workflow {
    /// Fix up next_id_counters from existing auto_ids after deserialization.
    /// Call this after loading a workflow from disk.
    pub fn fixup_auto_ids(&mut self) {
        if self.next_id_counters.is_empty() {
            let ids: Vec<&str> = self
                .nodes
                .iter()
                .filter(|n| !n.auto_id.is_empty())
                .map(|n| n.auto_id.as_str())
                .collect();
            crate::auto_id::fixup_counters(&ids, &mut self.next_id_counters);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;

    #[test]
    fn output_ref_serde_roundtrip() {
        let r = OutputRef {
            node: "find_text_1".into(),
            field: "coordinates".into(),
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: OutputRef = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn output_field_type_serde_roundtrip() {
        for t in [
            OutputFieldType::Bool,
            OutputFieldType::Number,
            OutputFieldType::String,
            OutputFieldType::Array,
            OutputFieldType::Object,
            OutputFieldType::Any,
        ] {
            let json = serde_json::to_string(&t).unwrap();
            let back: OutputFieldType = serde_json::from_str(&json).unwrap();
            assert_eq!(t, back);
        }
    }

    #[test]
    fn query_nodes_have_outputs() {
        assert!(
            !NodeType::FindText(FindTextParams::default())
                .output_schema()
                .is_empty()
        );
        assert!(
            !NodeType::FindImage(FindImageParams::default())
                .output_schema()
                .is_empty()
        );
        assert!(
            !NodeType::FindApp(FindAppParams::default())
                .output_schema()
                .is_empty()
        );
        assert!(
            !NodeType::CdpWait(CdpWaitParams::default())
                .output_schema()
                .is_empty()
        );
    }

    #[test]
    fn action_nodes_have_empty_base_outputs() {
        assert!(
            NodeType::Click(ClickParams::default())
                .output_schema()
                .is_empty()
        );
        assert!(
            NodeType::CdpClick(CdpClickParams::default())
                .output_schema()
                .is_empty()
        );
    }

    #[test]
    fn full_output_schema_adds_verification() {
        let click = NodeType::Click(ClickParams::default());
        let without = full_output_schema(&click, false);
        let with = full_output_schema(&click, true);
        assert!(without.is_empty());
        assert_eq!(with.len(), 2);
        assert_eq!(with[0].name, "verified");
    }

    #[test]
    fn find_text_has_four_outputs() {
        let ft = NodeType::FindText(FindTextParams::default());
        assert_eq!(ft.output_schema().len(), 4);
        assert_eq!(ft.output_schema()[0].name, "found");
        assert_eq!(ft.output_schema()[3].name, "coordinates");
    }

    #[test]
    fn click_has_target_ref_input() {
        let ck = NodeType::Click(ClickParams::default());
        assert_eq!(ck.input_schema().len(), 1);
        assert_eq!(ck.input_schema()[0].name, "target_ref");
        assert!(
            ck.input_schema()[0]
                .accepted_types
                .contains(&OutputFieldType::Object)
        );
    }

    #[test]
    fn condition_value_serde_roundtrip() {
        let lit = ConditionValue::Literal {
            value: LiteralValue::Bool { value: true },
        };
        let json = serde_json::to_string(&lit).unwrap();
        let back: ConditionValue = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            ConditionValue::Literal {
                value: LiteralValue::Bool { value: true }
            }
        ));

        let r = ConditionValue::Ref(OutputRef {
            node: "find_text_1".into(),
            field: "count".into(),
        });
        let json = serde_json::to_string(&r).unwrap();
        let back: ConditionValue = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(back, ConditionValue::Ref(OutputRef { ref node, .. }) if node == "find_text_1")
        );
    }
}
