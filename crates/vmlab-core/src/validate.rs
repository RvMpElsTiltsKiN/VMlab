//! Validación estricta de todo texto que acabará como argumento de un comando.
//! Como no hay shell, el riesgo real es la *inyección de opciones* (un valor que
//! empieza por `-` y se interpreta como flag), por eso todos los validadores lo prohíben.
use crate::error::{CoreError, Result};
use crate::model::{
    vm_os_catalog, CreateVmRequest, DockerRunOptions, EdgeKind, LinkConditions, NodeKind, NodeSpec,
    Topology, VmCreateParams,
};
use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;
use std::path::Path;

/// Tamaño usado al crear un VDI cuando el lienzo no indica uno.
pub const DEFAULT_VDI_SIZE_MB: u32 = 10_240;
/// Límite superior del `--size` de `VBoxManage createmedium` (≈ 2 TB).
pub const MAX_VDI_SIZE_MB: u32 = 2_000_000;

fn bad<T>(what: &str, v: &str) -> Result<T> {
    Err(CoreError::Invalid(format!("{what} «{}»", v.chars().take(60).collect::<String>())))
}

/// Nombre de contenedor Docker: `[A-Za-z0-9][A-Za-z0-9_.-]{0,63}`.
pub fn container_name(s: &str) -> Result<&str> {
    let mut chars = s.chars();
    let ok = matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && s.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if ok { Ok(s) } else { bad("nombre de contenedor no válido", s) }
}

/// Referencia de imagen Docker (`repo/nombre:tag`, `nombre@sha256:...`).
pub fn docker_image(s: &str) -> Result<&str> {
    let mut chars = s.chars();
    let ok = matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && s.len() <= 200
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | ':' | '@' | '-'));
    if ok { Ok(s) } else { bad("imagen Docker no válida", s) }
}

/// Nombre o UUID de una VM de VirtualBox. Los nombres pueden llevar espacios y acentos.
pub fn vbox_target(s: &str) -> Result<&str> {
    let ok = !s.is_empty()
        && s.chars().count() <= 128
        && !s.starts_with('-')
        && !s.chars().any(|c| c.is_control());
    if ok { Ok(s) } else { bad("máquina virtual no válida", s) }
}

/// Interfaz de red del host o nombre de una red de VirtualBox. Puede llevar espacios
/// (p. ej. `VirtualBox Host-Only Ethernet Adapter`), pero no empezar por `-` ni llevar controles.
pub fn host_iface(s: &str) -> Result<&str> {
    let ok = !s.is_empty()
        && s.chars().count() <= 128
        && !s.starts_with('-')
        && !s.chars().any(|c| c.is_control());
    if ok { Ok(s) } else { bad("interfaz de red no válida", s) }
}

/// Nombre de interfaz dentro de Linux (`IFNAMSIZ` = 16, incluido el NUL).
pub fn linux_iface(s: &str) -> Result<&str> {
    let mut chars = s.chars();
    let ok = matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && s.len() <= 15
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if ok { Ok(s) } else { bad("interfaz Linux no válida", s) }
}

/// Usuario Linux para `VBoxManage guestcontrol`.
pub fn guest_user(s: &str) -> Result<&str> {
    let mut chars = s.chars();
    let ok = matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric() || c == '_')
        && s.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if ok { Ok(s) } else { bad("usuario de la VM no válido", s) }
}

/// Contraseña: no puede llevar controles (romperían el fichero temporal) ni ser enorme.
pub fn guest_password(s: &str) -> Result<&str> {
    let ok = s.chars().count() <= 1024 && !s.chars().any(|c| c.is_control());
    if ok { Ok(s) } else { bad("contraseña no válida", s) }
}

/// Etiqueta de nombre de host para `/etc/hosts`: `[a-z0-9-]`, 1-63 caracteres y
/// sin empezar ni terminar en guion.
pub fn hostname_label(s: &str) -> Result<&str> {
    let ok = !s.is_empty()
        && s.len() <= 63
        && !s.starts_with('-')
        && !s.ends_with('-')
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok { Ok(s) } else { bad("nombre de host no válido", s) }
}

/// IPv4 que se puede escribir en `/etc/hosts`. Se rechazan las direcciones que no
/// identifican a una máquina concreta del laboratorio.
pub fn hosts_ipv4(s: &str) -> Result<Ipv4Addr> {
    let Ok(addr) = s.parse::<Ipv4Addr>() else {
        return bad("dirección IPv4 no válida", s);
    };
    if addr.is_unspecified() || addr.is_multicast() || addr.is_broadcast() || addr.is_loopback() {
        return bad("dirección IPv4 no utilizable en /etc/hosts", s);
    }
    Ok(addr)
}

/// Máscara decimal con puntos a partir de un prefijo CIDR (`24` → `255.255.255.0`).
///
/// Se usa con datos que llegan de VirtualBox o Docker, no de la UI; por eso un
/// prefijo fuera de rango simplemente devuelve `None` en vez de un error.
pub fn netmask_from_prefix(prefix: u8) -> Option<String> {
    if prefix > 32 {
        return None;
    }
    let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - u32::from(prefix)) };
    Some(Ipv4Addr::from(mask).to_string())
}

/// Normaliza una máscara decimal y comprueba que sea contigua (`/24`, `/16`…).
pub fn netmask(s: &str) -> Option<String> {
    let addr = s.trim().parse::<Ipv4Addr>().ok()?;
    let mask = u32::from(addr);
    let inverted = !mask;
    (inverted & inverted.wrapping_add(1) == 0).then(|| addr.to_string())
}

/// Redes gestionadas por VMLab: `vmlab-` + [a-z0-9-].
pub fn network_name(s: &str) -> Result<&str> {
    let ok = s.len() <= 40
        && s.strip_prefix("vmlab-").is_some_and(|r| {
            !r.is_empty() && r.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        });
    if ok { Ok(s) } else { bad("nombre de red no válido", s) }
}

/// `true` para redes nuevas (`vmlab-*`) y para las antiguas (`lablab-*`), que se
/// desconectan y limpian durante la migración. No se aceptan para crear redes.
pub fn managed_network(s: &str) -> bool {
    let valid = |r: &str| {
        !r.is_empty() && r.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    };
    s.len() <= 40
        && (s.strip_prefix("vmlab-").is_some_and(valid) || s.strip_prefix("lablab-").is_some_and(valid))
}

/// Identificador de una instantánea del laboratorio: `[a-z0-9-]`, 1-40 caracteres.
pub fn labsnap_id(s: &str) -> Result<&str> {
    let ok = !s.is_empty()
        && s.len() <= 40
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok { Ok(s) } else { bad("identificador de instantánea no válido", s) }
}

/// Nombre visible de una instantánea: 1-40 caracteres, letras, números, espacios
/// y `- _ . ( )`. Se admiten tildes y `ñ`; se rechazan comillas, barras y controles.
pub fn labsnap_label(s: &str) -> Result<&str> {
    let count = s.chars().count();
    let ok = count > 0
        && count <= 40
        && s.chars().all(|c| {
            c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '(' | ')')
        });
    if ok { Ok(s) } else { bad("nombre de instantánea no válido", s) }
}

