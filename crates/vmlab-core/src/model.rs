//! Tipos que cruzan el puente IPC (JSON en camelCase).
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    Vbox,
    Docker,
    /// Volumen Docker del lienzo. No es una máquina: no tiene red ni estado.
    Disk,
}

impl NodeKind {
    pub fn key(self, target: &str) -> String {
        match self {
            NodeKind::Vbox => format!("vbox:{target}"),
            NodeKind::Docker => format!("docker:{target}"),
            NodeKind::Disk => format!("disk:{target}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PowerState {
    Running,
    Stopped,
    Paused,
    Missing,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Action {
    Start,
    StartGui,
    Stop,
    ForceStop,
    Restart,
    Pause,
    Resume,
}

/// Modo de red de un adaptador de VirtualBox.
///
/// `Intnet` es el modo que gestiona VMLab: el adaptador se conecta a la red interna
/// del segmento del lienzo. El resto de modos los elige el usuario en el panel derecho.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NetMode {
    Nat,
    Bridged,
    #[default]
    Intnet,
    Hostonly,
    Natnetwork,
    None,
}

impl NetMode {
    pub fn label(self) -> &'static str {
        match self {
            NetMode::Nat => "NAT",
            NetMode::Bridged => "adaptador puente",
            NetMode::Intnet => "red interna",
            NetMode::Hostonly => "solo anfitrión",
            NetMode::Natnetwork => "red NAT",
            NetMode::None => "sin conectar",
        }
    }
}

/// Configuración de un adaptador de VirtualBox elegida en el panel derecho.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeNet {
    #[serde(default)]
    pub mode: NetMode,
    /// Número de adaptador (1..=4). VMLab usa el 2 por convención.
    #[serde(default = "default_adapter")]
    pub adapter: u8,
    /// Nombre de la red interna, red NAT o adaptador solo-anfitrión.
    #[serde(default)]
    pub name: Option<String>,
    /// Interfaz física del host para el modo puente (p. ej. `Wi-Fi` o `eth0`).
    #[serde(default)]
    pub bridge: Option<String>,
}

pub fn default_adapter() -> u8 {
    2
}

impl Default for NodeNet {
    fn default() -> Self {
        Self { mode: NetMode::Intnet, adapter: default_adapter(), name: None, bridge: None }
    }
}

/// Condiciones de red que se aplican a un nodo del lienzo.
///
/// Docker las aplica con `netem`; VirtualBox solo puede aplicar `rate_kbit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkConditions {
    /// Retardo en milisegundos (0..=10000).
    pub delay_ms: u32,
    /// Variación del retardo en milisegundos (0..=10000 y nunca mayor que `delay_ms`).
    pub jitter_ms: u32,
    /// Pérdida en décimas de porcentaje: 15 = 1,5 % (0..=1000).
    pub loss_tenths: u32,
    /// Límite de velocidad en kbit/s (8..=1000000). `None` = sin límite.
    #[serde(default)]
    pub rate_kbit: Option<u32>,
}

/// Protocolo de un puerto publicado por un contenedor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PortProto {
    #[default]
    Tcp,
    Udp,
}

impl PortProto {
    pub fn as_str(self) -> &'static str {
        match self {
            PortProto::Tcp => "tcp",
            PortProto::Udp => "udp",
        }
    }
}

/// Variable de entorno para crear un contenedor Docker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvVar {
    pub key: String,
    pub value: String,
}

/// Puerto publicado por un contenedor Docker. Siempre se enlaza a 127.0.0.1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortMap {
    pub host: u16,
    pub container: u16,
    #[serde(default)]
    pub proto: PortProto,
}

/// Opciones con las que VMLab crea un contenedor Docker que todavía no existe.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DockerRunOptions {
    #[serde(default)]
    pub env: Vec<EnvVar>,
    #[serde(default)]
    pub ports: Vec<PortMap>,
}

/// Tipo de arista del lienzo. Los cables de red unen nodos en un segmento; los
/// enlaces de montaje unen un disco con un contenedor y no crean red.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EdgeKind {
    #[default]
    Network,
    Mount,
}

/// Enlace de un disco con un nodo del lienzo. `container` conserva su nombre
/// histórico: es el `id` del nodo de destino (Docker o VirtualBox).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskMount {
    /// `id` del nodo de destino en el lienzo (no su nombre real).
    pub container: String,
    /// Ruta absoluta dentro del contenedor. Los enlaces con VM no usan ruta.
    #[serde(default)]
    pub path: Option<String>,
}

