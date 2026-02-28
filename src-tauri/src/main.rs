// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod menu;
mod platform;

use commands::*;
use std::sync::Mutex;
use tauri::{Emitter, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use tauri_specta::{Builder, collect_commands};
use tracing_subscriber::{EnvFilter, Layer, fmt, layer::SubscriberExt, util::SubscriberInitExt};

fn log_dir() -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    {
        std::path::PathBuf::from(std::env::var("HOME").expect("HOME should be set"))
            .join("Library/Logs/Clickweave")
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join("logs")
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
        "info,clickweave_core=debug,clickweave_engine=debug,clickweave_llm=debug,clickweave_mcp=debug",
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
        pick_workflow_file,
        pick_save_file,
        open_project,
        save_project,
        validate,
        node_type_defaults,
        plan_workflow,
        patch_workflow,
        assistant_chat,
        cancel_assistant_chat,
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
        .manage(Mutex::new(AssistantHandle::default()))
        .manage(Mutex::new(WalkthroughHandle::default()))
        .invoke_handler(builder.invoke_handler())
        .menu(menu::build_menu)
        .setup(move |app| {
            let app_data_dir = app
                .path()
                .app_data_dir()
                .expect("Failed to resolve app data dir");
            std::fs::create_dir_all(&app_data_dir).ok();
            app.manage(AppDataDir(app_data_dir));
            builder.mount_events(app);

            app.on_menu_event(|handle, event| {
                let id = event.id().as_ref();
                let _ = handle.emit(&format!("menu://{id}"), ());
            });

            // Global emergency stop: Cmd+Shift+Escape works even when another app has focus
            app.global_shortcut()
                .on_shortcut("CommandOrControl+Shift+Escape", |app, _shortcut, event| {
                    if event.state == ShortcutState::Pressed {
                        let state = app.state::<Mutex<ExecutorHandle>>();
                        let mut guard = state.lock().unwrap();
                        if guard.force_stop() {
                            tracing::info!("Emergency stop triggered via global shortcut");
                        }
                    }
                })
                .expect("Failed to register global stop shortcut");

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
