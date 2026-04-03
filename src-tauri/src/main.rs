// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod mcp_resolve;
mod menu;
mod platform;

use commands::*;
use std::sync::Mutex;
use tauri::{Emitter, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use tauri_specta::{Builder, collect_commands};
use tracing_subscriber::{EnvFilter, Layer, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// Idiomatic per-platform app data directory.
///
/// - macOS: `~/Library/Application Support/com.clickweave.app/` (reverse-DNS is the convention)
/// - Windows: `%APPDATA%\Clickweave\` (product name is the convention)
/// - Linux: `$XDG_DATA_HOME/clickweave/` or `~/.local/share/clickweave/`
fn app_data_dir() -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    {
        std::path::PathBuf::from(std::env::var("HOME").expect("HOME should be set"))
            .join("Library/Application Support/com.clickweave.app")
    }
    #[cfg(target_os = "windows")]
    {
        std::path::PathBuf::from(std::env::var("APPDATA").expect("APPDATA should be set"))
            .join("Clickweave")
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        std::path::PathBuf::from(std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| {
            let home = std::env::var("HOME").expect("HOME should be set");
            format!("{home}/.local/share")
        }))
        .join("clickweave")
    }
}

fn log_dir() -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    {
        std::path::PathBuf::from(std::env::var("HOME").expect("HOME should be set"))
            .join("Library/Logs/Clickweave")
    }
    #[cfg(target_os = "windows")]
    {
        std::path::PathBuf::from(std::env::var("LOCALAPPDATA").expect("LOCALAPPDATA should be set"))
            .join("Clickweave")
            .join("logs")
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        app_data_dir().join("logs")
    }
}

fn main() {
    let log_dir = log_dir();
    std::fs::create_dir_all(&log_dir).ok();

    let file_appender = tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("clickweave")
        .filename_suffix("txt")
        .build(&log_dir)
        .expect("failed to create log file appender");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    let console_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let file_filter = EnvFilter::new(
        "info,clickweave_core=debug,clickweave_engine=debug,clickweave_llm=debug,clickweave_mcp=debug,clickweave_tauri=debug",
    );

    tracing_subscriber::registry()
        .with(fmt::layer().with_filter(console_filter))
        .with(
            fmt::layer()
                .json()
                .with_writer(non_blocking)
                .with_filter(file_filter),
        )
        .init();

    let builder = Builder::<tauri::Wry>::new().commands(collect_commands![
        ping,
        get_mcp_status,
        pick_workflow_file,
        pick_save_file,
        open_project,
        save_project,
        validate,
        node_type_defaults,
        generate_auto_id,
        patch_workflow,
        assistant_chat,
        cancel_assistant_chat,
        get_assistant_session_id,
        rewind_conversation,
        clear_assistant_session,
        save_conversation,
        load_conversation,
        run_workflow,
        stop_workflow,
        supervision_respond,
        list_runs,
        load_run_events,
        read_artifact_base64,
        import_asset,
        start_walkthrough,
        pause_walkthrough,
        resume_walkthrough,
        stop_walkthrough,
        cancel_walkthrough,
        get_walkthrough_draft,
        apply_walkthrough_annotations,
        seed_walkthrough_cache,
        detect_cdp_apps,
        validate_app_path,
        list_chrome_profiles,
        create_chrome_profile,
        is_chrome_profile_configured,
        get_chrome_profile_path,
        launch_chrome_for_setup,
        planner_confirmation_respond,
        resolution_respond,
        confirmable_tools,
        check_endpoint,
    ]);

    #[cfg(debug_assertions)]
    {
        let bindings_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("CARGO_MANIFEST_DIR should have a parent")
            .join("ui/src/bindings.ts");
        builder
            .export(
                specta_typescript::Typescript::default()
                    .bigint(specta_typescript::BigIntExportBehavior::Number),
                bindings_path,
            )
            .expect("Failed to export typescript bindings");
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(Mutex::new(ExecutorHandle::default()))
        .manage(tokio::sync::Mutex::new(AssistantSessionHandle::default()))
        .manage(Mutex::new(ResolutionState::default()))
        .manage(Mutex::new(WalkthroughHandle::default()))
        .manage(std::sync::Arc::new(std::sync::Mutex::new(
            PlannerHandle::default(),
        )))
        .invoke_handler(builder.invoke_handler())
        .menu(menu::build_menu)
        .setup(move |app| {
            let app_data_dir = app_data_dir();
            std::fs::create_dir_all(&app_data_dir).ok();
            app.manage(AppDataDir(app_data_dir));
            builder.mount_events(app);

            app.on_menu_event(|handle, event| {
                let id = event.id().as_ref();
                let _ = handle.emit(&format!("menu://{id}"), ());
            });

            // Check MCP sidecar availability at startup.
            match mcp_resolve::resolve_mcp_binary() {
                Ok(path) => {
                    tracing::info!("MCP sidecar available: {path}");
                    app.manage(McpStatus(Ok(path)));
                }
                Err(e) => {
                    tracing::warn!("MCP sidecar not available: {e}");
                    app.manage(McpStatus(Err(e.to_string())));
                }
            }

            // Global emergency stop: works even when another app has focus.
            // Try preferred shortcut first, fall back if already taken (e.g. by Task Manager).
            let stop_handler =
                |app: &tauri::AppHandle,
                 _shortcut: &tauri_plugin_global_shortcut::Shortcut,
                 event: tauri_plugin_global_shortcut::ShortcutEvent| {
                    if event.state == ShortcutState::Pressed {
                        let state = app.state::<Mutex<ExecutorHandle>>();
                        let mut guard = state.lock().unwrap();
                        if guard.force_stop() {
                            tracing::info!("Emergency stop triggered via global shortcut");
                        }
                    }
                };
            let shortcuts = [
                "CommandOrControl+Shift+Escape",
                "CommandOrControl+Alt+Escape",
                "CommandOrControl+Shift+F12",
            ];
            let mut registered = false;
            for shortcut in &shortcuts {
                match app.global_shortcut().on_shortcut(*shortcut, stop_handler) {
                    Ok(()) => {
                        tracing::info!("Registered emergency stop shortcut: {shortcut}");
                        registered = true;
                        break;
                    }
                    Err(e) => {
                        tracing::warn!("Could not register shortcut {shortcut}: {e}");
                    }
                }
            }
            if !registered {
                tracing::warn!("No emergency stop shortcut could be registered");
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