/// Configuración de un nodo Disco. El nombre base es `NodeSpec::target`: con
/// contenedores se crea `vmlab-vol-<nombre saneado>` y con VMs
/// `vmlab-disk-<nombre saneado>.vdi`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskSpec {
    /// `true` permite montar el mismo volumen en varios contenedores a la vez.
    /// Por defecto es exclusivo: un solo contenedor (y siempre una sola VM).
    #[serde(default)]
    pub shared: bool,
    /// Un montaje por cada enlace Disco↔nodo del lienzo.
    #[serde(default)]
    pub mounts: Vec<DiskMount>,
    /// Tamaño en MB de un disco VDI. `None` usa 10240 MB al crearlo.
    #[serde(default)]
    pub size_mb: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeSpec {
    pub id: String,
    pub kind: NodeKind,
    /// VirtualBox: UUID o nombre de la VM. Docker: nombre del contenedor.
    /// Disco: nombre base del volumen (sin el prefijo `vmlab-vol-`).
    pub target: String,
    /// Solo Docker: imagen para crear el contenedor si todavía no existe.
    #[serde(default)]
    pub image: Option<String>,
    /// Solo Docker: opciones (`-e`/`-p`) con las que crear el contenedor.
    #[serde(default)]
    pub options: Option<DockerRunOptions>,
    #[serde(default)]
    pub label: Option<String>,
    /// Solo VirtualBox: modo y parámetros del adaptador de red.
    #[serde(default)]
    pub net: Option<NodeNet>,
    /// Condiciones de red opcionales del nodo.
    #[serde(default)]
    pub link: Option<LinkConditions>,
    /// Solo Disco: modo de uso y rutas de montaje en los contenedores enlazados.
    #[serde(default)]
    pub disk: Option<DiskSpec>,
}

impl Default for NodeSpec {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: NodeKind::Docker,
            target: String::new(),
            image: None,
            options: None,
            label: None,
            net: None,
            link: None,
            disk: None,
        }
    }
}

impl NodeSpec {
    pub fn display(&self) -> &str {
        self.label.as_deref().filter(|l| !l.is_empty()).unwrap_or(&self.target)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    /// Los `.vmlab` antiguos no tenían este campo: todo cable era de red.
    #[serde(default)]
    pub kind: EdgeKind,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Topology {
    pub nodes: Vec<NodeSpec>,
    pub edges: Vec<Edge>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineInfo {
    pub kind: NodeKind,
    pub target: String,
    pub label: String,
    pub state: PowerState,
    pub image: Option<String>,
    /// Solo VirtualBox: estado de las Guest Additions.
    pub guest_additions: Option<GuestAdditionsInfo>,
}

/// Guest Additions detectadas por la propiedad persistente que escribe VirtualBox
/// (`/VirtualBox/GuestAdd/Version`). Funciona también con la VM apagada, siempre que
/// las Guest Additions se hayan ejecutado al menos una vez.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuestAdditionsInfo {
    /// Versión instalada, o `None` si la VM no tiene Guest Additions.
    pub version: Option<String>,
    /// `false` si VirtualBox no pudo consultar (por ejemplo, VM bloqueada por otra sesión).
    pub known: bool,
}

/// Resultado de preparar la instalación guiada de las Guest Additions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuestAdditionsPrep {
    /// Ruta de la ISO de Guest Additions usada.
    pub iso: String,
    /// La ISO quedó montada en una unidad óptica de la VM.
    pub attached: bool,
    /// Se abrió la VM con ventana para poder ejecutar el instalador.
    pub started: bool,
    /// La VM ya estaba en marcha: se dejó como estaba.
    pub already_running: bool,
    /// `windows`, `linux` u `other`, para mostrar las instrucciones correctas.
    pub os: String,
    pub warnings: Vec<String>,
}


#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inventory {
    pub vbox: Vec<MachineInfo>,
    pub docker: Vec<MachineInfo>,
    pub vbox_error: Option<String>,
    /// Mensaje listo para mostrar (en español) cuando falla Docker.
    pub docker_error: Option<String>,
    /// `true` si el fallo es que el motor no está escuchando.
    pub docker_engine_off: bool,
    /// Error técnico original, para el desplegable de detalles.
    pub docker_error_detail: Option<String>,
}

