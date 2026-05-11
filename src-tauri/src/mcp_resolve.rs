/// Resolve the path to the native-devtools-mcp binary as a UTF-8 string.
///
/// In debug builds, checks `CLICKWEAVE_MCP_BINARY` env var first.
/// Otherwise resolves relative to the current executable (where Tauri
/// places sidecar binaries: `Contents/MacOS/` on macOS, install dir on Windows).
pub fn resolve_mcp_binary() -> anyhow::Result<String> {
    #[cfg(debug_assertions)]
    if let Ok(path) = std::env::var("CLICKWEAVE_MCP_BINARY") {
        if let Ok(path) = validated_mcp_binary_path(std::path::Path::new(&path)) {
            tracing::info!("Using MCP binary from env: {}", path);
            return Ok(path);
        }
        tracing::warn!(
            "CLICKWEAVE_MCP_BINARY='{}' not found, falling back to sidecar",
            path
        );
    }

    let exe_dir = std::env::current_exe()?
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine executable directory"))?
        .to_path_buf();

    let sidecar_path = sidecar_path(&exe_dir);
    let path_str = validated_mcp_binary_path(&sidecar_path)?;

    tracing::info!("Using sidecar MCP binary: {}", path_str);
    Ok(path_str)
}

fn sidecar_path(exe_dir: &std::path::Path) -> std::path::PathBuf {
    let binary_name = if cfg!(target_os = "windows") {
        "native-devtools-mcp.exe"
    } else {
        "native-devtools-mcp"
    };

    exe_dir.join(binary_name)
}

fn validated_mcp_binary_path(path: &std::path::Path) -> anyhow::Result<String> {
    if !path.is_file() {
        anyhow::bail!("MCP binary not found at {}", path.display());
    }

    Ok(path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("MCP binary path is not valid UTF-8"))?
        .to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_path_requires_existing_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let err = validated_mcp_binary_path(&sidecar_path(tmp.path())).unwrap_err();

        assert!(err.to_string().contains("MCP binary not found"));
    }

    #[test]
    fn sidecar_path_accepts_existing_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let sidecar = sidecar_path(tmp.path());
        std::fs::write(&sidecar, b"binary").unwrap();

        let path = validated_mcp_binary_path(&sidecar).unwrap();

        assert_eq!(path, sidecar.to_str().unwrap());
    }
}
