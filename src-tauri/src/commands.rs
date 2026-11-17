//! Puente IPC: la única superficie que la interfaz web puede invocar.
//! Todos los comandos son `async`, así que se ejecutan en el runtime de Tauri y
//! nunca bloquean el hilo de la UI. El frontend NO puede lanzar procesos arbitrarios:
//! solo enviar datos (tipos cerrados) que `vmlab-core` valida antes de ejecutar.
use crate::state::AppState;
use vmlab_core::model::{
    default_adapter, Action, ApplyReport, DockerRunOptions, DockerStartInfo, GuestAdditionsPrep, GuestConfigRequest,
    GuestHostsRequest,
    GuestProfile, GuestReport, Inventory, LabSnapInfo, LabSnapNode, LabSnapReport, LinkConditions, LinkReport, NodeKind,
    NodeProbe, ProbeNode, ScreenshotInfo, Snapshot, Topology,
};
use vmlab_core::capture::{self, CaptureFilter, CaptureInfo};
use vmlab_core::connectivity::LinkDiagnostics;
use vmlab_core::tools::Tools;
use vmlab_core::{compose, connectivity, engine, labsnap, netem, terminal, vbox};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tauri::{AppHandle, Manager, State};

const MAX_WORKSPACE_BYTES: usize = 5 * 1024 * 1024;

fn s<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

#[tauri::command]
pub async fn detect_tools(state: State<'_, AppState>) -> Result<Tools, String> {
    let fresh = Tools::detect();
    *state.tools.write().await = fresh.clone();
    Ok(fresh)
}

#[tauri::command]
pub async fn list_machines(state: State<'_, AppState>) -> Result<Inventory, String> {
    Ok(engine::inventory(&state.tools().await).await)
}

#[tauri::command]
pub async fn refresh_status(state: State<'_, AppState>) -> Result<Snapshot, String> {
    Ok(engine::snapshot(&state.tools().await).await)
}

#[tauri::command]
pub async fn labsnap_list(
    state: State<'_, AppState>,
    nodes: Vec<LabSnapNode>,
) -> Result<Vec<LabSnapInfo>, String> {
    Ok(labsnap::list(&state.tools().await, &nodes).await)
}

#[tauri::command]
pub async fn labsnap_take(
    state: State<'_, AppState>,
    nodes: Vec<LabSnapNode>,
    label: String,
) -> Result<LabSnapReport, String> {
    labsnap::take(&state.tools().await, &nodes, &label).await.map_err(s)
}

#[tauri::command]
pub async fn labsnap_restore(
    state: State<'_, AppState>,
    nodes: Vec<LabSnapNode>,
    id: String,
) -> Result<LabSnapReport, String> {
    labsnap::restore(&state.tools().await, &nodes, &id).await.map_err(s)
}

#[tauri::command]
pub async fn labsnap_delete(
    state: State<'_, AppState>,
    nodes: Vec<LabSnapNode>,
    id: String,
) -> Result<LabSnapReport, String> {
    labsnap::delete(&state.tools().await, &nodes, &id).await.map_err(s)
}

#[tauri::command]
pub async fn machine_action(
    state: State<'_, AppState>,
    kind: NodeKind,
    target: String,
    image: Option<String>,
    options: Option<DockerRunOptions>,
    action: Action,
) -> Result<(), String> {
    let tools = state.tools().await;
    engine::act(&tools, kind, &target, image.as_deref(), options, action).await.map_err(s)
}

/// Valida un `docker-compose.yml` con `docker compose config` y devuelve su JSON.
#[tauri::command]
pub async fn compose_config(state: State<'_, AppState>, text: String) -> Result<String, String> {
    compose::config(&state.tools().await, &text).await.map_err(s)
}

/// Abre una ventana de consola conectada al nodo (Docker exec o SSH a la VM).
#[tauri::command]
pub async fn open_terminal(
    state: State<'_, AppState>,
    kind: NodeKind,
    target: String,
    user: Option<String>,
    host: Option<String>,
) -> Result<(), String> {
    let tools = state.tools().await;
    terminal::open(kind, &target, user.as_deref(), host.as_deref(), &tools).map_err(s)
}