/// Resultado de arrancar Docker Desktop en segundo plano.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DockerStartInfo {
    /// Docker Desktop ya estaba en marcha antes de la llamada.
    pub already_running: bool,
    /// VMLab lanzó el proceso en esta llamada.
    pub launched: bool,
    /// El motor responde y acepta comandos.
    pub engine_ready: bool,
    pub waited_secs: u64,
    /// Ruta del ejecutable usado.
    pub desktop: String,
    /// Resumen en español para mostrar tal cual.
    pub message: String,
}

/// Ruta de una captura de pantalla guardada por VMLab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotInfo {
    pub path: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// Claves `vbox:<uuid|nombre>` y `docker:<nombre>`.
    pub states: HashMap<String, PowerState>,
    pub vbox_ok: bool,
    pub docker_ok: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyReport {
    pub networks: Vec<String>,
    pub applied: usize,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// Resultado de aplicar o quitar las condiciones de red de un nodo.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkReport {
    pub warnings: Vec<String>,
}

// ───────── Instantáneas del laboratorio (`labsnap`) ─────────

/// Nodo del lienzo sobre el que se guarda o restaura una instantánea.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabSnapNode {
    pub kind: NodeKind,
    /// VirtualBox: UUID o nombre de la VM. Docker: nombre del contenedor.
    pub target: String,
    /// Nombre visible del nodo en el lienzo (para los informes).
    #[serde(default)]
    pub label: Option<String>,
}

impl LabSnapNode {
    pub fn display(&self) -> &str {
        self.label.as_deref().filter(|l| !l.is_empty()).unwrap_or(&self.target)
    }
}

/// Instantánea presente en uno o varios nodos del lienzo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LabSnapInfo {
    pub id: String,
    pub label: String,
    pub created_at: Option<String>,
    /// Nombres de los nodos del lienzo que tienen esa instantánea.
    pub present: Vec<String>,
    /// Nombres de los nodos del lienzo que no la tienen.
    pub missing: Vec<String>,
}

/// Resultado por nodo de guardar, restaurar o borrar una instantánea.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LabSnapReport {
    pub done: u32,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

// ───────── Sonda de comunicación del lienzo ─────────

/// Nodo del lienzo cuya conexión se quiere comprobar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeNode {
    pub kind: NodeKind,
    pub target: String,
    /// Solo VirtualBox: adaptador que gestiona VMLab (2 por defecto).
    pub adapter: Option<u8>,
}

/// Motivo estructurado por el que un extremo del cable todavía no está listo.
///
/// Se serializa como un objeto con `code` en camelCase (`{"code":"notRunning"}`)
/// porque el frontend usará ese valor como clave de traducción. `probeFailed`
/// puede incluir un `detail` legible con el error original.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "camelCase")]
pub enum NodeIssue {
    /// La máquina o el contenedor está apagado, parado o pausado.
    NotRunning,
    /// VirtualBox: sin Guest Additions o sin respuesta de sus propiedades.
    NoGuestAdditions,
    /// El adaptador no está en el modo/red de VMLab esperado.
    AdapterNotOnSegment,
    /// Todavía no hay dirección IPv4 en el adaptador del segmento.
    NoIpv4,
    /// El sondeo falló; el detalle es el error original, si se conoce.
    ProbeFailed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

/// Enlace observado dentro de una máquina o contenedor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkInfo {
    /// Red concreta: `intnet:<nombre>`, `bridged:<interfaz>`, `docker:<red>`…
    pub segment: Option<String>,
    pub ipv4: Option<String>,
    /// Máscara decimal en formato con puntos (`255.255.255.0`).
    pub netmask: Option<String>,
    /// Lista vacía = el extremo está listo para comunicarse.
    pub issues: Vec<NodeIssue>,
    /// Motivo legible cuando todavía no se puede afirmar que haya comunicación.
    pub note: Option<String>,
}

/// Resultado de la sonda para un nodo del lienzo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeProbe {
    pub kind: NodeKind,
    pub target: String,
    pub segment: Option<String>,
    pub ipv4: Option<String>,
    #[serde(default)]
    pub netmask: Option<String>,
    /// Lista vacía = el extremo está listo para comunicarse.
    #[serde(default)]
    pub issues: Vec<NodeIssue>,
    pub note: Option<String>,
}

