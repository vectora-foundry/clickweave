use super::*;

// --- Draft synthesis ---

/// Vertical spacing between auto-positioned nodes (pixels in canvas coords).
const NODE_Y_SPACING: f32 = 100.0;
const NODE_X_POSITION: f32 = 250.0;

/// Synthesize a linear workflow draft from normalized walkthrough actions.
///
/// Pure function — no I/O. Produces a valid `Workflow` with linear edges.
pub fn synthesize_draft(
    actions: &[WalkthroughAction],
    workflow_id: Uuid,
    workflow_name: &str,
) -> crate::Workflow {
    let mut workflow = Workflow {
        id: workflow_id,
        name: workflow_name.to_string(),
        nodes: Vec::new(),
        edges: Vec::new(),
        groups: Vec::new(),
        next_id_counters: std::collections::HashMap::new(),
        intent: None,
    };

    let mut node_index = 0usize;
    for action in actions {
        // Skip unconfirmed candidates (e.g. hover suggestions the user hasn't kept).
        if action.candidate {
            continue;
        }
        let position = Position {
            x: NODE_X_POSITION,
            y: (node_index as f32) * NODE_Y_SPACING,
        };
        node_index += 1;

        let (node_type, name) = node_for_action(action);

        let auto_id = crate::auto_id::assign_auto_id(&node_type, &mut workflow.next_id_counters);
        let node = Node::new(node_type, position, name, auto_id);
        workflow.nodes.push(node);
    }

    // Wire linear edges.
    for i in 0..workflow.nodes.len().saturating_sub(1) {
        let from = workflow.nodes[i].id;
        let to = workflow.nodes[i + 1].id;
        workflow.edges.push(Edge { from, to });
    }

    workflow
}

fn node_for_action(action: &WalkthroughAction) -> (NodeType, String) {
    match &action.kind {
        WalkthroughActionKind::LaunchApp { app_name, app_kind } => (
            focus_window_node(app_name, *app_kind),
            format!("Launch {app_name}"),
        ),
        WalkthroughActionKind::FocusWindow {
            app_name,
            window_title,
            app_kind,
        } => (
            focus_window_node(app_name, *app_kind),
            focus_window_name(app_name, window_title),
        ),
        WalkthroughActionKind::Click {
            x,
            y,
            button,
            click_count,
        } => click_node(action, *x, *y, *button, *click_count),
        WalkthroughActionKind::TypeText { text } => (
            NodeType::TypeText(TypeTextParams {
                text: text.clone(),
                ..Default::default()
            }),
            type_text_name(text),
        ),
        WalkthroughActionKind::PressKey { key, modifiers } => (
            NodeType::PressKey(PressKeyParams {
                key: key.clone(),
                modifiers: modifiers.clone(),
                ..Default::default()
            }),
            press_key_name(key, modifiers),
        ),
        WalkthroughActionKind::Scroll { delta_y } => (
            NodeType::Scroll(ScrollParams {
                delta_y: *delta_y as i32,
                x: None,
                y: None,
                ..Default::default()
            }),
            format!("Scroll {}", if *delta_y < 0.0 { "up" } else { "down" }),
        ),
        WalkthroughActionKind::Hover { x, y, dwell_ms } => hover_node(action, *x, *y, *dwell_ms),
    }
}

fn focus_window_node(app_name: &str, app_kind: crate::AppKind) -> NodeType {
    NodeType::FocusWindow(FocusWindowParams {
        target: FocusTarget::AppName(app_name.to_string()),
        bring_to_front: true,
        app_kind,
        chrome_profile_id: None,
        ..Default::default()
    })
}

fn focus_window_name(app_name: &str, window_title: &Option<String>) -> String {
    match window_title {
        Some(t) => format!("Focus '{t}'"),
        None => format!("Focus {app_name}"),
    }
}

