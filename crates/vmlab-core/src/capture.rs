//! Captura de paquetes por nodo.
//!
//! Las capturas se guardan siempre en `%TEMP%\vmlab-captures` con un nombre que
//! genera el backend (`<slug-nodo>-<AAAAMMDDHHMMSS>.pcap`). La interfaz nunca
//! envía rutas: solo recibe un identificador opaco y el backend resuelve el
//! fichero a partir de su mapa interno.
//!
//! Docker usa `tcpdump` dentro de la red del contenedor; VirtualBox usa el
//! `nictrace` de `VBoxManage`. En ambos casos los procesos se lanzan sin shell.
use crate::error::{CoreError, Result};
use crate::model::NodeKind;
use crate::netem;
use crate::runner::run_ok;
use crate::tools::Tools;
use crate::validate;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Carpeta (bajo `%TEMP%`) donde viven las capturas.
pub const CAPTURE_DIR_NAME: &str = "vmlab-captures";

/// Una captura se detiene sola a los 10 minutos.
pub const MAX_CAPTURE_SECS: u64 = 10 * 60;

/// O al superar 200 MB.
pub const MAX_CAPTURE_BYTES: u64 = 200 * 1024 * 1024;

/// Los ficheros propios se conservan una semana.
pub const RETENTION_DAYS: u64 = 7;

/// Aviso exacto que se devuelve cuando Wireshark no está instalado.
pub const WIRESHARK_MISSING: &str =
    "Instala Wireshark (winget install WiresharkFoundation.Wireshark) para abrir las capturas";

const SHORT: Duration = Duration::from_secs(20);

/// Posibles filtros de captura. Es un tipo cerrado: no hay texto libre que
/// pueda acabar formando parte de un argumento de `tcpdump`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CaptureFilter {
    All,
    Icmp,
    Tcp,
    Udp,
    /// Puerto TCP/UDP concreto (1-65535).
    Port(u16),
    Arp,
}

impl CaptureFilter {
    /// Comprueba los límites que el tipo no puede expresar por sí solo.
    pub fn validate(&self) -> Result<()> {
        match self {
            CaptureFilter::Port(0) => {
                Err(CoreError::Invalid("el puerto debe estar entre 1 y 65535".into()))
            }
            _ => Ok(()),
        }
    }
}

/// Expresión BPF como **un único argumento** (vacío para capturar todo).
pub fn bpf(filter: &CaptureFilter) -> Vec<String> {
    match filter {
        CaptureFilter::All => Vec::new(),
        CaptureFilter::Icmp => vec!["icmp".to_string()],
        CaptureFilter::Tcp => vec!["tcp".to_string()],
        CaptureFilter::Udp => vec!["udp".to_string()],
        CaptureFilter::Port(port) => vec![format!("port {port}")],
        CaptureFilter::Arp => vec!["arp".to_string()],
    }
}

// ───────────────────────── Fechas UTC sin dependencias ─────────────────────────

/// Segundos desde la época UNIX (0 si el reloj está mal configurado).
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Algoritmo de Hinnant: días desde la época a `(año, mes, día)`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { y + 1 } else { y }, month, day)
}

fn parts_from_unix(secs: u64) -> (i64, u32, u32, u64, u64, u64) {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    (year, month, day, rem / 3_600, (rem % 3_600) / 60, rem % 60)
}

