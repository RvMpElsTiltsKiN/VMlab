//! Gestión de los discos creados por VMLab: listar, eliminar y ampliar.
//!
//! Solo se tocan medios bajo `Tools::disks_dir` con el patrón
//! `vmlab-disk-<nombre>.vdi` y volúmenes `vmlab-vol-<nombre>`. Todo nombre que
//! llega de la interfaz se valida aquí antes de convertirse en argumento; los
//! comandos se lanzan sin shell y con timeout. Ante la duda (salida ilegible,
//! estado desconocido, enlace simbólico) la operación se rechaza: nunca se
//! asume que un disco está sin uso.
use crate::error::{CoreError, Result};
use crate::model::{ManagedDisk, ManagedDiskKind, ManagedDiskUser};
use crate::runner::{run, run_ok};
use crate::tools::Tools;
use crate::{disks, validate, vbox};
use std::collections::BTreeSet;
use std::path::{Component, Path};
use std::time::Duration;

const SHORT: Duration = Duration::from_secs(15);
const LONG: Duration = Duration::from_secs(120);
const VDI_PREFIX: &str = "vmlab-disk-";
const VDI_SUFFIX: &str = ".vdi";
const VOLUME_PREFIX: &str = "vmlab-vol-";

/// Información leída de `showmediuminfo disk <ruta>` (sin `--machinereadable`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct MediumInfo {
    capacity_mb: Option<u64>,
    size_on_disk_mb: Option<u64>,
    in_use_by: Vec<ManagedDiskUser>,
    /// `false` si el comando falló o la salida no se pudo interpretar.
    readable: bool,
    note: Option<String>,
}

impl MediumInfo {
    fn unreadable(reason: &str) -> Self {
        Self {
            capacity_mb: None,
            size_on_disk_mb: None,
            in_use_by: Vec::new(),
            readable: false,
            note: Some(format!(
                "No se pudo leer la información del disco ({reason}); no se puede garantizar que no esté en uso."
            )),
        }
    }

    fn to_managed(&self, name: &str) -> ManagedDisk {
        ManagedDisk {
            name: name.to_string(),
            kind: ManagedDiskKind::Vdi,
            capacity_mb: self.capacity_mb,
            size_on_disk_mb: self.size_on_disk_mb,
            in_use_by: self.in_use_by.clone(),
            deletable: self.readable && self.in_use_by.is_empty(),
            resizable: self.readable,
            note: self.note.clone(),
        }
    }
}

// ───────────────────── Parseo tolerante de VirtualBox ─────────────────────

/// Valores de todas las líneas `clave: valor` (la clave puede repetirse).
fn field_values<'a>(info: &'a str, field: &str) -> Vec<&'a str> {
    info.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim().eq_ignore_ascii_case(field).then_some(value.trim())
        })
        .collect()
}

/// MB a partir de `1024 MBytes`, `2 MBytes`, `1 GBytes` o un número de bytes.
fn parse_mb(raw: &str) -> Option<u64> {
    let compact = raw.trim().trim_matches('"').trim().replace(' ', "").to_ascii_lowercase();
    for (suffix, factor) in [("mbytes", 1.0f64), ("mb", 1.0), ("gbytes", 1024.0), ("gb", 1024.0)] {
        if let Some(number) = compact.strip_suffix(suffix) {
            if let Ok(value) = number.parse::<f64>() {
                if value.is_finite() && value >= 0.0 {
                    return Some((value * factor).round() as u64);
                }
            }
        }
    }
    compact.parse::<u64>().ok().map(|bytes| bytes / 1024 / 1024)
}

