use super::planner_session::PlannerSession;
use clickweave_core::AppKind;
use clickweave_llm::{ChatBackend, Message};
use serde::Deserialize;
use std::fmt::Write as _;
use std::sync::LazyLock;
use tracing::{info, warn};

/// Result of the pre-gather phase.
pub struct PreGatherResult {
    /// Formatted context text to inject into the planner prompt.
    pub context_text: String,
    /// Whether all discovered apps are CDP-based (Electron/Chrome).
    pub all_cdp: bool,
    /// Whether all discovered apps are native.
    pub all_native: bool,
    /// Whether CDP was successfully connected during pre-gather.
    pub cdp_connected: bool,
}

#[derive(Deserialize)]
struct AppNamesResponse {
    apps: Vec<String>,
}

#[derive(Deserialize)]
struct ProbeResult {
    kind: String,
}

static APP_NAME_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)(?:open|launch|start|use)\s+(?:the\s+)?(.+?)(?:\s+app)?(?:\s+and|\s*,|$)",
    )
    .expect("app name regex should compile")
});

/// Extract app names from the user prompt using the fast model.
/// Falls back to matching against running apps, then regex heuristic.
async fn extract_app_names(
    user_prompt: &str,
    fast_backend: Option<&impl ChatBackend>,
    session: &PlannerSession,
) -> Vec<String> {
    // Try fast model first
    if let Some(backend) = fast_backend {
        let system = "Extract application names from this user request. Return ONLY a JSON object.\n\
                       {\"apps\": [\"Signal\", \"Chrome\"]}\n\
                       If no application names are found, return: {\"apps\": []}";
        let messages = vec![Message::system(system), Message::user(user_prompt)];
        match backend.chat(messages, None).await {
            Ok(response) => {
                if let Some(text) = response
                    .choices
                    .first()
                    .and_then(|c| c.message.content_text())
                {
                    let json_str = text.trim();
                    let json_str = json_str
                        .strip_prefix("```json")
                        .or_else(|| json_str.strip_prefix("```"))
                        .and_then(|s| s.strip_suffix("```"))
                        .map(|s| s.trim())
                        .unwrap_or(json_str);
                    if let Ok(parsed) = serde_json::from_str::<AppNamesResponse>(json_str) {
                        if !parsed.apps.is_empty() {
                            info!("Fast model extracted app names: {:?}", parsed.apps);
                            return parsed.apps;
                        }
                    }
                }
            }
            Err(e) => {
                warn!("Fast model app name extraction failed: {}", e);
            }
        }
    }

    // Fallback: match against running apps
    match session.call_mcp_tool("list_apps", None).await {
        Ok(apps_text) => {
            let prompt_lower = user_prompt.to_lowercase();
            let mut found = Vec::new();
            if let Ok(apps) = serde_json::from_str::<Vec<serde_json::Value>>(&apps_text) {
                for app in &apps {
                    if let Some(name) = app.get("name").and_then(|n| n.as_str()) {
                        if prompt_lower.contains(&name.to_lowercase()) {
                            found.push(name.to_string());
                        }
                    }
                }
            }
            if !found.is_empty() {
                info!("Fallback matched running apps: {:?}", found);
                return found;
            }
        }
        Err(e) => {
            warn!("list_apps failed: {}", e);
        }
    }

    // Final fallback: regex pattern
    let mut names = Vec::new();
    for cap in APP_NAME_REGEX.captures_iter(user_prompt) {
        if let Some(name) = cap.get(1) {
            let n = name.as_str().trim().to_string();
            if !n.is_empty() {
                names.push(n);
            }
        }
    }
    if !names.is_empty() {
        info!("Regex pattern extracted app names: {:?}", names);
        return names;
    }

    info!("No app names extracted from prompt");
    Vec::new()
}