/// Fecha ISO UTC que se devuelve a la interfaz en `startedAt`.
pub fn iso_from_unix(secs: u64) -> String {
    let (year, month, day, hour, minute, second) = parts_from_unix(secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Marca `AAAAMMDDHHMMSS` usada en el nombre del fichero.
pub fn stamp_from_unix(secs: u64) -> String {
    let (year, month, day, hour, minute, second) = parts_from_unix(secs);
    format!("{year:04}{month:02}{day:02}{hour:02}{minute:02}{second:02}")
}

// ───────────────────────────── Nombres y limpieza ─────────────────────────────

/// Deriva un slug `[a-z0-9-]` a partir del nodo. Nunca queda vacío.
pub fn node_slug(target: &str) -> String {
    let slug = validate::labsnap_slug(target);
    if slug.is_empty() {
        "nodo".to_string()
    } else {
        slug
    }
}

/// Nombre de fichero seguro y determinista para un nodo y un instante.
pub fn file_name(target: &str, secs: u64) -> String {
    format!("{}-{}.pcap", node_slug(target), stamp_from_unix(secs))
}

/// `true` solo para nombres con el patrón que genera [`file_name`].
pub fn is_capture_file(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".pcap") else {
        return false;
    };
    let Some((slug, stamp)) = stem.rsplit_once('-') else {
        return false;
    };
    !slug.is_empty()
        && slug.len() <= 40
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && stamp.len() == 14
        && stamp.chars().all(|c| c.is_ascii_digit())
}

/// `true` si `modified` supera la retención de 7 días respecto a `now`.
pub fn is_expired(modified: u64, now: u64) -> bool {
    now.saturating_sub(modified) >= RETENTION_DAYS * 86_400
}

/// Carpeta de capturas: `%TEMP%\vmlab-captures`.
pub fn capture_dir() -> PathBuf {
    std::env::temp_dir().join(CAPTURE_DIR_NAME)
}

/// Crea la carpeta de capturas si hace falta y la devuelve.
pub fn prepare_dir() -> Result<PathBuf> {
    let dir = capture_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| CoreError::Io(format!("no se pudo crear {}: {e}", dir.display())))?;
    Ok(dir)
}

fn allocate_file(dir: &Path, target: &str, mut secs: u64) -> PathBuf {
    // Una captura por nodo, pero dos nodos distintos (o Docker y VirtualBox con
    // el mismo nombre) podrían coincidir en el mismo segundo.
    for _ in 0..3_600 {
        let path = dir.join(file_name(target, secs));
        if !path.exists() {
            return path;
        }
        secs += 1;
    }
    dir.join(file_name(target, secs))
}

/// Borra solo los `*.pcap` con nuestro patrón y más de 7 días. Devuelve cuántos quitó.
pub fn cleanup_old(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let now = now_secs();
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !is_capture_file(name) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(modified) = meta.modified() else { continue };
        let Ok(age) = modified.duration_since(UNIX_EPOCH) else { continue };
        if is_expired(age.as_secs(), now) && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Limpieza que se ejecuta una vez al arrancar VMLab.
pub fn cleanup_on_start() -> usize {
    match prepare_dir() {
        Ok(dir) => cleanup_old(&dir),
        Err(_) => 0,
    }
}

// ───────────────────────────── Identificadores ─────────────────────────────

/// Identificador opaco de captura: `[a-z0-9-]`, hasta 64 caracteres.
pub fn validate_id(id: &str) -> Result<()> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(CoreError::Invalid(format!("identificador de captura no válido «{id}»")))
    }
}

/// Busca una captura conocida. Un id mal formado o inexistente siempre se rechaza.
pub fn known_capture<'a, T>(captures: &'a HashMap<String, T>, id: &str) -> Result<&'a T> {
    validate_id(id)?;
    captures
        .get(id)
        .ok_or_else(|| CoreError::Message("captura desconocida".into()))
}

// ───────────────────────────── Wireshark ─────────────────────────────

/// Busca `Wireshark.exe` en las dos ubicaciones estándar de Windows.
pub fn find_wireshark() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        for var in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(base) = std::env::var_os(var) {
                let path = PathBuf::from(base).join("Wireshark").join("Wireshark.exe");
                if path.is_file() {
                    return Some(path);
                }
            }
        }
    }
    None
}

fn detached(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0000_0008); // DETACHED_PROCESS
    }
}