/// Extrae `nombre (UUID: uuid)` de un valor `In use by VMs:`. Admite varias VMs
/// en la misma línea (separadas por comas), en líneas distintas y sin UUID.
fn parse_users(value: &str) -> Vec<ManagedDiskUser> {
    let mut users = Vec::new();
    let mut cursor = 0usize;
    loop {
        let tail = &value[cursor..];
        let Some(open_rel) = tail.to_ascii_lowercase().find("(uuid:") else {
            break;
        };
        let open = cursor + open_rel;
        let Some(close_rel) = value[open..].find(')') else {
            break;
        };
        let close = open + close_rel;
        let name = value[cursor..open]
            .trim()
            .trim_matches(|c: char| c == ',' || c == ';' || c.is_whitespace());
        let uuid = value[open + "(UUID:".len()..close].trim();
        if !name.is_empty() && !name.eq_ignore_ascii_case("none") {
            users.push(ManagedDiskUser {
                name: name.to_string(),
                uuid: (!uuid.is_empty()).then(|| uuid.to_string()),
            });
        }
        cursor = close + 1;
    }
    for part in value[cursor..].split([',', ';']) {
        let name = part.trim().trim_matches('"');
        if !name.is_empty() && !name.eq_ignore_ascii_case("none") && !name.eq_ignore_ascii_case("n/a") {
            users.push(ManagedDiskUser { name: name.to_string(), uuid: None });
        }
    }
    users
}

/// Parsea la salida legible de `showmediuminfo`. Si no aparece la capacidad o
/// el campo de uso, la información se marca como ilegible: ante la duda nunca
/// se permite borrar ni se informa de "sin uso".
fn parse_medium_info(info: &str) -> MediumInfo {
    let capacity_mb = field_values(info, "Capacity").first().and_then(|v| parse_mb(v));
    let size_on_disk_mb = field_values(info, "Size on disk").first().and_then(|v| parse_mb(v));
    let use_lines = field_values(info, "In use by VMs");

    let mut in_use_by = Vec::new();
    for line in &use_lines {
        for user in parse_users(line) {
            if !in_use_by.contains(&user) {
                in_use_by.push(user);
            }
        }
    }

    let mut problems = Vec::new();
    if capacity_mb.is_none() {
        problems.push("no se pudo leer la capacidad");
    }
    if use_lines.is_empty() {
        problems.push("no se pudo leer la lista de máquinas que usan el disco");
    }
    let readable = problems.is_empty();
    let note = if readable {
        if in_use_by.is_empty() {
            None
        } else {
            Some("El disco está en uso por una o más máquinas; apágalas antes de ampliarlo.".into())
        }
    } else {
        Some(format!(
            "No se pudo interpretar la información del disco ({}); no se puede garantizar que no esté en uso.",
            problems.join(" y ")
        ))
    };
    MediumInfo { capacity_mb, size_on_disk_mb, in_use_by, readable, note }
}

// ───────────────────── Validación de nombres ─────────────────────

/// Name base de un VDI: se sanea, pero se rechazan rutas, traversal y opciones.
fn clean_vdi_name(name: &str) -> Result<String> {
    let path = Path::new(name);
    let traversal = path
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::RootDir | Component::Prefix(_)));
    if name.is_empty()
        || name.starts_with('-')
        || name.contains('/')
        || name.contains('\\')
        || traversal
        || name.chars().any(|c| c.is_control())
    {
        return Err(CoreError::Invalid("nombre de disco no válido".into()));
    }
    let clean = validate::sanitize_disk_name(name);
    validate::disk_name(&clean)?;
    Ok(clean)
}

/// Nombre completo de un volumen gestionado: exige el prefijo de VMLab.
fn managed_volume_name(name: &str) -> Result<String> {
    let path = Path::new(name);
    let traversal = path
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::RootDir | Component::Prefix(_)));
    if name.is_empty()
        || name.starts_with('-')
        || name.contains('/')
        || name.contains('\\')
        || traversal
        || name.chars().any(|c| c.is_control())
    {
        return Err(CoreError::Invalid("nombre de volumen no válido".into()));
    }
    let Some(base) = name.strip_prefix(VOLUME_PREFIX) else {
        return Err(CoreError::Invalid("el nombre del volumen debe empezar por vmlab-vol-".into()));
    };
    validate::disk_name(base)?;
    Ok(name.to_string())
}

