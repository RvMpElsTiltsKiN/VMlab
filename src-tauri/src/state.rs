//! Estado compartido de la app: herramientas detectadas, propiedad de Docker
//! Desktop y capturas de paquetes en curso o ya terminadas.
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Child;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::Manager;
use tokio::sync::RwLock;
use vmlab_core::capture::{self, CaptureInfo, MAX_CAPTURE_BYTES, MAX_CAPTURE_SECS};
use vmlab_core::model::NodeKind;
use vmlab_core::tools::Tools;

/// Metadatos de una captura. Se conserva la entrada aunque la captura ya haya
/// terminado para poder abrir el fichero después.
pub struct CaptureHandle {
    pub kind: NodeKind,
    pub target: String,
    pub file: PathBuf,
    pub started_at: u64,
    pub child: Option<Child>,
    pub running: bool,
    /// Adaptador de VirtualBox (1-4) para poder apagar `nictrace`.
    pub adapter: Option<u8>,
}

fn info_of(id: &str, handle: &CaptureHandle) -> CaptureInfo {
    CaptureInfo {
        id: id.to_string(),
        kind: handle.kind,
        target: handle.target.clone(),
        file_name: handle
            .file
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        started_at: capture::iso_from_unix(handle.started_at),
        running: handle.running,
        size_bytes: std::fs::metadata(&handle.file).map(|meta| meta.len()).unwrap_or(0),
        warning: None,
    }
}

fn lock_error() -> String {
    "el estado de capturas está bloqueado".to_string()
}

/// Estado compartido de la app: rutas de los binarios detectados.
pub struct AppState {
    pub tools: RwLock<Tools>,
    /// `true` si VMLab arrancó Docker Desktop y, por tanto, debe pararlo al salir.
    docker_owned: AtomicBool,
    captures: Mutex<HashMap<String, CaptureHandle>>,
    capture_seq: AtomicU64,
}

impl AppState {
    pub fn new() -> Self {
        // Limpieza de capturas viejas (solo ficheros propios de más de 7 días).
        capture::cleanup_on_start();
        Self {
            tools: RwLock::new(Tools::detect()),
            docker_owned: AtomicBool::new(false),
            captures: Mutex::new(HashMap::new()),
            capture_seq: AtomicU64::new(0),
        }
    }

    /// Copia barata (dos rutas) para no mantener el lock durante la ejecución de un comando.
    pub async fn tools(&self) -> Tools {
        self.tools.read().await.clone()
    }

    /// Registra que Docker Desktop lo arrancó VMLab (no que ya estuviera en marcha).
    pub fn own_docker(&self) {
        self.docker_owned.store(true, Ordering::SeqCst);
    }

    /// Devuelve `true` una sola vez si VMLab arrancó Docker, para no pararlo dos veces.
    pub fn take_docker_ownership(&self) -> bool {
        self.docker_owned.swap(false, Ordering::SeqCst)
    }

    // ───────────────────────────── Capturas ─────────────────────────────

    /// Identificador opaco y único de captura. El frontend nunca elige rutas.
    pub fn next_capture_id(&self) -> String {
        let seq = self.capture_seq.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("cap-{nanos}-{seq}")
    }

    /// Reserva el hueco de una captura antes de arrancar el proceso, para que dos
    /// peticiones simultáneas no creen dos capturas del mismo nodo.
    pub fn reserve_capture(&self, id: &str, kind: NodeKind, target: &str) -> Result<(), String> {
        capture::validate_id(id).map_err(|e| e.to_string())?;
        let mut captures = self.captures.lock().map_err(|_| lock_error())?;
        let key = kind.key(target);
        if captures
            .values()
            .any(|handle| handle.running && handle.kind.key(&handle.target) == key)
        {
            return Err("ya hay una captura activa para este nodo".into());
        }
        captures.insert(
            id.to_string(),
            CaptureHandle {
                kind,
                target: target.to_string(),
                file: PathBuf::new(),
                started_at: capture::now_secs(),
                child: None,
                running: true,
                adapter: None,
            },
        );
        Ok(())
    }

    /// Completa la reserva con el fichero y el proceso reales.
    pub fn complete_capture(
        &self,
        id: &str,
        file: PathBuf,
        started_at: u64,
        adapter: Option<u8>,
        child: Option<Child>,
    ) -> Result<CaptureInfo, String> {
        let mut captures = self.captures.lock().map_err(|_| lock_error())?;
        let handle = captures
            .get_mut(id)
            .ok_or_else(|| "captura desconocida".to_string())?;
        handle.file = file;
        handle.started_at = started_at;
        handle.adapter = adapter;
        handle.child = child;
        Ok(info_of(id, handle))
    }

