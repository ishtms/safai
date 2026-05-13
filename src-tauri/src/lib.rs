mod cleaner;
mod commands;
mod onboarding;
mod scanner;
mod scheduler;
mod volumes;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    configure_linux_webview_env();
    scanner::work_budget::configure_global_rayon_pool();

    tauri::Builder::default()
        .plugin(tauri_plugin_os::init())
        // auto-updater. desktop-only, no-op on mobile. JS
        // hits it via @tauri-apps/plugin-updater.
        .plugin(tauri_plugin_updater::Builder::new().build())
        // process plugin. relaunch() used by UpdateBanner
        // after downloadAndInstall so user lands on the new version
        // without a manual restart.
        .plugin(tauri_plugin_process::init())
        // in-flight scan registry
        .manage(scanner::run::ScanRegistry::new())
        // most recent completed scan totals, read by smart_scan_summary
        .manage(scanner::LastScanStore::new())
        // streaming treemap registry
        .manage(scanner::treemap::TreemapRegistry::new())
        // followup, in-memory cache of completed treemaps.
        // seeded on_done_tree, read by serve_treemap_subtree so
        // drill-down + back-nav never re-walk. Arc so the tauri
        // emitter can clone a handle into its struct without fighting
        // the command-surface state borrow.
        .manage(std::sync::Arc::new(scanner::treemap::TreemapCache::new()))
        // dupe-finder registry
        .manage(scanner::dupes::DupesRegistry::new())
        // large & old registry
        .manage(scanner::largeold::LargeOldRegistry::new())
        // activity registry. one sampler per active stream,
        // sysinfo's delta cache is per-thread so stopping one can't
        // poison another.
        .manage(scanner::activity::ActivityRegistry::new())
        // malware scan registry
        .manage(scanner::malware::MalwareRegistry::new())
        // scheduler. setup instead of manage because it needs
        // the AppHandle for scheduler://fired emits.
        .setup(|app| {
            use tauri::Manager;
            let app_handle = app.handle().clone();
            let onboarding_store =
                std::sync::Arc::new(onboarding::OnboardingStore::new(commands::safai_data_dir()));
            app.manage(onboarding_store.clone());
            let cleaner = commands::build_cleaner(&app_handle)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, e))?;
            app.manage(cleaner);
            let sched = commands::spawn_scheduler(app_handle, onboarding_store);
            app.manage(sched);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::ping,
            commands::smart_scan_summary,
            commands::list_volumes,
            commands::start_scan,
            commands::cancel_scan,
            commands::pause_scan,
            commands::resume_scan,
            commands::scan_snapshot,
            commands::forget_scan,
            commands::junk_scan,
            commands::privacy_scan,
            commands::preview_delete,
            commands::commit_delete,
            commands::restore_last,
            commands::graveyard_stats,
            commands::purge_graveyard,
            commands::compute_treemap,
            commands::start_treemap,
            commands::cancel_treemap,
            commands::forget_treemap,
            commands::treemap_snapshot,
            commands::serve_treemap_subtree,
            commands::invalidate_treemap_cache,
            commands::find_duplicates,
            commands::start_duplicates,
            commands::cancel_duplicates,
            commands::forget_duplicates,
            commands::duplicates_snapshot,
            commands::find_large_old,
            commands::start_large_old,
            commands::cancel_large_old,
            commands::forget_large_old,
            commands::large_old_snapshot,
            commands::reveal_in_file_manager,
            commands::startup_scan,
            commands::startup_toggle,
            commands::activity_sample,
            commands::activity_process_detail,
            commands::start_activity,
            commands::refresh_activity,
            commands::set_activity_interval,
            commands::cancel_activity,
            commands::forget_activity,
            commands::kill_process,
            commands::malware_scan,
            commands::start_malware,
            commands::cancel_malware,
            commands::forget_malware,
            commands::malware_snapshot,
            commands::onboarding_state,
            commands::onboarding_save_prefs,
            commands::onboarding_set_step,
            commands::onboarding_record_permission,
            commands::onboarding_set_telemetry,
            commands::onboarding_complete,
            commands::onboarding_reset,
            commands::onboarding_permission_status,
            commands::open_permission_settings,
            commands::settings_get,
            commands::settings_update,
            commands::settings_reset_prefs,
            commands::scheduler_status,
            commands::scheduler_nudge,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Safai");
}

#[cfg(target_os = "linux")]
fn configure_linux_webview_env() {
    // Avoid WebKitGTK's DMABuf path on drivers/compositors that reject
    // the initial GBM surface allocation. Respect an explicit user override.
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }
}

#[cfg(not(target_os = "linux"))]
fn configure_linux_webview_env() {}