/// Comprueba que la ruta sea un fichero regular y no un enlace simbólico.
fn vdi_file(path: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            CoreError::Invalid("no existe el disco VDI indicado".into())
        } else {
            CoreError::Io(format!("no se pudo consultar {}: {e}", path.display()))
        }
    })?;
    if meta.file_type().is_symlink() || !meta.file_type().is_file() {
        return Err(CoreError::Invalid("el disco VDI no es un fichero regular".into()));
    }
    Ok(())
}

/// Argumentos puros de los comandos Docker usados por esta fase.
pub fn volume_list_args() -> Vec<&'static str> {
    vec!["volume", "ls", "--filter", "name=vmlab-vol-", "--format", "{{.Name}}"]
}

pub fn volume_users_args(name: &str) -> Result<Vec<String>> {
    let name = managed_volume_name(name)?;
    Ok(vec![
        "ps".into(),
        "-a".into(),
        "--filter".into(),
        format!("volume={name}"),
        "--format".into(),
        "{{.Names}}".into(),
    ])
}

pub fn volume_rm_args(name: &str) -> Result<Vec<String>> {
    let name = managed_volume_name(name)?;
    Ok(vec!["volume".into(), "rm".into(), name])
}

// ───────────────────── Decisiones de seguridad ─────────────────────

/// No se borra un disco en uso ni uno cuya información no se pudo leer.
fn delete_guard(info: &MediumInfo, name: &str) -> Result<()> {
    if !info.readable {
        let reason = info.note.as_deref().unwrap_or("información ilegible");
        return Err(CoreError::Invalid(format!("no se puede eliminar el disco «{name}»: {reason}")));
    }
    if !info.in_use_by.is_empty() {
        let users = info.in_use_by.iter().map(|u| u.name.as_str()).collect::<Vec<_>>().join(", ");
        return Err(CoreError::Invalid(format!(
            "el disco «{name}» está en uso por {users}; desconéctalo o apaga las máquinas antes de eliminarlo"
        )));
    }
    Ok(())
}

/// Solo se permite aumentar, dentro del rango de `validate::MAX_VDI_SIZE_MB`.
fn resize_guard(info: &MediumInfo, name: &str, new_size_mb: u64) -> Result<u32> {
    if !info.readable {
        let reason = info.note.as_deref().unwrap_or("información ilegible");
        return Err(CoreError::Invalid(format!("no se puede ampliar el disco «{name}»: {reason}")));
    }
    let capacity = info.capacity_mb.ok_or_else(|| {
        CoreError::Invalid(format!("no se pudo leer la capacidad del disco «{name}»"))
    })?;
    if new_size_mb <= capacity {
        return Err(CoreError::Invalid(format!(
            "el disco «{name}» mide {capacity} MB; el nuevo tamaño debe ser mayor"
        )));
    }
    if new_size_mb > u64::from(validate::MAX_VDI_SIZE_MB) {
        return Err(CoreError::Invalid(format!(
            "el tamaño máximo del disco es {} MB",
            validate::MAX_VDI_SIZE_MB
        )));
    }
    u32::try_from(new_size_mb)
        .map_err(|_| CoreError::Invalid("el tamaño del disco es demasiado grande".into()))
}

// ───────────────────── Consultas y operaciones ─────────────────────

