use super::super::Mcp;
use super::super::{ExecutorError, ExecutorResult, WorkflowExecutor};
use super::select_best_window;
use clickweave_core::{ClickParams, NodeRun, NodeType, WindowControlAction};
use clickweave_llm::ChatBackend;
use serde_json::Value;

/// Compute the click coordinates for a window control action given window bounds.
///
/// Returns `(win_x, win_y, click_x, click_y)` or an error if bounds are missing.
pub(super) fn compute_window_control_click(
    window: &Value,
    action: WindowControlAction,
) -> Result<(f64, f64, f64, f64), String> {
    let bounds = &window["bounds"];
    let win_x = bounds["x"]
        .as_f64()
        .ok_or_else(|| "Window bounds missing 'x'".to_string())?;
    let win_y = bounds["y"]
        .as_f64()
        .ok_or_else(|| "Window bounds missing 'y'".to_string())?;
    let (offset_x, offset_y) = action.window_offset();
    Ok((win_x, win_y, win_x + offset_x, win_y + offset_y))
}

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Resolve a window control click (close/minimize/maximize) to absolute
    /// screen coordinates by querying the focused window's bounds and applying
    /// the standard macOS traffic-light button offset.
    pub(in crate::executor) async fn resolve_window_control_click(
        &mut self,
        action: clickweave_core::WindowControlAction,
        mcp: &(impl Mcp + ?Sized),
        params: &ClickParams,
        node_run: &mut Option<&mut NodeRun>,
    ) -> ExecutorResult<NodeType> {
        let app_name = self.focused_app_name();
        if app_name.is_none() {
            return Err(ExecutorError::AppResolution(
                "No focused app — cannot determine target window for control click".to_string(),
            ));
        }
        self.log(format!(
            "Resolving window control '{}' for app {:?}",
            action.display_name(),
            app_name
        ));

        // Focus the window first -- it may be off-screen (different Space,
        // behind other windows) after a CDP relaunch or app switch.
        if let Some(ref name) = app_name {
            let focus_args = Some(serde_json::json!({"app_name": name}));
            let focus_result = mcp
                .call_tool("focus_window", focus_args)
                .await
                .map_err(|e| ExecutorError::ToolCall {
                    tool: "focus_window".to_string(),
                    message: format!("Failed to focus window: {}", e),
                })?;
            Self::check_tool_error(&focus_result, "focus_window")?;
        }

        // Call list_windows to get window bounds.
        let args = app_name
            .as_ref()
            .map(|name| serde_json::json!({"app_name": name}));
        self.record_event(
            node_run.as_deref(),
            "tool_call",
            serde_json::json!({"name": "list_windows", "args": args}),
        );
        let result = mcp
            .call_tool("list_windows", args)
            .await
            .map_err(|e| ExecutorError::ClickTarget(format!("list_windows failed: {}", e)))?;
        Self::check_tool_error(&result, "list_windows")?;

        let result_text = crate::cdp_lifecycle::extract_text(&result);
        let windows: Vec<Value> = serde_json::from_str(&result_text).map_err(|e| {
            ExecutorError::ClickTarget(format!("Failed to parse list_windows response: {e}"))
        })?;

        let window = select_best_window(&windows, app_name.as_deref());

        let window = window.ok_or_else(|| {
            ExecutorError::ClickTarget(format!(
                "No window found for app {:?} to resolve {}",
                app_name,
                action.display_name()
            ))
        })?;

        let (win_x, win_y, click_x, click_y) =
            compute_window_control_click(window, action).map_err(ExecutorError::ClickTarget)?;

        self.log(format!(
            "Resolved {} -> ({click_x}, {click_y}) (window at {win_x}, {win_y})",
            action.display_name()
        ));

        self.record_event(
            node_run.as_deref(),
            "target_resolved",
            serde_json::json!({
                "method": "window_control",
                "action": action.display_name(),
                "window_x": win_x,
                "window_y": win_y,
                "click_x": click_x,
                "click_y": click_y,
            }),
        );

        Ok(NodeType::Click(ClickParams {
            target: Some(clickweave_core::ClickTarget::Coordinates {
                x: click_x,
                y: click_y,
            }),
            button: params.button,
            click_count: params.click_count,
            ..Default::default()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_window(owner: &str, layer: i64, on_screen: bool, x: f64, y: f64) -> Value {
        json!({
            "owner_name": owner,
            "layer": layer,
            "is_on_screen": on_screen,
            "bounds": { "x": x, "y": y, "width": 800.0, "height": 600.0 }
        })
    }

    // --- select_best_window ---

    #[test]
    fn select_window_case_insensitive_match() {
        let windows = vec![make_window("Calculator", 0, true, 100.0, 200.0)];
        let result = select_best_window(&windows, Some("calculator"));
        assert!(result.is_some());
        assert_eq!(result.unwrap()["bounds"]["x"].as_f64(), Some(100.0));
    }

    #[test]
    fn select_window_no_match_returns_none() {
        let windows = vec![make_window("Finder", 0, true, 0.0, 0.0)];
        assert!(select_best_window(&windows, Some("Calculator")).is_none());
    }

    #[test]
    fn select_window_prefers_on_screen() {
        let windows = vec![
            make_window("App", 0, false, 10.0, 10.0),
            make_window("App", 0, true, 20.0, 20.0),
        ];
        let result = select_best_window(&windows, Some("App")).unwrap();
        assert_eq!(result["bounds"]["x"].as_f64(), Some(20.0));
    }

    #[test]
    fn select_window_prefers_lowest_layer() {
        let windows = vec![
            make_window("App", 3, true, 10.0, 10.0),
            make_window("App", 0, true, 20.0, 20.0),
        ];
        let result = select_best_window(&windows, Some("App")).unwrap();
        assert_eq!(result["bounds"]["x"].as_f64(), Some(20.0));
    }

    #[test]
    fn select_window_same_layer_picks_frontmost_by_index() {
        // First window in the list is frontmost (OS z-order).
        let windows = vec![
            make_window("App", 0, true, 10.0, 10.0),
            make_window("App", 0, true, 20.0, 20.0),
        ];
        let result = select_best_window(&windows, Some("App")).unwrap();
        assert_eq!(result["bounds"]["x"].as_f64(), Some(10.0));
    }

    #[test]
    fn select_window_falls_back_to_offscreen() {
        let windows = vec![make_window("App", 0, false, 30.0, 40.0)];
        let result = select_best_window(&windows, Some("App")).unwrap();
        assert_eq!(result["bounds"]["x"].as_f64(), Some(30.0));
    }

    #[test]
    fn select_window_no_app_name_returns_best_overall() {
        let windows = vec![
            make_window("Finder", 0, true, 10.0, 10.0),
            make_window("Calculator", 0, true, 20.0, 20.0),
        ];
        let result = select_best_window(&windows, None).unwrap();
        // No filter -- picks frontmost (first in list).
        assert_eq!(result["bounds"]["x"].as_f64(), Some(10.0));
    }

    #[test]
    fn select_window_empty_list() {
        let windows: Vec<Value> = vec![];
        assert!(select_best_window(&windows, Some("App")).is_none());
        assert!(select_best_window(&windows, None).is_none());
    }

    // --- compute_window_control_click ---

    #[test]
    fn compute_close_click() {
        let window = make_window("App", 0, true, 100.0, 200.0);
        let (wx, wy, cx, cy) =
            compute_window_control_click(&window, WindowControlAction::Close).unwrap();
        assert_eq!((wx, wy), (100.0, 200.0));
        assert_eq!((cx, cy), (114.0, 214.0));
    }

    #[test]
    fn compute_minimize_click() {
        let window = make_window("App", 0, true, 100.0, 200.0);
        let (wx, wy, cx, cy) =
            compute_window_control_click(&window, WindowControlAction::Minimize).unwrap();
        assert_eq!((wx, wy), (100.0, 200.0));
        assert_eq!((cx, cy), (134.0, 214.0));
    }

    #[test]
    fn compute_maximize_click() {
        let window = make_window("App", 0, true, 100.0, 200.0);
        let (wx, wy, cx, cy) =
            compute_window_control_click(&window, WindowControlAction::Maximize).unwrap();
        assert_eq!((wx, wy), (100.0, 200.0));
        assert_eq!((cx, cy), (154.0, 214.0));
    }

    #[test]
    fn compute_click_missing_bounds_errors() {
        let window = json!({"owner_name": "App", "bounds": {}});
        assert!(compute_window_control_click(&window, WindowControlAction::Close).is_err());
    }
}