/// Abre un `.pcap` en Wireshark sin bloquear la app.
pub fn open_in_wireshark(exe: &Path, file: &Path) -> Result<()> {
    let mut command = Command::new(exe);
    command
        .arg("-r")
        .arg(file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detached(&mut command);
    command
        .spawn()
        .map(|_| ())
        .map_err(|source| CoreError::Spawn { program: "Wireshark".into(), source })
}

/// Abre la carpeta de capturas en el explorador de Windows.
pub fn open_folder(dir: &Path) -> Result<()> {
    let mut command = Command::new("explorer.exe");
    command
        .arg(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detached(&mut command);
    command
        .spawn()
        .map(|_| ())
        .map_err(|source| CoreError::Spawn { program: "explorer.exe".into(), source })
}

// ───────────────────────────── Docker ─────────────────────────────

/// Argumentos exactos de `docker run ... tcpdump`. Solo el nombre del
/// contenedor (ya validado), el nombre del auxiliar y la expresión BPF generada
/// viajan como argv. El `--name` permite matar el contenedor auxiliar al parar.
pub fn docker_args(container: &str, id: &str, filter: &CaptureFilter) -> Vec<String> {
    let mut args = vec![
        "run".to_string(),
        "--rm".to_string(),
        "--name".to_string(),
        format!("vmlab-cap-{id}"),
        "--network".to_string(),
        format!("container:{container}"),
        "--cap-add".to_string(),
        "NET_ADMIN".to_string(),
        netem::TC_IMAGE.to_string(),
        "tcpdump".to_string(),
        "-i".to_string(),
        "any".to_string(),
        "-U".to_string(),
        "-w".to_string(),
        "-".to_string(),
    ];
    args.extend(bpf(filter));
    args
}

/// Elimina el contenedor auxiliar de una captura Docker. Es idempotente: si ya
/// no existe (`No such container`) se considera correcto.
pub async fn remove_docker_helper(tools: &Tools, id: &str) -> Result<()> {
    validate_id(id)?;
    let name = format!("vmlab-cap-{id}");
    match run_ok(tools.docker()?, &["rm", "-f", &name], SHORT).await {
        Ok(_) => Ok(()),
        Err(CoreError::Failed { stderr, .. }) if crate::docker::no_such_container(&stderr) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Proceso recién arrancado y fichero donde escribe.
pub struct StartedCapture {
    pub file: PathBuf,
    pub child: Option<Child>,
}

fn io_error(context: &str, error: std::io::Error) -> CoreError {
    CoreError::Io(format!("{context}: {error}"))
}

fn hidden(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
}

/// Arranca `tcpdump` en la red de un contenedor en marcha.
///
/// El `.pcap` se escribe desde el `stdout` del proceso; `stderr` se drena en un
/// hilo aparte para que un error temprano de Docker no bloquee la tubería.
pub async fn start_docker(
    tools: &Tools,
    container: &str,
    id: &str,
    filter: &CaptureFilter,
    dir: &Path,
) -> Result<StartedCapture> {
    validate::container_name(container)?;
    validate_id(id)?;
    filter.validate()?;
    if !netem::is_running(tools, container).await? {
        return Err(CoreError::Message("arranca el contenedor primero".into()));
    }

    let file = allocate_file(dir, container, now_secs());
    let stdout = std::fs::File::create(&file).map_err(|e| {
        io_error(&format!("no se pudo crear {}", file.display()), e)
    })?;
    let args = docker_args(container, id, filter);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();

    let mut command = Command::new(tools.docker()?);
    command
        .args(&refs)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::piped());
    hidden(&mut command);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(source) => {
            let _ = std::fs::remove_file(&file);
            return Err(CoreError::Spawn { program: "docker".into(), source });
        }
    };

    if let Some(mut stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            while matches!(stderr.read(&mut buffer), Ok(n) if n > 0) {}
        });
    }

    Ok(StartedCapture { file, child: Some(child) })
}

// ───────────────────────────── VirtualBox ─────────────────────────────

/// Argumentos de `VBoxManage controlvm` que fijan el fichero del nictrace.
pub fn vbox_file_args(vm: &str, adapter: u8, file: &Path) -> Vec<String> {
    vec![
        "controlvm".to_string(),
        vm.to_string(),
        format!("nictracefile{adapter}"),
        file.to_string_lossy().into_owned(),
    ]
}

