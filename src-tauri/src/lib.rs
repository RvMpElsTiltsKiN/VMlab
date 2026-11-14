mod commands;
mod migrate;
mod monitor;
mod state;

use tauri::Manager;

pub fn run() {
    // Antes de que Tauri/WebView2 creen las carpetas del nuevo identificador.
    migrate::legacy_data();
    let builder = tauri::Builder::default()
        .manage(state::AppState::new())
        .setup(|app| {
            monitor::spawn(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::detect_tools,
            commands::list_machines,
            commands::labsnap_list,
            commands::labsnap_take,
            commands::labsnap_restore,
            commands::labsnap_delete,
            commands::machine_action,
            commands::compose_config,
            commands::open_terminal,
            commands::apply_topology,
            commands::link_apply,
            commands::link_clear,
            commands::link_image_present,
            commands::capture_start,
            commands::capture_stop,
            commands::capture_list,
            commands::capture_open,
            commands::capture_open_folder,
            commands::guest_probe,
            commands::guest_apply,
            commands::guest_hosts_apply,
            commands::guest_additions_prepare,
            commands::network_probe,
            commands::docker_start,
            commands::refresh_status,
            commands::save_workspace,
            commands::load_workspace,
        ]);

    // Actualizaciones y reinicio: solo tienen sentido en escritorio.
    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init());

    let app = builder
        .build(tauri::generate_context!())
        .expect("error al iniciar VMLab");

    app.run(|handle, event| {
        // Al cerrar, primero se detienen las capturas activas (matar Docker y
        // apagar nictrace) y, si VMLab arrancó Docker Desktop, se detiene para
        // liberar su VM. Nunca toca un Docker que ya estuviera en marcha.
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            let state = handle.state::<state::AppState>();
            let captures = state.capture_active_count();
            let owned = state.take_docker_ownership();
            if captures > 0 || owned {
                api.prevent_exit();
                let handle = handle.clone();
                tauri::async_runtime::spawn(async move {
                    let tools = handle.state::<state::AppState>().tools().await;
                    handle.state::<state::AppState>().stop_all_captures(&tools).await;
                    if owned {
                        let _ = vmlab_core::engine::docker_stop(&tools).await;
                    }
                    handle.exit(0);
                });
            }
        }
    });
}