/// Deriva el identificador de una instantánea a partir de su nombre visible.
///
/// Las tildes y la `ñ` se convierten a su letra base; cualquier otro carácter
/// que no sea una letra ASCII o un dígito se convierte en un guion. Los guiones
/// repetidos y los de los extremos se eliminan, y el resultado se limita a 40.
pub fn labsnap_slug(label: &str) -> String {
    let mut out = String::new();
    for c in label.chars() {
        let base = match c {
            'á' | 'à' | 'ä' | 'â' | 'ã' | 'å' | 'Á' | 'À' | 'Ä' | 'Â' | 'Ã' | 'Å' => 'a',
            'é' | 'è' | 'ë' | 'ê' | 'É' | 'È' | 'Ë' | 'Ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' | 'Í' | 'Ì' | 'Ï' | 'Î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' | 'õ' | 'Ó' | 'Ò' | 'Ö' | 'Ô' | 'Õ' => 'o',
            'ú' | 'ù' | 'ü' | 'û' | 'Ú' | 'Ù' | 'Ü' | 'Û' => 'u',
            'ñ' | 'Ñ' => 'n',
            'ç' | 'Ç' => 'c',
            c if c.is_ascii_alphanumeric() => c.to_ascii_lowercase(),
            _ => '-',
        };
        if base == '-' {
            if !out.is_empty() && !out.ends_with('-') {
                out.push('-');
            }
        } else {
            out.push(base);
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.len() > 40 {
        out.truncate(40);
        while out.ends_with('-') {
            out.pop();
        }
    }
    out
}

/// Normaliza el nombre visible de un disco a `[a-z0-9][a-z0-9_.-]{0,39}`.
///
/// Las tildes y la `ñ` se convierten a su letra base; cualquier otro carácter
/// que no sea una letra ASCII, un dígito, `_` o `.` se convierte en un guion.
/// Los separadores repetidos se colapsan y los de los extremos se eliminan,
/// igual que en los identificadores de instantáneas.
pub fn sanitize_disk_name(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        let base = match c {
            'á' | 'à' | 'ä' | 'â' | 'ã' | 'å' | 'Á' | 'À' | 'Ä' | 'Â' | 'Ã' | 'Å' => 'a',
            'é' | 'è' | 'ë' | 'ê' | 'É' | 'È' | 'Ë' | 'Ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' | 'Í' | 'Ì' | 'Ï' | 'Î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' | 'õ' | 'Ó' | 'Ò' | 'Ö' | 'Ô' | 'Õ' => 'o',
            'ú' | 'ù' | 'ü' | 'û' | 'Ú' | 'Ù' | 'Ü' | 'Û' => 'u',
            'ñ' | 'Ñ' => 'n',
            'ç' | 'Ç' => 'c',
            c if c.is_ascii_alphanumeric() => c.to_ascii_lowercase(),
            '_' | '.' => c,
            _ => '-',
        };
        if base == '-' {
            if !out.is_empty() && !out.ends_with('-') {
                out.push('-');
            }
        } else {
            out.push(base);
        }
    }
    let trimmed = out.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    let mut result: String = trimmed.chars().take(40).collect();
    while result.chars().last().is_some_and(|c| !c.is_ascii_alphanumeric()) {
        result.pop();
    }
    result
}

/// Nombre base de un disco ya saneado: `[a-z0-9][a-z0-9_.-]{0,39}`.
pub fn disk_name(s: &str) -> Result<&str> {
    let mut chars = s.chars();
    let ok = matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && s.len() <= 40
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-'));
    if ok { Ok(s) } else { bad("nombre de disco no válido", s) }
}

/// Nombre real del volumen Docker: `vmlab-vol-<nombre saneado>`.
pub fn volume_name(s: &str) -> Result<String> {
    let clean = sanitize_disk_name(s);
    disk_name(&clean)?;
    Ok(format!("vmlab-vol-{clean}"))
}

/// Comprueba un nombre de volumen ya construido (`vmlab-vol-<x>`).
pub fn disk_volume_name(s: &str) -> Result<&str> {
    let Some(rest) = s.strip_prefix("vmlab-vol-") else {
        return bad("nombre de volumen no válido", s);
    };
    disk_name(rest)?;
    Ok(s)
}

/// Rutas del sistema que nunca se montan como disco de datos.
const DANGEROUS_MOUNT_PATHS: [&str; 7] = ["/", "/proc", "/sys", "/dev", "/etc", "/var/run", "/run"];

/// Normaliza una ruta de montaje para compararla (sin barras finales).
pub fn normalize_mount_path(s: &str) -> String {
    let trimmed = s.trim_end_matches('/');
    if trimmed.is_empty() { "/".to_string() } else { trimmed.to_string() }
}

/// Ruta de montaje dentro del contenedor: absoluta tipo Linux, sin `:` ni `,`
/// (Docker los usa como separadores de `-v`), sin controles, sin `..` y fuera
/// de las rutas peligrosas del sistema.
pub fn mount_path(s: &str) -> Result<&str> {
    let normalized = normalize_mount_path(s);
    let ok = s.starts_with('/')
        && s.chars().count() <= 1024
        && !s.contains(':')
        && !s.contains(',')
        && !s.contains('\\')
        && !s.chars().any(|c| c.is_control())
        && !s.split('/').any(|part| part == "..")
        && !DANGEROUS_MOUNT_PATHS
            .iter()
            .any(|p| normalized == *p || normalized.starts_with(&format!("{p}/")));
    if ok { Ok(s) } else { bad("ruta de montaje no válida", s) }
}

/// Tipo de almacenamiento que necesita un disco, según sus enlaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskKind {
    /// Volumen Docker montado en uno o más contenedores.
    Docker,
    /// VDI exclusivo conectado a una máquina virtual.
    Vdi,
    /// Sin enlaces: todavía no crea nada.
    Unused,
}

/// Clasifica un disco mirando tanto `disk.mounts` como las aristas del lienzo.
/// Un mismo disco no puede mezclar contenedores y máquinas virtuales.
pub fn disk_kind(t: &Topology, disk: &NodeSpec) -> Result<DiskKind> {
    let Some(spec) = disk.disk.as_ref() else {
        return Err(CoreError::Invalid(format!(
            "el disco «{}» no tiene configuración de montajes",
            disk.display()
        )));
    };
    let by_id: HashMap<&str, &NodeSpec> = t.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut found: Option<DiskKind> = None;
    let mut register = |id: &str| -> Result<()> {
        let Some(target) = by_id.get(id) else {
            return Err(CoreError::Invalid(format!(
                "el disco «{}» enlaza con «{}», que no está en el lienzo",
                disk.display(),
                id
            )));
        };
        let kind = match target.kind {
            NodeKind::Docker => DiskKind::Docker,
            NodeKind::Vbox => DiskKind::Vdi,
            NodeKind::Disk => {
                return Err(CoreError::Invalid(format!(
                    "el disco «{}» no puede enlazarse con otro disco",
                    disk.display()
                )))
            }
        };
        match found {
            Some(previous) if previous != kind => {
                return Err(CoreError::Invalid(format!(
                    "el disco «{}» no puede enlazarse a contenedores Docker y máquinas virtuales a la vez",
                    disk.display()
                )));
            }
            Some(_) => {}
            None => found = Some(kind),
        }
        Ok(())
    };
    for m in &spec.mounts {
        register(&m.container)?;
    }
    for e in &t.edges {
        if e.from == disk.id {
            register(&e.to)?;
        }
        if e.to == disk.id {
            register(&e.from)?;
        }
    }
    Ok(found.unwrap_or(DiskKind::Unused))
}

