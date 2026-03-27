use std::collections::{HashMap, HashSet, VecDeque};

use uuid::Uuid;

use crate::output_schema::NodeContext;
use crate::{Node, NodeType, Workflow};

/// Validation warning (not error) for CDP nodes without a CDP scope.
#[derive(Debug, Clone)]
pub struct CdpScopeWarning {
    pub node_name: String,
    pub node_id: Uuid,
    pub message: String,
}

/// Check that each CDP node has an upstream FocusWindow targeting a CDP-capable
/// app in its execution path. Returns warnings, not errors.
pub(crate) fn validate_cdp_scope(workflow: &Workflow) -> Vec<CdpScopeWarning> {
    let mut warnings = Vec::new();

    // Build node lookup map for O(1) access
    let node_map: HashMap<Uuid, &Node> = workflow.nodes.iter().map(|n| (n.id, n)).collect();

    // Build reverse adjacency list
    let mut predecessors: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for edge in &workflow.edges {
        predecessors.entry(edge.to).or_default().push(edge.from);
    }

    for node in &workflow.nodes {
        if node.node_type.node_context() != NodeContext::Cdp {
            continue;
        }

        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        let mut found_cdp_scope = false;

        if let Some(preds) = predecessors.get(&node.id) {
            for &pred_id in preds {
                queue.push_back(pred_id);
            }
        }

        while let Some(current_id) = queue.pop_front() {
            if !visited.insert(current_id) {
                continue;
            }
            let Some(current) = node_map.get(&current_id).copied() else {
                continue;
            };

            match &current.node_type {
                NodeType::FocusWindow(p) if p.app_kind.uses_cdp() => {
                    found_cdp_scope = true;
                    break;
                }
                NodeType::FocusWindow(_) | NodeType::QuitApp(_) => {
                    // Scope broken by a non-CDP focus or quit — don't walk
                    // further up this path.
                    continue;
                }
                _ => {}
            }

            if let Some(preds) = predecessors.get(&current_id) {
                for &pred_id in preds {
                    queue.push_back(pred_id);
                }
            }
        }

        if !found_cdp_scope {
            warnings.push(CdpScopeWarning {
                node_name: node.name.clone(),
                node_id: node.id,
                message: format!(
                    "{} may execute without a CDP app focused. \
                     Add a FocusWindow targeting Chrome or an Electron app before it.",
                    node.node_type.display_name()
                ),
            });
        }
    }

    warnings
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::pos;
    use super::super::validate_workflow;
    use crate::{
        AppKind, CdpClickParams, ClickParams, FocusMethod, FocusWindowParams, NodeType,
        QuitAppParams, Workflow,
    };

    #[test]
    fn cdp_node_with_upstream_chrome_focus_no_warning() {
        let mut wf = Workflow::default();
        let focus = wf.add_node(
            NodeType::FocusWindow(FocusWindowParams {
                method: FocusMethod::AppName,
                value: Some("Google Chrome".to_string()),
                bring_to_front: true,
                app_kind: AppKind::ChromeBrowser,
                ..Default::default()
            }),
            pos(0.0, 0.0),
        );
        let cdp = wf.add_node(
            NodeType::CdpClick(CdpClickParams::default()),
            pos(100.0, 0.0),
        );
        wf.add_edge(focus, cdp);

        let result = validate_workflow(&wf).expect("should pass validation");
        assert!(result.warnings.is_empty(), "expected no warnings");
    }

    #[test]
    fn cdp_node_without_upstream_focus_warns() {
        let mut wf = Workflow::default();
        wf.add_node(NodeType::CdpClick(CdpClickParams::default()), pos(0.0, 0.0));

        let result = validate_workflow(&wf).expect("should pass validation");
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].message.contains("CDP app focused"));
    }

    #[test]
    fn cdp_node_after_native_focus_warns() {
        let mut wf = Workflow::default();
        let focus = wf.add_node(
            NodeType::FocusWindow(FocusWindowParams {
                method: FocusMethod::AppName,
                value: Some("Calculator".to_string()),
                bring_to_front: true,
                app_kind: AppKind::Native,
                ..Default::default()
            }),
            pos(0.0, 0.0),
        );
        let cdp = wf.add_node(
            NodeType::CdpClick(CdpClickParams::default()),
            pos(100.0, 0.0),
        );
        wf.add_edge(focus, cdp);

        let result = validate_workflow(&wf).expect("should pass validation");
        assert_eq!(result.warnings.len(), 1);
    }

    #[test]
    fn cdp_scope_broken_by_quit_app() {
        let mut wf = Workflow::default();
        let focus = wf.add_node(
            NodeType::FocusWindow(FocusWindowParams {
                method: FocusMethod::AppName,
                value: Some("Google Chrome".to_string()),
                bring_to_front: true,
                app_kind: AppKind::ChromeBrowser,
                ..Default::default()
            }),
            pos(0.0, 0.0),
        );
        let quit = wf.add_node(
            NodeType::QuitApp(QuitAppParams {
                app_name: "Google Chrome".to_string(),
                ..Default::default()
            }),
            pos(100.0, 0.0),
        );
        let cdp = wf.add_node(
            NodeType::CdpClick(CdpClickParams::default()),
            pos(200.0, 0.0),
        );
        wf.add_edge(focus, quit);
        wf.add_edge(quit, cdp);

        let result = validate_workflow(&wf).expect("should pass validation");
        assert_eq!(result.warnings.len(), 1);
    }

    #[test]
    fn native_only_workflow_no_warnings() {
        let mut wf = Workflow::default();
        let a = wf.add_node(NodeType::Click(ClickParams::default()), pos(0.0, 0.0));
        let b = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 0.0));
        wf.add_edge(a, b);

        let result = validate_workflow(&wf).expect("should pass validation");
        assert!(result.warnings.is_empty());
    }
}