/// Argumentos de `VBoxManage controlvm` que encienden o apagan el nictrace.
pub fn vbox_toggle_args(vm: &str, adapter: u8, on: bool) -> Vec<String> {
    vec![
        "controlvm".to_string(),
        vm.to_string(),
        format!("nictrace{adapter}"),
        if on { "on" } else { "off" }.to_string(),
    ]
}

/// Activa el `nictrace` del adaptador indicado de una VM en marcha.
pub async fn start_vbox(
    tools: &Tools,
    vm: &str,
    adapter: u8,
    dir: &Path,
) -> Result<StartedCapture> {
    validate::vbox_target(vm)?;
    if !(1..=4).contains(&adapter) {
        return Err(CoreError::Invalid("el adaptador debe estar entre 1 y 4".into()));
    }
    let running = crate::vbox::running_keys(tools).await?;
    if !running.contains(vm) {
        return Err(CoreError::Message("arranca la máquina virtual primero".into()));
    }

    let file = allocate_file(dir, vm, now_secs());
    let bin = tools.vbox()?;
    let file_args = vbox_file_args(vm, adapter, &file);
    let file_refs: Vec<&str> = file_args.iter().map(String::as_str).collect();
    run_ok(bin, &file_refs, SHORT).await?;
    let on_args = vbox_toggle_args(vm, adapter, true);
    let on_refs: Vec<&str> = on_args.iter().map(String::as_str).collect();
    run_ok(bin, &on_refs, SHORT).await?;

    Ok(StartedCapture { file, child: None })
}

/// Apaga el `nictrace` de un adaptador (la VM puede haberse apagado ya).
pub async fn stop_vbox(tools: &Tools, vm: &str, adapter: u8) -> Result<()> {
    validate::vbox_target(vm)?;
    if !(1..=4).contains(&adapter) {
        return Err(CoreError::Invalid("el adaptador debe estar entre 1 y 4".into()));
    }
    let bin = tools.vbox()?;
    let args = vbox_toggle_args(vm, adapter, false);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_ok(bin, &refs, SHORT).await.map(|_| ())
}

// ───────────────────────────── Datos para la UI ─────────────────────────────