/// Valida la parte de discos de una topología: nombres y rutas, enlaces
/// Disco↔nodo, modo exclusivo/compartido y rutas duplicadas.
///
/// Es una comprobación pura que se ejecuta **antes** de tocar Docker o
/// VirtualBox. No mira redes ni plantillas: una topología sin discos siempre es válida.
pub fn disk_topology(t: &Topology) -> Result<()> {
    let by_id: HashMap<&str, &NodeSpec> = t.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let disks: Vec<&NodeSpec> = t.nodes.iter().filter(|n| n.kind == NodeKind::Disk).collect();
    let disk_ids: HashSet<&str> = disks.iter().map(|d| d.id.as_str()).collect();

    for n in &t.nodes {
        if n.kind != NodeKind::Disk && n.disk.is_some() {
            return Err(CoreError::Invalid(format!(
                "solo un nodo Disco puede tener montajes: «{}»",
                n.display()
            )));
        }
    }

    // El nombre se valida igual para volúmenes y para VDI: serviría para
    // construir ambos nombres de fichero.
    let mut names = HashSet::new();
    for d in &disks {
        let volume = volume_name(&d.target)?;
        if !names.insert(volume) {
            return Err(CoreError::Invalid(format!(
                "hay dos discos con el nombre «{}»",
                d.display()
            )));
        }
        let spec = d.disk.as_ref().ok_or_else(|| {
            CoreError::Invalid(format!("el disco «{}» no tiene configuración de montajes", d.display()))
        })?;
        if let Some(size) = spec.size_mb {
            if !(1..=MAX_VDI_SIZE_MB).contains(&size) {
                return Err(CoreError::Invalid(format!(
                    "el tamaño del disco «{}» debe estar entre 1 y {MAX_VDI_SIZE_MB} MB",
                    d.display()
                )));
            }
        }
    }

    let mut occupied: HashSet<(&str, String)> = HashSet::new();
    for d in &disks {
        let spec = d.disk.as_ref().expect("nombre validado arriba");
        match disk_kind(t, d)? {
            DiskKind::Unused => continue,
            DiskKind::Vdi => {
                let mut links: HashSet<&str> = spec.mounts.iter().map(|m| m.container.as_str()).collect();
                for e in &t.edges {
                    if e.from == d.id {
                        links.insert(e.to.as_str());
                    }
                    if e.to == d.id {
                        links.insert(e.from.as_str());
                    }
                }
                if spec.shared {
                    return Err(CoreError::Invalid(
                        "el modo compartido con máquinas virtuales todavía no está disponible".into(),
                    ));
                }
                if links.len() > 1 || spec.mounts.len() > 1 {
                    return Err(CoreError::Invalid(format!(
                        "el disco «{}» solo se puede enlazar con una máquina virtual",
                        d.display()
                    )));
                }
                for m in &spec.mounts {
                    let Some(vm) = by_id.get(m.container.as_str()) else {
                        return Err(CoreError::Invalid(format!(
                            "el disco «{}» enlaza con «{}», que no está en el lienzo",
                            d.display(),
                            m.container
                        )));
                    };
                    if vm.kind != NodeKind::Vbox {
                        return Err(CoreError::Invalid(format!(
                            "el disco «{}» mezcla enlaces de contenedor y de máquina virtual",
                            d.display()
                        )));
                    }
                    vbox_target(&vm.target)?;
                    // Las rutas solo aplican a contenedores. Si la UI envía una
                    // por compatibilidad con la fase A, se ignora.
                }
            }
            DiskKind::Docker => {
                if !spec.shared && spec.mounts.len() > 1 {
                    return Err(CoreError::Invalid(format!(
                        "el disco «{}» es exclusivo pero tiene {} enlaces de montaje; \
                         márcalo como compartido o deja un solo contenedor",
                        d.display(),
                        spec.mounts.len()
                    )));
                }
                let mut paths = HashSet::new();
                for m in &spec.mounts {
                    let Some(path) = m.path.as_deref().filter(|p| !p.is_empty()) else {
                        return Err(CoreError::Invalid(format!(
                            "el disco «{}» necesita una ruta de montaje para «{}»",
                            d.display(),
                            m.container
                        )));
                    };
                    mount_path(path)?;
                    let Some(container) = by_id.get(m.container.as_str()) else {
                        return Err(CoreError::Invalid(format!(
                            "el disco «{}» enlaza con «{}», que no está en el lienzo",
                            d.display(),
                            m.container
                        )));
                    };
                    if container.kind != NodeKind::Docker {
                        return Err(CoreError::Invalid(format!(
                            "el disco «{}» mezcla enlaces de contenedor y de máquina virtual",
                            d.display()
                        )));
                    }
                    let path = normalize_mount_path(path);
                    if !paths.insert((m.container.as_str(), path.clone())) {
                        return Err(CoreError::Invalid(format!(
                            "el disco «{}» tiene dos montajes en la misma ruta «{}» del contenedor «{}»",
                            d.display(),
                            path,
                            container.display()
                        )));
                    }
                    if !occupied.insert((container.target.as_str(), path.clone())) {
                        return Err(CoreError::Invalid(format!(
                            "el contenedor «{}» ya tiene otro disco montado en «{}»",
                            container.display(),
                            path
                        )));
                    }
                }
            }
        }
    }

    for e in &t.edges {
        let from_disk = disk_ids.contains(e.from.as_str());
        let to_disk = disk_ids.contains(e.to.as_str());
        if !from_disk && !to_disk {
            if e.kind == EdgeKind::Mount {
                return Err(CoreError::Invalid(
                    "un enlace de montaje debe tener un disco en uno de sus extremos".into(),
                ));
            }
            continue;
        }
        if from_disk && to_disk {
            return Err(CoreError::Invalid("no se puede enlazar un disco con otro disco".into()));
        }
        let (disk_id, other_id) = if from_disk {
            (e.from.as_str(), e.to.as_str())
        } else {
            (e.to.as_str(), e.from.as_str())
        };
        let Some(other) = by_id.get(other_id) else {
            return Err(CoreError::Invalid(format!(
                "el disco «{disk_id}» está enlazado con «{other_id}», que no está en el lienzo"
            )));
        };
        if !matches!(other.kind, NodeKind::Docker | NodeKind::Vbox) {
            return Err(CoreError::Invalid(format!(
                "el disco «{disk_id}» solo se puede enlazar con contenedores o máquinas virtuales; «{}» no es válido",
                other.display()
            )));
        }
        let Some(disk) = by_id.get(disk_id) else {
            continue;
        };
        let linked = disk
            .disk
            .as_ref()
            .is_some_and(|spec| spec.mounts.iter().any(|m| m.container == other_id));
        if !linked {
            let detail = if other.kind == NodeKind::Vbox {
                "no tiene un enlace en el disco"
            } else {
                "no tiene una ruta de montaje"
            };
            return Err(CoreError::Invalid(format!(
                "el enlace del disco «{}» con «{}» {detail}",
                disk.display(),
                other.display()
            )));
        }
    }

    for d in &disks {
        let Some(spec) = d.disk.as_ref() else { continue };
        for m in &spec.mounts {
            let linked = t
                .edges
                .iter()
                .any(|e| (e.from == d.id && e.to == m.container) || (e.to == d.id && e.from == m.container));
            if !linked {
                let label = m
                    .path
                    .as_deref()
                    .filter(|p| !p.is_empty())
                    .unwrap_or(m.container.as_str());
                return Err(CoreError::Invalid(format!(
                    "el montaje «{}» del disco «{}» no tiene un enlace en el lienzo",
                    label,
                    d.display()
                )));
            }
        }
    }
    Ok(())
}