    /// Cancela una reserva porque el proceso no llegó a arrancar.
    pub fn abort_capture(&self, id: &str) {
        if let Ok(mut captures) = self.captures.lock() {
            captures.remove(id);
        }
    }

    /// Ruta del fichero de una captura conocida (nunca se acepta una ruta externa).
    pub fn capture_file(&self, id: &str) -> Result<PathBuf, String> {
        let captures = self.captures.lock().map_err(|_| lock_error())?;
        capture::known_capture(&captures, id)
            .map(|handle| handle.file.clone())
            .map_err(|e| e.to_string())
    }

    /// Capturas activas y terminadas, de la más reciente a la más antigua.
    pub fn capture_list(&self) -> Vec<CaptureInfo> {
        let Ok(captures) = self.captures.lock() else {
            return Vec::new();
        };
        let mut list: Vec<CaptureInfo> = captures
            .iter()
            .filter(|(_, handle)| !handle.file.as_os_str().is_empty())
            .map(|(id, handle)| info_of(id, handle))
            .collect();
        list.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        list
    }

    pub fn capture_is_running(&self, id: &str) -> bool {
        self.captures
            .lock()
            .ok()
            .and_then(|captures| captures.get(id).map(|handle| handle.running))
            .unwrap_or(false)
    }

    pub fn capture_active_count(&self) -> usize {
        self.captures
            .lock()
            .map(|captures| captures.values().filter(|handle| handle.running).count())
            .unwrap_or(0)
    }

    /// `true` si la captura alcanzó el límite de tiempo o de tamaño, o si el
    /// proceso de Docker terminó por su cuenta.
    pub fn capture_should_stop(&self, id: &str) -> bool {
        let Ok(mut captures) = self.captures.lock() else {
            return false;
        };
        let Some(handle) = captures.get_mut(id) else {
            return false;
        };
        if !handle.running {
            return false;
        }
        if capture::now_secs().saturating_sub(handle.started_at) >= MAX_CAPTURE_SECS {
            return true;
        }
        let too_big = std::fs::metadata(&handle.file)
            .map(|meta| meta.len() > MAX_CAPTURE_BYTES)
            .unwrap_or(false);
        if too_big {
            return true;
        }
        matches!(handle.child.as_mut().map(|child| child.try_wait()), Some(Ok(Some(_))))
    }

    /// Detiene una captura y conserva sus metadatos para poder abrir el fichero.
    pub async fn stop_capture(&self, id: &str, tools: &Tools) -> Result<CaptureInfo, String> {
        let (kind, target, adapter, child, was_running) = {
            let mut captures = self.captures.lock().map_err(|_| lock_error())?;
            let handle = captures
                .get_mut(id)
                .ok_or_else(|| "captura desconocida".to_string())?;
            let was_running = handle.running;
            let kind = handle.kind;
            let target = handle.target.clone();
            let adapter = handle.adapter;
            let child = if was_running { handle.child.take() } else { None };
            handle.running = false;
            (kind, target, adapter, child, was_running)
        };

        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
        if was_running {
            match kind {
                // La VM puede haberse apagado ya: el apagado es de mejor esfuerzo.
                NodeKind::Vbox => {
                    if let Some(adapter) = adapter {
                        let _ = capture::stop_vbox(tools, &target, adapter).await;
                    }
                }
                // El cliente `docker run` muere con `kill()`, pero el contenedor
                // auxiliar con tcpdump puede seguir vivo: se elimina por nombre.
                NodeKind::Docker => {
                    let _ = capture::remove_docker_helper(tools, id).await;
                }
            }
        }

        let captures = self.captures.lock().map_err(|_| lock_error())?;
        let handle = captures
            .get(id)
            .ok_or_else(|| "captura desconocida".to_string())?;
        Ok(info_of(id, handle))
    }

    /// Detiene todas las capturas activas (al cerrar la aplicación).
    pub async fn stop_all_captures(&self, tools: &Tools) {
        let ids: Vec<String> = match self.captures.lock() {
            Ok(captures) => captures
                .iter()
                .filter(|(_, handle)| handle.running)
                .map(|(id, _)| id.clone())
                .collect(),
            Err(_) => return,
        };
        for id in ids {
            let _ = self.stop_capture(&id, tools).await;
        }
    }
}

/// Vigila una captura cada segundo y la detiene al llegar a 10 minutos, 200 MB
/// o si el proceso de Docker termina antes de tiempo.
pub fn spawn_capture_watchdog(app: tauri::AppHandle, id: String) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let state = app.state::<AppState>();
            if !state.capture_is_running(&id) {
                break;
            }
            if state.capture_should_stop(&id) {
                let tools = state.tools().await;
                let _ = state.stop_capture(&id, &tools).await;
                break;
            }
        }
    });
}
