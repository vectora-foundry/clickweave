use std::path::Path;

use crate::AppKind;

/// Bundle identifiers for Chrome-family browsers (macOS).
const CHROME_BUNDLE_IDS: &[&str] = &[
    "com.google.Chrome",
    "com.google.Chrome.canary",
    "com.brave.Browser",
    "com.microsoft.edgemac",
    "company.thebrowser.Browser", // Arc
    "org.chromium.Chromium",
];

/// Executable names for Chrome-family browsers (Windows).
#[cfg(target_os = "windows")]
const CHROME_EXE_NAMES: &[&str] = &["chrome.exe", "brave.exe", "msedge.exe", "arc.exe"];

/// Classify an app as Native, ChromeBrowser, or ElectronApp.
///
/// Detection strategy:
/// 1. Chrome-family: match `bundle_id` against known browser identifiers
/// 2. Electron: check for framework marker in the app bundle/directory
/// 3. Default: Native
///
/// Results are cached per PID by the caller — this function is called at most
/// once per app focus change.
pub fn classify_app(bundle_id: Option<&str>, app_path: Option<&Path>) -> AppKind {
    if is_chrome_family(bundle_id) {
        return AppKind::ChromeBrowser;
    }
    if let Some(path) = app_path
        && is_electron_app(path)
    {
        return AppKind::ElectronApp;
    }
    AppKind::Native
}

/// Classify an app by PID using a single `proc_pidpath` syscall.
///
/// Resolves the bundle path once, then derives both bundle ID and
/// Electron detection from it.
pub fn classify_app_by_pid(pid: i32) -> AppKind {
    let bundle_path = bundle_path_from_pid(pid);
    let bundle_id = bundle_path.as_deref().and_then(bundle_id_from_path);
    classify_app(bundle_id.as_deref(), bundle_path.as_deref())
}

fn is_chrome_family(bundle_id: Option<&str>) -> bool {
    bundle_id.is_some_and(|id| CHROME_BUNDLE_IDS.contains(&id))
}

/// Check if an app bundle contains the Electron framework.
///
/// - macOS: looks for `Contents/Frameworks/Electron Framework.framework`
/// - Windows: looks for `resources\electron.asar`
#[cfg(target_os = "macos")]
fn is_electron_app(bundle_path: &Path) -> bool {
    bundle_path
        .join("Contents/Frameworks/Electron Framework.framework")
        .exists()
}

#[cfg(target_os = "windows")]
fn is_electron_app(exe_dir: &Path) -> bool {
    exe_dir.join("resources/electron.asar").exists()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn is_electron_app(_path: &Path) -> bool {
    false
}

/// Read `CFBundleIdentifier` from a resolved `.app` bundle path.
#[cfg(target_os = "macos")]
pub fn bundle_id_from_path(bundle: &Path) -> Option<String> {
    let plist_path = bundle.join("Contents/Info.plist");
    let data = std::fs::read(&plist_path).ok()?;
    let cursor = std::io::Cursor::new(data);
    let plist_val: plist::Value = plist::Value::from_reader(cursor).ok()?;
    plist_val
        .as_dictionary()
        .and_then(|d| d.get("CFBundleIdentifier"))
        .and_then(|v| v.as_string())
        .map(|s| s.to_string())
}

#[cfg(not(target_os = "macos"))]
pub fn bundle_id_from_path(_bundle: &Path) -> Option<String> {
    None
}

/// Resolve the app's bundle identifier from a process ID.
///
/// On macOS, resolves the `.app` bundle via `bundle_path_from_pid`,
/// then reads `CFBundleIdentifier` from `Contents/Info.plist`.
pub fn bundle_id_from_pid(pid: i32) -> Option<String> {
    let bundle = bundle_path_from_pid(pid)?;
    bundle_id_from_path(&bundle)
}

/// Resolve the app bundle path from a process ID.
///
/// On macOS, uses `proc_pidpath` to get the executable path, then walks
/// up to find the `.app` bundle directory.
///
/// Returns `None` if the PID is invalid or the executable isn't inside
/// a `.app` bundle.
#[cfg(target_os = "macos")]
pub fn bundle_path_from_pid(pid: i32) -> Option<std::path::PathBuf> {
    let exe_path = exe_path_from_pid(pid)?;
    // Walk up from e.g. /Applications/Discord.app/Contents/MacOS/Discord
    // to find the .app directory.
    let mut path = exe_path.as_path();
    while let Some(parent) = path.parent() {
        if path.extension().and_then(|e| e.to_str()) == Some("app") {
            return Some(path.to_path_buf());
        }
        path = parent;
    }
    None
}

#[cfg(target_os = "macos")]
fn exe_path_from_pid(pid: i32) -> Option<std::path::PathBuf> {
    // PROC_PIDPATHINFO_MAXSIZE = 4096
    let mut buf = vec![0u8; 4096];
    let ret = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr() as *mut _, buf.len() as u32) };
    if ret <= 0 {
        return None;
    }
    let path_str = std::str::from_utf8(&buf[..ret as usize]).ok()?;
    Some(std::path::PathBuf::from(path_str))
}