/// Límites de las condiciones de red de un nodo. Todos los valores llegan de la UI
/// y se comprueban antes de convertirse en argumentos de `tc` o `VBoxManage`.
pub fn link_conditions(c: &LinkConditions) -> Result<()> {
    if c.delay_ms > 10_000 {
        return Err(CoreError::Invalid("el retardo debe estar entre 0 y 10000 ms".into()));
    }
    if c.jitter_ms > 10_000 {
        return Err(CoreError::Invalid("el jitter debe estar entre 0 y 10000 ms".into()));
    }
    if c.jitter_ms > c.delay_ms {
        return Err(CoreError::Invalid("el jitter no puede superar el retardo".into()));
    }
    if c.loss_tenths > 1_000 {
        return Err(CoreError::Invalid("la pérdida debe estar entre 0 y 100 %".into()));
    }
    if let Some(rate) = c.rate_kbit {
        if !(8..=1_000_000).contains(&rate) {
            return Err(CoreError::Invalid(
                "la velocidad debe estar entre 8 y 1000000 kbit/s".into(),
            ));
        }
    }
    Ok(())
}

/// Límites de las opciones con las que se crea un contenedor Docker: variables de
/// entorno y puertos publicados. Los nombres de variable solo admiten ASCII para
/// que Docker los interprete igual en cualquier plataforma.
pub fn docker_run_options(o: &DockerRunOptions) -> Result<()> {
    if o.env.len() > 32 {
        return Err(CoreError::Invalid(
            "no se pueden definir más de 32 variables de entorno".into(),
        ));
    }
    let mut keys = HashSet::new();
    for e in &o.env {
        let mut chars = e.key.chars();
        let key_ok = matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
            && e.key.len() <= 128
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !key_ok {
            return bad("nombre de variable de entorno no válido", &e.key);
        }
        if e.value.chars().count() > 1024 {
            return Err(CoreError::Invalid(format!(
                "el valor de la variable de entorno «{}» no puede superar los 1024 caracteres",
                e.key
            )));
        }
        if e.value.chars().any(|c| c.is_control()) {
            return Err(CoreError::Invalid(format!(
                "el valor de la variable de entorno «{}» no puede contener caracteres de control",
                e.key
            )));
        }
        if !keys.insert(e.key.as_str()) {
            return Err(CoreError::Invalid(format!(
                "la variable de entorno «{}» está repetida",
                e.key
            )));
        }
    }

    if o.ports.len() > 32 {
        return Err(CoreError::Invalid("no se pueden publicar más de 32 puertos".into()));
    }
    let mut published = HashSet::new();
    for p in &o.ports {
        if p.host == 0 || p.container == 0 {
            return Err(CoreError::Invalid("los puertos deben estar entre 1 y 65535".into()));
        }
        if !published.insert((p.host, p.proto)) {
            return Err(CoreError::Invalid(format!(
                "el puerto {} ya está publicado en {}",
                p.host,
                p.proto.as_str()
            )));
        }
    }
    Ok(())
}

// ───────────────────── Creación de VMs ─────────────────────

pub const MIN_VM_MEMORY_MB: u32 = 64;
pub const MAX_VM_MEMORY_MB: u32 = 131_072;
pub const MAX_VM_CPUS: u32 = 64;
pub const MAX_VM_VRAM_MB: u32 = 256;

/// `true` para nombres con forma de UUID (`8-4-4-4-12` hexadecimales), que se
/// reservan para los identificadores internos de VirtualBox.
fn looks_like_uuid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && [parts[0].len(), parts[1].len(), parts[2].len(), parts[3].len(), parts[4].len()]
            == [8, 4, 4, 4, 12]
        && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Nombre de la VM: 1..=64 caracteres ASCII de `[A-Za-z0-9 ._-()]`. No puede
/// empezar por `-`, espacio o punto, ni terminar en espacio o punto; tampoco
/// contener separadores de ruta, `..`, ni tener forma de UUID.
pub fn vm_name(s: &str) -> Result<&str> {
    let count = s.chars().count();
    let ok = count >= 1
        && count <= 64
        && !s.starts_with('-')
        && !s.starts_with(' ')
        && !s.starts_with('.')
        && !s.ends_with(' ')
        && !s.ends_with('.')
        && !s.contains("..")
        && !s.contains('/')
        && !s.contains('\\')
        && !looks_like_uuid(s)
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-' | '(' | ')'));
    if ok { Ok(s) } else { bad("nombre de máquina virtual no válido", s) }
}

/// Unicidad contra `VBoxManage list vms`, sin distinguir mayúsculas.
pub fn vm_name_unique<'a>(name: &'a str, existing: &[String]) -> Result<&'a str> {
    vm_name(name)?;
    if existing.iter().any(|e| e.eq_ignore_ascii_case(name)) {
        return Err(CoreError::Invalid(format!("ya existe una máquina virtual llamada «{name}»")));
    }
    Ok(name)
}

/// El `ostype` debe estar entre los que ofrece la versión instalada.
pub fn vm_ostype<'a>(id: &'a str, available: &[String]) -> Result<&'a str> {
    if available.iter().any(|a| a == id) {
        Ok(id)
    } else {
        bad("tipo de sistema operativo no disponible", id)
    }
}

pub fn vm_memory_mb(v: u32) -> Result<u32> {
    if (MIN_VM_MEMORY_MB..=MAX_VM_MEMORY_MB).contains(&v) {
        Ok(v)
    } else {
        Err(CoreError::Invalid(format!(
            "la memoria debe estar entre {MIN_VM_MEMORY_MB} y {MAX_VM_MEMORY_MB} MB"
        )))
    }
}

pub fn vm_cpus(v: u32) -> Result<u32> {
    if (1..=MAX_VM_CPUS).contains(&v) {
        Ok(v)
    } else {
        Err(CoreError::Invalid(format!("el número de CPU debe estar entre 1 y {MAX_VM_CPUS}")))
    }
}

pub fn vm_disk_mb(v: u32) -> Result<u32> {
    if (1..=MAX_VDI_SIZE_MB).contains(&v) {
        Ok(v)
    } else {
        Err(CoreError::Invalid(format!(
            "el tamaño del disco debe estar entre 1 y {MAX_VDI_SIZE_MB} MB"
        )))
    }
}

pub fn vm_vram_mb(v: u32) -> Result<u32> {
    if (1..=MAX_VM_VRAM_MB).contains(&v) {
        Ok(v)
    } else {
        Err(CoreError::Invalid(format!("la VRAM debe estar entre 1 y {MAX_VM_VRAM_MB} MB")))
    }
}

/// Devuelve el valor canónico en minúsculas.
pub fn vm_firmware(s: &str) -> Result<&'static str> {
    if s.eq_ignore_ascii_case("bios") {
        Ok("bios")
    } else if s.eq_ignore_ascii_case("efi") {
        Ok("efi")
    } else {
        bad("firmware no válido", s)
    }
}

/// Devuelve el valor canónico en minúsculas.
pub fn vm_graphics(s: &str) -> Result<&'static str> {
    for value in ["vboxvga", "vboxsvga", "vmsvga"] {
        if s.eq_ignore_ascii_case(value) {
            return Ok(value);
        }
    }
    bad("controlador gráfico no válido", s)
}

/// Devuelve el valor canónico en minúsculas.
pub fn vm_disk_bus(s: &str) -> Result<&'static str> {
    if s.eq_ignore_ascii_case("ide") {
        Ok("ide")
    } else if s.eq_ignore_ascii_case("sata") {
        Ok("sata")
    } else {
        bad("controlador de disco no válido", s)
    }
}