#[tauri::command]
pub async fn apply_topology(
    app: AppHandle,
    state: State<'_, AppState>,
    topology: Topology,
) -> Result<ApplyReport, String> {
    // La carpeta de discos la decide el backend con la ruta de datos de la app;
    // el frontend nunca puede enviar una ruta. No se crea aquí: `engine::apply`
    // la crea solo cuando hay un VDI que construir.
    let mut tools = state.tools().await;
    tools.disks_dir = Some(app.path().app_data_dir().map_err(s)?.join("disks"));
    Ok(engine::apply(&tools, &topology).await)
}

/// Aplica las condiciones de red de un nodo (netem en Docker, límite en VirtualBox).
#[tauri::command]
pub async fn link_apply(
    state: State<'_, AppState>,
    kind: NodeKind,
    target: String,
    adapter: Option<u8>,
    conditions: LinkConditions,
) -> Result<LinkReport, String> {
    let tools = state.tools().await;
    let mut report = LinkReport::default();
    match kind {
        NodeKind::Docker => netem::apply_docker(&tools, &target, &conditions).await.map_err(s)?,
        NodeKind::Vbox => {
            if conditions.delay_ms > 0 || conditions.jitter_ms > 0 || conditions.loss_tenths > 0 {
                report.warnings.push(
                    "VirtualBox solo aplica el límite de velocidad; se ignoran la latencia y la pérdida."
                        .into(),
                );
            }
            let adapter = adapter.unwrap_or_else(default_adapter);
            if let Some(warning) =
                vbox::set_bandwidth(&tools, &target, adapter, conditions.rate_kbit).await.map_err(s)?
            {
                report.warnings.push(warning);
            }
        }
        NodeKind::Disk => {
            return Err("un disco no tiene red: los enlaces de montaje no admiten condiciones de red".into());
        }
    }
    Ok(report)
}

/// Quita las condiciones de red de un nodo.
#[tauri::command]
pub async fn link_clear(
    state: State<'_, AppState>,
    kind: NodeKind,
    target: String,
    adapter: Option<u8>,
) -> Result<LinkReport, String> {
    let tools = state.tools().await;
    let mut report = LinkReport::default();
    match kind {
        NodeKind::Docker => netem::clear_docker(&tools, &target).await.map_err(s)?,
        NodeKind::Vbox => {
            let adapter = adapter.unwrap_or_else(default_adapter);
            if let Some(warning) =
                vbox::set_bandwidth(&tools, &target, adapter, None).await.map_err(s)?
            {
                report.warnings.push(warning);
            }
        }
        NodeKind::Disk => {
            return Err("un disco no tiene red: no hay condiciones de red que quitar".into());
        }
    }
    Ok(report)
}

/// Indica si la imagen de `tc` ya está descargada (para pedir confirmación antes).
#[tauri::command]
pub async fn link_image_present(state: State<'_, AppState>) -> Result<bool, String> {
    Ok(netem::image_present(&state.tools().await).await)
}

/// Arranca una captura de paquetes. El backend elige siempre el fichero y el id.
#[tauri::command]
pub async fn capture_start(
    app: AppHandle,
    state: State<'_, AppState>,
    kind: NodeKind,
    target: String,
    adapter: Option<u8>,
    filter: CaptureFilter,
) -> Result<CaptureInfo, String> {
    let tools = state.tools().await;
    let dir = capture::prepare_dir().map_err(s)?;
    let id = state.next_capture_id();
    state.reserve_capture(&id, kind, &target)?;

    let started_at = capture::now_secs();
    let adapter = adapter.unwrap_or_else(default_adapter);
    let mut warning = None;

    let started = match kind {
        NodeKind::Docker => {
            if !netem::image_present(&tools).await {
                warning = Some(format!(
                    "la imagen {} aún no está descargada; se descargará al iniciar la captura",
                    netem::TC_IMAGE
                ));
            }
            match capture::start_docker(&tools, &target, &id, &filter, &dir).await {
                Ok(started) => started,
                Err(e) => {
                    state.abort_capture(&id);
                    return Err(s(e));
                }
            }
        }
        NodeKind::Vbox => {
            if filter != CaptureFilter::All {
                warning = Some(
                    "VirtualBox no admite filtros BPF; se capturará todo el tráfico del adaptador."
                        .into(),
                );
            }
            match capture::start_vbox(&tools, &target, adapter, &dir).await {
                Ok(started) => started,
                Err(e) => {
                    state.abort_capture(&id);
                    return Err(s(e));
                }
            }
        }
        NodeKind::Disk => {
            state.abort_capture(&id);
            return Err("un disco no tiene tráfico de red: no se puede capturar".into());
        }
    };

    let handle_adapter = matches!(kind, NodeKind::Vbox).then_some(adapter);
    let mut info =
        state.complete_capture(&id, started.file, started_at, handle_adapter, started.child)?;
    info.warning = warning;
    crate::state::spawn_capture_watchdog(app, id);
    Ok(info)
}