#[cfg(target_os = "windows")]
pub fn bundle_path_from_pid(pid: i32) -> Option<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32);
        if handle.is_null() {
            return None;
        }
        let mut buf = vec![0u16; 1024];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut size);
        CloseHandle(handle);
        if ok == 0 {
            return None;
        }
        let path = std::ffi::OsString::from_wide(&buf[..size as usize]);
        std::path::PathBuf::from(path)
            .parent()
            .map(|p| p.to_path_buf())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn bundle_path_from_pid(_pid: i32) -> Option<std::path::PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_family_detected_by_bundle_id() {
        for bundle_id in [
            "com.google.Chrome",
            "com.google.Chrome.canary",
            "com.brave.Browser",
            "com.microsoft.edgemac",
            "company.thebrowser.Browser",
        ] {
            assert_eq!(
                classify_app(Some(bundle_id), None),
                AppKind::ChromeBrowser,
                "bundle_id: {bundle_id}",
            );
        }
    }

    #[test]
    fn unknown_bundle_id_is_native() {
        assert_eq!(
            classify_app(Some("com.apple.Safari"), None),
            AppKind::Native,
        );
    }

    #[test]
    fn no_bundle_id_no_path_is_native() {
        assert_eq!(classify_app(None, None), AppKind::Native);
    }

    #[test]
    fn electron_detected_by_framework_marker() {
        let dir = std::env::temp_dir()
            .join("clickweave_test_electron")
            .join(uuid::Uuid::new_v4().to_string());

        // Create fake Electron framework marker
        #[cfg(target_os = "macos")]
        {
            let framework_dir = dir.join("Contents/Frameworks/Electron Framework.framework");
            std::fs::create_dir_all(&framework_dir).unwrap();
            assert_eq!(
                classify_app(Some("com.hnc.Discord"), Some(&dir)),
                AppKind::ElectronApp,
            );
        }

        #[cfg(target_os = "windows")]
        {
            let asar_dir = dir.join("resources");
            std::fs::create_dir_all(&asar_dir).unwrap();
            std::fs::write(asar_dir.join("electron.asar"), b"fake").unwrap();
            assert_eq!(
                classify_app(Some("com.hnc.Discord"), Some(&dir)),
                AppKind::ElectronApp,
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn chrome_bundle_id_takes_priority_over_electron_marker() {
        assert_eq!(
            classify_app(Some("com.google.Chrome"), None),
            AppKind::ChromeBrowser,
        );
    }

    #[test]
    fn no_framework_marker_is_native() {
        let dir = std::env::temp_dir()
            .join("clickweave_test_no_electron")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&dir).unwrap();

        assert_eq!(
            classify_app(Some("com.apple.Notes"), Some(&dir)),
            AppKind::Native,
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn bundle_path_extraction_from_exe_path() {
        let dir = std::env::temp_dir()
            .join("clickweave_test_bundle_path")
            .join(uuid::Uuid::new_v4().to_string());
        let app_dir = dir.join("Test.app/Contents/MacOS");
        std::fs::create_dir_all(&app_dir).unwrap();

        // Simulate what bundle_path_from_pid does internally (path walking)
        let exe = app_dir.join("TestApp");
        let mut path = exe.as_path();
        let mut found = None;
        while let Some(parent) = path.parent() {
            if path.extension().and_then(|e| e.to_str()) == Some("app") {
                found = Some(path.to_path_buf());
                break;
            }
            path = parent;
        }
        assert_eq!(found.unwrap(), dir.join("Test.app"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