/// ISO opcional: ruta absoluta, fichero existente, extensión `.iso`, sin `-`
/// inicial ni caracteres de control. Nunca se copia ni se mueve.
pub fn vm_iso(s: &str) -> Result<&str> {
    let ok_name = !s.is_empty() && !s.starts_with('-') && !s.chars().any(|c| c.is_control());
    if !ok_name {
        return bad("ruta de ISO no válida", s);
    }
    let path = Path::new(s);
    if !path.is_absolute() {
        return Err(CoreError::Invalid("la ruta de la ISO debe ser absoluta".into()));
    }
    let extension = path.extension().and_then(|e| e.to_str());
    if !extension.is_some_and(|e| e.eq_ignore_ascii_case("iso")) {
        return Err(CoreError::Invalid("la imagen debe tener extensión .iso".into()));
    }
    if !path.is_file() {
        return Err(CoreError::Invalid(format!(
            "la ISO «{}» no existe o no es un fichero",
            s.chars().take(120).collect::<String>()
        )));
    }
    Ok(s)
}

/// VRAM por defecto según el catálogo; 128 MB si el `ostype` no está en él.
fn default_vram(ostype: &str) -> u32 {
    vm_os_catalog()
        .iter()
        .find(|e| e.id == ostype)
        .map(|e| e.defaults.vram_mb)
        .unwrap_or(128)
}

