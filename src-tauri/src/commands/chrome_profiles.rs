use super::error::CommandError;
use super::types::AppDataDir;
use clickweave_core::chrome_profiles::{ChromeProfile, ChromeProfileStore};
use tauri::Manager;

fn get_store(app: &tauri::AppHandle) -> ChromeProfileStore {
    let app_data = app.state::<AppDataDir>();
    ChromeProfileStore::new(app_data.0.join("chrome-profiles"))
}

#[tauri::command]
#[specta::specta]
pub fn list_chrome_profiles(app: tauri::AppHandle) -> Result<Vec<ChromeProfile>, CommandError> {
    let store = get_store(&app);
    store
        .ensure_profiles()
        .map_err(|e| CommandError::io(e.to_string()))
}

#[tauri::command]
#[specta::specta]
pub fn create_chrome_profile(
    app: tauri::AppHandle,
    name: String,
) -> Result<ChromeProfile, CommandError> {
    let store = get_store(&app);
    store
        .create_profile(&name)
        .map_err(|e| CommandError::io(e.to_string()))
}

#[tauri::command]
#[specta::specta]
pub fn is_chrome_profile_configured(
    app: tauri::AppHandle,
    profile_id: String,
) -> Result<bool, CommandError> {
    let store = get_store(&app);
    Ok(store.is_configured(&profile_id))
}

#[tauri::command]
#[specta::specta]
pub fn get_chrome_profile_path(
    app: tauri::AppHandle,
    profile_id: String,
) -> Result<String, CommandError> {
    let store = get_store(&app);
    Ok(store
        .profile_path(&profile_id)
        .to_string_lossy()
        .into_owned())
}

#[tauri::command]
#[specta::specta]
pub async fn launch_chrome_for_setup(
    app: tauri::AppHandle,
    profile_id: String,
) -> Result<(), CommandError> {
    let store = get_store(&app);
    let profile_path = store.profile_path(&profile_id);
    std::fs::create_dir_all(&profile_path).map_err(|e| CommandError::io(e.to_string()))?;

    let dir_str = profile_path.to_string_lossy().to_string();
    let args = [
        format!("--user-data-dir={}", dir_str),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
    ];

    use std::process::Stdio;

    // Launch the Chrome binary directly — `open -a` reuses the existing
    // instance and ignores --args when Chrome is already running.
    #[cfg(target_os = "macos")]
    let result = tokio::process::Command::new(
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    )
    .args(&args)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn();

    #[cfg(target_os = "windows")]
    let result = tokio::process::Command::new("chrome")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    #[cfg(target_os = "linux")]
    let result = tokio::process::Command::new("google-chrome")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    result
        .map(|_| ())
        .map_err(|e| CommandError::io(format!("Failed to launch Chrome: {}", e)))?;

    Ok(())
}