/// Vuelve a consultar `showmediuminfo` en el momento de borrar o ampliar.
async fn read_medium(tools: &Tools, path: &Path) -> MediumInfo {
    let bin = match tools.vbox() {
        Ok(bin) => bin,
        Err(e) => return MediumInfo::unreadable(&e.to_string()),
    };
    let args = match vbox::showmediuminfo_args(path) {
        Ok(args) => args,
        Err(e) => return MediumInfo::unreadable(&e.to_string()),
    };
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    match run(bin, &argv, SHORT).await {
        Ok(out) if out.code == 0 => parse_medium_info(&out.stdout),
        Ok(out) => {
            let message = if out.stderr.trim().is_empty() { &out.stdout } else { &out.stderr };
            let message: String = message.trim().chars().take(300).collect();
            MediumInfo::unreadable(&format!("VBoxManage devolvió {} ({message})", out.code))
        }
        Err(e) => MediumInfo::unreadable(&e.to_string()),
    }
}

/// Contenedores que usan un volumen (`docker ps -a`). Si falla la consulta se
/// devuelve el error: no se puede afirmar que el volumen esté libre.
async fn volume_users(tools: &Tools, name: &str) -> Result<Vec<String>> {
    let args = volume_users_args(name)?;
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run_ok(tools.docker()?, &argv, SHORT).await?;
    Ok(out.stdout.lines().map(str::trim).filter(|n| !n.is_empty()).map(String::from).collect())
}

/// Lista los VDI de VMLab y los volúmenes `vmlab-vol-*` de Docker.
pub async fn list(tools: &Tools, disks_dir: &Path) -> Result<Vec<ManagedDisk>> {
    let mut disks = Vec::new();

    let entries: Vec<std::fs::DirEntry> = match std::fs::read_dir(disks_dir) {
        Ok(read) => read
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|e| CoreError::Io(format!("no se pudo leer la carpeta de discos: {e}")))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(CoreError::Io(format!("no se pudo leer la carpeta de discos: {e}"))),
    };

    let mut seen = BTreeSet::new();
    for entry in entries {
        // `DirEntry::file_type` no sigue enlaces simbólicos.
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.path();
        if !disks::vdi_path_allowed(disks_dir, &path) {
            continue;
        }
        let Some(file) = path.file_name().and_then(|n| n.to_str()) else { continue };
        let Some(base) = file.strip_prefix(VDI_PREFIX).and_then(|s| s.strip_suffix(VDI_SUFFIX)) else {
            continue;
        };
        if validate::disk_name(base).is_err() || !seen.insert(base.to_string()) {
            continue;
        }
        disks.push(read_medium(tools, &path).await.to_managed(base));
    }

    let args = volume_list_args();
    let listed = run_ok(tools.docker()?, &args, SHORT).await?;
    for line in listed.stdout.lines() {
        let name = line.trim();
        if validate::disk_volume_name(name).is_err() {
            continue;
        }
        match volume_users(tools, name).await {
            Ok(users) => {
                let deletable = users.is_empty();
                disks.push(ManagedDisk {
                    name: name.to_string(),
                    kind: ManagedDiskKind::Volume,
                    capacity_mb: None,
                    size_on_disk_mb: None,
                    in_use_by: users
                        .into_iter()
                        .map(|name| ManagedDiskUser { name, uuid: None })
                        .collect(),
                    deletable,
                    resizable: false,
                    note: None,
                });
            }
            Err(e) => disks.push(ManagedDisk {
                name: name.to_string(),
                kind: ManagedDiskKind::Volume,
                capacity_mb: None,
                size_on_disk_mb: None,
                in_use_by: Vec::new(),
                deletable: false,
                resizable: false,
                note: Some(format!("No se pudo comprobar qué contenedores usan el volumen: {e}")),
            }),
        }
    }

    disks.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.name.cmp(&b.name)));
    Ok(disks)
}

/// Borra un VDI gestionado: valida el nombre, comprueba el fichero, vuelve a
/// leer `showmediuminfo` y solo entonces lo quita también del registro de VBox.
pub async fn delete_managed_disk(tools: &Tools, disks_dir: &Path, name: &str) -> Result<()> {
    let clean = clean_vdi_name(name)?;
    let path = disks::vdi_path(disks_dir, &clean)?;
    vdi_file(&path)?;

    let info = read_medium(tools, &path).await;
    delete_guard(&info, &clean)?;

    let args = vbox::closemedium_delete_args(&path, disks_dir)?;
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    run_ok(tools.vbox()?, &argv, LONG)
        .await
        .map_err(|e| CoreError::Message(format!("no se pudo eliminar el disco «{clean}»: {e}")))?;
    Ok(())
}

