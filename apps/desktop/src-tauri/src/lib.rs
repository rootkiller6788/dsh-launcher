mod commands;
mod error;
mod jobs;
mod state;
mod usage_proxy;

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use launcher_core::{AppPaths, AppSettings};
use state::AppState;
use tauri::Manager;
use tracing_subscriber::EnvFilter;

pub fn run() {
    // Resolve paths once so the crash hook, the consent seed, and the
    // setup-time telemetry flush all agree on the same logs directory.
    let paths_result = AppPaths::from_env();
    let logs_dir = paths_result
        .as_ref()
        .map(|p| p.logs.clone())
        .unwrap_or_else(|_| std::env::temp_dir().join("AIHarnessLauncher").join("logs"));

    // #602 telemetry is default-off. Seed live consent from disk so a panic
    // during early startup (before setup runs) still honours a previous opt-in;
    // the Preferences toggle updates the atomic afterwards.
    let consent_seed = paths_result
        .as_ref()
        .map(AppSettings::load)
        .is_ok_and(|s| s.telemetry_enabled);
    let telemetry_consent = Arc::new(AtomicBool::new(consent_seed));

    // Install the crash hook first so a panic anywhere — including during path
    // resolution or Tauri init — still lands a crash-*.txt in the logs dir.
    launcher_core::crash::install_panic_hook(logs_dir, telemetry_consent.clone());

    tauri::Builder::default()
        .setup(move |app| {
            let paths = AppPaths::from_env()?;
            paths.ensure_dirs()?;
            init_logging(&paths.launcher_log);

            let settings = launcher_core::AppSettings::load(&paths);
            let vault = launcher_core::ProviderVault::new(paths.clone());
            // The resource dir is where a bundled node/dsh live (packaged);
            // resolve it now so the adapter's runtime chain can use it.
            let resource_dir = app.path().resource_dir().ok();
            let telemetry_enabled = settings.telemetry_enabled;
            let telemetry_endpoint = settings.telemetry_endpoint.clone();
            let logs_dir = paths.logs.clone();
            app.manage(AppState::new(
                paths,
                settings,
                vault,
                resource_dir,
                telemetry_consent.clone(),
            ));
            // Stage 8: after a restart, re-drain any install jobs still `waiting`
            // when the app closed last time.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                crate::jobs::resume_pending_jobs(&handle).await;
            });
            // #602: on the launch after a crash, upload any pending crash
            // sidecars — but only if consent is on AND an endpoint is set.
            // Non-fatal and never blocks startup; a failed send keeps the
            // sidecars in place so the next launch retries.
            if telemetry_enabled {
                if let Some(endpoint) = telemetry_endpoint
                    .as_deref()
                    .filter(|e| !e.trim().is_empty())
                {
                    let logs_dir = logs_dir.clone();
                    let endpoint = endpoint.to_string();
                    let os = std::env::consts::OS.to_string();
                    tauri::async_runtime::spawn(async move {
                        match launcher_core::telemetry::flush(
                            &logs_dir,
                            &endpoint,
                            env!("CARGO_PKG_VERSION"),
                            &os,
                        )
                        .await
                        {
                            Ok(0) => {}
                            Ok(n) => tracing::info!("telemetry: uploaded {n} crash report(s)"),
                            Err(e) => tracing::debug!("telemetry flush skipped: {e:#}"),
                        }
                    });
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::system::system_info,
            commands::system::system_stats,
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
            commands::plugins::library_inventory_summaries,
            commands::plugins::library_inventory_detail,
            commands::plugins::library_inventory_refresh,
            commands::plugins::plugin_install,
            commands::plugins::plugin_uninstall,
            commands::plugins::plugin_toggle,
            commands::plugins::plugin_updates,
            commands::plugins::plugin_update,
            commands::content::skill_list,
            commands::content::skill_install,
            commands::content::skill_uninstall,
            commands::content::skill_updates,
            commands::content::skill_update,
            commands::content::mcp_list,
            commands::content::mcp_install,
            commands::content::mcp_uninstall,
            commands::content::mcp_set_enabled,
            commands::content::market_install,
            commands::content::bundle_import,
            commands::jobs::jobs_list,
            commands::jobs::jobs_cancel,
            commands::jobs::jobs_retry,
            commands::jobs::jobs_delete,
            commands::jobs::jobs_clear_finished,
            commands::environment::environment_export,
            commands::environment::environment_preview,
            commands::environment::environment_import,
            commands::environment::environment_import_package,
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
            commands::process::open_dsh,
            commands::process::open_dsh_external,
            commands::paths::reveal_instance_workspace,
            commands::paths::reveal_instance_config,
            commands::paths::app_paths,
            commands::paths::reveal_data_dir,
            commands::process::current_dsh_url,
            commands::process::process_state,
            commands::process::running_instance,
            commands::history::recent_sessions,
            commands::usage::usage_recent,
            commands::usage::usage_summary,
            commands::usage::usage_record,
            commands::usage::usage_export,
            commands::settings::get_settings,
            commands::settings::set_settings,
            commands::theme::set_theme,
            commands::theme::dsh_theme,
            commands::language::set_language,
            commands::language::dsh_language,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Route `tracing` to a rolling file at `%LOCALAPPDATA%/AIHarnessLauncher/logs`
/// plus stdout (so `tauri dev` still shows it in the terminal).
fn init_logging(log_path: &Path) {
    if let Some(dir) = log_path.parent() {
        let _ = std::fs::create_dir_all(dir);
        match tracing_appender::rolling::RollingFileAppender::builder()
            .filename_prefix("launcher")
            .filename_suffix("log")
            .build(dir)
        {
            Ok(file) => {
                let (writer, guard) = tracing_appender::non_blocking(file);
                // Keep the writer guard alive for the process lifetime.
                std::mem::forget(guard);

                tracing_subscriber::fmt()
                    .with_env_filter(logging_filter())
                    .with_writer(writer)
                    .with_ansi(false)
                    .init();
                return;
            }
            Err(e) => {
                eprintln!(
                    "launcher file logging disabled: failed to create log file at {} ({e})",
                    log_path.display()
                );
            }
        }
    }

    tracing_subscriber::fmt()
        .with_env_filter(logging_filter())
        .with_ansi(false)
        .init();
}

fn logging_filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(
            "info,ai_harness_launcher_lib=debug,launcher_core=debug,dsh_adapter=debug,dsh=info",
        )
    })
}