/// Detiene una captura activa o devuelve los metadatos de una ya terminada.
#[tauri::command]
pub async fn capture_stop(state: State<'_, AppState>, id: String) -> Result<CaptureInfo, String> {
    let tools = state.tools().await;
    state.stop_capture(&id, &tools).await
}

/// Lista las capturas (activas y terminadas) de esta sesión.
#[tauri::command]
pub async fn capture_list(state: State<'_, AppState>) -> Result<Vec<CaptureInfo>, String> {
    Ok(state.capture_list())
}

/// Abre una captura con Wireshark o, si no está instalado, su carpeta.
#[tauri::command]
pub async fn capture_open(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let file = state.capture_file(&id)?;
    if !file.is_file() {
        return Err("el fichero de captura ya no existe".into());
    }
    match capture::find_wireshark() {
        Some(exe) => capture::open_in_wireshark(&exe, &file).map_err(s),
        None => {
            let dir = capture::prepare_dir().map_err(s)?;
            let _ = capture::open_folder(&dir);
            Err(capture::WIRESHARK_MISSING.to_string())
        }
    }
}

/// Abre la carpeta de capturas en el explorador de Windows.
#[tauri::command]
pub async fn capture_open_folder() -> Result<(), String> {
    let dir = capture::prepare_dir().map_err(s)?;
    capture::open_folder(&dir).map_err(s)
}

// ───────────────────── Capturas de pantalla ─────────────────────

/// Carpeta de capturas: `Pictures/VMLab`; si no se puede crear, la carpeta de
/// datos de la app + `screenshots`. Siempre la elige el backend.
fn screenshots_dir(app: &AppHandle) -> Result<PathBuf, String> {
    if let Ok(dir) = app.path().picture_dir().map(|p| p.join("VMLab")) {
        if std::fs::create_dir_all(&dir).is_ok() {
            return Ok(dir);
        }
    }
    let dir = app.path().app_data_dir().map_err(s)?.join("screenshots");
    std::fs::create_dir_all(&dir).map_err(|e| {
        format!("no se pudo crear la carpeta de capturas {}: {e}", dir.display())
    })?;
    Ok(dir)
}

/// Captura la pantalla de una VM en marcha y devuelve la ruta del PNG.
/// La ruta destino la construye siempre el backend.
#[tauri::command]
pub async fn take_screenshot(
    app: AppHandle,
    state: State<'_, AppState>,
    target: String,
) -> Result<ScreenshotInfo, String> {
    let dir = screenshots_dir(&app)?;
    let dest = dir.join(vbox::screenshot_filename(&target, SystemTime::now()));
    vbox::screenshot(&state.tools().await, &target, &dest).await.map_err(s)?;
    Ok(ScreenshotInfo { path: dest.to_string_lossy().into_owned() })
}

/// Devuelve los bytes de un PNG solo si está dentro de la carpeta de capturas.
#[tauri::command]
pub async fn read_screenshot(app: AppHandle, path: String) -> Result<tauri::ipc::Response, String> {
    let dir = screenshots_dir(&app)?;
    let candidate = Path::new(&path);
    if !vbox::screenshot_path_allowed(&dir, candidate) {
        return Err("la ruta indicada no está dentro de la carpeta de capturas".into());
    }

    // Canonicalizar ambas rutas evita que un enlace simbólico escape de la carpeta.
    let real_dir = std::fs::canonicalize(&dir).map_err(s)?;
    let real_file = std::fs::canonicalize(candidate).map_err(s)?;
    if !vbox::screenshot_path_allowed(&real_dir, &real_file) {
        return Err("la ruta indicada no está dentro de la carpeta de capturas".into());
    }
    if !real_file.is_file() {
        return Err("el fichero de la captura no existe".into());
    }

    tokio::fs::read(real_file)
        .await
        .map(tauri::ipc::Response::new)
        .map_err(s)
}

