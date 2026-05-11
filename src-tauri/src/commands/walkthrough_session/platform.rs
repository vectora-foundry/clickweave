use super::*;

pub(crate) async fn spawn_mcp(mcp_binary_path: &str) -> Option<McpClient> {
    match McpClient::spawn(mcp_binary_path, &[]).await {
        Ok(client) => {
            tracing::info!(
                "MCP client spawned for walkthrough enrichment: {} tools",
                client.tool_count()
            );
            Some(client)
        }
        Err(e) => {
            tracing::warn!(
                "Failed to spawn MCP client for walkthrough: {e}. Continuing without enrichment."
            );
            None
        }
    }
}

pub(crate) async fn populate_app_cache(mcp: &McpClient, cache: &mut HashMap<i32, CachedApp>) {
    let result = mcp
        .call_tool(
            "list_apps",
            Some(serde_json::json!({"user_apps_only": true})),
        )
        .await;

    if let Ok(result) = result {
        for content in &result.content {
            if let Some(text) = content.as_text() {
                for (pid, name, bundle_id) in session_lib::parse_app_list(text) {
                    cache.insert(pid, CachedApp { name, bundle_id });
                }
            }
        }
        tracing::debug!("App cache populated with {} entries", cache.len());
    }
}

pub(super) async fn resolve_app_name(
    pid: i32,
    mcp: &Option<std::sync::Arc<McpClient>>,
    cache: &mut HashMap<i32, CachedApp>,
) -> String {
    if let Some(cached) = cache.get(&pid) {
        return cached.name.clone();
    }

    // Re-fetch the app list from MCP to find the new PID.
    if let Some(mcp) = mcp {
        populate_app_cache(mcp.as_ref(), cache).await;
        if let Some(cached) = cache.get(&pid) {
            return cached.name.clone();
        }
    }

    // Insert negative-cache entry to avoid repeated MCP calls for unknown PIDs.
    let fallback = format!("PID:{pid}");
    cache.insert(
        pid,
        CachedApp {
            name: fallback.clone(),
            bundle_id: None,
        },
    );
    fallback
}
