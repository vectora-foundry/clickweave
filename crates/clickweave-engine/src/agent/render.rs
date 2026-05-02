#![allow(dead_code)] // Phase 1: module wired to its own tests only; runtime consumers land in later phases.

use std::fmt::Write;

use crate::agent::task_state::TaskState;
use crate::agent::world_model::{ObservedElement, WorldModel};
use clickweave_core::cdp::CdpFindElementMatch;

pub const DEFAULT_MAX_ELEMENTS: usize = 120;
const MAX_RENDERED_TEXT_CHARS: usize = 120;
const MAX_RENDERED_PARENT_CHARS: usize = 80;
const MAX_RENDERED_SAMPLE_CHARS: usize = 80;

/// Render the state block for a single step with the default element cap (D19).
pub fn render_step_input(wm: &WorldModel, ts: &TaskState, current_step: usize) -> String {
    render_step_input_with_cap(wm, ts, current_step, DEFAULT_MAX_ELEMENTS)
}

pub fn render_step_input_with_cap(
    wm: &WorldModel,
    ts: &TaskState,
    current_step: usize,
    max_elements: usize,
) -> String {
    let mut out = String::new();

    // World model block
    let _ = writeln!(out, "<world_model>");
    if let Some(app) = &wm.focused_app {
        let _ = writeln!(
            out,
            "focused_app: {} ({:?}, pid={}) [fresh@{}]",
            app.value.name, app.value.kind, app.value.pid, app.written_at
        );
    }
    if let Some(page) = &wm.cdp_page {
        let _ = writeln!(out, "cdp_page:");
        let _ = writeln!(out, "  url: {}", page.value.url);
        let _ = writeln!(out, "  fingerprint: {}", page.value.page_fingerprint);
        if !page.value.element_inventory.is_empty() {
            let _ = writeln!(out, "  inventory:");
            let shown = page.value.element_inventory.len().min(8);
            for item in page.value.element_inventory.iter().take(shown) {
                let samples = item
                    .sample_labels
                    .iter()
                    .take(3)
                    .map(|s| {
                        format!(
                            "\"{}\"",
                            compact_for_state_with_cap(s, MAX_RENDERED_SAMPLE_CHARS)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                if samples.is_empty() {
                    let _ = writeln!(out, "    {}: {}", item.role, item.count);
                } else {
                    let _ = writeln!(
                        out,
                        "    {}: {} sample_labels=[{}]",
                        item.role, item.count, samples
                    );
                }
            }
            if page.value.element_inventory.len() > shown {
                let _ = writeln!(
                    out,
                    "    ... (+{} roles truncated)",
                    page.value.element_inventory.len() - shown
                );
            }
        }
    }
    if let Some(status) = &wm.cdp_connect_status {
        let _ = writeln!(
            out,
            "cdp_connect_status: {} [fresh@{}]",
            status.value, status.written_at
        );
    }
    if let Some(m) = &wm.modal_present {
        let _ = writeln!(out, "modal_present: {}", m.value);
    }
    if let Some(d) = &wm.dialog_present {
        let _ = writeln!(out, "dialog_present: {}", d.value);
    }
    if let Some(s) = &wm.last_screenshot {
        let _ = writeln!(
            out,
            "last_screenshot: {} (step {})",
            s.value.screenshot_id, s.value.captured_at_step
        );
    }
    if let Some(ax) = &wm.last_native_ax_snapshot {
        let _ = writeln!(
            out,
            "last_native_ax_snapshot: {} elements, captured step {}",
            ax.value.element_count, ax.value.captured_at_step
        );
    }
    if let Some(els) = &wm.elements {
        let total = els.value.len();
        let shown = total.min(max_elements);
        let _ = writeln!(out, "elements ({} of {}):", shown, total);
        for el in els.value.iter().take(shown) {
            match el {
                ObservedElement::Cdp(m) => {
                    let _ = writeln!(out, "{}", render_cdp_element(m));
                }
                ObservedElement::Ax(a) => {
                    let name = a.name.as_deref().unwrap_or("");
                    let _ = writeln!(out, "  [ax] {} {} \"{}\"", a.uid, a.role, name);
                }
                ObservedElement::Ocr(o) => {
                    let _ = writeln!(
                        out,
                        "  [ocr] \"{}\" at ({},{}) {}x{}",
                        o.text, o.x, o.y, o.width, o.height
                    );
                }
            }
        }
        if total > shown {
            let _ = writeln!(out, "  ... (+{} truncated)", total - shown);
        }
    }
    if wm.uncertainty.score > 0.0 {
        let _ = writeln!(
            out,
            "uncertainty: {:.2} ({})",
            wm.uncertainty.score,
            wm.uncertainty.reasons.join(", ")
        );
    }
    let _ = writeln!(out, "</world_model>");

    // Task state block
    let _ = writeln!(out, "<task_state>");
    let _ = writeln!(out, "goal: {}", ts.goal);
    let phase_str = match ts.phase {
        crate::agent::phase::Phase::Exploring => "exploring",
        crate::agent::phase::Phase::Executing => "executing",
        crate::agent::phase::Phase::Recovering => "recovering",
    };
    let _ = writeln!(out, "phase: {}", phase_str);
    if let Some(top) = ts.subgoal_stack.last() {
        let _ = writeln!(out, "active_subgoal: {}", top.text);
        if ts.subgoal_stack.len() > 1 {
            let _ = writeln!(out, "subgoal_stack:");
            for (i, sg) in ts.subgoal_stack.iter().enumerate() {
                let _ = writeln!(out, "  [{}] {}", i, sg.text);
            }
        }
    }
    if !ts.watch_slots.is_empty() {
        let _ = writeln!(out, "watch_slots:");
        for ws in &ts.watch_slots {
            let _ = writeln!(
                out,
                "  {}: {}",
                serde_json::to_string(&ws.name).unwrap().trim_matches('"'),
                ws.note
            );
        }
    }
    if !ts.hypotheses.is_empty() {
        let _ = writeln!(out, "hypotheses:");
        for (i, h) in ts.hypotheses.iter().enumerate() {
            let mark = if h.refuted { " [refuted]" } else { "" };
            let _ = writeln!(out, "  [{}] {}{}", i, h.text, mark);
        }
    }
    if !ts.milestones.is_empty() {
        let _ = writeln!(out, "milestones: {}", ts.milestones.len());
    }
    let _ = writeln!(out, "</task_state>");
    let _ = writeln!(out, "current_step: {}", current_step);

    out
}

fn render_cdp_element(m: &CdpFindElementMatch) -> String {
    let mut line = format!("  [cdp] {} {}", m.uid, m.role);
    append_quoted_if_present(&mut line, "label", &m.label);
    append_quoted_if_distinct(&mut line, "accessible_name", &m.accessible_name, &m.label);
    append_quoted_if_distinct(&mut line, "visible_text", &m.visible_text, &m.label);
    append_quoted_if_distinct(&mut line, "value", &m.value, &m.label);
    append_quoted_if_distinct(&mut line, "placeholder", &m.placeholder, &m.label);
    append_quoted_if_distinct(&mut line, "title", &m.title, &m.label);
    append_quoted_if_distinct(&mut line, "alt_text", &m.alt_text, &m.label);
    append_quoted_if_present(&mut line, "test_id", &m.test_id);
    if !m.tag.is_empty() {
        let _ = write!(line, " tag={}", m.tag);
    }
    if m.disabled {
        line.push_str(" disabled");
    }
    if let Some(rect) = &m.viewport_rect {
        let _ = write!(
            line,
            " rect=({:.0},{:.0} {:.0}x{:.0})",
            rect.x, rect.y, rect.width, rect.height
        );
    }
    if m.in_viewport == Some(false) {
        line.push_str(" offscreen");
    }
    if !m.matched_on.is_empty() {
        let _ = write!(line, " matched_on={}", m.matched_on.join(","));
    }
    if !m.warnings.is_empty() {
        let _ = write!(line, " warnings={}", m.warnings.join(","));
    }
    match (&m.parent_role, &m.parent_name) {
        (Some(role), Some(name)) if !role.is_empty() && !name.is_empty() => {
            let _ = write!(
                line,
                " (in {} \"{}\")",
                role,
                compact_for_state_with_cap(name, MAX_RENDERED_PARENT_CHARS)
            );
        }
        (Some(role), _) if !role.is_empty() => {
            let _ = write!(line, " (in {})", role);
        }
        _ => {}
    }
    line
}

fn append_quoted_if_present(line: &mut String, key: &str, value: &str) {
    if value.trim().is_empty() {
        return;
    }
    let _ = write!(line, " {}=\"{}\"", key, compact_for_state(value));
}

fn append_quoted_if_distinct(line: &mut String, key: &str, value: &str, primary: &str) {
    let normalized = normalized_for_state(value);
    if normalized.is_empty() || normalized == normalized_for_state(primary) {
        return;
    }
    let compact = compact_normalized_for_state(&normalized, MAX_RENDERED_TEXT_CHARS);
    let _ = write!(line, " {}=\"{}\"", key, compact);
}

fn compact_for_state(value: &str) -> String {
    compact_for_state_with_cap(value, MAX_RENDERED_TEXT_CHARS)
}

fn compact_for_state_with_cap(value: &str, cap: usize) -> String {
    compact_normalized_for_state(&normalized_for_state(value), cap)
}

fn normalized_for_state(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn compact_normalized_for_state(value: &str, cap: usize) -> String {
    let mut compact = value.to_string();
    if compact.len() > cap {
        let mut boundary = cap;
        while boundary > 0 && !compact.is_char_boundary(boundary) {
            boundary -= 1;
        }
        compact.truncate(boundary);
        compact.push_str("...");
    }
    compact.replace('"', "\\\"")
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)] // Tests build WorldModel in stages for readability.
mod tests {
    use super::*;
    use crate::agent::task_state::{TaskState, TaskStateMutation, WatchSlotName};
    use crate::agent::world_model::{
        AppKind, AxSnapshotData, CdpElementInventorySummary, CdpPageState, FocusedApp, Fresh,
        FreshnessSource, WorldModel,
    };

    fn make_wm() -> WorldModel {
        WorldModel {
            focused_app: Some(Fresh {
                value: FocusedApp {
                    name: "Chrome".to_string(),
                    kind: AppKind::ChromeBrowser,
                    pid: 1234,
                },
                written_at: 3,
                source: FreshnessSource::DirectObservation,
                ttl_steps: None,
            }),
            cdp_page: Some(Fresh {
                value: CdpPageState {
                    url: "https://example.com/".to_string(),
                    page_fingerprint: "abc".to_string(),
                    element_inventory: Vec::new(),
                },
                written_at: 3,
                source: FreshnessSource::DirectObservation,
                ttl_steps: None,
            }),
            ..WorldModel::default()
        }
    }

    #[test]
    fn renders_minimal_state_block() {
        let ts = TaskState::new("my goal".to_string());
        let wm = WorldModel::default();
        let out = render_step_input(&wm, &ts, 1);
        assert!(out.contains("<world_model>"));
        assert!(out.contains("</world_model>"));
        assert!(out.contains("<task_state>"));
        assert!(out.contains("</task_state>"));
        assert!(out.contains("phase: exploring"));
        assert!(out.contains("goal: my goal"));
    }

    #[test]
    fn renders_focused_app_and_cdp_page() {
        let ts = TaskState::new("g".to_string());
        let wm = make_wm();
        let out = render_step_input(&wm, &ts, 3);
        assert!(out.contains("focused_app: Chrome"));
        assert!(out.contains("url: https://example.com/"));
    }

    #[test]
    fn renders_cdp_inventory_summary() {
        let ts = TaskState::new("g".to_string());
        let mut wm = make_wm();
        wm.cdp_page.as_mut().unwrap().value.element_inventory = vec![CdpElementInventorySummary {
            role: "button".to_string(),
            count: 129,
            sample_labels: vec!["Hide Tabs".to_string(), "Chat with Alice".to_string()],
        }];

        let out = render_step_input(&wm, &ts, 3);

        assert!(out.contains("inventory:"));
        assert!(out.contains("button: 129"));
        assert!(out.contains("sample_labels=[\"Hide Tabs\", \"Chat with Alice\"]"));
    }

    #[test]
    fn renders_cdp_connect_status_when_set() {
        // The status block is the LLM's signal that auto-connect
        // failed permanently (vs. "in flight"). Without this rendering
        // the field would be invisible to the agent.
        let mut wm = WorldModel::default();
        wm.cdp_connect_status = Some(Fresh {
            value: "cdp_connect failed after retries on port 9222 for Signal: timeout".to_string(),
            written_at: 4,
            source: FreshnessSource::DirectObservation,
            ttl_steps: None,
        });
        let ts = TaskState::new("g".to_string());
        let out = render_step_input(&wm, &ts, 5);
        assert!(out.contains("cdp_connect_status:"));
        assert!(out.contains("cdp_connect failed after retries"));
        assert!(out.contains("[fresh@4]"));
    }

    #[test]
    fn omits_cdp_connect_status_block_when_unset() {
        let wm = WorldModel::default();
        let ts = TaskState::new("g".to_string());
        let out = render_step_input(&wm, &ts, 1);
        assert!(!out.contains("cdp_connect_status"));
    }

    #[test]
    fn renders_cdp_visible_text_and_match_provenance() {
        use crate::agent::world_model::ObservedElement;
        use clickweave_core::cdp::{CdpFindElementMatch, CdpViewportRect};

        let mut wm = WorldModel::default();
        wm.elements = Some(Fresh {
            value: vec![ObservedElement::Cdp(CdpFindElementMatch {
                uid: "d7".to_string(),
                role: "button".to_string(),
                label: "Chat with Ljuba Isakovic".to_string(),
                tag: "button".to_string(),
                visible_text: "Note to Self Tue Photo".to_string(),
                matched_on: vec!["visible_text".to_string()],
                warnings: vec!["accessible_name_visible_text_mismatch".to_string()],
                viewport_rect: Some(CdpViewportRect {
                    x: 91.0,
                    y: 157.0,
                    width: 357.0,
                    height: 72.0,
                }),
                in_viewport: Some(true),
                ..Default::default()
            })],
            written_at: 1,
            source: FreshnessSource::DirectObservation,
            ttl_steps: None,
        });

        let out = render_step_input(&wm, &TaskState::new("g".to_string()), 1);

        assert!(out.contains("label=\"Chat with Ljuba Isakovic\""));
        assert!(out.contains("visible_text=\"Note to Self Tue Photo\""));
        assert!(out.contains("matched_on=visible_text"));
        assert!(out.contains("warnings=accessible_name_visible_text_mismatch"));
        assert!(out.contains("rect=(91,157 357x72)"));
    }

    #[test]
    fn renders_cdp_evidence_with_bounded_text_fields() {
        use crate::agent::world_model::ObservedElement;
        use clickweave_core::cdp::CdpFindElementMatch;

        let long_visible_text = format!("{} end", "visible ".repeat(80));
        let long_parent_name = format!("{} end", "parent ".repeat(80));
        let mut wm = WorldModel::default();
        wm.elements = Some(Fresh {
            value: vec![ObservedElement::Cdp(CdpFindElementMatch {
                uid: "d9".to_string(),
                role: "button".to_string(),
                label: "Stable label".to_string(),
                tag: "button".to_string(),
                visible_text: long_visible_text.clone(),
                parent_role: Some("row".to_string()),
                parent_name: Some(long_parent_name.clone()),
                ..Default::default()
            })],
            written_at: 1,
            source: FreshnessSource::DirectObservation,
            ttl_steps: None,
        });

        let out = render_step_input(&wm, &TaskState::new("g".to_string()), 1);
        let cdp_line = out
            .lines()
            .find(|line| line.contains("[cdp] d9"))
            .expect("CDP line should render");

        assert!(cdp_line.contains("visible_text=\"visible visible"));
        assert!(cdp_line.contains("..."));
        assert!(!cdp_line.contains("end\""));
        assert!(
            cdp_line.len() < 360,
            "single CDP evidence line should remain bounded, got {} chars: {cdp_line}",
            cdp_line.len()
        );
    }

    #[test]
    fn renders_subgoal_stack_top_first_most_recent_last() {
        let mut ts = TaskState::new("g".to_string());
        ts.apply(
            &TaskStateMutation::PushSubgoal {
                text: "open login".to_string(),
            },
            1,
        )
        .unwrap();
        ts.apply(
            &TaskStateMutation::PushSubgoal {
                text: "enter password".to_string(),
            },
            2,
        )
        .unwrap();
        let out = render_step_input(&WorldModel::default(), &ts, 3);
        // Top-of-stack (latest push) is the active subgoal.
        assert!(out.contains("active_subgoal: enter password"));
        assert!(out.contains("subgoal_stack:"));
    }

    #[test]
    fn renders_active_watch_slots_only_when_present() {
        let mut ts = TaskState::new("g".to_string());
        let out1 = render_step_input(&WorldModel::default(), &ts, 1);
        assert!(!out1.contains("watch_slots:"));

        ts.apply(
            &TaskStateMutation::SetWatchSlot {
                name: WatchSlotName::PendingModal,
                note: "confirm dialog may appear".to_string(),
            },
            1,
        )
        .unwrap();
        let out2 = render_step_input(&WorldModel::default(), &ts, 2);
        assert!(out2.contains("watch_slots:"));
        assert!(out2.contains("pending_modal"));
    }

    #[test]
    fn renders_ax_snapshot_as_summary_not_body() {
        let mut wm = WorldModel::default();
        wm.last_native_ax_snapshot = Some(Fresh {
            value: AxSnapshotData {
                snapshot_id: "a1g3".to_string(),
                element_count: 42,
                captured_at_step: 5,
                ax_tree_text: "SHOULD NOT APPEAR".to_string(),
            },
            written_at: 5,
            source: FreshnessSource::DirectObservation,
            ttl_steps: None,
        });
        let ts = TaskState::new("g".to_string());
        let out = render_step_input(&wm, &ts, 6);
        assert!(out.contains("42 elements"));
        assert!(
            !out.contains("SHOULD NOT APPEAR"),
            "full AX tree body must not appear in the state block"
        );
    }

    #[test]
    fn renders_element_count_capped_by_max_elements_arg() {
        // Verify truncation signaling when element list exceeds cap.
        use crate::agent::world_model::ObservedElement;
        use clickweave_core::cdp::CdpFindElementMatch;
        let mut wm = WorldModel::default();
        let mut els = Vec::new();
        for i in 0..350 {
            els.push(ObservedElement::Cdp(CdpFindElementMatch {
                uid: format!("d{}", i),
                role: "button".to_string(),
                label: format!("btn{}", i),
                tag: "button".to_string(),
                disabled: false,
                parent_role: None,
                parent_name: None,
                ..Default::default()
            }));
        }
        wm.elements = Some(Fresh {
            value: els,
            written_at: 1,
            source: FreshnessSource::DirectObservation,
            ttl_steps: None,
        });
        let ts = TaskState::new("g".to_string());
        // Cap at 300 even though the runtime default is lower.
        let out = render_step_input_with_cap(&wm, &ts, 2, 300);
        // Should not render the 301st element.
        assert!(!out.contains("d301"));
        // Should render first 300.
        assert!(out.contains("d299"));
        // Should indicate truncation.
        assert!(out.to_lowercase().contains("truncated") || out.contains("+50"));
    }
}
