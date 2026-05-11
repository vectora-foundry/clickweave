use super::*;

pub(super) fn cdp_tool_invocation_to_node_type(name: &str, args: &Value) -> Option<NodeType> {
    match name {
        // CDP tool mappings — prefixed names for agent disambiguation
        "cdp_click" => {
            let uid = optional_str(args, "uid");
            let target_str = args
                .get("target")
                .or_else(|| args.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // Both `uid` and `target`/`text` from the agent are exact element
            // labels (the prompt instructs "use the exact element name from
            // cdp_find_elements"). Intent is only constructed programmatically
            // for runtime-resolution paths, never from agent output.
            let label = if !uid.is_empty() { uid } else { target_str };
            let target = if label.is_empty() {
                CdpTarget::default()
            } else {
                CdpTarget::ExactLabel(label)
            };
            Some(NodeType::CdpClick(CdpClickParams {
                target,
                ..Default::default()
            }))
        }
        "cdp_hover" => {
            let uid = optional_str(args, "uid");
            let target_str = args
                .get("target")
                .or_else(|| args.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let label = if !uid.is_empty() { uid } else { target_str };
            let target = if label.is_empty() {
                CdpTarget::default()
            } else {
                CdpTarget::ExactLabel(label)
            };
            Some(NodeType::CdpHover(CdpHoverParams {
                target,
                ..Default::default()
            }))
        }
        "cdp_type_text" => Some(NodeType::CdpType(CdpTypeParams {
            text: optional_str(args, "text"),
            ..Default::default()
        })),
        "cdp_press_key" => Some(NodeType::CdpPressKey(CdpPressKeyParams {
            key: optional_str(args, "key"),
            modifiers: args
                .get("modifiers")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            ..Default::default()
        })),
        // CDP tool mappings — accept both old (fill, navigate_page) and new (cdp_fill, cdp_navigate) names
        "fill" | "cdp_fill" => Some(NodeType::CdpFill(CdpFillParams {
            target: CdpTarget::ExactLabel(optional_str(args, "uid")),
            value: optional_str(args, "value"),
            ..Default::default()
        })),
        "navigate_page" | "cdp_navigate" => Some(NodeType::CdpNavigate(CdpNavigateParams {
            url: optional_str(args, "url"),
            ..Default::default()
        })),
        "new_page" | "cdp_new_page" => Some(NodeType::CdpNewPage(CdpNewPageParams {
            url: optional_str(args, "url"),
            ..Default::default()
        })),
        "close_page" | "cdp_close_page" => Some(NodeType::CdpClosePage(CdpClosePageParams {
            page_index: args
                .get("page_index")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32),
            ..Default::default()
        })),
        "select_page" | "cdp_select_page" => Some(NodeType::CdpSelectPage(CdpSelectPageParams {
            page_index: args.get("page_index").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            ..Default::default()
        })),
        "wait_for" | "cdp_wait_for" => Some(NodeType::CdpWait(CdpWaitParams {
            text: optional_str(args, "text"),
            timeout_ms: args
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(10_000),
        })),
        "handle_dialog" | "cdp_handle_dialog" => {
            Some(NodeType::CdpHandleDialog(CdpHandleDialogParams {
                accept: args.get("accept").and_then(|v| v.as_bool()).unwrap_or(true),
                prompt_text: args
                    .get("prompt_text")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                ..Default::default()
            }))
        }
        // CDP inspection tools — available after cdp_connect, not always in known_tools
        "cdp_take_snapshot" | "cdp_list_pages" => Some(NodeType::McpToolCall(McpToolCallParams {
            tool_name: name.to_string(),
            arguments: args.clone(),
        })),
        _ => None,
    }
}
