use super::*;

pub(super) fn ax_tool_invocation_to_node_type(name: &str, args: &Value) -> Option<NodeType> {
    match name {
        // AX dispatch — macOS accessibility-tree actions. The agent passes a
        // uid captured from the most recent `take_ax_snapshot`; we store it as
        // `AxTarget::ResolvedUid`. An agent-loop post-hook upgrades this to
        // `AxTarget::Descriptor { role, name }` using the snapshot so the node
        // is replay-stable across snapshot generations.
        "ax_click" => Some(NodeType::AxClick(AxClickParams {
            target: AxTarget::ResolvedUid(optional_str(args, "uid")),
            ..Default::default()
        })),
        "ax_set_value" => Some(NodeType::AxSetValue(AxSetValueParams {
            target: AxTarget::ResolvedUid(optional_str(args, "uid")),
            value: optional_str(args, "value"),
            ..Default::default()
        })),
        "ax_select" => Some(NodeType::AxSelect(AxSelectParams {
            target: AxTarget::ResolvedUid(optional_str(args, "uid")),
            ..Default::default()
        })),
        _ => None,
    }
}
