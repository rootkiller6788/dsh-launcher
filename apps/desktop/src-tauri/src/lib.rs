mod commands;
mod error;
mod state;

use std::path::Path;

use launcher_core::AppPaths;
use state::AppState;
use tauri::Manager;
use tracing_subscriber::EnvFilter;

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let paths = AppPaths::from_env()?;
            paths.ensure_dirs()?;
            init_logging(&paths.launcher_log);

            let settings = launcher_core::AppSettings::load(&paths);
            let vault = launcher_core::ProviderVault::new(paths.clone());
            // The resource dir is where a bundled node/dsh live (packaged);
            // resolve it now so the adapter's runtime chain can use it.
            let resource_dir = app.path().resource_dir().ok();
            app.manage(AppState::new(paths, settings, vault, resource_dir));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::system::system_info,
            commands::instance::list_instances,
            commands::instance::get_instance,
            commands::instance::create_instance,
            commands::instance::rename_instance,
            commands::instance::clone_instance,
            commands::instance::delete_instance,
            commands::instance::switch_instance,
            commands::market::market_registry,
            commands::market::market_recommend,
            commands::plugins::plugins_list,
            commands::plugins::plugin_install,
            commands::plugins::plugin_uninstall,
            commands::plugins::plugin_toggle,
            commands::plugins::plugin_updates,
            commands::plugins::plugin_update,
            commands::diagnostics::profile_diagnostics,
            commands::provider::get_provider,
            commands::provider::list_providers,
            commands::provider::list_provider_presets,
            commands::provider::save_provider,
            commands::provider::delete_provider,
            commands::provider::remove_provider_key,
            commands::runtimes::runtime_list,
            commands::runtimes::runtime_install,
            commands::runtimes::runtime_set_active,
            commands::runtimes::runtime_remove,
            commands::runtimes::runtime_verify,
            commands::runtimes::runtime_repair,
            commands::process::launch,
            commands::process::stop,
            commands::process::process_state,
            commands::process::running_instance,
            commands::history::recent_sessions,
            commands::settings::get_settings,
            commands::settings::set_settings,
            commands::theme::set_theme,
            commands::theme::dsh_theme,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Route `tracing` to a rolling file at `%LOCALAPPDATA%/AIHarnessLauncher/logs`
/// plus stdout (so `tauri dev` still shows it in the terminal).
fn init_logging(log_path: &Path) {
    if let Some(dir) = log_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let file = tracing_appender::rolling::never(log_path.parent().unwrap_or(log_path), "launcher.log");
    let (writer, guard) = tracing_appender::non_blocking(file);
    // Keep the writer guard alive for the process lifetime.
    std::mem::forget(guard);

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("info,ai_harness_launcher_lib=debug,launcher_core=debug,dsh_adapter=debug,dsh=info")
    });
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .init();
}