fn click_node(
    action: &WalkthroughAction,
    x: f64,
    y: f64,
    button: MouseButton,
    click_count: u32,
) -> (NodeType, String) {
    if let Some(wc_action) = window_control_candidate(action) {
        let name = wc_action.display_name().to_string();
        let params = ClickParams {
            target: Some(ClickTarget::WindowControl { action: wc_action }),
            button,
            click_count,
            ..Default::default()
        };
        return (NodeType::Click(params), name);
    }

    if let Some((role, name, parent_name)) = ax_candidate(action) {
        return ax_click_or_select_node(role, name, parent_name);
    }

    if let Some(cdp_name) = cdp_candidate(action) {
        return (
            NodeType::CdpClick(CdpClickParams {
                target: CdpTarget::ExactLabel(cdp_name.clone()),
                ..Default::default()
            }),
            format!("Click '{cdp_name}'"),
        );
    }

    if let Some(target) = preferred_text_candidate(action) {
        return (
            NodeType::Click(ClickParams {
                target: Some(ClickTarget::Text {
                    text: target.clone(),
                }),
                button,
                click_count,
                ..Default::default()
            }),
            format!("Click '{target}'"),
        );
    }

    (
        NodeType::Click(ClickParams {
            target: Some(ClickTarget::Coordinates { x, y }),
            button,
            click_count,
            ..Default::default()
        }),
        format!("Click ({x:.0}, {y:.0})"),
    )
}

fn window_control_candidate(
    action: &WalkthroughAction,
) -> Option<crate::node_params::WindowControlAction> {
    action.target_candidates.iter().find_map(|c| match c {
        TargetCandidate::WindowControl { action } => Some(*action),
        _ => None,
    })
}

fn ax_candidate(action: &WalkthroughAction) -> Option<(String, String, Option<String>)> {
    action.target_candidates.iter().find_map(|c| match c {
        TargetCandidate::AxElement {
            role,
            name,
            parent_name,
        } => Some((role.clone(), name.clone(), parent_name.clone())),
        _ => None,
    })
}

fn ax_click_or_select_node(
    role: String,
    name: String,
    parent_name: Option<String>,
) -> (NodeType, String) {
    // AXRow / AXOutlineRow targets fire AXSelectedRows on the enclosing
    // outline/table, not AXPress, so ax_select is the right MCP entry point.
    let label = if name.is_empty() {
        role.clone()
    } else {
        name.clone()
    };
    let is_row = role == "AXRow" || role == "AXOutlineRow";
    let ax_target = AxTarget::Descriptor {
        role,
        name,
        parent_name,
    };
    if is_row {
        (
            NodeType::AxSelect(AxSelectParams {
                target: ax_target,
                ..Default::default()
            }),
            format!("Select '{label}'"),
        )
    } else {
        (
            NodeType::AxClick(AxClickParams {
                target: ax_target,
                ..Default::default()
            }),
            format!("Click '{label}'"),
        )
    }
}

fn cdp_candidate(action: &WalkthroughAction) -> Option<&String> {
    action.target_candidates.iter().find_map(|c| match c {
        TargetCandidate::CdpElement { name, .. } => Some(name),
        _ => None,
    })
}

fn preferred_text_candidate(action: &WalkthroughAction) -> Option<String> {
    action
        .target_candidates
        .iter()
        .find_map(|c| c.preferred_label().map(|s| s.to_string()))
}

fn type_text_name(text: &str) -> String {
    if text.chars().count() > 20 {
        let truncated: String = text.chars().take(20).collect();
        format!("Type '{truncated}'...")
    } else {
        format!("Type '{text}'")
    }
}

fn press_key_name(key: &str, modifiers: &[String]) -> String {
    WindowControl::from_shortcut(key, modifiers)
        .map(|wc| wc.display_name().to_string())
        .or_else(|| shortcut_display_name(key, modifiers))
        .unwrap_or_else(|| {
            if modifiers.is_empty() {
                format!("Press {key}")
            } else {
                format!("Press {}+{key}", modifiers.join("+"))
            }
        })
}

fn hover_node(action: &WalkthroughAction, x: f64, y: f64, dwell_ms: u64) -> (NodeType, String) {
    if let Some(cdp_name) = cdp_candidate(action) {
        return (
            NodeType::CdpHover(CdpHoverParams {
                target: CdpTarget::ExactLabel(cdp_name.clone()),
                ..Default::default()
            }),
            format!("Hover '{cdp_name}'"),
        );
    }

    if let Some(target) = preferred_text_candidate(action) {
        return (
            NodeType::Hover(HoverParams {
                target: Some(ClickTarget::Text {
                    text: target.clone(),
                }),
                dwell_ms,
                ..Default::default()
            }),
            format!("Hover '{target}'"),
        );
    }

    (
        NodeType::Hover(HoverParams {
            target: Some(ClickTarget::Coordinates { x, y }),
            dwell_ms,
            ..Default::default()
        }),
        format!("Hover ({x:.0}, {y:.0})"),
    )
}