// ───────── Configuración dentro de una VM Linux (Guest Additions) ─────────

/// Gestor de red Linux que VMLab sabe escribir.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum GuestSystem {
    /// Elegir automáticamente el gestor recomendado por la detección.
    #[default]
    Auto,
    Netplan,
    NetworkManager,
    SystemdNetworkd,
    Ifupdown,
    Wicked,
    Dhcpcd,
    Connman,
    NetworkScripts,
    /// Servidor DHCP ISC (`dhcpd.conf`), no cliente.
    Dhcpd,
}

impl GuestSystem {
    pub fn label(self) -> &'static str {
        match self {
            GuestSystem::Auto => "Automático",
            GuestSystem::Netplan => "Netplan",
            GuestSystem::NetworkManager => "NetworkManager",
            GuestSystem::SystemdNetworkd => "systemd-networkd",
            GuestSystem::Ifupdown => "ifupdown (/etc/network/interfaces)",
            GuestSystem::Wicked => "wicked (SUSE)",
            GuestSystem::Dhcpcd => "dhcpcd",
            GuestSystem::Connman => "ConnMan",
            GuestSystem::NetworkScripts => "network-scripts (RHEL/CentOS)",
            GuestSystem::Dhcpd => "Servidor DHCP ISC (dhcpd.conf)",
        }
    }

    pub fn kind(self) -> GuestSystemKind {
        match self {
            GuestSystem::Dhcpd => GuestSystemKind::Server,
            _ => GuestSystemKind::Client,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GuestSystemKind {
    Client,
    Server,
}

/// Resultado de una detección dentro de la VM, con la ruta exacta que usaría VMLab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuestSystemInfo {
    pub system: GuestSystem,
    pub label: String,
    pub config_path: String,
    pub active: bool,
    pub recommended: bool,
    pub kind: GuestSystemKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuestInterface {
    pub name: String,
    pub mac: Option<String>,
    pub state: Option<String>,
    pub ipv4: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuestProfile {
    pub os: String,
    pub os_id: String,
    pub kernel: String,
    pub is_linux: bool,
    pub guest_additions: bool,
    pub is_root: bool,
    pub sudo: bool,
    pub systems: Vec<GuestSystemInfo>,
    pub interfaces: Vec<GuestInterface>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GuestIpMode {
    Dhcp,
    Static,
    /// La VM actúa como servidor DHCP de la red (escribe `dhcpd.conf`).
    Server,
}

/// Petición cerrada: todos los campos se validan antes de tocar la VM.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuestConfigRequest {
    pub target: String,
    pub username: String,
    /// Solo vive en memoria durante la operación; nunca se guarda en el lienzo.
    pub password: String,
    #[serde(default)]
    pub system: GuestSystem,
    pub interface: String,
    pub ip_mode: GuestIpMode,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub prefix: Option<u8>,
    #[serde(default)]
    pub gateway: Option<String>,
    #[serde(default)]
    pub dns: Vec<String>,
    /// Solo para `ipMode = server`: subred en CIDR (`10.77.5.0/24`).
    #[serde(default)]
    pub subnet: Option<String>,
    #[serde(default)]
    pub range_start: Option<String>,
    #[serde(default)]
    pub range_end: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuestReport {
    pub system: GuestSystem,
    pub files: Vec<String>,
    pub messages: Vec<String>,
    pub warnings: Vec<String>,
}

/// Una entrada del bloque gestionado de `/etc/hosts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuestHostEntry {
    pub name: String,
    pub ipv4: String,
}

/// Nombres de red que VMLab escribe dentro de una VM Linux.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuestHostsRequest {
    pub target: String,
    pub username: String,
    /// Solo vive en memoria durante la operación; nunca se guarda en el lienzo.
    pub password: String,
    #[serde(default)]
    pub entries: Vec<GuestHostEntry>,
}

// ───────────────────── Creación de VMs ─────────────────────

/// Valores por defecto de una entrada del catálogo de sistemas operativos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VmOsDefaults {
    pub memory_mb: u32,
    pub cpus: u32,
    pub disk_gb: u32,
    pub vram_mb: u32,
    pub firmware: String,
    pub tpm: bool,
    pub graphics: String,
    pub disk_bus: String,
}

/// Entrada del catálogo: `id` es el `ostype` exacto de VirtualBox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VmOsEntry {
    pub id: String,
    pub label: String,
    pub group: String,
    pub defaults: VmOsDefaults,
}