/// Run the pre-gather phase: extract app names, probe, inspect.
pub async fn pre_gather(
    user_prompt: &str,
    session: &PlannerSession,
    fast_backend: Option<&impl ChatBackend>,
) -> PreGatherResult {
    let app_names = extract_app_names(user_prompt, fast_backend, session).await;

    if app_names.is_empty() {
        return PreGatherResult {
            context_text: String::new(),
            all_cdp: false,
            all_native: false,
            cdp_connected: false,
        };
    }

    let mut context = String::from("## Pre-gathered app context\n\n");
    let mut app_kinds: Vec<Option<AppKind>> = Vec::new();
    let mut cdp_connected = false;

    for app_name in &app_names {
        write!(context, "### {}", app_name).unwrap();

        // Probe the app
        let probe_result = match session
            .call_mcp_tool("probe_app", Some(serde_json::json!({"app_name": app_name})))
            .await
        {
            Ok(text) => match serde_json::from_str::<ProbeResult>(&text) {
                Ok(p) => Some(p),
                Err(_) => {
                    writeln!(context, " (probe failed: could not parse result)").unwrap();
                    app_kinds.push(None);
                    context.push('\n');
                    None
                }
            },
            Err(e) => {
                writeln!(context, " (probe failed: {})", e).unwrap();
                app_kinds.push(None);
                context.push('\n');
                None
            }
        };

        let Some(probe) = probe_result else {
            continue;
        };

        let kind = AppKind::parse(&probe.kind);

        writeln!(context, " ({})", probe.kind).unwrap();

        match kind {
            Some(k) if k.uses_cdp() => {
                // CDP path: cdp_connect requires user confirmation (restarts the app).
                use clickweave_llm::planner::tool_use::{PlannerToolExecutor, ToolPermission};

                // Skip if MCP server doesn't advertise cdp_connect
                let has_cdp = session.available_planning_tools().iter().any(|t| {
                    clickweave_llm::planner::tool_use::tool_name(t) == Some("cdp_connect")
                });
                if !has_cdp {
                    writeln!(context, "CDP not available on MCP server").unwrap();
                    app_kinds.push(None); // Don't classify as CDP if connect unavailable
                    context.push('\n');
                    continue;
                }

                let perm = session.permission("cdp_connect");
                if perm == ToolPermission::RequiresConfirmation {
                    let confirmed = session
                        .request_confirmation(
                            &format!("Pre-gather wants to connect to {} via CDP. This will restart the app.", app_name),
                            "cdp_connect",
                        )
                        .await
                        .unwrap_or(false);
                    if !confirmed {
                        writeln!(context, "CDP connect skipped (user declined)").unwrap();
                        app_kinds.push(None); // Don't classify as CDP if user declined
                        context.push('\n');
                        continue;
                    }
                }
                // Try connecting on the default debug port. If the app isn't
                // already running with --remote-debugging-port, this will fail
                // and the LLM planner will handle the full quit→relaunch→connect
                // flow during its conversation loop.
                match session
                    .call_mcp_tool("cdp_connect", Some(serde_json::json!({"port": 9222})))
                    .await
                {
                    Ok(_) => {
                        // Refresh MCP tools so CDP-specific tools become available
                        session.refresh_planning_tools().await;
                        app_kinds.push(kind);
                        cdp_connected = true;

                        if let Ok(pages_text) = session
                            .call_mcp_tool("cdp_list_pages", Some(serde_json::json!({})))
                            .await
                        {
                            writeln!(context, "{}", pages_text.trim()).unwrap();
                        }

                        // Build element inventory
                        match session.build_pre_gather_inventory().await {
                            Ok(inventory) => {
                                writeln!(context, "{}", inventory.trim()).unwrap();
                            }
                            Err(e) => {
                                writeln!(context, "Element inventory failed: {}", e).unwrap();
                            }
                        }
                    }
                    Err(e) => {
                        writeln!(context, "CDP connect failed: {}", e).unwrap();
                        app_kinds.push(None); // Don't classify as CDP if connect failed
                    }
                }
            }
            Some(AppKind::Native) => {
                app_kinds.push(kind);
                // Native path: take accessibility snapshot
                match session
                    .call_mcp_tool(
                        "take_ax_snapshot",
                        Some(serde_json::json!({"app_name": app_name})),
                    )
                    .await
                {
                    Ok(snapshot) => {
                        let max_chars = 4000;
                        if snapshot.len() > max_chars {
                            let truncated_end = snapshot
                                .char_indices()
                                .nth(max_chars)
                                .map(|(i, _)| i)
                                .unwrap_or(snapshot.len());
                            writeln!(
                                context,
                                "Accessibility snapshot (truncated to {} chars):\n{}",
                                max_chars,
                                &snapshot[..truncated_end]
                            )
                            .unwrap();
                        } else {
                            writeln!(context, "Accessibility snapshot:\n{}", snapshot).unwrap();
                        }
                    }
                    Err(e) => {
                        writeln!(context, "Accessibility snapshot failed: {}", e).unwrap();
                    }
                }
            }
            _ => {
                app_kinds.push(kind);
            }
        }

        context.push('\n');
    }

    let all_cdp =
        !app_kinds.is_empty() && app_kinds.iter().all(|k| k.is_some_and(|k| k.uses_cdp()));
    let all_native =
        !app_kinds.is_empty() && app_kinds.iter().all(|k| matches!(k, Some(AppKind::Native)));

    info!(
        "Pre-gather complete: {} apps, all_cdp={}, all_native={}",
        app_names.len(),
        all_cdp,
        all_native
    );

    PreGatherResult {
        context_text: context,
        all_cdp,
        all_native,
        cdp_connected,
    }
}