/// Borra un volumen gestionado, nunca un volumen ajeno a VMLab.
pub async fn delete_managed_volume(tools: &Tools, name: &str) -> Result<()> {
    let name = managed_volume_name(name)?;
    let users = volume_users(tools, &name)
        .await
        .map_err(|e| CoreError::Message(format!("no se pudo comprobar el uso del volumen «{name}»: {e}")))?;
    if !users.is_empty() {
        let names = users.join(", ");
        return Err(CoreError::Invalid(format!(
            "el volumen «{name}» está en uso por {names}; elimina o para esos contenedores antes de borrarlo"
        )));
    }
    let args = volume_rm_args(&name)?;
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    run_ok(tools.docker()?, &argv, LONG)
        .await
        .map_err(|e| CoreError::Message(format!("no se pudo eliminar el volumen «{name}»: {e}")))?;
    Ok(())
}

/// Estado de una VM que tiene el disco conectado, leído por UUID.
async fn vm_state(tools: &Tools, user: &ManagedDiskUser) -> Result<String> {
    let uuid = user.uuid.as_deref().ok_or_else(|| {
        CoreError::Invalid(format!("no se conoce el UUID de «{}»; no se puede comprobar su estado", user.name))
    })?;
    validate::vm_uuid(uuid)?;
    let out = run_ok(tools.vbox()?, &["showvminfo", uuid, "--machinereadable"], SHORT)
        .await
        .map_err(|e| CoreError::Message(format!("no se pudo leer el estado de «{}»: {e}", user.name)))?;
    let state = vbox::parse_vm_storage(&out.stdout).state;
    if state.trim().is_empty() {
        return Err(CoreError::Message(format!(
            "no se pudo determinar el estado de «{}»",
            user.name
        )));
    }
    Ok(state)
}