/// Tipo de sistema operativo ofrecido por la versión instalada de VirtualBox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VmOsType {
    pub id: String,
    pub description: String,
    pub family: String,
    /// Texto de la línea `Architecture:` (`x86`, `x86 (64-bit)`, `ARMv8…`).
    /// Vacío en la salida antigua de VirtualBox, que no lo informaba.
    pub architecture: String,
}

/// Petición cerrada para crear una VM vacía. La ruta de la ISO la elige el
/// usuario en el frontend, pero el backend la valida antes de tocar VirtualBox.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateVmRequest {
    pub name: String,
    pub ostype: String,
    pub memory_mb: u32,
    pub cpus: u32,
    /// Tamaño en GB. Se usa si no viene `diskMb`.
    #[serde(default)]
    pub disk_gb: Option<u32>,
    /// Tamaño en MB. Tiene prioridad sobre `diskGb`.
    #[serde(default)]
    pub disk_mb: Option<u32>,
    #[serde(default)]
    pub vram_mb: Option<u32>,
    pub firmware: String,
    #[serde(default)]
    pub tpm: bool,
    pub graphics: String,
    pub disk_bus: String,
    #[serde(default)]
    pub iso: Option<String>,
}

/// Parámetros ya validados: solo se usan para construir argumentos de `VBoxManage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmCreateParams {
    pub name: String,
    pub ostype: String,
    pub memory_mb: u32,
    pub cpus: u32,
    pub disk_mb: u32,
    pub vram_mb: u32,
    pub firmware: String,
    pub tpm: bool,
    pub graphics: String,
    pub disk_bus: String,
    pub iso: Option<String>,
}

/// Resultado de la creación. `cfgFile` y `vdiPath` son rutas reales leídas de
/// VirtualBox, no supuestas por el frontend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedVm {
    pub uuid: String,
    pub name: String,
    pub cfg_file: String,
    pub vdi_path: String,
}

/// Petición para quitar una VM de VirtualBox. Solo se acepta su UUID: nunca un
/// nombre, para no borrar la máquina equivocada por un error de escritura.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VmDeleteRequest {
    pub uuid: String,
    pub delete_files: bool,
}

/// Resultado de `vm_delete`: siempre informa de los avisos de protección de datos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VmDeleteResult {
    pub removed: bool,
    pub warnings: Vec<String>,
}

fn os_defaults(
    memory_mb: u32,
    cpus: u32,
    disk_gb: u32,
    vram_mb: u32,
    firmware: &str,
    tpm: bool,
    graphics: &str,
    disk_bus: &str,
) -> VmOsDefaults {
    VmOsDefaults {
        memory_mb,
        cpus,
        disk_gb,
        vram_mb,
        firmware: firmware.into(),
        tpm,
        graphics: graphics.into(),
        disk_bus: disk_bus.into(),
    }
}

fn os_entry(id: &str, label: &str, group: &str, defaults: VmOsDefaults) -> VmOsEntry {
    VmOsEntry {
        id: id.into(),
        label: label.into(),
        group: group.into(),
        defaults,
    }
}

