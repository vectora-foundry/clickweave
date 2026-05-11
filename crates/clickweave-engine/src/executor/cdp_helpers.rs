//! Standalone CDP-process helpers preserved across the 1.D executor
//! tear-down. The agent's CDP lifecycle still needs them (the deleted
//! `WorkflowExecutor::deterministic` module previously housed them).

/// Check if an app is already running with `--remote-debugging-port=<N>`.
/// Returns the port if found, so the caller can skip the quit/relaunch
/// cycle.
pub(crate) async fn existing_debug_port(app_name: &str) -> Option<u16> {
    #[cfg(target_os = "windows")]
    return None;

    #[cfg(not(target_os = "windows"))]
    {
        let output = tokio::process::Command::new("pgrep")
            .args(["-x", app_name])
            .output()
            .await
            .ok()?;
        if !output.status.success() {
            tracing::info!(
                "existing_debug_port: pgrep -x '{}' found no processes",
                app_name
            );
            return None;
        }
        let pids = String::from_utf8_lossy(&output.stdout);
        tracing::info!(
            "existing_debug_port: pgrep -x '{}' found pids: {}",
            app_name,
            pids.trim()
        );
        for pid_str in pids.split_whitespace() {
            let Ok(args_output) = tokio::process::Command::new("ps")
                .args(["-p", pid_str, "-o", "args="])
                .output()
                .await
            else {
                continue;
            };
            let args = String::from_utf8_lossy(&args_output.stdout);
            tracing::info!("existing_debug_port: pid {} args: {}", pid_str, args.trim());
            if let Some(flag) = args
                .split_whitespace()
                .find(|a| a.starts_with("--remote-debugging-port="))
                && let Some(port_str) = flag.strip_prefix("--remote-debugging-port=")
                && let Ok(port) = port_str.parse::<u16>()
            {
                tracing::info!(
                    "existing_debug_port: found port {} for '{}'",
                    port,
                    app_name
                );
                return Some(port);
            }
        }
        tracing::info!(
            "existing_debug_port: no debug port found for '{}'",
            app_name
        );
        None
    }
}
