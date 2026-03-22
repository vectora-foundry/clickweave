fn main() {
    // In release builds, verify the sidecar binary exists so CI fails fast
    // if the download step was missed. Tauri also checks this, but our error
    // message is more actionable.
    if std::env::var("PROFILE").unwrap_or_default() == "release" {
        let target = std::env::var("TARGET").unwrap_or_default();
        let binary_name = if target.contains("windows") {
            format!("native-devtools-mcp-{}.exe", target)
        } else {
            format!("native-devtools-mcp-{}", target)
        };
        let sidecar_path = std::path::Path::new("binaries").join(&binary_name);
        if !sidecar_path.exists() {
            panic!(
                "Sidecar binary not found at {}. \
                 Run scripts/fetch-sidecar.sh or download the binary manually.",
                sidecar_path.display()
            );
        }
    }

    tauri_build::build()
}