/// Abre la carpeta de capturas en el explorador de Windows.
#[tauri::command]
pub async fn open_screenshots_folder(app: AppHandle) -> Result<(), String> {
    let dir = screenshots_dir(&app)?;
    capture::open_folder(&dir).map_err(s)
}

/// Sondea la VM (Linux) a través de Guest Additions y devuelve gestores, rutas e interfaces.
#[tauri::command]
pub async fn guest_probe(
    state: State<'_, AppState>,
    target: String,
    username: String,
    password: String,
) -> Result<GuestProfile, String> {
    engine::guest_probe(&state.tools().await, &target, &username, &password)
        .await
        .map_err(s)
}

/// Escribe la configuración de red dentro de la VM Linux sin abrir su consola.
#[tauri::command]
pub async fn guest_apply(state: State<'_, AppState>, request: GuestConfigRequest) -> Result<GuestReport, String> {
    engine::guest_apply(&state.tools().await, request).await.map_err(s)
}

/// Escribe los nombres de la red del lienzo en `/etc/hosts` de la VM Linux.
#[tauri::command]
pub async fn guest_hosts_apply(
    state: State<'_, AppState>,
    request: GuestHostsRequest,
) -> Result<GuestReport, String> {
    engine::guest_hosts_apply(&state.tools().await, request).await.map_err(s)
}

/// Monta la ISO de Guest Additions y abre la VM para instalar o actualizar las adiciones.
#[tauri::command]
pub async fn guest_additions_prepare(
    state: State<'_, AppState>,
    target: String,
) -> Result<GuestAdditionsPrep, String> {
    engine::guest_additions_prepare(&state.tools().await, &target).await.map_err(s)
}

/// Comprueba la comunicación real de los nodos del lienzo (IP en la red de VMLab).
#[tauri::command]
pub async fn network_probe(
    state: State<'_, AppState>,
    nodes: Vec<ProbeNode>,
) -> Result<Vec<NodeProbe>, String> {
    Ok(engine::network_probe(&state.tools().await, nodes).await)
}

/// Diagnóstico puro del cable: no ejecuta nada externo, solo compara los dos sondeos.
#[tauri::command]
pub async fn diagnose_link(a: NodeProbe, b: NodeProbe) -> Result<LinkDiagnostics, String> {
    Ok(connectivity::diagnose_link(&a, &b))
}

/// Arranca Docker Desktop en segundo plano (sin abrir su panel) y espera al motor.
#[tauri::command]
pub async fn docker_start(state: State<'_, AppState>) -> Result<DockerStartInfo, String> {
    let mut tools = state.tools().await;
    if tools.docker.is_none() {
        tools = Tools::detect();
    }
    let info = engine::docker_start(&tools, 180).await.map_err(s)?;
    if info.launched {
        // Docker Desktop no estaba en marcha: VMLab pasa a ser responsable de pararlo.
        state.own_docker();
    }
    if info.engine_ready {
        // Por si el CLI no estaba en el PATH y Docker Desktop acaba de instalarlo/registrarlo.
        *state.tools.write().await = Tools::detect();
    }
    Ok(info)
}

fn workspace_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(s)?;
    std::fs::create_dir_all(&dir).map_err(s)?;
    Ok(dir.join("workspace.json"))
}

/// Guardado atómico: se escribe a un temporal y se renombra, para no corromper el lienzo si se cae la app.
#[tauri::command]
pub async fn save_workspace(app: AppHandle, json: String) -> Result<(), String> {
    if json.len() > MAX_WORKSPACE_BYTES {
        return Err("el espacio de trabajo es demasiado grande".into());
    }
    serde_json::from_str::<serde_json::Value>(&json).map_err(s)?;
    let path = workspace_path(&app)?;
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, json).await.map_err(s)?;
    tokio::fs::rename(&tmp, &path).await.map_err(s)
}

#[tauri::command]
pub async fn load_workspace(app: AppHandle) -> Result<Option<String>, String> {
    let path = workspace_path(&app)?;
    match tokio::fs::read_to_string(&path).await {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(s(e)),
    }
}