/// Información de una captura que cruza el puente IPC.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureInfo {
    pub id: String,
    pub kind: NodeKind,
    pub target: String,
    pub file_name: String,
    pub started_at: String,
    pub running: bool,
    pub size_bytes: u64,
    /// Avisos no fatales (filtro ignorado en VirtualBox, imagen por descargar...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_one_bpf_argument_per_filter() {
        assert!(bpf(&CaptureFilter::All).is_empty());
        assert_eq!(bpf(&CaptureFilter::Icmp), vec!["icmp"]);
        assert_eq!(bpf(&CaptureFilter::Tcp), vec!["tcp"]);
        assert_eq!(bpf(&CaptureFilter::Udp), vec!["udp"]);
        assert_eq!(bpf(&CaptureFilter::Port(443)), vec!["port 443"]);
        assert_eq!(bpf(&CaptureFilter::Port(1)), vec!["port 1"]);
        assert_eq!(bpf(&CaptureFilter::Port(65_535)), vec!["port 65535"]);
        assert_eq!(bpf(&CaptureFilter::Arp), vec!["arp"]);
    }

    #[test]
    fn validates_port_limits() {
        assert!(CaptureFilter::Port(0).validate().is_err());
        assert!(CaptureFilter::Port(1).validate().is_ok());
        assert!(CaptureFilter::Port(65_535).validate().is_ok());
        for filter in [
            CaptureFilter::All,
            CaptureFilter::Icmp,
            CaptureFilter::Tcp,
            CaptureFilter::Udp,
            CaptureFilter::Arp,
        ] {
            assert!(filter.validate().is_ok());
        }
    }

    #[test]
    fn generates_safe_file_names() {
        let name = file_name("Web Server #1", 0);
        assert_eq!(name, "web-server-1-19700101000000.pcap");
        assert!(is_capture_file(&name));

        let fallback = file_name("¿¡###", 1_700_000_000);
        assert_eq!(fallback, "nodo-20231114221320.pcap");
        assert!(is_capture_file(&fallback));

        assert_eq!(node_slug("Añadir ü"), "anadir-u");

        let long = file_name(&"A".repeat(200), 0);
        assert!(long.len() <= 40 + 1 + 14 + 5, "{long}: {}", long.len());
        assert!(is_capture_file(&long));
        assert!(long.starts_with('a'));
        assert!(long.ends_with("-19700101000000.pcap"));
    }

    #[test]
    fn only_recognizes_own_capture_files() {
        assert!(is_capture_file("nodo-20231114221320.pcap"));
        assert!(is_capture_file("web-01-20231114221320.pcap"));
        assert!(!is_capture_file("nodo.pcap"));
        assert!(!is_capture_file("Nodo-20231114221320.pcap"));
        assert!(!is_capture_file("nodo-2023111422132.pcap"));
        assert!(!is_capture_file("nodo-20231114221320.PCAP"));
        assert!(!is_capture_file("../nodo-20231114221320.pcap"));
        assert!(!is_capture_file("nodo/otro-20231114221320.pcap"));
        assert!(!is_capture_file("nodo_1-20231114221320.pcap"));
        assert!(!is_capture_file("nodo-2023111422132a.pcap"));
    }

    #[test]
    fn applies_retention_only_to_old_files() {
        let now = 1_700_000_000;
        assert!(!is_expired(now, now));
        assert!(!is_expired(now - RETENTION_DAYS * 86_400 + 1, now));
        assert!(is_expired(now - RETENTION_DAYS * 86_400, now));
    }

    #[test]
    fn rejects_unknown_and_malformed_capture_ids() {
        let mut captures = HashMap::new();
        captures.insert("cap-1".to_string(), 42u32);

        assert_eq!(*known_capture(&captures, "cap-1").unwrap(), 42);
        assert!(known_capture(&captures, "cap-2").is_err());
        for id in ["", "Cap-1", "cap_1", "cap 1", "../cap-1", &"a".repeat(65)] {
            assert!(known_capture(&captures, id).is_err(), "{id:?}");
        }
    }

    #[test]
    fn docker_arguments_are_closed() {
        for filter in [
            CaptureFilter::All,
            CaptureFilter::Icmp,
            CaptureFilter::Tcp,
            CaptureFilter::Udp,
            CaptureFilter::Port(443),
            CaptureFilter::Arp,
        ] {
            let args = docker_args("web-01", "cap-1700000000000000000-0", &filter);
            assert_eq!(
                &args[..9],
                [
                    "run",
                    "--rm",
                    "--name",
                    "vmlab-cap-cap-1700000000000000000-0",
                    "--network",
                    "container:web-01",
                    "--cap-add",
                    "NET_ADMIN",
                    netem::TC_IMAGE,
                ]
            );
            assert_eq!(
                &args[9..15],
                ["tcpdump", "-i", "any", "-U", "-w", "-"]
            );
            let tail = &args[15..];
            assert_eq!(tail, bpf(&filter));
            for arg in &args {
                assert!(!arg.contains(';') && !arg.contains('|') && !arg.contains('&'), "{arg}");
            }
        }
    }

    #[test]
    fn vbox_arguments_are_closed() {
        let file = Path::new(r"C:\Temp\vmlab-captures\nodo-20231114221320.pcap");
        let set_file = vbox_file_args("Ubuntu Server 22.04", 2, file);
        assert_eq!(
            set_file,
            vec![
                "controlvm",
                "Ubuntu Server 22.04",
                "nictracefile2",
                r"C:\Temp\vmlab-captures\nodo-20231114221320.pcap",
            ]
        );
        assert_eq!(
            vbox_toggle_args("Ubuntu Server 22.04", 2, true),
            vec!["controlvm", "Ubuntu Server 22.04", "nictrace2", "on"]
        );
        assert_eq!(
            vbox_toggle_args("Ubuntu Server 22.04", 4, false),
            vec!["controlvm", "Ubuntu Server 22.04", "nictrace4", "off"]
        );
    }

    #[test]
    fn formats_utc_dates() {
        assert_eq!(iso_from_unix(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_from_unix(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(stamp_from_unix(1_700_000_000), "20231114221320");
    }
}