/// Catálogo puro y determinista. La VRAM es de 16 MB para XP y de 128 MB para
/// Windows 7 en adelante y para las distribuciones Linux gráficas.
pub fn vm_os_catalog() -> Vec<VmOsEntry> {
    vec![
        os_entry("WindowsXP", "Windows XP", "windows", os_defaults(1024, 1, 20, 16, "bios", false, "vboxvga", "ide")),
        os_entry("Windows7_64", "Windows 7 (64 bits)", "windows", os_defaults(2048, 2, 40, 128, "bios", false, "vboxvga", "sata")),
        os_entry("Windows81_64", "Windows 8.1 (64 bits)", "windows", os_defaults(4096, 2, 50, 128, "bios", false, "vboxsvga", "sata")),
        os_entry("Windows10_64", "Windows 10 (64 bits)", "windows", os_defaults(4096, 2, 50, 128, "bios", false, "vboxsvga", "sata")),
        os_entry("Windows11_64", "Windows 11 (64 bits)", "windows", os_defaults(4096, 2, 64, 128, "efi", true, "vboxsvga", "sata")),
        os_entry("Ubuntu_64", "Ubuntu", "linux", os_defaults(4096, 2, 25, 128, "bios", false, "vmsvga", "sata")),
        os_entry("Ubuntu_64", "Pop!_OS", "linux", os_defaults(4096, 2, 25, 128, "bios", false, "vmsvga", "sata")),
        os_entry("Debian_64", "Debian", "linux", os_defaults(2048, 2, 20, 128, "bios", false, "vmsvga", "sata")),
        os_entry("Debian_64", "Kali Linux", "linux", os_defaults(2048, 2, 20, 128, "bios", false, "vmsvga", "sata")),
        os_entry("ArchLinux_64", "Arch Linux", "linux", os_defaults(4096, 2, 25, 128, "bios", false, "vmsvga", "sata")),
        os_entry("Fedora_64", "Fedora", "linux", os_defaults(4096, 2, 25, 128, "bios", false, "vmsvga", "sata")),
        os_entry("openSUSE_64", "openSUSE", "linux", os_defaults(2048, 2, 25, 128, "bios", false, "vmsvga", "sata")),
        os_entry("Linux26_64", "Linux genérico", "linux", os_defaults(2048, 2, 20, 128, "bios", false, "vmsvga", "sata")),
    ]
}

/// Filtra el catálogo con los `ostype` que realmente ofrece esta instalación.
pub fn available_os_catalog(ids: &[String]) -> Vec<VmOsEntry> {
    let ids: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
    vm_os_catalog()
        .into_iter()
        .filter(|e| ids.contains(e.id.as_str()))
        .collect()
}