/// Amplía un VDI gestionado. Solo aumenta; si está conectado, todas las VMs
/// deben estar en `poweroff`/`aborted`. Si VirtualBox rechaza `modifymedium`,
/// su error se propaga sin disfrazarlo.
pub async fn resize_managed_disk(
    tools: &Tools,
    disks_dir: &Path,
    name: &str,
    new_size_mb: u64,
) -> Result<()> {
    let clean = clean_vdi_name(name)?;
    let path = disks::vdi_path(disks_dir, &clean)?;
    vdi_file(&path)?;

    let info = read_medium(tools, &path).await;
    let size_mb = resize_guard(&info, &clean, new_size_mb)?;

    for user in &info.in_use_by {
        let state = vm_state(tools, user).await?;
        if !vbox::storage_editable(&state) {
            return Err(CoreError::Invalid(format!(
                "«{}» está en estado «{}»; apágala del todo antes de ampliar el disco",
                user.name,
                state.trim()
            )));
        }
    }

    let args = vbox::modifymedium_resize_args(&path, size_mb)?;
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    run_ok(tools.vbox()?, &argv, LONG).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Salida real de `VBoxManage showmediuminfo disk <ruta>`.
    const SAMPLE: &str = "\
UUID:           c8eaa491-9fd4-48e4-a4d8-8c5a6d0989a2
Parent UUID:    base
State:          created
Type:           normal (base)
Location:       C:\\Users\\x\\AppData\\Roaming\\dev.vmlab.app\\disks\\vmlab-disk-datos.vdi
Storage format: VDI
Format variant: dynamic default
Capacity:       1024 MBytes
Size on disk:   2 MBytes
Encryption:     disabled
Property:       AllocationBlockSize=1048576
In use by VMs:  windows 10 client (UUID: ed89600c-3c10-4645-92a4-e9856684ae1f)
";

    #[test]
    fn parses_a_real_showmediuminfo_output() {
        let info = parse_medium_info(SAMPLE);
        assert_eq!(info.capacity_mb, Some(1024));
        assert_eq!(info.size_on_disk_mb, Some(2));
        assert_eq!(
            info.in_use_by,
            [ManagedDiskUser {
                name: "windows 10 client".into(),
                uuid: Some("ed89600c-3c10-4645-92a4-e9856684ae1f".into()),
            }]
        );
        assert!(info.readable);
        let disk = info.to_managed("datos");
        assert_eq!(disk.kind, ManagedDiskKind::Vdi);
        assert!(!disk.deletable, "un disco en uso nunca es borrable");
        assert!(disk.resizable);
    }

    #[test]
    fn parses_zero_one_and_two_virtual_machines() {
        let unused = parse_medium_info("Capacity: 1024 MBytes\nSize on disk: 2 MBytes\nIn use by VMs:\n");
        assert!(unused.in_use_by.is_empty());
        assert!(unused.to_managed("datos").deletable);

        let two_same_line = parse_medium_info(
            "Capacity: 1024 MBytes\nSize on disk: 2 MBytes\n\
             In use by VMs: vm uno (UUID: 11111111-1111-1111-1111-111111111111), \
             vm dos (UUID: 22222222-2222-2222-2222-222222222222)\n",
        );
        assert_eq!(two_same_line.in_use_by.len(), 2);
        assert_eq!(two_same_line.in_use_by[0].name, "vm uno");
        assert_eq!(two_same_line.in_use_by[1].uuid.as_deref(), Some("22222222-2222-2222-2222-222222222222"));

        let two_lines = parse_medium_info(
            "Capacity: 1024 MBytes\nSize on disk: 2 MBytes\n\
             In use by VMs: vm uno (UUID: 11111111-1111-1111-1111-111111111111)\n\
             In use by VMs: vm dos (UUID: 22222222-2222-2222-2222-222222222222)\n",
        );
        assert_eq!(two_lines.in_use_by.len(), 2);

        let without_uuid = parse_medium_info(
            "Capacity: 1024 MBytes\nSize on disk: 2 MBytes\nIn use by VMs: vm uno, vm dos\n",
        );
        assert_eq!(without_uuid.in_use_by.len(), 2);
        assert_eq!(without_uuid.in_use_by[0].uuid, None);
    }

    #[test]
    fn a_broken_or_failed_medium_is_never_considered_unused() {
        let broken = parse_medium_info("Capacity: ???\nState: created\n");
        assert!(!broken.readable);
        assert!(broken.note.as_deref().is_some_and(|n| n.contains("no se puede garantizar")));
        let disk = broken.to_managed("datos");
        assert!(!disk.deletable);
        assert!(!disk.resizable);
        assert_eq!(disk.capacity_mb, None);

        let no_usage_field = parse_medium_info("Capacity: 1024 MBytes\nSize on disk: 2 MBytes\n");
        assert!(!no_usage_field.readable);
        assert!(!no_usage_field.to_managed("datos").deletable);

        let failed = MediumInfo::unreadable("VBoxManage devolvió 1");
        assert!(!failed.readable);
        assert!(!failed.to_managed("datos").deletable);
    }

    #[test]
    fn builds_closed_commands_for_docker_and_modifymedium() {
        assert_eq!(
            volume_list_args(),
            ["volume", "ls", "--filter", "name=vmlab-vol-", "--format", "{{.Name}}"]
        );
        assert_eq!(
            volume_users_args("vmlab-vol-datos").unwrap(),
            [
                "ps",
                "-a",
                "--filter",
                "volume=vmlab-vol-datos",
                "--format",
                "{{.Names}}",
            ]
        );
        assert_eq!(volume_rm_args("vmlab-vol-datos").unwrap(), ["volume", "rm", "vmlab-vol-datos"]);

        let path = Path::new(r"C:\Users\lab\AppData\Roaming\VMLab\disks\vmlab-disk-datos.vdi");
        assert_eq!(
            vbox::modifymedium_resize_args(path, 2048).unwrap(),
            [
                "modifymedium",
                "disk",
                r"C:\Users\lab\AppData\Roaming\VMLab\disks\vmlab-disk-datos.vdi",
                "--resize",
                "2048",
            ]
        );
        assert!(vbox::modifymedium_resize_args(path, 0).is_err());
        assert!(vbox::modifymedium_resize_args(path, validate::MAX_VDI_SIZE_MB + 1).is_err());

        let show = vbox::showmediuminfo_args(path).unwrap();
        assert!(show.iter().all(|a| !a.starts_with("--machinereadable")));
    }

    #[test]
    fn rejects_traversal_option_like_names_and_wrong_volume_prefix() {
        assert_eq!(clean_vdi_name("Datos DB").unwrap(), "datos-db");
        assert_eq!(clean_vdi_name("datos..db").unwrap(), "datos..db");
        for bad in ["../datos", "sub/datos", "..\\datos", "-datos", "", "###", "datos\nmal"] {
            assert!(clean_vdi_name(bad).is_err(), "{bad:?}");
        }
        assert_eq!(managed_volume_name("vmlab-vol-datos-db").unwrap(), "vmlab-vol-datos-db");
        for bad in [
            "datos",
            "vmlab-disk-datos",
            "lablab-vol-datos",
            "vmlab-vol-../etc",
            "vmlab-vol--datos",
            "-vmlab-vol-datos",
        ] {
            assert!(managed_volume_name(bad).is_err(), "{bad:?}");
        }
        assert!(volume_rm_args("vmlab-disk-datos").is_err());
        assert!(volume_users_args("datos").is_err());
    }

    #[test]
    fn refuses_to_delete_a_medium_in_use_or_unreadable() {
        let busy = parse_medium_info(SAMPLE);
        let err = delete_guard(&busy, "datos").unwrap_err().to_string();
        assert!(err.contains("está en uso"), "{err}");
        assert!(err.contains("windows 10 client"), "{err}");

        let unreadable = MediumInfo::unreadable("sin respuesta");
        let err = delete_guard(&unreadable, "datos").unwrap_err().to_string();
        assert!(err.contains("no se puede eliminar"), "{err}");
        assert!(err.contains("sin respuesta"), "{err}");

        let free = parse_medium_info("Capacity: 1024 MBytes\nSize on disk: 2 MBytes\nIn use by VMs:\n");
        assert!(delete_guard(&free, "datos").is_ok());
    }

    #[test]
    fn resizing_only_grows_and_stays_inside_the_limit() {
        let info = parse_medium_info(SAMPLE); // 1024 MB reales
        assert_eq!(resize_guard(&info, "datos", 2048).unwrap(), 2048);
        for smaller in [1024, 512, 0] {
            assert!(resize_guard(&info, "datos", smaller).is_err(), "{smaller}");
        }
        assert!(resize_guard(&info, "datos", u64::from(validate::MAX_VDI_SIZE_MB) + 1).is_err());

        let unreadable = MediumInfo::unreadable("formato roto");
        assert!(resize_guard(&unreadable, "datos", 4096).is_err());
    }

    #[test]
    fn volume_listing_only_accepts_our_prefix() {
        let out = "vmlab-vol-datos\nvmlab-vol-Datos\notro-volumen\nvmlab-vol-../etc\nvmlab-vol-logs\n";
        let names: Vec<&str> = out
            .lines()
            .map(str::trim)
            .filter(|name| validate::disk_volume_name(name).is_ok())
            .collect();
        assert_eq!(names, ["vmlab-vol-datos", "vmlab-vol-logs"]);
    }
}