/// Valida toda la petición antes de ejecutar cualquier comando. Los listados
/// de ostypes y de VMs los aporta el llamador (leídos con `VBoxManage`).
pub fn validate_create_request(
    req: &CreateVmRequest,
    available_ostypes: &[String],
    existing_names: &[String],
) -> Result<VmCreateParams> {
    let name = vm_name_unique(&req.name, existing_names)?;
    let ostype = vm_ostype(&req.ostype, available_ostypes)?;
    let memory_mb = vm_memory_mb(req.memory_mb)?;
    let cpus = vm_cpus(req.cpus)?;
    let disk_mb = match (req.disk_mb, req.disk_gb) {
        (Some(mb), _) => vm_disk_mb(mb)?,
        (None, Some(gb)) => {
            let mb = gb.checked_mul(1024).ok_or_else(|| CoreError::Invalid("el tamaño del disco es demasiado grande".into()))?;
            vm_disk_mb(mb)?
        }
        (None, None) => return Err(CoreError::Invalid("indica el tamaño del disco en GB o en MB".into())),
    };
    let vram_mb = match req.vram_mb {
        Some(v) => vm_vram_mb(v)?,
        None => vm_vram_mb(default_vram(ostype))?,
    };
    let firmware = vm_firmware(&req.firmware)?;
    let tpm = req.tpm;
    if tpm && firmware != "efi" {
        return Err(CoreError::Invalid("TPM 2.0 solo se puede usar con firmware EFI".into()));
    }
    let graphics = vm_graphics(&req.graphics)?;
    let disk_bus = vm_disk_bus(&req.disk_bus)?;
    let iso = match req.iso.as_deref() {
        Some(s) if !s.is_empty() => Some(vm_iso(s)?.to_string()),
        _ => None,
    };
    Ok(VmCreateParams {
        name: name.to_string(),
        ostype: ostype.to_string(),
        memory_mb,
        cpus,
        disk_mb,
        vram_mb,
        firmware: firmware.to_string(),
        tpm,
        graphics: graphics.to_string(),
        disk_bus: disk_bus.to_string(),
        iso,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DiskMount, DiskSpec, Edge, EnvVar, PortMap, PortProto};

    #[test]
    fn validates_docker_run_options() {
        let ok = DockerRunOptions {
            env: vec![EnvVar { key: "MI_VAR".into(), value: "hola mundo".into() }],
            ports: vec![
                PortMap { host: 8080, container: 80, proto: PortProto::Tcp },
                PortMap { host: 5353, container: 53, proto: PortProto::Udp },
            ],
        };
        assert!(docker_run_options(&ok).is_ok());

        let too_many_env = DockerRunOptions {
            env: (0..33).map(|i| EnvVar { key: format!("V{i}"), value: String::new() }).collect(),
            ..Default::default()
        };
        assert!(docker_run_options(&too_many_env).is_err());

        for key in ["", "1VAR", "CON-GUION", "CON ESPACIO", &"a".repeat(129)] {
            let o = DockerRunOptions {
                env: vec![EnvVar { key: key.into(), value: String::new() }],
                ..Default::default()
            };
            assert!(docker_run_options(&o).is_err(), "{key:?}");
        }
        let longest = DockerRunOptions {
            env: vec![EnvVar { key: "a".repeat(128), value: String::new() }],
            ..Default::default()
        };
        assert!(docker_run_options(&longest).is_ok());

        let control = DockerRunOptions {
            env: vec![EnvVar { key: "OK".into(), value: "a\nb".into() }],
            ..Default::default()
        };
        assert!(docker_run_options(&control).is_err());

        let too_long = DockerRunOptions {
            env: vec![EnvVar { key: "OK".into(), value: "a".repeat(1025) }],
            ..Default::default()
        };
        assert!(docker_run_options(&too_long).is_err());

        let repeated = DockerRunOptions {
            env: vec![
                EnvVar { key: "A".into(), value: "1".into() },
                EnvVar { key: "A".into(), value: "2".into() },
            ],
            ..Default::default()
        };
        assert!(docker_run_options(&repeated).is_err());

        let too_many_ports = DockerRunOptions {
            ports: (1..=33)
                .map(|i| PortMap { host: i, container: i, proto: PortProto::Tcp })
                .collect(),
            ..Default::default()
        };
        assert!(docker_run_options(&too_many_ports).is_err());

        let zero_host = DockerRunOptions {
            ports: vec![PortMap { host: 0, container: 80, proto: PortProto::Tcp }],
            ..Default::default()
        };
        assert!(docker_run_options(&zero_host).is_err());

        let zero_container = DockerRunOptions {
            ports: vec![PortMap { host: 8080, container: 0, proto: PortProto::Tcp }],
            ..Default::default()
        };
        assert!(docker_run_options(&zero_container).is_err());

        let repeated_port = DockerRunOptions {
            ports: vec![
                PortMap { host: 8080, container: 80, proto: PortProto::Tcp },
                PortMap { host: 8080, container: 81, proto: PortProto::Tcp },
            ],
            ..Default::default()
        };
        assert!(docker_run_options(&repeated_port).is_err());

        let same_host_other_proto = DockerRunOptions {
            ports: vec![
                PortMap { host: 8080, container: 80, proto: PortProto::Tcp },
                PortMap { host: 8080, container: 80, proto: PortProto::Udp },
            ],
            ..Default::default()
        };
        assert!(docker_run_options(&same_host_other_proto).is_ok());
    }

    #[test]
    fn rejects_option_injection() {
        assert!(container_name("--privileged").is_err());
        assert!(docker_image("-v /:/host").is_err());
        assert!(vbox_target("--type").is_err());
    }

    #[test]
    fn rejects_shell_metacharacters() {
        for s in ["a;rm -rf /", "a b", "a$(id)", "a`id`", "a|b", "a&b", "a\nb"] {
            assert!(container_name(s).is_err(), "{s}");
        }
        assert!(docker_image("nginx; id").is_err());
    }

    #[test]
    fn accepts_normal_values() {
        assert!(container_name("web-01_a.b").is_ok());
        assert!(docker_image("ghcr.io/org/app:1.2@sha256").is_ok());
        assert!(vbox_target("Ubuntu Server 22.04 (lab)").is_ok());
        assert!(vbox_target("c0ffee00-1111-2222-3333-444455556666").is_ok());
        assert!(network_name("vmlab-0a1b2c3d").is_ok());
        assert!(network_name("bridge").is_err());
        assert!(network_name("vmlab-").is_err());
        assert!(network_name("lablab-0a1b2c3d").is_err(), "las redes antiguas no se crean");
        assert!(managed_network("vmlab-0a1b2c3d"));
        assert!(managed_network("lablab-0a1b2c3d"));
        assert!(!managed_network("bridge"));
    }

    #[test]
    fn validates_guest_inputs() {
        assert!(linux_iface("eth0").is_ok());
        assert!(linux_iface("enp0s3.100").is_ok());
        assert!(linux_iface("eth 0").is_err());
        assert!(linux_iface("-eth0").is_err());
        assert!(guest_user("ubuntu").is_ok());
        assert!(guest_user("--root").is_err());
        assert!(guest_password("una contraseña con espacios").is_ok());
        assert!(guest_password("a\nb").is_err());
        assert!(host_iface("VirtualBox Host-Only Ethernet Adapter").is_ok());
        assert!(host_iface("--evil").is_err());
    }

    #[test]
    fn validates_hosts_names_and_addresses() {
        assert!(hostname_label("web-01").is_ok());
        assert!(hostname_label("a").is_ok());
        assert!(hostname_label(&"a".repeat(63)).is_ok());
        for s in ["", "-web", "web-", "Web", "web_01", "web 01", "web;id", "ñandu", &"a".repeat(64)] {
            assert!(hostname_label(s).is_err(), "{s}");
        }

        assert_eq!(hosts_ipv4("10.77.9.11").unwrap(), Ipv4Addr::new(10, 77, 9, 11));
        for s in ["0.0.0.0", "224.0.0.1", "255.255.255.255", "127.0.0.1", "10.77.9", "10.77.9.999", "::1"] {
            assert!(hosts_ipv4(s).is_err(), "{s}");
        }
    }

    #[test]
    fn derives_and_validates_netmasks() {
        assert_eq!(netmask_from_prefix(24).as_deref(), Some("255.255.255.0"));
        assert_eq!(netmask_from_prefix(16).as_deref(), Some("255.255.0.0"));
        assert_eq!(netmask_from_prefix(0).as_deref(), Some("0.0.0.0"));
        assert_eq!(netmask_from_prefix(32).as_deref(), Some("255.255.255.255"));
        assert_eq!(netmask_from_prefix(33), None);

        assert_eq!(netmask("255.255.255.0").as_deref(), Some("255.255.255.0"));
        assert_eq!(netmask(" 255.255.0.0 ").as_deref(), Some("255.255.0.0"));
        assert_eq!(netmask("255.0.255.0"), None);
        assert_eq!(netmask("24"), None);
        assert_eq!(netmask(""), None);
    }

    #[test]
    fn derives_labsnap_slugs() {
        assert_eq!(labsnap_slug("Añadir ü ÁÉÍÓÚ"), "anadir-u-aeiou");
        assert_eq!(labsnap_slug("Lab prueba #1 (copia)"), "lab-prueba-1-copia");
        assert_eq!(labsnap_slug("--Hola--  mundo--"), "hola-mundo");
        assert_eq!(labsnap_slug("¿¡###"), "");
        assert_eq!(labsnap_slug(&"a".repeat(60)), "a".repeat(40));
        let exact = "a".repeat(40);
        assert_eq!(labsnap_slug(&exact), exact);
    }

    #[test]
    fn validates_labsnap_ids_and_labels() {
        assert!(labsnap_id("lab-1").is_ok());
        assert!(labsnap_id("").is_err());
        assert!(labsnap_id("Lab-1").is_err());
        assert!(labsnap_id("lab_1").is_err());
        assert!(labsnap_id(&"a".repeat(41)).is_err());

        assert!(labsnap_label("Lab 1 (copia) - ñandú").is_ok());
        assert!(labsnap_label("").is_err());
        assert!(labsnap_label(&"a".repeat(41)).is_err());
        for s in ["a\"b", "a|b", "a/b", "a\\b", "a\nb", "a\u{7}b"] {
            assert!(labsnap_label(s).is_err(), "{s:?}");
        }
    }

    #[test]
    fn validates_link_conditions() {
        let ok = LinkConditions {
            delay_ms: 50,
            jitter_ms: 10,
            loss_tenths: 15,
            rate_kbit: Some(20_000),
        };
        assert!(link_conditions(&ok).is_ok());

        let jitter_over_delay = LinkConditions { jitter_ms: 51, ..ok.clone() };
        assert!(link_conditions(&jitter_over_delay).is_err());

        let too_much_loss = LinkConditions { loss_tenths: 1_001, ..ok.clone() };
        assert!(link_conditions(&too_much_loss).is_err());

        let too_slow = LinkConditions { rate_kbit: Some(7), ..ok.clone() };
        assert!(link_conditions(&too_slow).is_err());

        let too_fast = LinkConditions { rate_kbit: Some(1_000_001), ..ok.clone() };
        assert!(link_conditions(&too_fast).is_err());

        let delay_out_of_range = LinkConditions { delay_ms: 10_001, jitter_ms: 0, ..ok };
        assert!(link_conditions(&delay_out_of_range).is_err());
    }

    fn machine(id: &str, kind: NodeKind) -> NodeSpec {
        NodeSpec { id: id.into(), kind, target: id.into(), ..NodeSpec::default() }
    }

    fn disk(id: &str, shared: bool, mounts: &[(&str, &str)]) -> NodeSpec {
        NodeSpec {
            id: id.into(),
            kind: NodeKind::Disk,
            target: id.into(),
            disk: Some(DiskSpec {
                shared,
                mounts: mounts
                    .iter()
                    .map(|(container, path)| DiskMount {
                        container: (*container).into(),
                        path: Some((*path).into()),
                    })
                    .collect(),
                size_mb: None,
            }),
            ..NodeSpec::default()
        }
    }

    fn vm_disk(id: &str, shared: bool, size_mb: Option<u32>, vm: &str) -> NodeSpec {
        NodeSpec {
            id: id.into(),
            kind: NodeKind::Disk,
            target: id.into(),
            disk: Some(DiskSpec {
                shared,
                mounts: vec![DiskMount { container: vm.into(), path: None }],
                size_mb,
            }),
            ..NodeSpec::default()
        }
    }

    fn link(a: &str, b: &str) -> Edge {
        Edge { from: a.into(), to: b.into(), kind: EdgeKind::Mount }
    }

    #[test]
    fn sanitizes_and_validates_disk_names() {
        assert_eq!(sanitize_disk_name("Datos DB"), "datos-db");
        assert_eq!(sanitize_disk_name("--Datos..DB--"), "datos..db");
        assert_eq!(sanitize_disk_name("áé ñandú"), "ae-nandu");
        assert_eq!(sanitize_disk_name("rock&roll"), "rock-roll");
        assert_eq!(sanitize_disk_name("###"), "");
        assert_eq!(sanitize_disk_name(&"a".repeat(60)), "a".repeat(40));

        assert_eq!(volume_name("Datos DB").unwrap(), "vmlab-vol-datos-db");
        assert_eq!(volume_name("-datos").unwrap(), "vmlab-vol-datos");
        assert!(volume_name("###").is_err());
        assert!(disk_volume_name("vmlab-vol-datos").is_ok());
        assert!(disk_volume_name("datos").is_err());
        assert!(disk_volume_name("vmlab-vol-Datos").is_err());
        assert!(disk_name(&"a".repeat(40)).is_ok());
        for s in ["", "-datos", ".datos", "Datos", "a/b", "a:b", "a b", &"a".repeat(41)] {
            assert!(disk_name(s).is_err(), "{s}");
        }
    }

    #[test]
    fn validates_mount_paths() {
        for p in ["/data", "/var/lib/mysql", "/srv/app-1/data", "/datos/"] {
            assert!(mount_path(p).is_ok(), "{p}");
        }
        assert_eq!(normalize_mount_path("/datos/"), "/datos");

        for p in [
            "data",
            "",
            "/",
            "/proc",
            "/proc/cpuinfo",
            "/sys",
            "/dev",
            "/etc",
            "/etc/hosts",
            "/var/run",
            "/var/run/docker.sock",
            "/run",
            "/run/user",
            "/datos/../etc",
            "/datos:x",
            "/datos,x",
            "/datos\\x",
            "/datos\nx",
            "/datos\u{7}",
            &format!("/{}", "a".repeat(1025)),
        ] {
            assert!(mount_path(p).is_err(), "{p:?}");
        }
    }

    #[test]
    fn exclusive_disk_rejects_two_links_and_shared_accepts_them() {
        let exclusive = Topology {
            nodes: vec![
                machine("web", NodeKind::Docker),
                machine("db", NodeKind::Docker),
                disk("datos", false, &[("web", "/data"), ("db", "/data")]),
            ],
            edges: vec![link("datos", "web"), link("datos", "db")],
        };
        let err = disk_topology(&exclusive).unwrap_err().to_string();
        assert!(err.contains("exclusivo"), "{err}");

        let shared = Topology {
            nodes: vec![
                machine("web", NodeKind::Docker),
                machine("db", NodeKind::Docker),
                disk("datos", true, &[("web", "/data"), ("db", "/data")]),
            ],
            edges: vec![link("datos", "web"), link("datos", "db")],
        };
        assert!(disk_topology(&shared).is_ok());
    }

    #[test]
    fn a_vm_disk_is_valid_without_a_mount_path() {
        let t = Topology {
            nodes: vec![machine("vm", NodeKind::Vbox), vm_disk("datos", false, None, "vm")],
            edges: vec![link("datos", "vm")],
        };
        assert_eq!(disk_kind(&t, &t.nodes[1]).unwrap(), DiskKind::Vdi);
        assert!(disk_topology(&t).is_ok());
    }

    #[test]
    fn shared_mode_with_virtual_machines_is_rejected() {
        let t = Topology {
            nodes: vec![machine("vm", NodeKind::Vbox), vm_disk("datos", true, None, "vm")],
            edges: vec![link("datos", "vm")],
        };
        let err = disk_topology(&t).unwrap_err().to_string();
        assert_eq!(err, "entrada no válida: el modo compartido con máquinas virtuales todavía no está disponible");
    }

    #[test]
    fn a_vm_disk_rejects_more_than_one_link() {
        let t = Topology {
            nodes: vec![
                machine("vm1", NodeKind::Vbox),
                machine("vm2", NodeKind::Vbox),
                NodeSpec {
                    id: "datos".into(),
                    kind: NodeKind::Disk,
                    target: "datos".into(),
                    disk: Some(DiskSpec {
                        shared: false,
                        mounts: vec![
                            DiskMount { container: "vm1".into(), path: None },
                            DiskMount { container: "vm2".into(), path: None },
                        ],
                        size_mb: None,
                    }),
                    ..NodeSpec::default()
                },
            ],
            edges: vec![link("datos", "vm1"), link("datos", "vm2")],
        };
        let err = disk_topology(&t).unwrap_err().to_string();
        assert!(err.contains("una máquina virtual"), "{err}");
    }

    #[test]
    fn a_disk_cannot_mix_docker_and_virtual_machine_links() {
        let t = Topology {
            nodes: vec![
                machine("web", NodeKind::Docker),
                machine("vm", NodeKind::Vbox),
                NodeSpec {
                    id: "datos".into(),
                    kind: NodeKind::Disk,
                    target: "datos".into(),
                    disk: Some(DiskSpec {
                        shared: true,
                        mounts: vec![
                            DiskMount { container: "web".into(), path: Some("/data".into()) },
                            DiskMount { container: "vm".into(), path: None },
                        ],
                        size_mb: None,
                    }),
                    ..NodeSpec::default()
                },
            ],
            edges: vec![link("datos", "web"), link("datos", "vm")],
        };
        let err = disk_kind(&t, &t.nodes[2]).unwrap_err().to_string();
        assert!(err.contains("no puede enlazarse a contenedores Docker y máquinas virtuales"), "{err}");
        assert!(disk_topology(&t).is_err());
    }

    #[test]
    fn vm_disk_size_must_be_in_range() {
        for size in [0, MAX_VDI_SIZE_MB + 1] {
            let t = Topology {
                nodes: vec![
                    machine("vm", NodeKind::Vbox),
                    vm_disk("datos", false, Some(size), "vm"),
                ],
                edges: vec![link("datos", "vm")],
            };
            let err = disk_topology(&t).unwrap_err().to_string();
            assert!(err.contains("tamaño"), "{err}");
            assert!(err.contains(&MAX_VDI_SIZE_MB.to_string()), "{err}");
        }
        let t = Topology {
            nodes: vec![
                machine("vm", NodeKind::Vbox),
                vm_disk("datos", false, Some(MAX_VDI_SIZE_MB), "vm"),
            ],
            edges: vec![link("datos", "vm")],
        };
        assert!(disk_topology(&t).is_ok());
    }

    #[test]
    fn rejects_duplicate_paths_and_incomplete_links() {
        let same_disk = Topology {
            nodes: vec![
                machine("web", NodeKind::Docker),
                disk("datos", true, &[("web", "/data"), ("web", "/data/")]),
            ],
            edges: vec![link("datos", "web")],
        };
        let err = disk_topology(&same_disk).unwrap_err().to_string();
        assert!(err.contains("misma ruta"), "{err}");

        let two_disks = Topology {
            nodes: vec![
                machine("web", NodeKind::Docker),
                disk("uno", false, &[("web", "/data")]),
                disk("dos", false, &[("web", "/data/")]),
            ],
            edges: vec![link("uno", "web"), link("dos", "web")],
        };
        let err = disk_topology(&two_disks).unwrap_err().to_string();
        assert!(err.contains("otro disco montado"), "{err}");

        let no_path = Topology {
            nodes: vec![machine("web", NodeKind::Docker), disk("datos", false, &[])],
            edges: vec![link("datos", "web")],
        };
        let err = disk_topology(&no_path).unwrap_err().to_string();
        assert!(err.contains("ruta de montaje"), "{err}");

        let no_link = Topology {
            nodes: vec![machine("web", NodeKind::Docker), disk("datos", false, &[("web", "/data")])],
            edges: vec![],
        };
        let err = disk_topology(&no_link).unwrap_err().to_string();
        assert!(err.contains("no tiene un enlace"), "{err}");
    }

    #[test]
    fn topologies_without_disks_are_always_valid() {
        let t = Topology {
            nodes: vec![machine("vm", NodeKind::Vbox), machine("web", NodeKind::Docker)],
            edges: vec![Edge { from: "vm".into(), to: "web".into(), kind: EdgeKind::Network }],
        };
        assert!(disk_topology(&t).is_ok());
    }

    // ── C1: creación de VMs ──

    #[test]
    fn validates_vm_names() {
        for name in ["Ubuntu", "Windows 10 (lab)", "a_b-c.d", "VM-01", "a"] {
            assert_eq!(vm_name(name).unwrap(), name, "{name}");
        }
        for name in [
            "",
            "-vm",
            " vm",
            ".vm",
            "a..b",
            "a/b",
            "a\\b",
            "ñandú",
            "vm;id",
            "vm\n",
            "vm ",
            "vm.",
            "c0ffee00-1111-2222-3333-444455556666",
            &"a".repeat(65),
        ] {
            assert!(vm_name(name).is_err(), "{name:?}");
        }
        assert_eq!(vm_name(&"a".repeat(64)).unwrap().len(), 64);
    }

    #[test]
    fn vm_name_rejects_trailing_space_or_dot_and_uuid_shapes() {
        for name in ["Ubuntu ", "Ubuntu.", " Equipo ", ".Equipo", "C0FFEE00-1111-2222-3333-444455556666"] {
            assert!(vm_name(name).is_err(), "{name:?}");
        }
        // Un guion al final sigue siendo un nombre válido; solo se prohíbe el inicio.
        assert!(vm_name("Equipo-").is_ok());
        // Casi-UUID: no encaja en 8-4-4-4-12, así que es un nombre normal.
        assert!(vm_name("c0ffee00-1111-2222-3333-44445555666").is_ok());
    }

    #[test]
    fn vm_name_uniqueness_ignores_case() {
        let existing = vec!["Ubuntu Server".into(), "win10".into()];
        assert!(vm_name_unique("Nueva VM", &existing).is_ok());
        assert!(vm_name_unique("ubuntu server", &existing).is_err());
        assert!(vm_name_unique("WIN10", &existing).is_err());
    }

    #[test]
    fn validates_ostype_against_a_real_trimmed_list() {
        // Recorte real de `VBoxManage list ostypes`.
        let out = r#"ID:          WindowsXP
Description: Windows XP
ID:          Windows11_64
Description: Windows 11 (64-bit)
ID:          Ubuntu_64
Description: Ubuntu (64-bit)
ID:          Other
Description: Other/Unknown
"#;
        let ids: Vec<String> = out
            .lines()
            .filter_map(|l| l.strip_prefix("ID:").map(|v| v.trim().to_string()))
            .collect();
        assert_eq!(ids, ["WindowsXP", "Windows11_64", "Ubuntu_64", "Other"]);
        assert_eq!(vm_ostype("Windows11_64", &ids).unwrap(), "Windows11_64");
        assert!(vm_ostype("NoExiste", &ids).is_err());
        assert!(vm_ostype("--evil", &ids).is_err());
    }

    #[test]
    fn validates_create_ranges_and_enums() {
        assert_eq!(vm_memory_mb(64).unwrap(), 64);
        assert_eq!(vm_memory_mb(MAX_VM_MEMORY_MB).unwrap(), MAX_VM_MEMORY_MB);
        for v in [0, 63, MAX_VM_MEMORY_MB + 1] {
            assert!(vm_memory_mb(v).is_err(), "{v}");
        }
        assert_eq!(vm_cpus(1).unwrap(), 1);
        assert_eq!(vm_cpus(MAX_VM_CPUS).unwrap(), MAX_VM_CPUS);
        for v in [0, MAX_VM_CPUS + 1] {
            assert!(vm_cpus(v).is_err(), "{v}");
        }
        assert_eq!(vm_disk_mb(1).unwrap(), 1);
        assert_eq!(vm_disk_mb(MAX_VDI_SIZE_MB).unwrap(), MAX_VDI_SIZE_MB);
        for v in [0, MAX_VDI_SIZE_MB + 1] {
            assert!(vm_disk_mb(v).is_err(), "{v}");
        }
        assert_eq!(vm_vram_mb(1).unwrap(), 1);
        assert_eq!(vm_vram_mb(MAX_VM_VRAM_MB).unwrap(), MAX_VM_VRAM_MB);
        for v in [0, MAX_VM_VRAM_MB + 1] {
            assert!(vm_vram_mb(v).is_err(), "{v}");
        }
        assert_eq!(vm_firmware("EFI").unwrap(), "efi");
        assert_eq!(vm_graphics("VBoxSVGA").unwrap(), "vboxsvga");
        assert_eq!(vm_disk_bus("SATA").unwrap(), "sata");
        for bad_value in ["", "uefi", "--flag"] {
            assert!(vm_firmware(bad_value).is_err());
            assert!(vm_graphics(bad_value).is_err());
            assert!(vm_disk_bus(bad_value).is_err());
        }
    }

    #[test]
    fn tpm_requires_efi() {
        let mut req = CreateVmRequest {
            name: "Win11".into(),
            ostype: "Windows11_64".into(),
            memory_mb: 4096,
            cpus: 2,
            disk_gb: Some(64),
            disk_mb: None,
            vram_mb: None,
            firmware: "bios".into(),
            tpm: true,
            graphics: "vboxsvga".into(),
            disk_bus: "sata".into(),
            iso: None,
        };
        let available = vec!["Windows11_64".into()];
        let err = validate_create_request(&req, &available, &[]).unwrap_err().to_string();
        assert!(err.contains("TPM"), "{err}");

        req.firmware = "efi".into();
        let params = validate_create_request(&req, &available, &[]).unwrap();
        assert!(params.tpm);
        assert_eq!(params.firmware, "efi");
        assert_eq!(params.disk_mb, 64 * 1024);
        assert_eq!(params.vram_mb, 128); // viene del catálogo
    }

    #[test]
    fn validates_iso_path() {
        let dir = std::env::temp_dir();
        let iso = dir.join(format!("vmlab-c1-{}-{}.ISO", std::process::id(), 1));
        std::fs::write(&iso, b"ISO").unwrap();
        let good = iso.to_string_lossy().into_owned();
        assert_eq!(vm_iso(&good).unwrap(), good);

        for bad_value in [
            "discos/instalador.iso".to_string(), // relativa
            dir.join("instalador.img").to_string_lossy().into_owned(),
            format!("-{}", good),
            dir.join(format!("vmlab-c1-{}-no-existe.iso", std::process::id()))
                .to_string_lossy()
                .into_owned(),
        ] {
            assert!(vm_iso(&bad_value).is_err(), "{bad_value:?}");
        }
        std::fs::remove_file(&iso).unwrap();
    }

    #[test]
    fn every_catalog_default_passes_validation() {
        for entry in vm_os_catalog() {
            let d = entry.defaults;
            assert!(vm_memory_mb(d.memory_mb).is_ok(), "{}", entry.label);
            assert!(vm_cpus(d.cpus).is_ok(), "{}", entry.label);
            assert!(vm_disk_mb(d.disk_gb * 1024).is_ok(), "{}", entry.label);
            assert!(vm_vram_mb(d.vram_mb).is_ok(), "{}", entry.label);
            assert!(vm_firmware(&d.firmware).is_ok(), "{}", entry.label);
            assert!(vm_graphics(&d.graphics).is_ok(), "{}", entry.label);
            assert!(vm_disk_bus(&d.disk_bus).is_ok(), "{}", entry.label);
            assert!(!(d.tpm && d.firmware != "efi"), "{}", entry.label);
        }
    }
}