/// Familia aproximada de un `ostype` de `list ostypes` (para el selector «Otro…»).
pub fn os_family(id: &str, description: &str) -> String {
    let text = format!("{id} {description}").to_ascii_lowercase();
    if text.contains("windows") {
        return "windows".into();
    }
    const LINUX: &[&str] = &[
        "linux", "ubuntu", "debian", "fedora", "redhat", "centos", "oracle", "suse", "arch",
        "kali", "mint", "gentoo", "alma", "rocky",
    ];
    if LINUX.iter().any(|k| text.contains(k)) {
        return "linux".into();
    }
    "other".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_catalog_has_the_expected_defaults_and_aliases() {
        let catalog = vm_os_catalog();
        let xp = catalog.iter().find(|e| e.label == "Windows XP").unwrap();
        assert_eq!(xp.id, "WindowsXP");
        assert_eq!(xp.group, "windows");
        assert_eq!((xp.defaults.cpus, xp.defaults.memory_mb, xp.defaults.disk_gb), (1, 1024, 20));
        assert_eq!(xp.defaults.firmware, "bios");
        assert_eq!(xp.defaults.disk_bus, "ide");
        assert_eq!(xp.defaults.graphics, "vboxvga");
        assert!(xp.defaults.vram_mb >= 16 && xp.defaults.vram_mb <= 32);

        let w11 = catalog.iter().find(|e| e.label == "Windows 11 (64 bits)").unwrap();
        assert_eq!(w11.id, "Windows11_64");
        assert_eq!(w11.defaults.firmware, "efi");
        assert!(w11.defaults.tpm);
        assert_eq!(w11.defaults.disk_bus, "sata");
        assert_eq!(w11.defaults.disk_gb, 64);

        let pop = catalog.iter().find(|e| e.label == "Pop!_OS").unwrap();
        assert_eq!(pop.id, "Ubuntu_64");
        let kali = catalog.iter().find(|e| e.label == "Kali Linux").unwrap();
        assert_eq!(kali.id, "Debian_64");
        let generic = catalog.iter().find(|e| e.label == "Linux genérico").unwrap();
        assert_eq!(generic.id, "Linux26_64");

        for entry in &catalog {
            assert!(entry.defaults.vram_mb >= 16, "{}", entry.label);
            assert!(matches!(entry.group.as_str(), "windows" | "linux"));
        }
    }

    #[test]
    fn catalog_is_filtered_by_the_installed_ostypes() {
        let ids = vec!["WindowsXP".into(), "Windows11_64".into(), "Ubuntu_64".into()];
        let filtered = available_os_catalog(&ids);
        let labels: Vec<&str> = filtered.iter().map(|e| e.label.as_str()).collect();
        assert!(labels.contains(&"Windows XP"));
        assert!(labels.contains(&"Windows 11 (64 bits)"));
        assert!(labels.contains(&"Ubuntu"));
        assert!(labels.contains(&"Pop!_OS")); // mismo id, etiqueta distinta
        assert!(!labels.contains(&"Windows 10 (64 bits)"));
        assert!(available_os_catalog(&[]).is_empty());
    }

    #[test]
    fn os_family_comes_from_the_ostype_or_description() {
        assert_eq!(os_family("Windows7_64", "Windows 7 (64-bit)"), "windows");
        assert_eq!(os_family("Ubuntu_64", "Ubuntu (64-bit)"), "linux");
        assert_eq!(os_family("Other", "Other/Unknown"), "other");
    }

    #[test]
    fn node_issue_serializes_with_stable_camel_case_codes() {
        let cases = [
            (NodeIssue::NotRunning, "notRunning"),
            (NodeIssue::NoGuestAdditions, "noGuestAdditions"),
            (NodeIssue::AdapterNotOnSegment, "adapterNotOnSegment"),
            (NodeIssue::NoIpv4, "noIpv4"),
            (NodeIssue::ProbeFailed { detail: None }, "probeFailed"),
        ];
        for (issue, code) in cases {
            let json = serde_json::to_value(&issue).unwrap();
            assert_eq!(json["code"], code, "{issue:?}");
        }
        let detail = NodeIssue::ProbeFailed { detail: Some("sin respuesta".into()) };
        assert_eq!(
            serde_json::to_value(&detail).unwrap(),
            serde_json::json!({ "code": "probeFailed", "detail": "sin respuesta" })
        );
    }

    #[test]
    fn old_lab_files_load_without_disk_fields() {
        let json = serde_json::json!({
            "nodes": [
                { "id": "web", "kind": "docker", "target": "web", "image": "nginx:latest" }
            ],
            "edges": [{ "from": "web", "to": "web" }]
        });
        let t: Topology = serde_json::from_value(json).unwrap();
        assert_eq!(t.nodes[0].kind, NodeKind::Docker);
        assert_eq!(t.nodes[0].disk, None);
        assert_eq!(t.edges[0].kind, EdgeKind::Network);
    }

    #[test]
    fn old_disk_mounts_without_size_or_optional_path_still_load() {
        let json = serde_json::json!({
            "nodes": [
                { "id": "web", "kind": "docker", "target": "web" },
                {
                    "id": "d",
                    "kind": "disk",
                    "target": "datos",
                    "disk": {
                        "shared": false,
                        "mounts": [{ "container": "web", "path": "/data" }]
                    }
                },
                {
                    "id": "d2",
                    "kind": "disk",
                    "target": "vmdisk",
                    "disk": {
                        "shared": false,
                        "mounts": [{ "container": "vm" }]
                    }
                }
            ],
            "edges": []
        });
        let t: Topology = serde_json::from_value(json).unwrap();
        let docker = t.nodes[1].disk.as_ref().unwrap();
        assert_eq!(docker.size_mb, None);
        assert_eq!(docker.mounts[0].path.as_deref(), Some("/data"));
        let vdi = t.nodes[2].disk.as_ref().unwrap();
        assert_eq!(vdi.mounts[0].path, None);
        assert_eq!(vdi.size_mb, None);
    }

    #[test]
    fn disk_fields_serialize_in_camel_case() {
        let t = Topology {
            nodes: vec![NodeSpec {
                id: "d".into(),
                kind: NodeKind::Disk,
                target: "datos".into(),
                disk: Some(DiskSpec {
                    shared: true,
                    mounts: vec![DiskMount { container: "web".into(), path: Some("/data".into()) }],
                    size_mb: Some(20_480),
                }),
                ..NodeSpec::default()
            }],
            edges: vec![Edge { from: "d".into(), to: "web".into(), kind: EdgeKind::Mount }],
        };
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(json["nodes"][0]["kind"], "disk");
        assert_eq!(json["nodes"][0]["disk"]["shared"], true);
        assert_eq!(json["nodes"][0]["disk"]["sizeMb"], 20_480);
        assert_eq!(json["nodes"][0]["disk"]["mounts"][0]["container"], "web");
        assert_eq!(json["nodes"][0]["disk"]["mounts"][0]["path"], "/data");
        assert_eq!(json["edges"][0]["kind"], "mount");
    }
}
