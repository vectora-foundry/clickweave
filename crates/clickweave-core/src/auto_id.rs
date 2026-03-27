use crate::NodeType;
use std::collections::HashMap;

/// Return the auto-ID base string for a NodeType variant.
pub fn auto_id_base(node_type: &NodeType) -> &'static str {
    match node_type {
        NodeType::FindText(_) => "find_text",
        NodeType::FindImage(_) => "find_image",
        NodeType::FindApp(_) => "find_app",
        NodeType::TakeScreenshot(_) => "take_screenshot",
        NodeType::Click(_) => "click",
        NodeType::Hover(_) => "hover",
        NodeType::Drag(_) => "drag",
        NodeType::TypeText(_) => "type_text",
        NodeType::PressKey(_) => "press_key",
        NodeType::Scroll(_) => "scroll",
        NodeType::FocusWindow(_) => "focus_window",
        NodeType::LaunchApp(_) => "launch_app",
        NodeType::QuitApp(_) => "quit_app",
        NodeType::CdpClick(_) => "cdp_click",
        NodeType::CdpHover(_) => "cdp_hover",
        NodeType::CdpFill(_) => "cdp_fill",
        NodeType::CdpType(_) => "cdp_type",
        NodeType::CdpPressKey(_) => "cdp_press_key",
        NodeType::CdpNavigate(_) => "cdp_navigate",
        NodeType::CdpNewPage(_) => "cdp_new_page",
        NodeType::CdpClosePage(_) => "cdp_close_page",
        NodeType::CdpSelectPage(_) => "cdp_select_page",
        NodeType::CdpWait(_) => "cdp_wait",
        NodeType::CdpHandleDialog(_) => "cdp_handle_dialog",
        NodeType::AiStep(_) => "ai_step",
        NodeType::If(_) => "if",
        NodeType::Switch(_) => "switch",
        NodeType::Loop(_) => "loop",
        NodeType::EndLoop(_) => "end_loop",
        NodeType::McpToolCall(_) => "mcp_tool_call",
        NodeType::AppDebugKitOp(_) => "app_debug_kit_op",
    }
}

/// Assign an auto_id using the workflow's counters. Returns the new auto_id.
pub fn assign_auto_id(node_type: &NodeType, counters: &mut HashMap<String, u32>) -> String {
    let base = auto_id_base(node_type);
    let counter = if let Some(c) = counters.get_mut(base) {
        *c += 1;
        *c
    } else {
        counters.insert(base.to_string(), 1);
        1
    };
    format!("{}_{}", base, counter)
}

/// Scan existing nodes' auto_ids and set counters to max(existing).
/// Called on workflow load when `next_id_counters` is empty/missing.
/// Stores the highest seen number so `assign_auto_id` (which increments
/// before returning) produces the correct next value.
pub fn fixup_counters(existing_auto_ids: &[&str], counters: &mut HashMap<String, u32>) {
    for auto_id in existing_auto_ids {
        if let Some(pos) = auto_id.rfind('_') {
            let base = &auto_id[..pos];
            if let Ok(num) = auto_id[pos + 1..].parse::<u32>() {
                let entry = counters.entry(base.to_string()).or_insert(0);
                if num > *entry {
                    *entry = num;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;

    #[test]
    fn assign_auto_id_increments() {
        let mut counters = HashMap::new();
        let ft = NodeType::FindText(FindTextParams::default());
        assert_eq!(assign_auto_id(&ft, &mut counters), "find_text_1");
        assert_eq!(assign_auto_id(&ft, &mut counters), "find_text_2");
        let ck = NodeType::Click(ClickParams::default());
        assert_eq!(assign_auto_id(&ck, &mut counters), "click_1");
        assert_eq!(assign_auto_id(&ft, &mut counters), "find_text_3");
    }

    #[test]
    fn fixup_counters_from_existing() {
        let mut counters = HashMap::new();
        fixup_counters(&["find_text_1", "find_text_3", "click_2"], &mut counters);
        // Stores max seen (3 and 2), so next assign_auto_id produces 4 and 3
        assert_eq!(counters["find_text"], 3);
        assert_eq!(counters["click"], 2);
    }

    #[test]
    fn fixup_then_assign_produces_correct_next() {
        let mut counters = HashMap::new();
        fixup_counters(&["find_text_1", "find_text_3"], &mut counters);
        let ft = NodeType::FindText(FindTextParams::default());
        // Should produce find_text_4 (counter was 3, increments to 4)
        assert_eq!(assign_auto_id(&ft, &mut counters), "find_text_4");
    }

    #[test]
    fn fixup_counters_handles_empty() {
        let mut counters = HashMap::new();
        fixup_counters(&[], &mut counters);
        assert!(counters.is_empty());
    }
}
