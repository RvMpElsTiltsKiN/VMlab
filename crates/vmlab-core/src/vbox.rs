//! Control de VirtualBox mediante `VBoxManage`.
//!
//! Convención de red: VMLab es dueño del **adaptador 2** de cada VM (red interna
//! `vmlab-xxxxxxxx`). El adaptador 1 (NAT, internet) no se toca.
use crate::disks;
use crate::error::{CoreError, Result};
use crate::model::{
    available_os_catalog, os_family, Action, CreateVmRequest, CreatedVm, GuestAdditionsInfo,
    GuestAdditionsPrep, LinkInfo, NetMode, NodeIssue, NodeNet, VmCreateParams, VmDeleteRequest,
    VmDeleteResult, VmOsEntry, VmOsType,
};
use crate::runner::{run, run_ok};
use crate::tools::Tools;
use crate::validate;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SHORT: Duration = Duration::from_secs(15);
const LONG: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmInfo {
    pub name: String,
    pub uuid: String,
    pub running: bool,
    pub guest_additions: GuestAdditionsInfo,
}

/// Parsea líneas `"Nombre de la VM" {uuid}` de `VBoxManage list vms`.
pub fn parse_vm_list(out: &str) -> Vec<(String, String)> {
    out.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix('"')?;
            let end = rest.rfind("\" {")?;
            let uuid = rest[end + 3..].strip_suffix('}')?;
            Some((rest[..end].to_string(), uuid.to_string()))
        })
        .collect()
}

const OSTYPES_PARSE_ERROR: &str = "no se pudo interpretar la lista de sistemas operativos de VirtualBox";

/// `Family: Windows (Windows)` → `windows`; `Family: Other (Other)` → `other`.
fn family_from_output(value: &str) -> &'static str {
    let text = value.trim();
    let name = text.split_once(" (").map_or(text, |(name, _)| name);
    match name.trim().to_ascii_lowercase().as_str() {
        "windows" => "windows",
        "linux" => "linux",
        _ => "other",
    }
}

/// Parsea `VBoxManage list ostypes` en los formatos 7.x (`ID / Description: <id> --
/// <texto>`, `Family:`, `Architecture:`) y antiguo (`ID:` / `Description:`).
pub fn parse_ostypes(out: &str) -> Vec<VmOsType> {
    let mut result = Vec::new();
    let mut current: Option<VmOsType> = None;
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("ID / Description:") {
            let rest = rest.trim();
            let (id, description) = rest.split_once(" -- ").unwrap_or((rest, ""));
            let (id, description) = (id.trim(), description.trim());
            if id.is_empty() {
                continue;
            }
            if let Some(previous) = current.take() {
                result.push(previous);
            }
            current = Some(VmOsType {
                id: id.to_string(),
                description: description.to_string(),
                family: os_family(id, description),
                architecture: String::new(),
            });
        } else if let Some(id) = line.strip_prefix("ID:") {
            let id = id.trim();
            if id.is_empty() {
                continue;
            }
            if let Some(previous) = current.take() {
                result.push(previous);
            }
            current = Some(VmOsType {
                id: id.to_string(),
                description: String::new(),
                family: os_family(id, ""),
                architecture: String::new(),
            });
        } else if let Some(value) = line.strip_prefix("Family:") {
            if let Some(entry) = current.as_mut() {
                entry.family = family_from_output(value).to_string();
            }
        } else if let Some(value) = line.strip_prefix("Architecture:") {
            if let Some(entry) = current.as_mut() {
                entry.architecture = value.trim().to_string();
            }
        } else if let Some(description) = line.strip_prefix("Description:") {
            if let Some(entry) = current.as_mut() {
                entry.description = description.trim().to_string();
                // En el formato antiguo no hay línea `Family:`.
                entry.family = os_family(&entry.id, &entry.description);
            }
        }
    }
    if let Some(entry) = current {
        result.push(entry);
    }
    result
}

/// Como [`parse_ostypes`], pero avisa si VirtualBox devolvió texto sin tipos
/// reconocibles (por ejemplo, porque cambió otra vez el formato de salida).
pub fn parse_ostypes_checked(out: &str) -> Result<Vec<VmOsType>> {
    let types = parse_ostypes(out);
    if types.is_empty() && !out.trim().is_empty() {
        return Err(CoreError::Message(OSTYPES_PARSE_ERROR.into()));
    }
    Ok(types)
}

/// `true` para tipos ARM (`ARMv8 (64-bit)`, `arm64`, …).
pub fn ostype_is_arm(entry: &VmOsType) -> bool {
    entry.architecture.to_ascii_lowercase().contains("arm")
}

/// Tipos que se ofrecen en el selector «Otro sistema…» (sin ARM).
pub fn ostypes_for_selection(types: Vec<VmOsType>) -> Vec<VmOsType> {
    types.into_iter().filter(|entry| !ostype_is_arm(entry)).collect()
}

/// Lista completa de tipos de sistema ofrecidos por la versión instalada.
/// La usan la validación y el catálogo: incluye también los tipos ARM.
pub async fn list_ostypes(tools: &Tools) -> Result<Vec<VmOsType>> {
    let bin = tools.vbox()?;
    let out = run_ok(bin, &["list", "ostypes"], SHORT)
        .await
        .map_err(|e| create_vm_error("consultar los tipos de sistema operativo", e))?;
    parse_ostypes_checked(&out.stdout)
}

/// Lista para el desplegable «Otro sistema…», sin los tipos incompatibles ARM.
pub async fn list_ostypes_for_selection(tools: &Tools) -> Result<Vec<VmOsType>> {
    Ok(ostypes_for_selection(list_ostypes(tools).await?))
}

/// Catálogo filtrado con los `ostype` que existen en esta instalación.
pub async fn available_vm_os_catalog(tools: &Tools) -> Result<Vec<VmOsEntry>> {
    let ids: Vec<String> = list_ostypes(tools).await?.into_iter().map(|o| o.id).collect();
    Ok(available_os_catalog(&ids))
}

/// Listado rápido: solo `list vms` + `list runningvms`, sin consultar Guest Additions.
///
/// Es la base del refresco periódico (`engine::snapshot`): dos procesos cortos por
/// ciclo y ninguna consulta por VM. Las Guest Additions se leen bajo demanda desde
/// [`inventory`], que es la que usa el comando `list_machines`.
pub async fn list_basic(tools: &Tools) -> Result<Vec<VmInfo>> {
    let bin = tools.vbox()?;
    let (all, running) = tokio::join!(
        run_ok(bin, &["list", "vms"], SHORT),
        run_ok(bin, &["list", "runningvms"], SHORT)
    );
    let running: HashSet<String> = parse_vm_list(&running?.stdout).into_iter().map(|(_, u)| u).collect();
    Ok(parse_vm_list(&all?.stdout)
        .into_iter()
        .map(|(name, uuid)| VmInfo {
            running: running.contains(&uuid),
            guest_additions: GuestAdditionsInfo { version: None, known: false },
            name,
            uuid,
        })
        .collect())
}

pub async fn inventory(tools: &Tools) -> Result<Vec<VmInfo>> {
    let mut vms = list_basic(tools).await?;

    // La propiedad persiste aunque la VM esté apagada. Si VirtualBox no puede
    // consultarla (VM bloqueada por la GUI, etc.) se marca como desconocida en vez
    // de afirmar que no hay Guest Additions. Esta consulta es costosa (un proceso
    // por VM), así que solo se hace bajo demanda, nunca en el refresco periódico.
    for vm in &mut vms {
        if let Ok(version) = guest_additions(tools, &vm.uuid).await {
            vm.guest_additions = GuestAdditionsInfo { version, known: true };
        }
    }
    Ok(vms)
}

/// Conjunto de UUIDs *y* nombres de las VMs en ejecución (los nodos pueden usar cualquiera).
///
/// No usa [`inventory`] para evitar una consulta de Guest Additions por VM.
pub async fn running_keys(tools: &Tools) -> Result<HashSet<String>> {
    let mut set = HashSet::new();
    for vm in list_basic(tools).await?.into_iter().filter(|v| v.running) {
        set.insert(vm.uuid);
        set.insert(vm.name);
    }
    Ok(set)
}

pub async fn action(tools: &Tools, target: &str, action: Action) -> Result<()> {
    validate::vbox_target(target)?;
    let bin = tools.vbox()?;
    let (args, limit): (Vec<&str>, Duration) = match action {
        Action::Start => (vec!["startvm", target, "--type", "headless"], LONG),
        Action::StartGui => (vec!["startvm", target, "--type", "gui"], LONG),
        Action::Stop => (vec!["controlvm", target, "acpipowerbutton"], SHORT),
        Action::ForceStop => (vec!["controlvm", target, "poweroff"], SHORT),
        Action::Restart => (vec!["controlvm", target, "reset"], SHORT),
        Action::Pause => (vec!["controlvm", target, "pause"], SHORT),
        Action::Resume => (vec!["controlvm", target, "resume"], SHORT),
    };
    run_ok(bin, &args, limit).await.map(|_| ())
}

/// Conecta (o desconecta con `None`) el adaptador 2 a una red interna. La VM debe estar apagada.
pub async fn set_network(tools: &Tools, target: &str, network: Option<&str>) -> Result<()> {
    let cfg = NodeNet { mode: if network.is_some() { NetMode::Intnet } else { NetMode::None }, ..Default::default() };
    set_nic(tools, target, &cfg, network).await
}

/// Nombre de la red interna del modo `Intnet`.
///
/// Si el nodo está en un segmento del lienzo, manda la red del segmento y se
/// valida como red gestionada (`vmlab-*`). Si está aislado y define un nombre
/// propio, es una red interna cualquiera de VirtualBox y se valida como interfaz.
fn internal_net_name<'a>(cfg: &'a NodeNet, intnet_fallback: Option<&'a str>) -> Result<Option<&'a str>> {
    if cfg.mode != NetMode::Intnet {
        return Ok(None);
    }
    match intnet_fallback {
        Some(name) => validate::network_name(name).map(Some),
        None => match cfg.name.as_deref().filter(|n| !n.is_empty()) {
            Some(name) => validate::host_iface(name).map(Some),
            None => Ok(None),
        },
    }
}

/// Configura el adaptador indicado por el panel derecho. La VM debe estar apagada.
///
/// `intnet_fallback` es la red interna del segmento del lienzo: se usa en el modo
/// `Intnet` cuando el nodo no define un nombre propio.
pub async fn set_nic(tools: &Tools, target: &str, cfg: &NodeNet, intnet_fallback: Option<&str>) -> Result<()> {
    validate::vbox_target(target)?;
    if !(1..=4).contains(&cfg.adapter) {
        return Err(crate::error::CoreError::Invalid("el adaptador debe estar entre 1 y 4".into()));
    }
    let bin = tools.vbox()?;
    let n = cfg.adapter.to_string();
    let nic = format!("--nic{n}");
    let cable = format!("--cableconnected{n}");
    let named = |suffix: &str| format!("--{suffix}{n}");

    // Prioridad original: si el nodo está conectado en el lienzo, manda la red del
    // segmento; el nombre propio solo se usa cuando está aislado (sin enlaces), tal
    // como explica el panel derecho.
    //
    // Se valida aquí, antes del reset de la NIC, para que un nombre inválido no deje
    // el adaptador desactivado.
    let intnet_name = internal_net_name(cfg, intnet_fallback)?;

    // Al cambiar de modo, VirtualBox conserva opciones del modo anterior
    // (`--intnetN`, `--hostonlyadapterN`, `--bridgeadapterN`…). Se desactiva la NIC
    // primero para partir de un estado limpio y después se aplica la configuración
    // nueva. Si el reset falla (VM encendida, etc.) no se toca nada más.
    if cfg.mode != NetMode::None {
        run_ok(bin, &["modifyvm", target, nic.as_str(), "none"], SHORT).await?;
    }

    let mut args: Vec<String> = vec!["modifyvm".into(), target.into()];

    match cfg.mode {
        NetMode::None => {
            args.extend([nic, "none".into()]);
        }
        NetMode::Nat => {
            args.extend([nic, "nat".into(), cable, "on".into()]);
        }
        NetMode::Intnet => match intnet_name {
            Some(name) => {
                args.extend([nic, "intnet".into(), named("intnet"), name.into(), cable, "on".into()]);
            }
            None => args.extend([nic, "none".into()]),
        },
        NetMode::Bridged => {
            let bridge = cfg.bridge.as_deref().or(cfg.name.as_deref()).ok_or_else(|| {
                crate::error::CoreError::Invalid("el modo puente necesita la interfaz del host".into())
            })?;
            validate::host_iface(bridge)?;
            args.extend([nic, "bridged".into(), named("bridgeadapter"), bridge.into(), cable, "on".into()]);
        }
        NetMode::Hostonly => {
            let name = cfg.name.as_deref().ok_or_else(|| {
                crate::error::CoreError::Invalid("el modo solo anfitrión necesita el nombre del adaptador".into())
            })?;
            validate::host_iface(name)?;
            args.extend([nic, "hostonly".into(), named("hostonlyadapter"), name.into(), cable, "on".into()]);
        }
        NetMode::Natnetwork => {
            let name = cfg.name.as_deref().ok_or_else(|| {
                crate::error::CoreError::Invalid("la red NAT necesita un nombre".into())
            })?;
            validate::host_iface(name)?;
            args.extend([nic, "natnetwork".into(), named("nat-network"), name.into(), cable, "on".into()]);
        }
    }

    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    run_ok(bin, &argv, SHORT).await.map(|_| ())
}

// ───────────────────── Límite de ancho de banda ─────────────────────

const BANDWIDTH_HINT: &str = "apaga la VM una vez para asignar el límite";

fn bandwidth_group(adapter: u8) -> String {
    format!("vmlab-nic{adapter}")
}

fn bandwidth_set_args(vm: &str, group: &str, limit: &str) -> Vec<String> {
    vec![
        "bandwidthctl".into(),
        vm.into(),
        "set".into(),
        group.into(),
        "--limit".into(),
        limit.into(),
    ]
}

fn bandwidth_add_args(vm: &str, group: &str, rate_kbit: u32) -> Vec<String> {
    vec![
        "bandwidthctl".into(),
        vm.into(),
        "add".into(),
        group.into(),
        "--type".into(),
        "network".into(),
        "--limit".into(),
        format!("{rate_kbit}k"),
    ]
}

fn bandwidth_remove_args(vm: &str, group: &str) -> Vec<String> {
    vec!["bandwidthctl".into(), vm.into(), "remove".into(), group.into()]
}

fn nic_group_args(vm: &str, adapter: u8, group: Option<&str>) -> Vec<String> {
    vec![
        "modifyvm".into(),
        vm.into(),
        format!("--nicbandwidthgroup{adapter}"),
        group.unwrap_or("none").into(),
    ]
}

fn show_vm_on(out: &str) -> bool {
    out.lines()
        .find_map(|line| line.trim().strip_prefix("VMState="))
        .map(|value| {
            let value = value.trim().trim_matches('"');
            value.eq_ignore_ascii_case("running") || value.eq_ignore_ascii_case("paused")
        })
        .unwrap_or(false)
}

fn group_is_assigned(out: &str, adapter: u8, group: &str) -> bool {
    let key = format!("nicbandwidthgroup{adapter}=");
    out.lines().any(|line| {
        line.trim()
            .strip_prefix(&key)
            .is_some_and(|value| value.trim().trim_matches('"') == group)
    })
}

fn list_has_group(out: &str, group: &str) -> bool {
    out.lines().any(|line| {
        line.split(|c: char| c == '=' || c == ':' || c.is_whitespace())
            .map(|token| token.trim().trim_matches('"'))
            .any(|token| token == group)
    })
}

async fn bandwidth_exists(bin: &Path, vm: &str, group: &str) -> bool {
    run(bin, &["bandwidthctl", vm, "list"], SHORT)
        .await
        .map(|out| out.code == 0 && list_has_group(&out.stdout, group))
        .unwrap_or(false)
}

async fn run_bandwidth(bin: &Path, args: Vec<String>) -> Result<()> {
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    run_ok(bin, &argv, SHORT).await.map(|_| ())
}

/// Aplica o retira el límite de ancho de banda del adaptador indicado.
///
/// El grupo `vmlab-nic<N>` se puede modificar con la VM encendida si ya estaba
/// asignado. Si no lo estaba, `modifyvm` exige apagarla y se devuelve un aviso.
pub async fn set_bandwidth(
    tools: &Tools,
    vm: &str,
    adapter: u8,
    rate_kbit: Option<u32>,
) -> Result<Option<String>> {
    validate::vbox_target(vm)?;
    if !(1..=4).contains(&adapter) {
        return Err(CoreError::Invalid("el adaptador debe estar entre 1 y 4".into()));
    }
    if let Some(rate) = rate_kbit {
        if !(8..=1_000_000).contains(&rate) {
            return Err(CoreError::Invalid(
                "la velocidad debe estar entre 8 y 1000000 kbit/s".into(),
            ));
        }
    }

    let bin = tools.vbox()?;
    let group = bandwidth_group(adapter);
    let info = run_ok(bin, &["showvminfo", vm, "--machinereadable"], SHORT).await?;
    let vm_on = show_vm_on(&info.stdout);
    let assigned = group_is_assigned(&info.stdout, adapter, &group);

    let Some(rate) = rate_kbit else {
        if vm_on {
            if assigned {
                run_bandwidth(bin, bandwidth_set_args(vm, &group, "0")).await?;
            }
            return Ok(None);
        }
        run_bandwidth(bin, nic_group_args(vm, adapter, None)).await?;
        if bandwidth_exists(bin, vm, &group).await {
            run_bandwidth(bin, bandwidth_remove_args(vm, &group)).await?;
        }
        return Ok(None);
    };

    if vm_on && !assigned {
        return Ok(Some(BANDWIDTH_HINT.into()));
    }

    if !bandwidth_exists(bin, vm, &group).await {
        run_bandwidth(bin, bandwidth_add_args(vm, &group, rate)).await?;
    }
    run_bandwidth(bin, bandwidth_set_args(vm, &group, &format!("{rate}k"))).await?;

    if !vm_on {
        run_bandwidth(bin, nic_group_args(vm, adapter, Some(&group))).await?;
    }
    Ok(None)
}

// ───────────────────── Guest Additions ─────────────────────

/// `guestproperty get` devuelve `Value: <versión>` o `No value set!` (exit 0).
/// La propiedad la escribe el servicio de Guest Additions y VirtualBox la conserva
/// en la configuración de la VM, así que se puede leer con la máquina apagada.
fn parse_guest_additions_version(out: &str) -> Option<String> {
    out.lines()
        .find_map(|l| l.trim().strip_prefix("Value:"))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
}

/// Consulta la versión de Guest Additions. `None` significa «no hay propiedad»,
/// es decir, no se han instalado (o nunca se han ejecutado dentro de la VM).
pub async fn guest_additions(tools: &Tools, target: &str) -> Result<Option<String>> {
    validate::vbox_target(target)?;
    let bin = tools.vbox()?;
    let out = run(
        bin,
        &["guestproperty", "get", target, "/VirtualBox/GuestAdd/Version"],
        SHORT,
    )
    .await?;
    if out.code == 0 {
        return Ok(parse_guest_additions_version(&out.stdout));
    }
    let msg = if out.stderr.trim().is_empty() { &out.stdout } else { &out.stderr };
    Err(CoreError::Failed {
        program: "VBoxManage".into(),
        code: out.code,
        stderr: msg.trim().chars().take(300).collect(),
    })
}

// ───────────────────── Sonda de comunicación ─────────────────────

/// Una interfaz de red reportada por Guest Additions.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GuestNetProp {
    index: u32,
    mac: Option<String>,
    ipv4: Option<String>,
    netmask: Option<String>,
}

fn normalize_mac(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_hexdigit()).collect::<String>().to_ascii_uppercase()
}

/// Separa `Nombre = 'valor' @ fecha` de `guestproperty enumerate`.
fn property_value(line: &str) -> Option<(&str, &str)> {
    let (name, rest) = line.split_once('=')?;
    let start = rest.find('\'')?;
    let after = &rest[start + 1..];
    let end = after.find('\'')?;
    Some((name.trim(), &after[..end]))
}

fn parse_guest_networks(out: &str) -> Vec<GuestNetProp> {
    let mut nets: Vec<GuestNetProp> = Vec::new();
    for line in out.lines() {
        let Some((name, value)) = property_value(line) else { continue };
        let Some(rest) = name.strip_prefix("/VirtualBox/GuestInfo/Net/") else { continue };
        let Some((index, field)) = rest.split_once('/') else { continue };
        let Ok(index) = index.parse::<u32>() else { continue };
        let slot = match nets.iter_mut().find(|n| n.index == index) {
            Some(slot) => slot,
            None => {
                nets.push(GuestNetProp { index, mac: None, ipv4: None, netmask: None });
                nets.last_mut().expect("recién insertado")
            }
        };
        match field.trim() {
            "MAC" => slot.mac = Some(normalize_mac(value)),
            "V4/IP" => {
                let (ip, prefix) = match value.split_once('/') {
                    Some((ip, prefix)) => (ip.trim(), prefix.trim().parse::<u8>().ok()),
                    None => (value.trim(), None),
                };
                if ip.parse::<std::net::Ipv4Addr>().is_ok() {
                    slot.ipv4 = Some(ip.to_string());
                }
                // Algunos guests publican la máscara solo como prefijo de la IP.
                if let Some(mask) = prefix.and_then(validate::netmask_from_prefix) {
                    slot.netmask.get_or_insert(mask);
                }
            }
            "V4/Netmask" => {
                if let Some(mask) = validate::netmask(value) {
                    slot.netmask = Some(mask);
                }
            }
            _ => {}
        }
    }
    nets
}

/// Describe el enlace del adaptador `adapter`: nombre de la red, IPv4 del guest y
/// el motivo cuando aún no hay comunicación. Se apoya en las guest properties que
/// publican las Guest Additions, así que no necesita usuario ni contraseña.
pub async fn guest_link(tools: &Tools, target: &str, adapter: u8) -> Result<LinkInfo> {
    validate::vbox_target(target)?;
    if !(1..=4).contains(&adapter) {
        return Err(CoreError::Invalid("el adaptador debe estar entre 1 y 4".into()));
    }
    let bin = tools.vbox()?;
    let info = run_ok(bin, &["showvminfo", target, "--machinereadable"], SHORT).await?;
    let running = vm_field(&info.stdout, "VMState")
        .is_some_and(|state| state.eq_ignore_ascii_case("running"));
    // Si no está en marcha no merece la pena consultar properties: no responderían.
    let props = if running {
        run_ok(bin, &["guestproperty", "enumerate", target], SHORT)
            .await
            .ok()
            .map(|out| out.stdout)
    } else {
        None
    };
    Ok(guest_link_from(adapter, &info.stdout, props.as_deref()))
}

/// Cálculo puro del enlace a partir de `showvminfo` y, si la VM está en marcha,
/// de `guestproperty enumerate`. `None` en `props` significa que no se pudieron
/// leer (sin Guest Additions, máquina parada o error de VirtualBox).
fn guest_link_from(adapter: u8, info: &str, props: Option<&str>) -> LinkInfo {
    let state = vm_field(info, "VMState").unwrap_or_default();
    if !state.eq_ignore_ascii_case("running") {
        return LinkInfo {
            segment: None,
            ipv4: None,
            netmask: None,
            issues: vec![NodeIssue::NotRunning],
            note: Some(if state.is_empty() {
                "no se pudo leer el estado de la máquina".into()
            } else {
                format!("la máquina no está en marcha ({state})")
            }),
        };
    }

    let mode = vm_field(info, &format!("nic{adapter}")).unwrap_or_else(|| "none".into());
    let off_segment = |text: String| LinkInfo {
        segment: None,
        ipv4: None,
        netmask: None,
        issues: vec![NodeIssue::AdapterNotOnSegment],
        note: Some(text),
    };

    if mode == "none" || mode == "null" {
        return off_segment(format!("adaptador {adapter} desconectado"));
    }
    if mode == "nat" {
        return off_segment("NAT aísla la VM: no comparte el segmento del lienzo".into());
    }
    if vm_field(info, &format!("cableconnected{adapter}")).as_deref() == Some("off") {
        return off_segment(format!("cable del adaptador {adapter} desconectado"));
    }

    let field = |suffix: &str| vm_field(info, &format!("{suffix}{adapter}"));
    let segment = match mode.as_str() {
        "intnet" => Some(format!("intnet:{}", field("intnet").unwrap_or_else(|| "default".into()))),
        "hostonly" => field("hostonlyadapter").map(|n| format!("hostonly:{n}")),
        "bridged" => field("bridgeadapter").map(|n| format!("bridged:{n}")),
        "natnetwork" => field("nat-network").map(|n| format!("natnetwork:{n}")),
        other => return off_segment(format!("modo de red no reconocido: {other}")),
    };
    let Some(segment) = segment else {
        return off_segment(format!("no se pudo leer la red del adaptador {adapter}"));
    };
    let Some(mac) = field("macaddress") else {
        return LinkInfo {
            segment: Some(segment),
            ipv4: None,
            netmask: None,
            issues: vec![NodeIssue::AdapterNotOnSegment],
            note: Some("el adaptador no tiene MAC".into()),
        };
    };

    let on_segment = mode == "intnet"
        && segment
            .strip_prefix("intnet:")
            .is_some_and(validate::managed_network);
    let Some(props) = props else {
        return LinkInfo {
            segment: Some(segment),
            ipv4: None,
            netmask: None,
            issues: vec![NodeIssue::NoGuestAdditions],
            note: Some(
                "sin respuesta de propiedades: ¿Guest Additions instaladas y en marcha?".into(),
            ),
        };
    };
    if !props.contains("/VirtualBox/GuestAdd/Version") {
        return LinkInfo {
            segment: Some(segment),
            ipv4: None,
            netmask: None,
            issues: vec![NodeIssue::NoGuestAdditions],
            note: Some("sin Guest Additions: no se puede leer la IP del guest".into()),
        };
    }

    let wanted = normalize_mac(&mac);
    let net = parse_guest_networks(props)
        .into_iter()
        .find(|n| n.mac.as_deref() == Some(wanted.as_str()));
    let ipv4 = net.as_ref().and_then(|n| n.ipv4.clone());
    let netmask = net.as_ref().and_then(|n| n.netmask.clone());
    let mut issues = Vec::new();
    if !on_segment {
        issues.push(NodeIssue::AdapterNotOnSegment);
    }
    if ipv4.is_none() {
        issues.push(NodeIssue::NoIpv4);
    }
    let note = match (ipv4.is_none(), on_segment) {
        (true, true) => Some(
            "sin IPv4 en ese adaptador (¿Guest Additions activas y DHCP/estática configurada?)"
                .into(),
        ),
        (true, false) => Some(format!(
            "el adaptador {adapter} no está en una red interna de VMLab y no tiene IPv4"
        )),
        (false, false) => {
            Some(format!("el adaptador {adapter} no está en una red interna de VMLab"))
        }
        (false, true) => None,
    };
    LinkInfo { segment: Some(segment), ipv4, netmask, issues, note }
}

/// Ruta de la ISO de Guest Additions que acompaña a la instalación de VirtualBox.
pub fn guest_additions_iso(tools: &Tools) -> Result<PathBuf> {
    let bin = tools.vbox()?;
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = bin.parent() {
        candidates.push(dir.join("VBoxGuestAdditions.iso"));
    }
    if let Some(pf) = std::env::var_os("ProgramFiles") {
        candidates.push(PathBuf::from(pf).join("Oracle").join("VirtualBox").join("VBoxGuestAdditions.iso"));
    }
    candidates.push("/usr/share/virtualbox/VBoxGuestAdditions.iso".into());
    candidates.push("/usr/lib/virtualbox/VBoxGuestAdditions.iso".into());
    candidates.push("/Applications/VirtualBox.app/Contents/MacOS/VBoxGuestAdditions.iso".into());
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .ok_or_else(|| CoreError::Invalid(
            "no se encontró VBoxGuestAdditions.iso. Reinstala VirtualBox o monta la ISO a mano desde Dispositivos > Unidades ópticas.".into(),
        ))
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct StorageController {
    name: String,
    kind: String,
    ports: u32,
    /// `storagecontrollermaxportcount<N>`; 30 si `showvminfo` no lo informa.
    max_ports: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StorageSlot {
    controller: String,
    port: u32,
    device: u32,
    medium: String,
}

/// Desescapa los valores de `--machinereadable` (`C:\\Users\\…` → `C:\Users\…`).
fn machinereadable_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            } else {
                out.push(c);
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn parse_kv(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    let (key, value) = line.split_once('=')?;
    let key = key.trim().trim_matches('"');
    if key.is_empty() {
        return None;
    }
    Some((
        key.to_string(),
        machinereadable_value(value.trim().trim_matches('"')),
    ))
}

fn parse_storage(out: &str) -> (Vec<StorageController>, Vec<StorageSlot>) {
    let pairs: Vec<(String, String)> = out.lines().filter_map(parse_kv).collect();
    let mut controllers: Vec<StorageController> = Vec::new();

    for (key, value) in &pairs {
        let Some(rest) = key.strip_prefix("storagecontroller") else { continue };
        let split = rest.find(|c: char| c.is_ascii_digit()).unwrap_or(rest.len());
        let (field, idx) = rest.split_at(split);
        let Ok(idx) = idx.parse::<usize>() else { continue };
        if controllers.len() <= idx {
            controllers.resize_with(idx + 1, StorageController::default);
        }
        match field {
            "name" => controllers[idx].name = value.clone(),
            "type" => controllers[idx].kind = value.clone(),
            "portcount" => {
                if let Ok(n) = value.parse::<u32>() {
                    controllers[idx].ports = n;
                }
            }
            "maxportcount" => {
                if let Ok(n) = value.parse::<u32>() {
                    controllers[idx].max_ports = n;
                    // Compatibilidad: si no hay `portcount`, se usaba el máximo.
                    if controllers[idx].ports == 0 {
                        controllers[idx].ports = n;
                    }
                }
            }
            _ => {}
        }
    }
    controllers.retain(|c| !c.name.is_empty());

    let mut slots = Vec::new();
    for (key, value) in &pairs {
        if key.starts_with("storagecontroller") {
            continue;
        }
        for c in &controllers {
            let prefix = format!("{}-", c.name);
            let Some(rest) = key.strip_prefix(&prefix) else { continue };
            let parts: Vec<&str> = rest.split('-').collect();
            if parts.len() == 2 {
                if let (Ok(port), Ok(device)) = (parts[0].parse(), parts[1].parse()) {
                    slots.push(StorageSlot {
                        controller: c.name.clone(),
                        port,
                        device,
                        medium: value.clone(),
                    });
                }
            }
            break;
        }
    }
    (controllers, slots)
}

fn empty_medium(medium: &str) -> bool {
    medium.is_empty() || medium.eq_ignore_ascii_case("none") || medium.eq_ignore_ascii_case("emptydrive")
}

fn same_path(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

fn controller_rank(kind: &str) -> u8 {
    match kind {
        // IDE: dos dispositivos por canal.
        "PIIX4" | "PIIX3" | "ICH6" => 0,
        "IntelAhci" => 1,
        // La disquetera no sirve como DVD.
        "I82078" => u8::MAX,
        _ => 2,
    }
}

/// Devuelve un hueco libre para una unidad óptica, prefiriendo IDE y SATA.
fn pick_free_slot(controllers: &[StorageController], slots: &[StorageSlot]) -> Option<StorageSlot> {
    let mut ordered: Vec<&StorageController> = controllers
        .iter()
        .filter(|c| controller_rank(&c.kind) != u8::MAX && c.ports > 0)
        .collect();
    ordered.sort_by_key(|c| (controller_rank(&c.kind), c.name.clone()));

    for c in ordered {
        let devices = if controller_rank(&c.kind) == 0 { 2 } else { 1 };
        for port in 0..c.ports {
            for device in 0..devices {
                let occupied = slots.iter().any(|s| {
                    s.controller == c.name
                        && s.port == port
                        && s.device == device
                        && !empty_medium(&s.medium)
                });
                if !occupied {
                    return Some(StorageSlot {
                        controller: c.name.clone(),
                        port,
                        device,
                        medium: String::new(),
                    });
                }
            }
        }
    }
    None
}

fn is_iso_attached(slots: &[StorageSlot], iso: &Path) -> bool {
    let iso = iso.to_string_lossy();
    slots.iter().any(|s| same_path(&s.medium, &iso))
}

/// Monta la ISO de Guest Additions en una unidad óptica libre. Devuelve `false`
/// si no hay unidad disponible o la VM no admite insertar la ISO en caliente.
async fn attach_guest_additions_iso(tools: &Tools, target: &str, iso: &Path) -> Result<bool> {
    let bin = tools.vbox()?;
    let info = run_ok(bin, &["showvminfo", target, "--machinereadable"], SHORT).await?;
    let (controllers, slots) = parse_storage(&info.stdout);
    if is_iso_attached(&slots, iso) {
        return Ok(true);
    }
    let Some(slot) = pick_free_slot(&controllers, &slots) else {
        return Ok(false);
    };
    let port = slot.port.to_string();
    let device = slot.device.to_string();
    let iso = iso.to_string_lossy();
    let args = [
        "storageattach",
        target,
        "--storagectl",
        slot.controller.as_str(),
        "--port",
        port.as_str(),
        "--device",
        device.as_str(),
        "--type",
        "dvddrive",
        "--medium",
        iso.as_ref(),
    ];
    run_ok(bin, &args, SHORT).await?;
    Ok(true)
}

fn vm_field(info: &str, field: &str) -> Option<String> {
    info.lines()
        .filter_map(parse_kv)
        .find(|(k, _)| k == field)
        .map(|(_, v)| v)
}

fn guest_os(ostype: &str) -> &'static str {
    let s = ostype.to_lowercase();
    if s.contains("windows") {
        return "windows";
    }
    const LINUX: &[&str] = &[
        "linux", "ubuntu", "debian", "fedora", "red hat", "redhat", "centos", "oracle", "suse",
        "opensuse", "arch", "gentoo", "mageia", "mandriva", "turbolinux", "xandros", "alma",
        "rocky", "kali", "mint",
    ];
    if LINUX.iter().any(|k| s.contains(k)) {
        return "linux";
    }
    "other"
}

/// Prepara la instalación de Guest Additions sin poder ejecutar nada dentro del
/// guest (VirtualBox solo ofrece `guestcontrol` cuando ya están instaladas).
///
/// Monta la ISO en una unidad óptica y, si la VM está apagada, la abre con ventana
/// para que el usuario ejecute el instalador. Devuelve el sistema y las advertencias.
pub async fn prepare_guest_additions(tools: &Tools, target: &str) -> Result<GuestAdditionsPrep> {
    validate::vbox_target(target)?;
    let bin = tools.vbox()?;
    let iso = guest_additions_iso(tools)?;
    let info = run_ok(bin, &["showvminfo", target, "--machinereadable"], SHORT).await?;
    let state = vm_field(&info.stdout, "VMState").unwrap_or_else(|| "poweroff".into());
    let ostype = vm_field(&info.stdout, "ostype").unwrap_or_default();
    let os = guest_os(&ostype).to_string();
    let mut warnings = Vec::new();

    let attached = match attach_guest_additions_iso(tools, target, &iso).await {
        Ok(v) => v,
        Err(e) => {
            warnings.push(format!("No se pudo montar la ISO automáticamente: {e}"));
            false
        }
    };

    let already_running = !matches!(state.as_str(), "poweroff" | "saved" | "aborted");
    let mut started = false;
    if already_running {
        if !attached {
            warnings.push(
                "La VM ya está en marcha y no se pudo insertar la ISO en caliente. Usa Dispositivos > Unidades ópticas > Insertar imagen de CD/DVD."
                    .into(),
            );
        }
    } else {
        match run_ok(bin, &["startvm", target, "--type", "gui"], LONG).await {
            Ok(_) => started = true,
            Err(e) => warnings.push(format!("No se pudo abrir la VM: {e}")),
        }
    }

    if !attached && !warnings.iter().any(|w| w.contains("Unidades ópticas")) {
        warnings.push(
            "No hay una unidad óptica libre. En la VM, usa Dispositivos > Unidades ópticas > Insertar imagen de CD/DVD."
                .into(),
        );
    }

    Ok(GuestAdditionsPrep {
        iso: iso.to_string_lossy().into_owned(),
        attached,
        started,
        already_running,
        os,
        warnings,
    })
}

// ───────────────────── Unidades ópticas ─────────────────────

/// Prefijo de las claves de extradata con las que VMLab recuerda qué unidades
/// ópticas ha creado. Nunca se usa una heurística sobre el medio: una ISO que el
/// usuario montó a mano jamás debe ser tocada ni borrada.
pub const VMLAB_OPTICAL_EXTRADATA: &str = "VMLab/Optical";
/// Controlador IDE que se crea solo si la VM no tiene IDE y no hay SATA libre.
pub const VMLAB_IDE_CONTROLLER: &str = "VMLab IDE";
/// Medio de una unidad vacía en la sintaxis de `storageattach`.
pub const OPTICAL_EMPTY_MEDIUM: &str = "emptydrive";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpticalSlot {
    pub controller: String,
    pub port: u32,
    pub device: u32,
}

impl OpticalSlot {
    /// Valor guardado en `VBoxManage setextradata`: `controlador|puerto|dispositivo`.
    pub fn location(&self) -> Result<String> {
        validate::host_iface(&self.controller)?;
        Ok(format!("{}|{}|{}", self.controller, self.port, self.device))
    }
}

/// Lee el valor de `VBoxManage getextradata`. `None` cuando no hay valor.
pub fn parse_extradata_value(out: &str) -> Option<String> {
    for line in out.lines() {
        let line = line.trim();
        let Some(value) = line.strip_prefix("Value:") else { continue };
        let value = value.trim();
        if value.is_empty() || value.starts_with('<') || value.eq_ignore_ascii_case("null") {
            return None;
        }
        return Some(value.to_string());
    }
    None
}

/// Lee la salida de `VBoxManage getextradata <vm> enumerate` como pares clave/valor.
pub fn parse_extradata_enumeration(out: &str) -> Vec<(String, Option<String>)> {
    out.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("Key:")?;
            let (key, value) = rest.split_once(',')?;
            let key = key.trim();
            if key.is_empty() {
                return None;
            }
            let value = value.trim().strip_prefix("Value:").and_then(|v| {
                let v = v.trim();
                (!v.is_empty() && !v.starts_with('<')).then(|| v.to_string())
            });
            Some((key.to_string(), value))
        })
        .collect()
}

/// Parsea `controlador|puerto|dispositivo` con los mismos límites que los argv.
pub fn parse_optical_location(value: &str) -> Option<OpticalSlot> {
    let mut parts = value.split('|');
    let controller = parts.next()?.trim();
    let port = parts.next()?.trim().parse::<u32>().ok()?;
    let device = parts.next()?.trim().parse::<u32>().ok()?;
    if parts.next().is_some()
        || controller.is_empty()
        || controller.starts_with('-')
        || controller.chars().count() > 128
        || controller.chars().any(|c| c.is_control())
        || port > MAX_SATA_PORTS
        || device > 1
    {
        return None;
    }
    Some(OpticalSlot { controller: controller.to_string(), port, device })
}

pub fn optical_extradata_key(node_id: &str) -> Result<String> {
    validate::optical_node_id(node_id)?;
    Ok(format!("{VMLAB_OPTICAL_EXTRADATA}/{node_id}"))
}

fn extradata_key(key: &str) -> Result<&str> {
    let ok = !key.is_empty()
        && key.chars().count() <= 160
        && !key.starts_with('-')
        && !key.chars().any(|c| c.is_control());
    if ok { Ok(key) } else { Err(CoreError::Invalid("clave de extradata no válida".into())) }
}

pub fn setextradata_args(vm: &str, key: &str, value: &str) -> Result<Vec<String>> {
    validate::vbox_target(vm)?;
    extradata_key(key)?;
    if value.starts_with('-') || value.chars().any(|c| c.is_control()) {
        return Err(CoreError::Invalid("valor de extradata no válido".into()));
    }
    Ok(vec!["setextradata".into(), vm.into(), key.into(), value.into()])
}

pub fn getextradata_args(vm: &str, key: &str) -> Result<Vec<String>> {
    validate::vbox_target(vm)?;
    extradata_key(key)?;
    Ok(vec!["getextradata".into(), vm.into(), key.into()])
}

pub fn enumerate_extradata_args(vm: &str) -> Result<Vec<String>> {
    validate::vbox_target(vm)?;
    Ok(vec!["getextradata".into(), vm.into(), "enumerate".into()])
}

/// Medio que se quiere dejar en la unidad. La ISO se valida como en la creación
/// de VMs: nunca se copia, nunca se borra y nunca empieza por `-`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpticalMedium<'a> {
    Iso(&'a str),
    Empty,
    None,
}

pub fn storageattach_optical_args(
    vm: &str,
    slot: &OpticalSlot,
    medium: OpticalMedium<'_>,
    force_unmount: bool,
) -> Result<Vec<String>> {
    validate::vbox_target(vm)?;
    validate::host_iface(&slot.controller)?;
    let value = match medium {
        OpticalMedium::Iso(iso) => validate::vm_iso(iso)?.to_string(),
        OpticalMedium::Empty => OPTICAL_EMPTY_MEDIUM.to_string(),
        OpticalMedium::None => "none".into(),
    };
    let mut args = vec![
        "storageattach".into(),
        vm.into(),
        "--storagectl".into(),
        slot.controller.clone(),
        "--port".into(),
        slot.port.to_string(),
        "--device".into(),
        slot.device.to_string(),
        "--type".into(),
        "dvddrive".into(),
        "--medium".into(),
        value,
    ];
    if force_unmount {
        args.push("--forceunmount".into());
    }
    Ok(args)
}

/// Un hueco solo está libre si `showvminfo` no muestra ningún dispositivo allí.
/// `emptydrive` significa que ya hay una unidad óptica del usuario: no se toca.
fn optical_slot_free(storage: &VmStorageInfo, controller: &str, port: u32, device: u32) -> bool {
    !storage.attachments.iter().any(|a| {
        a.controller == controller
            && a.port == port
            && a.device == device
            && !a.medium.is_empty()
            && !a.medium.eq_ignore_ascii_case("none")
    })
}

/// Busca un hueco real en los IDE (`2 × 2`) o en los SATA (`1 × puertos`).
fn pick_optical_slot(storage: &VmStorageInfo, ide: bool) -> Option<OpticalSlot> {
    let mut controllers: Vec<&VmStorageController> = storage
        .controllers
        .iter()
        .filter(|c| {
            let suitable = if ide { controller_rank(&c.kind) == 0 } else { is_sata(&c.kind) };
            suitable && c.ports > 0 && !c.name.starts_with('-')
        })
        .collect();
    controllers.sort_by_key(|c| c.name.clone());
    for c in controllers {
        let devices = if ide { 2 } else { 1 };
        for port in 0..c.ports.min(MAX_SATA_PORTS) {
            for device in 0..devices {
                if optical_slot_free(storage, &c.name, port, device) {
                    return Some(OpticalSlot { controller: c.name.clone(), port, device });
                }
            }
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpticalPrepare {
    None,
    /// Ampliar el SATA existente como se hace con los discos VDI.
    GrowSata { controller: String, ports: u32 },
    /// Crear `VMLab IDE`: solo cuando la VM no tiene ningún IDE.
    CreateIde,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpticalPlacement {
    pub slot: OpticalSlot,
    pub prepare: OpticalPrepare,
}

/// Orden de preferencia: IDE libre → SATA libre → ampliar SATA → crear IDE.
/// Si el IDE existe y está lleno y no hay SATA, devuelve un error claro.
pub fn plan_optical_slot(storage: &VmStorageInfo) -> Result<OpticalPlacement> {
    if let Some(slot) = pick_optical_slot(storage, true) {
        return Ok(OpticalPlacement { slot, prepare: OpticalPrepare::None });
    }
    if let Some(slot) = pick_optical_slot(storage, false) {
        return Ok(OpticalPlacement { slot, prepare: OpticalPrepare::None });
    }
    if let Some((controller, ports)) = grow_sata_controller(storage) {
        let slot = OpticalSlot { controller: controller.clone(), port: ports - 1, device: 0 };
        return Ok(OpticalPlacement {
            slot,
            prepare: OpticalPrepare::GrowSata { controller, ports },
        });
    }
    if !storage.controllers.iter().any(|c| controller_rank(&c.kind) == 0) {
        let slot = OpticalSlot { controller: VMLAB_IDE_CONTROLLER.into(), port: 0, device: 0 };
        return Ok(OpticalPlacement { slot, prepare: OpticalPrepare::CreateIde });
    }
    Err(CoreError::Message(
        "no queda ningún hueco libre: el controlador IDE está lleno y SATA no admite más puertos".into(),
    ))
}

/// ¿El medio montado en ese hueco coincide ya con lo que pide el lienzo?
pub fn optical_medium_is(storage: &VmStorageInfo, slot: &OpticalSlot, iso: Option<&str>) -> bool {
    let Some(a) = storage
        .attachments
        .iter()
        .find(|a| a.controller == slot.controller && a.port == slot.port && a.device == slot.device)
    else {
        return false;
    };
    match iso {
        Some(iso) => !empty_medium(&a.medium) && same_path(&a.medium, iso),
        None => empty_medium(&a.medium),
    }
}

/// Mismo aviso que para los discos, con «unidad óptica» en el texto.
pub fn optical_state_warning(vm: &str, state: &str, action: &str) -> Option<String> {
    if storage_editable(state) {
        return None;
    }
    let state = state.trim();
    if state.eq_ignore_ascii_case("saved") {
        return Some(format!(
            "«{vm}» tiene un estado guardado. Inicia la máquina y apágala del todo, o descarta el estado guardado desde VirtualBox, y vuelve a aplicar."
        ));
    }
    if state.eq_ignore_ascii_case("running") || state.eq_ignore_ascii_case("paused") {
        return Some(format!("apaga «{vm}» para {action} la unidad óptica"));
    }
    Some(format!(
        "«{vm}» está en estado «{state}»: apágala del todo para cambiar sus unidades ópticas."
    ))
}

pub async fn vm_optical_storage(tools: &Tools, vm: &str) -> Result<VmStorageInfo> {
    validate::vbox_target(vm)?;
    let bin = tools.vbox()?;
    let info = run_ok(bin, &["showvminfo", vm, "--machinereadable"], SHORT)
        .await
        .map_err(|e| vdi_command_error("leer la información de la VM", e))?;
    Ok(parse_vm_storage(&info.stdout))
}

pub async fn read_optical_extradata(
    tools: &Tools,
    vm: &str,
    node_id: &str,
) -> Result<Option<OpticalSlot>> {
    let key = optical_extradata_key(node_id)?;
    let bin = tools.vbox()?;
    let args = getextradata_args(vm, &key)?;
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run_ok(bin, &argv, SHORT)
        .await
        .map_err(|e| vdi_command_error("leer la ubicación de la unidad óptica", e))?;
    Ok(parse_extradata_value(&out.stdout).and_then(|v| parse_optical_location(&v)))
}

/// Todas las unidades que VMLab ha creado en una VM, con el `id` del nodo.
pub async fn list_optical_extradata(tools: &Tools, vm: &str) -> Result<Vec<(String, OpticalSlot)>> {
    validate::vbox_target(vm)?;
    let bin = tools.vbox()?;
    let args = enumerate_extradata_args(vm)?;
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run_ok(bin, &argv, SHORT)
        .await
        .map_err(|e| vdi_command_error("listar las unidades ópticas gestionadas", e))?;
    let prefix = format!("{VMLAB_OPTICAL_EXTRADATA}/");
    Ok(parse_extradata_enumeration(&out.stdout)
        .into_iter()
        .filter_map(|(key, value)| {
            let id = key.strip_prefix(&prefix)?;
            if validate::optical_node_id(id).is_err() {
                return None;
            }
            let slot = value.and_then(|v| parse_optical_location(&v))?;
            Some((id.to_string(), slot))
        })
        .collect())
}

pub async fn set_optical_extradata(
    tools: &Tools,
    vm: &str,
    node_id: &str,
    slot: &OpticalSlot,
) -> Result<()> {
    let key = optical_extradata_key(node_id)?;
    let value = slot.location()?;
    let bin = tools.vbox()?;
    let args = setextradata_args(vm, &key, &value)?;
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    run_ok(bin, &argv, SHORT)
        .await
        .map_err(|e| vdi_command_error("guardar la ubicación de la unidad óptica", e))?;
    Ok(())
}

pub async fn clear_optical_extradata(tools: &Tools, vm: &str, node_id: &str) -> Result<()> {
    let key = optical_extradata_key(node_id)?;
    let bin = tools.vbox()?;
    // Un valor vacío borra la clave en VirtualBox.
    let args = setextradata_args(vm, &key, "")?;
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    run_ok(bin, &argv, SHORT)
        .await
        .map_err(|e| vdi_command_error("borrar la ubicación de la unidad óptica", e))?;
    Ok(())
}

fn optical_locked(e: &CoreError) -> bool {
    let text = e.to_string().to_ascii_lowercase();
    text.contains("locked")
        || text.contains("bloquead")
        || text.contains("in use")
        || text.contains("en uso")
        || text.contains("unmount")
        || text.contains("desmont")
}

fn optical_medium_error(medium: OpticalMedium<'_>, e: CoreError) -> CoreError {
    let action = match medium {
        OpticalMedium::Iso(_) => "insertar la ISO",
        OpticalMedium::Empty => "expulsar la ISO",
        OpticalMedium::None => "quitar la unidad óptica",
    };
    vdi_command_error(action, e)
}

/// Cambia el medio en caliente (VM encendida incluida). `--forceunmount` se usa
/// solo cuando se expulsa o se retira y el invitado tiene el medio bloqueado.
pub async fn set_optical_medium(
    tools: &Tools,
    vm: &str,
    slot: &OpticalSlot,
    medium: OpticalMedium<'_>,
    allow_force_unmount: bool,
) -> Result<()> {
    validate::vbox_target(vm)?;
    let bin = tools.vbox()?;
    let args = storageattach_optical_args(vm, slot, medium, false)?;
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    if let Err(e) = run_ok(bin, &argv, SHORT).await {
        let can_force = allow_force_unmount
            && matches!(medium, OpticalMedium::Empty | OpticalMedium::None)
            && optical_locked(&e);
        if !can_force {
            return Err(optical_medium_error(medium, e));
        }
        let retry = storageattach_optical_args(vm, slot, medium, true)?;
        let retry_argv: Vec<&str> = retry.iter().map(String::as_str).collect();
        run_ok(bin, &retry_argv, SHORT)
            .await
            .map_err(|e| optical_medium_error(medium, e))?;
    }
    Ok(())
}

async fn prepare_optical_slot(tools: &Tools, vm: &str, placement: &OpticalPlacement) -> Result<()> {
    let bin = tools.vbox()?;
    let args = match &placement.prepare {
        OpticalPrepare::None => return Ok(()),
        OpticalPrepare::GrowSata { controller, ports } => {
            storagectl_portcount_args(vm, controller, *ports)?
        }
        OpticalPrepare::CreateIde => storagectl_create_args(vm, VMLAB_IDE_CONTROLLER, "ide", None)?,
    };
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    run_ok(bin, &argv, SHORT)
        .await
        .map_err(|e| vdi_storagectl_error("preparar la unidad óptica", e))?;
    Ok(())
}

/// Crea la unidad, monta la ISO (o la deja vacía) y recuerda su ubicación.
pub async fn create_optical_drive(
    tools: &Tools,
    vm: &str,
    node_id: &str,
    placement: &OpticalPlacement,
    iso: Option<&str>,
) -> Result<()> {
    validate::vbox_target(vm)?;
    validate::optical_node_id(node_id)?;
    prepare_optical_slot(tools, vm, placement).await?;
    let medium = iso.map_or(OpticalMedium::Empty, OpticalMedium::Iso);
    set_optical_medium(tools, vm, &placement.slot, medium, false).await?;
    set_optical_extradata(tools, vm, node_id, &placement.slot).await
}

/// Quita solo una unidad creada por VMLab y borra su extradata. La ISO del
/// usuario nunca se borra: `--medium none` solo desconecta el medio.
pub async fn remove_optical_drive(
    tools: &Tools,
    vm: &str,
    node_id: &str,
    slot: &OpticalSlot,
) -> Result<()> {
    validate::vbox_target(vm)?;
    validate::optical_node_id(node_id)?;
    set_optical_medium(tools, vm, slot, OpticalMedium::None, true).await?;
    clear_optical_extradata(tools, vm, node_id).await
}

// ───────────────────── Discos VDI ─────────────────────

/// Controlador creado por VMLab cuando la VM no tiene ninguno compatible libre.
pub const VMLAB_VDI_CONTROLLER: &str = "VMLab SATA";
/// Tope de puertos SATA que admite VirtualBox (y de `storagectl --portcount`).
pub const MAX_SATA_PORTS: u32 = 30;

/// Controladores de `showvminfo` admitidos para discos duros. Nunca IDE: solo
/// SATA, SCSI, NVMe y VirtIO. El orden es la preferencia al elegir un hueco.
fn vdi_controller_rank(kind: &str) -> Option<u8> {
    match kind.trim().to_ascii_lowercase().as_str() {
        "intelahci" | "ahci" | "sata" => Some(0),
        "nvme" => Some(1),
        "virtio" | "virtio-scsi" | "virtio scsi" => Some(2),
        "lsilogic" | "lsilogicsas" | "buslogic" | "scsi" => Some(3),
        _ => None,
    }
}

/// Vista pública y pura de `showvminfo --machinereadable` para almacenamiento.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmStorageController {
    pub name: String,
    pub kind: String,
    pub ports: u32,
    /// Máximo de puertos del controlador según `showvminfo` (30 por defecto).
    pub max_ports: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmStorageAttachment {
    pub controller: String,
    pub port: u32,
    pub device: u32,
    pub medium: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VmStorageInfo {
    pub running: bool,
    /// Estado exacto de `VMState` (p. ej. `poweroff`, `saved`, `running`).
    pub state: String,
    pub controllers: Vec<VmStorageController>,
    pub attachments: Vec<VmStorageAttachment>,
}

/// Parsea controladores, puertos ocupados y estado de la VM sin ejecutar nada.
pub fn parse_vm_storage(info: &str) -> VmStorageInfo {
    let (controllers, attachments) = parse_storage(info);
    VmStorageInfo {
        running: show_vm_on(info),
        state: vm_field(info, "VMState").unwrap_or_default(),
        controllers: controllers
            .into_iter()
            .map(|c| VmStorageController {
                name: c.name,
                kind: c.kind,
                ports: c.ports,
                max_ports: if c.max_ports == 0 { MAX_SATA_PORTS } else { c.max_ports },
            })
            .collect(),
        attachments: attachments
            .into_iter()
            .map(|s| VmStorageAttachment {
                controller: s.controller,
                port: s.port,
                device: s.device,
                medium: s.medium,
            })
            .collect(),
    }
}

/// VirtualBox solo permite cambiar el almacenamiento en `poweroff` y `aborted`.
/// Cualquier otro estado (incluido `saved`) bloquea `storagectl`/`storageattach`.
pub fn storage_editable(state: &str) -> bool {
    let state = state.trim();
    state.eq_ignore_ascii_case("poweroff") || state.eq_ignore_ascii_case("aborted")
}

/// Aviso cuando el estado de la VM impide tocar el almacenamiento. `None` si se
/// puede modificar. `action` es `conectar` o `desconectar`.
fn storage_state_warning(vm: &str, state: &str, action: &str) -> Option<String> {
    if storage_editable(state) {
        return None;
    }
    let state = state.trim();
    if state.eq_ignore_ascii_case("saved") {
        return Some(format!(
            "«{vm}» tiene un estado guardado. Inicia la máquina y apágala del todo, o descarta el estado guardado desde VirtualBox, y vuelve a aplicar."
        ));
    }
    if state.eq_ignore_ascii_case("running") || state.eq_ignore_ascii_case("paused") {
        return Some(format!("apaga «{vm}» para {action} el disco"));
    }
    Some(format!(
        "«{vm}» está en estado «{state}»: apágala del todo para cambiar sus discos."
    ))
}

/// Los tipos que VirtualBox considera SATA (IntelAhci/AHCI).
fn is_sata(kind: &str) -> bool {
    vdi_controller_rank(kind) == Some(0)
}

/// ¿Hay ya un controlador SATA en la VM? VirtualBox solo admite uno.
fn has_sata_controller(storage: &VmStorageInfo) -> bool {
    storage.controllers.iter().any(|c| is_sata(&c.kind))
}

/// Plan cuando no queda ningún hueco en los controladores compatibles.
#[derive(Debug, PartialEq, Eq)]
enum SataPlan {
    /// Ampliar el controlador SATA real: `(nombre, nuevos puertos)`.
    Grow(String, u32),
    /// No hay SATA: crear `VMLab SATA`.
    Create,
}

/// Decide cómo obtener un puerto SATA cuando `pick_vdi_free_slot` no encuentra
/// ninguno. Error si hay SATA pero está en su tope: no se puede crear otro.
fn sata_plan(storage: &VmStorageInfo) -> Result<SataPlan> {
    if let Some((name, ports)) = grow_sata_controller(storage) {
        Ok(SataPlan::Grow(name, ports))
    } else if has_sata_controller(storage) {
        Err(CoreError::Message("la VM no tiene puertos SATA libres".into()))
    } else {
        Ok(SataPlan::Create)
    }
}

/// Plan para ampliar el SATA existente: `(nombre real, nuevos puertos)`.
/// `None` si no hay SATA o si ya está en su tope (`maxportcount`, máximo 30).
pub fn grow_sata_controller(storage: &VmStorageInfo) -> Option<(String, u32)> {
    let mut controllers: Vec<&VmStorageController> = storage
        .controllers
        .iter()
        .filter(|c| is_sata(&c.kind))
        .collect();
    controllers.sort_by_key(|c| c.name.clone());
    for c in controllers {
        let cap = c.max_ports.min(MAX_SATA_PORTS);
        if c.ports > 0 && c.ports < cap {
            return Some((c.name.clone(), c.ports + 1));
        }
    }
    None
}

/// Primer puerto libre (`device = 0`) de un controlador compatible.
fn pick_vdi_free_slot(storage: &VmStorageInfo) -> Option<VmStorageAttachment> {
    let mut controllers: Vec<&VmStorageController> = storage
        .controllers
        .iter()
        .filter(|c| vdi_controller_rank(&c.kind).is_some() && c.ports > 0 && !c.name.starts_with('-'))
        .collect();
    controllers.sort_by_key(|c| (vdi_controller_rank(&c.kind).unwrap_or(u8::MAX), c.name.clone()));

    for c in controllers {
        for port in 0..c.ports {
            let occupied = storage.attachments.iter().any(|s| {
                s.controller == c.name
                    && s.port == port
                    && s.device == 0
                    && !empty_medium(&s.medium)
            });
            if !occupied {
                return Some(VmStorageAttachment {
                    controller: c.name.clone(),
                    port,
                    device: 0,
                    medium: String::new(),
                });
            }
        }
    }
    None
}

fn vdi_is_attached(storage: &VmStorageInfo, path: &Path) -> bool {
    let path = path.to_string_lossy();
    storage
        .attachments
        .iter()
        .any(|s| !empty_medium(&s.medium) && same_path(&s.medium, path.as_ref()))
}

/// Arg separado y seguro: nunca empieza por `-` ni lleva caracteres de control.
fn vdi_path_arg(path: &Path) -> Result<String> {
    let value = path.to_string_lossy().into_owned();
    if value.is_empty()
        || value.starts_with('-')
        || value.chars().any(|c| c.is_control())
    {
        return Err(CoreError::Invalid("ruta de disco VDI no válida".into()));
    }
    Ok(value)
}

pub fn createmedium_args(path: &Path, size_mb: u32) -> Result<Vec<String>> {
    if !(1..=validate::MAX_VDI_SIZE_MB).contains(&size_mb) {
        return Err(CoreError::Invalid("el tamaño del disco VDI no está en el rango permitido".into()));
    }
    Ok(vec![
        "createmedium".into(),
        "disk".into(),
        "--filename".into(),
        vdi_path_arg(path)?,
        "--size".into(),
        size_mb.to_string(),
        "--format".into(),
        "VDI".into(),
        "--variant".into(),
        "Standard".into(),
    ])
}

/// Construye `closemedium disk <vdi> --delete` para borrar el VDI creado por
/// una VM que se está deshaciendo. Solo acepta ficheros `.vdi` dentro de la
/// carpeta de esa VM (la que se lee de `CfgFile`).
pub fn closemedium_delete_args(path: &Path, vm_dir: &Path) -> Result<Vec<String>> {
    let value = vdi_path_arg(path)?;
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    if file_name.starts_with('-') {
        return Err(CoreError::Invalid("la ruta del disco VDI no es válida".into()));
    }
    let extension = path.extension().and_then(|e| e.to_str());
    if !extension.is_some_and(|e| e.eq_ignore_ascii_case("vdi")) {
        return Err(CoreError::Invalid("el disco debe tener extensión .vdi".into()));
    }
    if !path.starts_with(vm_dir) {
        return Err(CoreError::Invalid("el disco está fuera de la carpeta de la VM".into()));
    }
    Ok(vec!["closemedium".into(), "disk".into(), value, "--delete".into()])
}

pub fn showmediuminfo_args(path: &Path) -> Result<Vec<String>> {
    Ok(vec!["showmediuminfo".into(), "disk".into(), vdi_path_arg(path)?])
}

/// Argumentos de `VBoxManage modifymedium disk <ruta> --resize <MB>`.
/// Solo se construyen con un tamaño dentro del rango permitido y una ruta que
/// no pueda interpretarse como opción.
pub fn modifymedium_resize_args(path: &Path, size_mb: u32) -> Result<Vec<String>> {
    if !(1..=validate::MAX_VDI_SIZE_MB).contains(&size_mb) {
        return Err(CoreError::Invalid("el tamaño del disco VDI no está en el rango permitido".into()));
    }
    Ok(vec![
        "modifymedium".into(),
        "disk".into(),
        vdi_path_arg(path)?,
        "--resize".into(),
        size_mb.to_string(),
    ])
}

pub fn storagectl_args(vm: &str) -> Result<Vec<String>> {
    validate::vbox_target(vm)?;
    validate::host_iface(VMLAB_VDI_CONTROLLER)?;
    Ok(vec![
        "storagectl".into(),
        vm.into(),
        "--name".into(),
        VMLAB_VDI_CONTROLLER.into(),
        "--add".into(),
        "sata".into(),
        "--controller".into(),
        "IntelAhci".into(),
        "--portcount".into(),
        "8".into(),
    ])
}

/// Construye `storagectl <vm> --name <controlador real> --portcount <puertos>`
/// para ampliar un controlador SATA existente (VirtualBox solo admite uno).
pub fn storagectl_portcount_args(vm: &str, controller: &str, ports: u32) -> Result<Vec<String>> {
    validate::vbox_target(vm)?;
    validate::host_iface(controller)?;
    if !(1..=MAX_SATA_PORTS).contains(&ports) {
        return Err(CoreError::Invalid(format!(
            "el número de puertos SATA debe estar entre 1 y {MAX_SATA_PORTS}"
        )));
    }
    Ok(vec![
        "storagectl".into(),
        vm.into(),
        "--name".into(),
        controller.into(),
        "--portcount".into(),
        ports.to_string(),
    ])
}

/// Construye `storageattach`. `medium = None` desconecta sin borrar el fichero.
pub fn storageattach_args(
    vm: &str,
    controller: &str,
    port: u32,
    device: u32,
    medium: Option<&Path>,
) -> Result<Vec<String>> {
    validate::vbox_target(vm)?;
    validate::host_iface(controller)?;
    let medium = match medium {
        Some(path) => vdi_path_arg(path)?,
        None => "none".to_string(),
    };
    Ok(vec![
        "storageattach".into(),
        vm.into(),
        "--storagectl".into(),
        controller.into(),
        "--port".into(),
        port.to_string(),
        "--device".into(),
        device.to_string(),
        "--type".into(),
        "hdd".into(),
        "--medium".into(),
        medium,
    ])
}

// ───────────────────── Creación de VMs ─────────────────────

/// Lee UUID y fichero de configuración de `showvminfo --machinereadable`.
pub fn parse_created_vm_info(info: &str) -> Option<(String, String)> {
    let uuid = vm_field(info, "UUID")?;
    let cfg = vm_field(info, "CfgFile")?;
    if uuid.is_empty() || cfg.is_empty() {
        return None;
    }
    Some((uuid, cfg))
}

/// Comprueba que el tipo que VirtualBox dice haber creado es el pedido.
///
/// Campo usado: `OSTypeId` (ID estable) cuando la versión lo expone; si no,
/// `ostype`, que en 7.2 es un valor legible (`Ubuntu (64-bit)`) y se compara
/// con la descripción del tipo pedido. También se acepta que `ostype` traiga
/// directamente el ID, como hacen versiones anteriores.
pub fn created_ostype_matches(requested: &VmOsType, info: &str) -> bool {
    let same = |a: &str, b: &str| !a.trim().is_empty() && a.trim().eq_ignore_ascii_case(b.trim());
    if let Some(id) = vm_field_ci(info, "OSTypeId") {
        return same(&id, &requested.id);
    }
    match vm_field_ci(info, "ostype") {
        Some(value) => same(&value, &requested.id) || same(&value, &requested.description),
        None => false,
    }
}

pub fn createvm_args(name: &str, ostype: &str) -> Result<Vec<String>> {
    validate::vbox_target(name)?;
    let valid_ostype = !ostype.is_empty()
        && ostype.chars().count() <= 128
        && !ostype.starts_with('-')
        && ostype.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+' | '!'))
        && !ostype.chars().any(|c| c.is_control());
    if !valid_ostype {
        return Err(CoreError::Invalid("tipo de sistema operativo no válido".into()));
    }
    Ok(vec![
        "createvm".into(),
        "--name".into(),
        name.into(),
        "--ostype".into(),
        ostype.into(),
        "--register".into(),
    ])
}

/// Configuración inicial de la VM nueva, sintaxis de VirtualBox 7.x.
pub fn modifyvm_args(
    name: &str,
    memory_mb: u32,
    cpus: u32,
    vram_mb: u32,
    graphics: &str,
    firmware: &str,
    tpm: bool,
) -> Result<Vec<String>> {
    validate::vbox_target(name)?;
    validate::vm_memory_mb(memory_mb)?;
    validate::vm_cpus(cpus)?;
    validate::vm_vram_mb(vram_mb)?;
    let graphics = validate::vm_graphics(graphics)?;
    let firmware = validate::vm_firmware(firmware)?;
    if tpm && firmware != "efi" {
        return Err(CoreError::Invalid("TPM 2.0 solo se puede usar con firmware EFI".into()));
    }
    let mut args = vec![
        "modifyvm".into(),
        name.into(),
        "--memory".into(),
        memory_mb.to_string(),
        "--cpus".into(),
        cpus.to_string(),
        "--vram".into(),
        vram_mb.to_string(),
        "--graphicscontroller".into(),
        graphics.into(),
        "--firmware".into(),
        firmware.into(),
        "--ioapic".into(),
        "on".into(),
        "--nic1".into(),
        "nat".into(),
        "--boot1".into(),
        "dvd".into(),
        "--boot2".into(),
        "disk".into(),
        "--boot3".into(),
        "none".into(),
        "--boot4".into(),
        "none".into(),
    ];
    if tpm {
        args.extend(["--tpm-type".into(), "2.0".into()]);
    }
    Ok(args)
}

/// Crea el controlador inicial de la VM nueva. `portcount` solo aplica a SATA.
pub fn storagectl_create_args(
    vm: &str,
    controller: &str,
    kind: &str,
    portcount: Option<u32>,
) -> Result<Vec<String>> {
    validate::vbox_target(vm)?;
    validate::host_iface(controller)?;
    let mut args = vec![
        "storagectl".into(),
        vm.into(),
        "--name".into(),
        controller.into(),
        "--add".into(),
    ];
    if kind.eq_ignore_ascii_case("sata") {
        let ports = portcount.unwrap_or(2);
        if !(1..=MAX_SATA_PORTS).contains(&ports) {
            return Err(CoreError::Invalid(format!(
                "el número de puertos SATA debe estar entre 1 y {MAX_SATA_PORTS}"
            )));
        }
        args.extend([
            "sata".into(),
            "--controller".into(),
            "IntelAhci".into(),
            "--portcount".into(),
            ports.to_string(),
        ]);
    } else if kind.eq_ignore_ascii_case("ide") {
        args.extend(["ide".into(), "--controller".into(), "PIIX4".into()]);
    } else {
        return Err(CoreError::Invalid("el controlador debe ser ide o sata".into()));
    }
    Ok(args)
}

/// Conecta la ISO en una unidad óptica (`dvddrive`), sin copiar el fichero.
pub fn storageattach_iso_args(
    vm: &str,
    controller: &str,
    port: u32,
    device: u32,
    iso: &Path,
) -> Result<Vec<String>> {
    validate::vbox_target(vm)?;
    validate::host_iface(controller)?;
    Ok(vec![
        "storageattach".into(),
        vm.into(),
        "--storagectl".into(),
        controller.into(),
        "--port".into(),
        port.to_string(),
        "--device".into(),
        device.to_string(),
        "--type".into(),
        "dvddrive".into(),
        "--medium".into(),
        vdi_path_arg(iso)?,
    ])
}

pub fn unregistervm_args(target: &str) -> Result<Vec<String>> {
    validate::vbox_target(target)?;
    Ok(vec!["unregistervm".into(), target.into(), "--delete".into()])
}

/// Argumentos para borrar una VM desde la interfaz. Solo UUID estricto: un nombre
/// nunca llega a `unregistervm`. `delete_files` decide si VirtualBox borra también
/// los ficheros propios de la máquina.
pub fn unregistervm_delete_args(uuid: &str, delete_files: bool) -> Result<Vec<String>> {
    validate::vm_uuid(uuid)?;
    let mut args = vec!["unregistervm".into(), uuid.into()];
    if delete_files {
        args.push("--delete".into());
    }
    Ok(args)
}

/// Error específico por estado al intentar eliminar una VM. Los textos indican
/// exactamente qué hacer; nunca se descarta un estado guardado automáticamente.
pub fn delete_vm_state_error(vm: &str, state: &str) -> String {
    let state = state.trim();
    if state.eq_ignore_ascii_case("saved") {
        format!(
            "«{vm}» tiene un estado guardado; descarta el estado guardado o inicia y apaga la máquina del todo antes de eliminarla."
        )
    } else if state.eq_ignore_ascii_case("running") {
        format!("«{vm}» está en marcha; apágala del todo antes de eliminarla.")
    } else if state.eq_ignore_ascii_case("paused") {
        format!("«{vm}» está en pausa; apágala del todo antes de eliminarla.")
    } else if state.eq_ignore_ascii_case("starting") {
        format!("«{vm}» se está iniciando; espera a que arranque y apágala del todo antes de eliminarla.")
    } else {
        format!("«{vm}» está en estado «{state}»; solo se puede eliminar con la máquina apagada del todo.")
    }
}

/// Quita una VM de VirtualBox. Comprueba UUID, existencia y estado antes de tocar
/// nada; desconecta primero los VDI de datos de VMLab para que `--delete` no los
/// borre, y aborta si esa desconexión falla.
pub async fn delete_vm(tools: &Tools, req: &VmDeleteRequest) -> Result<VmDeleteResult> {
    let uuid = validate::vm_uuid(&req.uuid)?;
    let bin = tools.vbox()?;

    let listed = run_ok(bin, &["list", "vms"], SHORT).await?;
    if !parse_vm_list(&listed.stdout).into_iter().any(|(_, u)| u == uuid) {
        return Err(CoreError::Invalid(format!("no existe ninguna máquina virtual con UUID «{uuid}»")));
    }

    let info = run_ok(bin, &["showvminfo", uuid, "--machinereadable"], SHORT).await?;
    let state = vm_field(&info.stdout, "VMState").unwrap_or_default();
    if !storage_editable(&state) {
        return Err(CoreError::Invalid(delete_vm_state_error(uuid, &state)));
    }

    let disks_dir = tools.disks_dir.as_deref().ok_or_else(|| {
        CoreError::Invalid("no se ha configurado la carpeta de discos de VMLab".into())
    })?;
    // `detach_stale_vdis` solo devuelve avisos cuando no puede desconectar; tras
    // una desconexión correcta devuelve una lista vacía. Sacamos los nombres antes
    // para poder informar de que esos discos se han conservado.
    let preserved: Vec<String> = parse_vm_storage(&info.stdout)
        .attachments
        .iter()
        .filter(|a| disks::vdi_path_allowed(disks_dir, Path::new(&a.medium)))
        .filter_map(|a| data_disk_name(&a.medium))
        .collect();
    // Un vector no vacío significa que no se pudo desconectar (p. ej. la VM
    // cambió de estado mientras tanto): en ese caso no se elimina nada.
    let detach_warnings = detach_stale_vdis(tools, disks_dir, uuid, &[], false).await?;
    if !detach_warnings.is_empty() {
        return Err(CoreError::Invalid(detach_warnings.join(" ")));
    }
    let warnings = preserved
        .into_iter()
        .map(|disk| format!("El disco «{disk}» se ha conservado."))
        .collect();

    let args = unregistervm_delete_args(uuid, req.delete_files)?;
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    run_ok(bin, &argv, LONG).await?;
    Ok(VmDeleteResult { removed: true, warnings })
}

/// Nombre visible de un VDI de datos de VMLab a partir de su fichero
/// `vmlab-disk-<nombre>.vdi`. Devuelve `None` para cualquier otro medio.
pub fn data_disk_name(medium: &str) -> Option<String> {
    let file = Path::new(medium).file_name()?.to_str()?;
    file.strip_prefix("vmlab-disk-")?
        .strip_suffix(".vdi")
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

/// Plan puro de los pasos de almacenamiento de una VM recién creada: devuelve
/// los `argv` de cada comando sin ejecutar nada. XP usa IDE (disco en 0:0 e ISO
/// en 1:0 del mismo controlador); los demás usan SATA para el disco y crean un
/// controlador IDE aparte para la ISO.
pub fn create_vm_steps(params: &VmCreateParams, vdi: &Path, ide_present: bool) -> Result<Vec<Vec<String>>> {
    let mut steps = Vec::new();
    steps.push(createmedium_args(vdi, params.disk_mb)?);

    let disk_on_ide = params.disk_bus == "ide";
    let disk_controller = if disk_on_ide { "IDE" } else { "SATA" };
    if disk_on_ide {
        steps.push(storagectl_create_args(&params.name, "IDE", "ide", None)?);
    } else {
        steps.push(storagectl_create_args(&params.name, "SATA", "sata", Some(2))?);
    }
    steps.push(storageattach_args(&params.name, disk_controller, 0, 0, Some(vdi))?);

    if let Some(iso) = params.iso.as_deref() {
        if !disk_on_ide && !ide_present {
            steps.push(storagectl_create_args(&params.name, "IDE", "ide", None)?);
        }
        steps.push(storageattach_iso_args(&params.name, "IDE", 1, 0, Path::new(iso))?);
    }
    Ok(steps)
}

fn create_vm_error(action: &str, e: CoreError) -> CoreError {
    vdi_command_error(action, e)
}

fn create_step_error(step: &[String], e: CoreError) -> CoreError {
    let action = match step.first().map(String::as_str) {
        Some("createmedium") => "crear el disco VDI",
        Some("storagectl") => "configurar el controlador de almacenamiento",
        Some("storageattach") => "conectar el medio",
        _ => "crear la máquina virtual",
    };
    create_vm_error(action, e)
}

/// Deshace solo la VM que acabamos de crear, identificada por su UUID (o por
/// su nombre antes de conocer el UUID). Si `createmedium` llegó a crear el VDI,
/// también se cierra y se borra ese fichero. El error original se conserva
/// siempre; los fallos del rollback se añaden como mensajes secundarios.
async fn rollback_create_vm(
    bin: &Path,
    target: &str,
    original: CoreError,
    vdi: Option<&Path>,
    vm_dir: Option<&Path>,
) -> CoreError {
    let mut message = original.to_string();

    // El unregister se intenta siempre, incluso si luego falla cerrar el medio.
    match unregistervm_args(target) {
        Ok(args) => {
            let argv: Vec<&str> = args.iter().map(String::as_str).collect();
            match run_ok(bin, &argv, SHORT).await {
                Ok(_) => message.push_str("; se ha eliminado la VM creada para deshacer los cambios"),
                Err(e) => message.push_str(&format!("; además no se pudo revertir la creación: {e}")),
            }
        }
        Err(e) => message.push_str(&format!("; no se pudo revertir la creación: {e}")),
    }

    if let (Some(vdi), Some(vm_dir)) = (vdi, vm_dir) {
        match closemedium_delete_args(vdi, vm_dir) {
            Ok(args) => {
                let argv: Vec<&str> = args.iter().map(String::as_str).collect();
                match run_ok(bin, &argv, SHORT).await {
                    Ok(_) => message.push_str("; se ha eliminado el disco VDI creado"),
                    Err(e) => message.push_str(&format!("; además no se pudo eliminar el disco VDI: {e}")),
                }
            }
            Err(e) => message.push_str(&format!("; además no se pudo preparar la eliminación del disco VDI: {e}")),
        }
    }

    CoreError::Message(message)
}

/// Crea una VM vacía, con un VDI nuevo y una ISO opcional ya conectada.
/// No instala nada dentro del invitado y no toca VMs existentes.
pub async fn create_vm(tools: &Tools, req: CreateVmRequest) -> Result<CreatedVm> {
    let bin = tools.vbox()?;

    // 1. Consultar el conjunto cerrado de ostypes y los nombres existentes.
    let types = list_ostypes(tools).await?;
    let ids: Vec<String> = types.iter().map(|o| o.id.clone()).collect();
    let out = run_ok(bin, &["list", "vms"], SHORT)
        .await
        .map_err(|e| create_vm_error("listar las máquinas virtuales", e))?;
    let names: Vec<String> = parse_vm_list(&out.stdout).into_iter().map(|(name, _)| name).collect();

    // 2. Validar todo antes de crear nada.
    let params = validate::validate_create_request(&req, &ids, &names)?;
    let create_args = createvm_args(&params.name, &params.ostype)?;
    let modify_args = modifyvm_args(
        &params.name,
        params.memory_mb,
        params.cpus,
        params.vram_mb,
        &params.graphics,
        &params.firmware,
        params.tpm,
    )?;

    // 3. Registrar la VM. Hasta aquí no hay nada que deshacer.
    let argv: Vec<&str> = create_args.iter().map(String::as_str).collect();
    run_ok(bin, &argv, SHORT)
        .await
        .map_err(|e| create_vm_error("crear la máquina virtual", e))?;

    // 4. Leer UUID y CfgFile de la VM recién creada (para rollback y VDI).
    let info = run_ok(bin, &["showvminfo", &params.name, "--machinereadable"], SHORT).await;
    let info = match info {
        Ok(o) => o,
        Err(e) => {
            let e = create_vm_error("leer la información de la VM creada", e);
            // Aquí todavía no se conoce el UUID. El nombre se ha validado como
            // único justo antes (sin distinguir mayúsculas) y esta es la única
            // VM con ese nombre, así que usarlo como objetivo del rollback no
            // puede afectar a ninguna otra máquina. Tampoco hay VDI que borrar.
            return Err(rollback_create_vm(bin, &params.name, e, None, None).await);
        }
    };
    let (uuid, cfg_file) = match parse_created_vm_info(&info.stdout) {
        Some(v) => v,
        None => {
            let e = CoreError::Message("no se pudo leer el UUID o la configuración de la VM creada".into());
            // Mismo motivo que arriba: el nombre es único y aún no hay VDI.
            return Err(rollback_create_vm(bin, &params.name, e, None, None).await);
        }
    };

    // 4b. Defensa: si VirtualBox ignoró el `--ostype` pedido, deshacemos la VM
    //     antes de seguir creando discos. El UUID ya es conocido, así que el
    //     rollback no puede afectar a ninguna otra máquina.
    if let Some(requested) = types.iter().find(|o| o.id == params.ostype) {
        if !created_ostype_matches(requested, &info.stdout) {
            let reported = vm_field_ci(&info.stdout, "OSTypeId")
                .or_else(|| vm_field_ci(&info.stdout, "ostype"))
                .unwrap_or_else(|| "desconocido".into());
            let e = CoreError::Message(format!(
                "VirtualBox creó la VM con el tipo '{reported}' en vez del pedido '{}' ({})",
                requested.id, requested.description
            ));
            return Err(rollback_create_vm(bin, &uuid, e, None, None).await);
        }
    }

    // 5. Configurar, crear el VDI y conectarlo todo. Cualquier fallo deshace
    //    únicamente esta VM, usando su UUID.
    let argv: Vec<&str> = modify_args.iter().map(String::as_str).collect();
    if let Err(e) = run_ok(bin, &argv, SHORT).await {
        return Err(rollback_create_vm(bin, &uuid, create_vm_error("configurar la máquina virtual", e), None, None).await);
    }

    let vm_dir = match Path::new(&cfg_file).parent() {
        Some(parent) => parent,
        None => {
            let e = CoreError::Message("la ruta de configuración de la VM no tiene carpeta".into());
            return Err(rollback_create_vm(bin, &uuid, e, None, None).await);
        }
    };
    let vdi = vm_dir.join(format!("{}.vdi", params.name));
    let steps = match create_vm_steps(&params, &vdi, false) {
        Ok(s) => s,
        Err(e) => return Err(rollback_create_vm(bin, &uuid, e, None, None).await),
    };
    // Solo se cerrará y borrará el VDI si `createmedium` terminó con éxito.
    let mut vdi_created = false;
    for step in &steps {
        let is_medium = step.first().map(String::as_str) == Some("createmedium");
        let argv: Vec<&str> = step.iter().map(String::as_str).collect();
        let limit = if is_medium { LONG } else { SHORT };
        if let Err(e) = run_ok(bin, &argv, limit).await {
            let created = vdi_created.then_some(vdi.as_path());
            return Err(rollback_create_vm(bin, &uuid, create_step_error(step, e), created, Some(vm_dir)).await);
        }
        if is_medium {
            vdi_created = true;
        }
    }

    Ok(CreatedVm {
        uuid,
        name: params.name,
        cfg_file,
        vdi_path: vdi.to_string_lossy().into_owned(),
    })
}

fn vm_field_ci(info: &str, field: &str) -> Option<String> {
    info.lines()
        .filter_map(parse_kv)
        .find(|(k, _)| k.eq_ignore_ascii_case(field))
        .map(|(_, v)| v)
}

/// Tamaño real de un medio a partir de `showmediuminfo`.
/// Acepta bytes, `MB`/`GB` y líneas legibles como `Capacity:       10240 MBytes`.
pub fn parse_medium_size_mb(info: &str) -> Option<u32> {
    for key in ["Capacity", "Size", "LogicalCapacity"] {
        let raw = vm_field_ci(info, key).or_else(|| {
            info.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case(key)
                    .then(|| value.trim().to_string())
            })
        });
        let Some(raw) = raw else { continue };
        let value = raw.trim().trim_matches('"').trim();
        let lower = value.to_ascii_lowercase();
        for (suffix, factor) in [
            ("mbytes", 1.0),
            ("gbytes", 1024.0),
            ("mib", 1.0),
            ("mb", 1.0),
            ("gib", 1024.0),
            ("gb", 1024.0),
        ] {
            if let Some(number) = lower.strip_suffix(suffix) {
                if let Ok(n) = number.trim().parse::<f64>() {
                    return Some((n * factor).round() as u32);
                }
            }
        }
        if let Ok(bytes) = value.replace(' ', "").parse::<u64>() {
            if let Ok(mb) = u32::try_from(bytes / 1024 / 1024) {
                return Some(mb);
            }
        }
    }
    None
}

/// GB legibles a partir de MB, sin ceros sobrantes (`1024` → `1`, `1536` → `1.5`).
pub fn vdi_gb_text(mb: u32) -> String {
    let value = f64::from(mb) / 1024.0;
    let text = format!("{value:.2}");
    let trimmed = text.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() { "0".into() } else { trimmed.into() }
}

/// Aviso al aplicar una topología con un VDI que ya existe: se indican la
/// capacidad real y la del lienzo para que quede claro que no se ha ampliado.
pub fn vdi_size_warning(label: &str, real_mb: u32, wanted_mb: u32) -> String {
    format!(
        "Disco «{label}»: el disco existente mide {} GB y el lienzo indica {} GB; \
         el tamaño no cambia (usa «Gestionar discos» para ampliarlo).",
        vdi_gb_text(real_mb),
        vdi_gb_text(wanted_mb)
    )
}

fn vdi_command_error(action: &str, e: CoreError) -> CoreError {
    CoreError::Message(format!("no se pudo {action}: {e}"))
}

/// `VBoxManage` devuelve "Too many storage controllers of this type" en inglés;
/// se traduce para que el aviso lleve contexto y no parezca un fallo de VMLab.
fn vdi_storagectl_error(action: &str, e: CoreError) -> CoreError {
    let text = e.to_string().to_ascii_lowercase();
    if text.contains("too many storage controllers") || text.contains("demasiados controladores") {
        return CoreError::Message(format!(
            "la VM ya tiene demasiados controladores de almacenamiento; no se pudo {action}"
        ));
    }
    vdi_command_error(action, e)
}

fn vdi_attach_error(e: CoreError) -> CoreError {
    let text = e.to_string().to_ascii_lowercase();
    let other_vm = text.contains("already attached")
        || text.contains("already in use")
        || text.contains("is in use")
        || text.contains("attached to another")
        || text.contains("ya está")
        || text.contains("ya esta")
        || text.contains("en uso");
    if other_vm {
        CoreError::Message(format!("el disco VDI ya está conectado a otra máquina virtual: {e}"))
    } else {
        vdi_command_error("conectar el disco VDI", e)
    }
}

/// Crea el VDI si no existe. Si ya existe no lo toca; solo avisa si el tamaño
/// pedido no coincide con el real. El directorio lo elige siempre el backend.
pub async fn ensure_vdi_file(
    tools: &Tools,
    disks_dir: &Path,
    path: &Path,
    size_mb: u32,
    label: &str,
) -> Result<Option<String>> {
    if !disks::vdi_path_allowed(disks_dir, path) {
        return Err(CoreError::Invalid("la ruta del disco VDI queda fuera de la carpeta de discos".into()));
    }
    let bin = tools.vbox()?;
    let create = createmedium_args(path, size_mb)?;
    let create_argv: Vec<&str> = create.iter().map(String::as_str).collect();

    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => {
            let args = showmediuminfo_args(path)?;
            let argv: Vec<&str> = args.iter().map(String::as_str).collect();
            if let Ok(info) = run(bin, &argv, SHORT).await {
                if info.code == 0 {
                    if let Some(real_mb) = parse_medium_size_mb(&info.stdout) {
                        if real_mb != size_mb {
                            // Solo avisamos cuando se ha podido leer la capacidad real.
                            return Ok(Some(vdi_size_warning(label, real_mb, size_mb)));
                        }
                    }
                }
            }
            Ok(None)
        }
        Ok(_) => Err(CoreError::Invalid("la ruta del disco VDI no es un fichero".into())),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    CoreError::Io(format!("no se pudo crear la carpeta de discos {}: {e}", parent.display()))
                })?;
            }
            run_ok(bin, &create_argv, SHORT)
                .await
                .map_err(|e| vdi_command_error("crear el disco VDI", e))?;
            Ok(None)
        }
        Err(source) => Err(CoreError::Io(format!("no se pudo consultar {}: {source}", path.display()))),
    }
}

/// Desconecta los VDI de VMLab de esta VM que ya no están en el plan. Nunca
/// borra el fichero ni lo quita del registro de VirtualBox.
pub async fn detach_stale_vdis(
    tools: &Tools,
    disks_dir: &Path,
    vm: &str,
    wanted: &[PathBuf],
    _running: bool,
) -> Result<Vec<String>> {
    validate::vbox_target(vm)?;
    let bin = tools.vbox()?;
    let info = run_ok(bin, &["showvminfo", vm, "--machinereadable"], SHORT)
        .await
        .map_err(|e| vdi_command_error("leer la información de la VM", e))?;
    let storage = parse_vm_storage(&info.stdout);
    let mut stale = Vec::new();
    for a in &storage.attachments {
        if empty_medium(&a.medium) || !disks::vdi_path_allowed(disks_dir, Path::new(&a.medium)) {
            continue;
        }
        if wanted.iter().any(|p| same_path(&p.to_string_lossy(), &a.medium)) {
            continue;
        }
        stale.push((a.controller.clone(), a.port, a.device));
    }
    if stale.is_empty() {
        return Ok(Vec::new());
    }
    // Un estado guardado (o cualquier estado no editable) bloquea storageattach.
    if let Some(warning) = storage_state_warning(vm, &storage.state, "desconectar") {
        return Ok(vec![warning]);
    }
    for (controller, port, device) in stale {
        let args = storageattach_args(vm, &controller, port, device, None)?;
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        run_ok(bin, &argv, SHORT)
            .await
            .map_err(|e| vdi_command_error("desconectar el disco VDI", e))?;
    }
    Ok(Vec::new())
}

/// Conecta los VDI del plan que todavía no están en esta VM. Si está encendida
/// devuelve avisos y no lanza ninguna modificación de almacenamiento.
pub async fn attach_wanted_vdis(
    tools: &Tools,
    disks_dir: &Path,
    vm: &str,
    wanted: &[PathBuf],
    _running: bool,
) -> Result<Vec<String>> {
    validate::vbox_target(vm)?;
    let bin = tools.vbox()?;
    let info = run_ok(bin, &["showvminfo", vm, "--machinereadable"], SHORT)
        .await
        .map_err(|e| vdi_command_error("leer la información de la VM", e))?;
    let mut storage = parse_vm_storage(&info.stdout);
    let wanted: Vec<&Path> = wanted
        .iter()
        .map(PathBuf::as_path)
        .filter(|p| disks::vdi_path_allowed(disks_dir, p))
        .collect();
    if wanted.is_empty() {
        return Ok(Vec::new());
    }

    // Avisa solo si hay algo que conectar; un estado no editable bloquea
    // tanto storagectl como storageattach. No se descarta el estado guardado.
    if wanted.iter().any(|p| !vdi_is_attached(&storage, p)) {
        if let Some(warning) = storage_state_warning(vm, &storage.state, "conectar") {
            return Ok(vec![warning]);
        }
    }

    for path in wanted {
        if vdi_is_attached(&storage, path) {
            continue;
        }
        let slot = match pick_vdi_free_slot(&storage) {
            Some(slot) => slot,
            None => {
                // No crear otro SATA: ampliar el real cuando existe y tiene hueco.
                match sata_plan(&storage)? {
                    SataPlan::Grow(name, new_ports) => {
                        let args = storagectl_portcount_args(vm, &name, new_ports)?;
                        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
                        run_ok(bin, &argv, SHORT)
                            .await
                            .map_err(|e| vdi_storagectl_error("ampliar el controlador SATA", e))?;
                        if let Some(controller) = storage.controllers.iter_mut().find(|c| c.name == name) {
                            controller.ports = new_ports;
                        }
                    }
                    SataPlan::Create => {
                        let args = storagectl_args(vm)?;
                        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
                        run_ok(bin, &argv, SHORT)
                            .await
                            .map_err(|e| vdi_storagectl_error("crear el controlador VMLab SATA", e))?;
                        storage.controllers.push(VmStorageController {
                            name: VMLAB_VDI_CONTROLLER.into(),
                            kind: "IntelAhci".into(),
                            ports: 8,
                            max_ports: MAX_SATA_PORTS,
                        });
                    }
                }
                pick_vdi_free_slot(&storage)
                    .ok_or_else(|| CoreError::Message("no hay puertos libres para conectar el disco VDI".into()))?
            }
        };
        let args = storageattach_args(vm, &slot.controller, slot.port, slot.device, Some(path))?;
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        run_ok(bin, &argv, SHORT).await.map_err(vdi_attach_error)?;
        storage.attachments.push(VmStorageAttachment {
            controller: slot.controller,
            port: slot.port,
            device: slot.device,
            medium: path.to_string_lossy().into_owned(),
        });
    }
    Ok(Vec::new())
}

/// Subred /24 determinista por red: `10.77.N.0/24`, con N en 1..=254 derivado del nombre.
pub fn subnet_octet(net: &str) -> u8 {
    let h = net.bytes().fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(u32::from(b)));
    (h % 254) as u8 + 1
}


/// Crea (o actualiza) un servidor DHCP de VirtualBox para la red interna, de modo que
/// las VMs reciban IP automáticamente (rango `10.77.N.10-250`).
/// VirtualBox 7 y 6 usan nombres de opción distintos, así que se prueban ambos.
pub async fn ensure_dhcp(tools: &Tools, net: &str) -> Result<()> {
    validate::network_name(net)?;
    let bin = tools.vbox()?;
    let n = subnet_octet(net);
    let (server, lower, upper) = (format!("10.77.{n}.2"), format!("10.77.{n}.10"), format!("10.77.{n}.250"));
    let new_net = format!("--network={net}");
    let old_net = format!("--netname={net}");
    let variants: [(&str, &str, &str); 2] = [(&new_net, "--server-ip", "7"), (&old_net, "--ip", "6")];

    let mut last_err = None;
    for (net_arg, ip_opt, _) in variants {
        for verb in ["add", "modify"] {
            let args = [
                "dhcpserver", verb, net_arg, ip_opt, &server, "--netmask", "255.255.255.0",
                "--lower-ip", &lower, "--upper-ip", &upper, "--enable",
            ];
            match run_ok(bin, &args, SHORT).await {
                Ok(_) => return Ok(()),
                Err(e) => last_err = Some(e),
            }
        }
    }
    Err(last_err.expect("al menos un intento"))
}

/// Nombres de red de los servidores DHCP que deben borrarse: los de redes
/// gestionadas (`vmlab-*` y las antiguas `lablab-*`) que no estén en uso.
fn orphan_dhcp_networks(out: &str, in_use: &HashSet<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    out.lines()
        .filter_map(|line| line.trim().strip_prefix("NetworkName:"))
        .map(str::trim)
        .filter(|name| validate::managed_network(name))
        .filter(|name| !in_use.contains(*name))
        .filter(|name| seen.insert((*name).to_string()))
        .map(ToOwned::to_owned)
        .collect()
}

/// Borra los servidores DHCP de redes gestionadas que el plan actual ya no usa.
///
/// A diferencia de las redes internas (que desaparecen cuando ningún adaptador las
/// usa), los servidores DHCP de VirtualBox persisten en su configuración. Se prueban
/// las dos variantes de opción (`--network` en VirtualBox 7 y `--netname` en 6).
pub async fn prune_orphan_dhcp(tools: &Tools, in_use: &HashSet<String>) -> Result<()> {
    let bin = tools.vbox()?;
    let out = run_ok(bin, &["list", "dhcpservers"], SHORT).await?;
    for net in orphan_dhcp_networks(&out.stdout, in_use) {
        let new_arg = format!("--network={net}");
        let old_arg = format!("--netname={net}");
        for net_arg in [new_arg.as_str(), old_arg.as_str()] {
            if run_ok(bin, &["dhcpserver", "remove", net_arg], SHORT).await.is_ok() {
                break;
            }
        }
    }
    Ok(())
}

// ───────────────────── Instantáneas del laboratorio ─────────────────────

/// Crear o borrar una instantánea puede tardar minutos (VirtualBox fusiona discos).
const SNAPSHOT: Duration = Duration::from_secs(600);

fn output_message(out: &crate::runner::Output) -> String {
    let raw = if out.stderr.trim().is_empty() { &out.stdout } else { &out.stderr };
    raw.trim().chars().take(500).collect()
}

/// VirtualBox responde con un error cuando la VM todavía no tiene instantáneas.
fn no_snapshots(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("does not have any snapshots") || m.contains("no snapshots")
}

/// Extrae `(fecha, nombre)` de `VMLab|<fecha ISO UTC>|<label>`; si no tiene ese
/// formato se usa el id como nombre visible y no se muestra fecha.
fn parse_snapshot_description(desc: &str, id: &str) -> (String, Option<String>) {
    let mut parts = desc.splitn(3, '|');
    if parts.next().is_some_and(|p| p == "VMLab") {
        let date = parts.next().unwrap_or("").trim();
        let label = parts.next().unwrap_or("").trim();
        if !label.is_empty() {
            return (label.to_string(), (!date.is_empty()).then(|| date.to_string()));
        }
    }
    (id.to_string(), None)
}

/// Parsea `snapshot <vm> list --machinereadable`. Solo devuelve las instantáneas
/// creadas por VMLab (`vmlab-<id>`) y descarta nombres ajenos.
pub fn parse_snapshot_list(out: &str) -> Vec<(String, String, Option<String>)> {
    let pairs: Vec<(String, String)> = out.lines().filter_map(parse_kv).collect();
    let mut rows = Vec::new();
    for (key, value) in &pairs {
        let Some(suffix) = key.strip_prefix("SnapshotName") else { continue };
        let valid_suffix = suffix.is_empty()
            || suffix
                .strip_prefix('-')
                .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        if !valid_suffix {
            continue;
        }
        let Some(id) = value.strip_prefix("vmlab-") else { continue };
        if validate::labsnap_id(id).is_err() {
            continue;
        }
        let description_key = format!("SnapshotDescription{suffix}");
        let description = pairs
            .iter()
            .find(|(k, _)| *k == description_key)
            .map(|(_, v)| v.as_str())
            .unwrap_or("");
        let (label, date) = parse_snapshot_description(description, id);
        rows.push((id.to_string(), label, date));
    }
    rows
}

pub async fn snapshot_list(tools: &Tools, target: &str) -> Result<Vec<(String, String, Option<String>)>> {
    validate::vbox_target(target)?;
    let bin = tools.vbox()?;
    let out = run(bin, &["snapshot", target, "list", "--machinereadable"], SHORT).await?;
    if out.code == 0 {
        return Ok(parse_snapshot_list(&out.stdout));
    }
    let message = output_message(&out);
    if no_snapshots(&message) {
        return Ok(Vec::new());
    }
    Err(CoreError::Failed { program: "VBoxManage".into(), code: out.code, stderr: message })
}

pub async fn snapshot_take(tools: &Tools, target: &str, id: &str, label: &str) -> Result<()> {
    validate::vbox_target(target)?;
    validate::labsnap_id(id)?;
    validate::labsnap_label(label)?;
    let bin = tools.vbox()?;
    let name = format!("vmlab-{id}");
    let description = format!("VMLab|{}|{}", crate::labsnap::utc_now_iso(), label);
    run_ok(
        bin,
        &["snapshot", target, "take", name.as_str(), "--description", description.as_str()],
        SNAPSHOT,
    )
    .await
    .map(|_| ())
}

pub async fn snapshot_restore(tools: &Tools, target: &str, id: &str) -> Result<()> {
    validate::vbox_target(target)?;
    validate::labsnap_id(id)?;
    let bin = tools.vbox()?;
    let name = format!("vmlab-{id}");
    run_ok(bin, &["snapshot", target, "restore", name.as_str()], SNAPSHOT).await.map(|_| ())
}

pub async fn snapshot_delete(tools: &Tools, target: &str, id: &str) -> Result<()> {
    validate::vbox_target(target)?;
    validate::labsnap_id(id)?;
    let bin = tools.vbox()?;
    let name = format!("vmlab-{id}");
    run_ok(bin, &["snapshot", target, "delete", name.as_str()], SNAPSHOT).await.map(|_| ())
}

// ───────────────────── Capturas de pantalla ─────────────────────

/// Deja en el nombre solo `[A-Za-z0-9._-]`; el resto se convierte en `_`.
fn sanitize_screenshot_name(vm: &str) -> String {
    vm.chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '_' })
        .collect()
}

/// Nombre del PNG: `<vm>-AAAAMMDD-HHMMSS.png`. La fecha es UTC, igual que las
/// instantáneas, y se inyecta para poder probar el formato sin depender del reloj.
pub fn screenshot_filename(vm: &str, now: SystemTime) -> String {
    let secs = now.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (year, month, day, hour, minute, second) = crate::labsnap::parts_from_unix(secs);
    format!(
        "{}-{year:04}{month:02}{day:02}-{hour:02}{minute:02}{second:02}.png",
        sanitize_screenshot_name(vm)
    )
}

/// `true` solo si `path` está dentro de `dir` y no contiene `..`.
///
/// Es la comprobación pura previa; el comando de Tauri además canonicaliza ambas
/// rutas para que un enlace simbólico no pueda escapar de la carpeta.
pub fn screenshot_path_allowed(dir: &Path, path: &Path) -> bool {
    if path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return false;
    }
    path.starts_with(dir)
}

/// Captura la pantalla de una VM en marcha y la guarda como PNG en `dest`.
/// La ruta la construye siempre el backend dentro de su carpeta de capturas.
pub async fn screenshot(tools: &Tools, vm: &str, dest: &Path) -> Result<()> {
    validate::vbox_target(vm)?;
    let bin = tools.vbox()?;

    let info = run_ok(bin, &["showvminfo", vm, "--machinereadable"], SHORT).await?;
    if !show_vm_on(&info.stdout) {
        return Err(CoreError::Message("La VM no está encendida".into()));
    }

    let dest_str = dest.to_string_lossy();
    run_ok(bin, &["controlvm", vm, "screenshotpng", dest_str.as_ref()], SHORT).await?;
    if !dest.is_file() {
        return Err(CoreError::Message("VBoxManage no generó el fichero de la captura".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_screenshot_file_names_with_sanitized_vm_names() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000); // 2023-11-14 22:13:20 UTC
        assert_eq!(
            screenshot_filename("Ubuntu Server 22.04 (lab)", now),
            "Ubuntu_Server_22.04__lab_-20231114-221320.png"
        );
        assert_eq!(
            screenshot_filename("vm/../x", now),
            "vm_.._x-20231114-221320.png"
        );
        assert_eq!(screenshot_filename("web_1", now), "web_1-20231114-221320.png");
    }

    #[test]
    fn screenshot_paths_must_stay_inside_the_folder() {
        let dir = Path::new("/home/u/Pictures/VMLab");
        assert!(screenshot_path_allowed(dir, &dir.join("vm-20231114-221320.png")));
        assert!(!screenshot_path_allowed(dir, &dir.join("../fuera.png")));
        assert!(!screenshot_path_allowed(dir, Path::new("/home/u/Pictures/Otra/fuera.png")));
        assert!(!screenshot_path_allowed(dir, Path::new("/home/u/Pictures/VMLab2/fuera.png")));
    }

    #[tokio::test]
    async fn screenshot_rejects_option_like_vm_names() {
        let tools = Tools::default();
        for vm in ["-vm", "--type"] {
            let error = screenshot(&tools, vm, Path::new("captura.png")).await.unwrap_err();
            assert!(matches!(error, CoreError::Invalid(_)), "{vm}: {error}");
        }
    }

    #[test]
    fn subnet_octet_is_stable_and_in_range() {
        let a = subnet_octet("vmlab-0a1b2c3d");
        assert_eq!(a, subnet_octet("vmlab-0a1b2c3d"));
        assert!((1..=254).contains(&a));
    }

    #[test]
    fn finds_orphan_dhcp_servers_but_keeps_the_ones_in_use() {
        let out = "\
NetworkName:    vmlab-en-uso
IP:             10.77.5.2
Enabled:        Yes

NetworkName:    vmlab-huerfano
IP:             10.77.6.2
Enabled:        Yes

NetworkName:    lablab-vieja
IP:             10.77.7.2
Enabled:        Yes

NetworkName:    Mi Red
IP:             10.77.8.2
Enabled:        Yes

NetworkName:    otra-red
IP:             10.77.9.2
Enabled:        Yes

NetworkName:    lablab-
IP:             10.77.10.2
Enabled:        Yes
";
        let in_use: HashSet<String> = ["vmlab-en-uso".to_string()].into_iter().collect();
        assert_eq!(
            orphan_dhcp_networks(out, &in_use),
            vec!["vmlab-huerfano".to_string(), "lablab-vieja".to_string()]
        );
    }

    #[test]
    fn isolated_intnet_accepts_a_custom_name() {
        let cfg = NodeNet { name: Some("lab1".into()), ..Default::default() };
        assert_eq!(internal_net_name(&cfg, None).unwrap(), Some("lab1"));
    }

    #[test]
    fn isolated_intnet_rejects_an_option_like_name() {
        let cfg = NodeNet { name: Some("--x".into()), ..Default::default() };
        assert!(internal_net_name(&cfg, None).is_err());
    }

    #[test]
    fn segment_network_wins_over_the_custom_name() {
        let cfg = NodeNet { mode: NetMode::Intnet, name: Some("lab1".into()), ..Default::default() };
        assert_eq!(internal_net_name(&cfg, Some("vmlab-0a1b2c3d")).unwrap(), Some("vmlab-0a1b2c3d"));

        // Con red de segmento, un nombre propio inválido no se llega a validar.
        let odd = NodeNet { mode: NetMode::Intnet, name: Some("--x".into()), ..Default::default() };
        assert_eq!(internal_net_name(&odd, Some("vmlab-0a1b2c3d")).unwrap(), Some("vmlab-0a1b2c3d"));
    }

    #[test]
    fn parses_names_with_spaces_and_quotes() {
        let out = "\"Ubuntu Server 22.04\" {aaaa-1111}\n\"weird \" name\" {bbbb-2222}\n<inaccessible> {x}\n\n";
        let vms = parse_vm_list(out);
        assert_eq!(vms.len(), 2);
        assert_eq!(vms[0], ("Ubuntu Server 22.04".into(), "aaaa-1111".into()));
        assert_eq!(vms[1].0, "weird \" name");
    }

    #[test]
    fn parses_guest_additions_version() {
        assert_eq!(parse_guest_additions_version("Value: 7.2.22\n").as_deref(), Some("7.2.22"));
        assert_eq!(parse_guest_additions_version("Value: 7.2.20"), Some("7.2.20".into()));
        assert_eq!(parse_guest_additions_version("No value set!"), None);
        assert_eq!(parse_guest_additions_version(""), None);
    }

    #[test]
    fn parses_guest_networks_and_normalizes_mac() {
        let out = r#"
/VirtualBox/GuestAdd/Version        = '7.2.22'                       @ 2026-11-04T15:59:12.715Z
/VirtualBox/GuestInfo/Net/0/MAC     = '080027d6cadd'                 @ 2026-11-04T16:00:00.000Z
/VirtualBox/GuestInfo/Net/0/V4/IP   = '10.0.2.15'                    @ 2026-11-04T16:00:00.000Z
/VirtualBox/GuestInfo/Net/1/MAC     = '08:00:27:aa:bb:cc'            @ 2026-11-04T16:00:01.000Z
/VirtualBox/GuestInfo/Net/1/V4/IP   = '10.77.9.10/24'                @ 2026-11-04T16:00:01.000Z
/VirtualBox/GuestInfo/Net/1/V4/Netmask = '255.255.255.0'              @ 2026-11-04T16:00:01.000Z
"#;
        let nets = parse_guest_networks(out);
        assert_eq!(nets.len(), 2);
        assert_eq!(nets[0].mac.as_deref(), Some("080027D6CADD"));
        assert_eq!(nets[0].ipv4.as_deref(), Some("10.0.2.15"));
        assert_eq!(nets[1].mac.as_deref(), Some("080027AABBCC"));
        assert_eq!(nets[1].ipv4.as_deref(), Some("10.77.9.10"));
        assert_eq!(nets[1].netmask.as_deref(), Some("255.255.255.0"));
        assert!(parse_guest_networks("/VirtualBox/GuestAdd/Version = '7.2.22' @ x").is_empty());
    }

    #[test]
    fn maps_vbox_states_to_structured_issues() {
        let show = |state: &str, mode: &str| {
            format!(
                "VMState=\"{state}\"\nnic2=\"{mode}\"\ncableconnected2=\"on\"\n\
                 intnet2=\"vmlab-0a1b2c3d\"\nbridgeadapter2=\"eth0\"\nmacaddress2=\"080027AABBCC\"\n"
            )
        };
        let ga = "/VirtualBox/GuestAdd/Version = '7.2.22' @ x\n";
        let props_ok = format!(
            "{ga}/VirtualBox/GuestInfo/Net/0/MAC = '080027aabbcc' @ x\n\
             /VirtualBox/GuestInfo/Net/0/V4/IP = '10.77.9.10' @ x\n\
             /VirtualBox/GuestInfo/Net/0/V4/Netmask = '255.255.255.0' @ x\n"
        );

        let off = guest_link_from(2, &show("poweroff", "intnet"), None);
        assert_eq!(off.issues, vec![NodeIssue::NotRunning]);
        assert_eq!(off.segment, None);

        let no_ga = guest_link_from(2, &show("running", "intnet"), None);
        assert_eq!(no_ga.issues, vec![NodeIssue::NoGuestAdditions]);
        assert_eq!(no_ga.segment.as_deref(), Some("intnet:vmlab-0a1b2c3d"));

        let no_ip = guest_link_from(
            2,
            &show("running", "intnet"),
            Some("/VirtualBox/GuestAdd/Version = '7.2.22' @ x\n\
                  /VirtualBox/GuestInfo/Net/0/MAC = '080027aabbcc' @ x\n"),
        );
        assert_eq!(no_ip.issues, vec![NodeIssue::NoIpv4]);
        assert_eq!(no_ip.ipv4, None);

        let ok = guest_link_from(2, &show("running", "intnet"), Some(&props_ok));
        assert!(ok.issues.is_empty());
        assert_eq!(ok.ipv4.as_deref(), Some("10.77.9.10"));
        assert_eq!(ok.netmask.as_deref(), Some("255.255.255.0"));

        let nat = guest_link_from(2, &show("running", "nat"), Some(&props_ok));
        assert_eq!(nat.issues, vec![NodeIssue::AdapterNotOnSegment]);

        let bridged = guest_link_from(2, &show("running", "bridged"), Some(&props_ok));
        assert_eq!(bridged.issues, vec![NodeIssue::AdapterNotOnSegment]);
        assert_eq!(bridged.segment.as_deref(), Some("bridged:eth0"));
    }

    #[test]
    fn unescapes_machinereadable_paths() {
        assert_eq!(
            machinereadable_value(r"C:\\Program Files\\Oracle\\VirtualBox\\VBoxGuestAdditions.iso"),
            r"C:\Program Files\Oracle\VirtualBox\VBoxGuestAdditions.iso"
        );
    }

    #[test]
    fn parses_storage_and_finds_a_free_ide_slot() {
        let out = r#"
ostype="Debian 13 Trixie (64-bit)"
VMState="poweroff"
storagecontrollername0="IDE"
storagecontrollertype0="PIIX4"
storagecontrollerportcount0="2"
storagecontrollerportcount1="2"
storagecontrollername1="SATA Controller"
storagecontrollertype1="IntelAhci"
"IDE-0-0"="C:\\Program Files\\Oracle\\VirtualBox\\VBoxGuestAdditions.iso"
"IDE-0-1"="none"
"IDE-1-0"="none"
"IDE-1-1"="none"
"SATA Controller-0-0"="C:\\VMs\\disk.vdi"
"SATA Controller-0-1"="none"
"#;
        let (controllers, slots) = parse_storage(out);
        assert_eq!(controllers.len(), 2);
        assert_eq!(controllers[0].name, "IDE");
        assert_eq!(controllers[1].name, "SATA Controller");
        assert_eq!(slots.len(), 6);
        assert!(is_iso_attached(
            &slots,
            Path::new(r"C:\Program Files\Oracle\VirtualBox\VBoxGuestAdditions.iso")
        ));
        let free = pick_free_slot(&controllers, &slots).unwrap();
        assert_eq!(free.controller, "IDE");
        assert_eq!((free.port, free.device), (0, 1));
    }

    #[test]
    fn skips_occupied_slots_and_floppy_controllers() {
        let out = r#"
storagecontrollername0="Floppy"
storagecontrollertype0="I82078"
storagecontrollerportcount0="1"
storagecontrollername1="SATA"
storagecontrollertype1="IntelAhci"
storagecontrollerportcount1="1"
"Floppy-0-0"="none"
"SATA-0-0"="C:\\VMs\\disk.vdi"
"#;
        let (controllers, slots) = parse_storage(out);
        assert!(pick_free_slot(&controllers, &slots).is_none());
    }

    #[test]
    fn classifies_guest_os() {
        assert_eq!(guest_os("Windows 11 (64-bit)"), "windows");
        assert_eq!(guest_os("Debian 13 Trixie (64-bit)"), "linux");
        assert_eq!(guest_os("Ubuntu (64-bit)"), "linux");
        assert_eq!(guest_os("Other/Unknown"), "other");
    }

    #[test]
    fn parses_labsnap_snapshots_and_ignores_foreign_names() {
        let out = r#"
SnapshotName="vmlab-lab-uno"
SnapshotUUID="aaaa"
SnapshotDescription="VMLab|2026-11-09T10:00:00Z|Lab uno"
SnapshotName-1="vmlab-lab-dos"
SnapshotUUID-1="bbbb"
SnapshotDescription-1="VMLab|2026-11-09T11:00:00Z|Lab dos"
SnapshotName-1-1="snapshot ajeno"
SnapshotUUID-1-1="cccc"
SnapshotDescription-1-1="VMLab|2026-11-09T12:00:00Z|No debe salir"
SnapshotName-2="vmlab-invalida!"
SnapshotUUID-2="dddd"
SnapshotDescription-2="VMLab|2026-11-09T13:00:00Z|Inválida"
"#;
        let rows = parse_snapshot_list(out);
        assert_eq!(
            rows,
            vec![
                (
                    "lab-uno".to_string(),
                    "Lab uno".to_string(),
                    Some("2026-11-09T10:00:00Z".to_string())
                ),
                (
                    "lab-dos".to_string(),
                    "Lab dos".to_string(),
                    Some("2026-11-09T11:00:00Z".to_string())
                ),
            ]
        );
    }

    #[test]
    fn parses_empty_snapshot_list_and_fallback_description() {
        assert!(parse_snapshot_list("").is_empty());
        let out = "SnapshotName=\"vmlab-sin-fecha\"\nSnapshotDescription=\"otra cosa\"\n";
        assert_eq!(
            parse_snapshot_list(out),
            vec![("sin-fecha".to_string(), "sin-fecha".to_string(), None)]
        );
    }

    #[test]
    fn recognizes_the_no_snapshots_error() {
        assert!(no_snapshots("VBoxManage: error: This machine does not have any snapshots"));
        assert!(no_snapshots("No snapshots found"));
        assert!(!no_snapshots("VBoxManage: error: Could not find a snapshot named vmlab-x"));
    }

    #[test]
    fn builds_bandwidthctl_and_modifyvm_arguments() {
        fn strings(items: &[&str]) -> Vec<String> {
            items.iter().map(|item| item.to_string()).collect()
        }

        assert_eq!(bandwidth_group(2), "vmlab-nic2");
        assert_eq!(
            bandwidth_set_args("Ubuntu", "vmlab-nic2", "1000k"),
            strings(&["bandwidthctl", "Ubuntu", "set", "vmlab-nic2", "--limit", "1000k"])
        );
        assert_eq!(
            bandwidth_add_args("Ubuntu", "vmlab-nic2", 1000),
            strings(&[
                "bandwidthctl", "Ubuntu", "add", "vmlab-nic2", "--type", "network", "--limit",
                "1000k"
            ])
        );
        assert_eq!(
            bandwidth_remove_args("Ubuntu", "vmlab-nic2"),
            strings(&["bandwidthctl", "Ubuntu", "remove", "vmlab-nic2"])
        );
        assert_eq!(
            nic_group_args("Ubuntu", 2, Some("vmlab-nic2")),
            strings(&["modifyvm", "Ubuntu", "--nicbandwidthgroup2", "vmlab-nic2"])
        );
        assert_eq!(
            nic_group_args("Ubuntu", 1, None),
            strings(&["modifyvm", "Ubuntu", "--nicbandwidthgroup1", "none"])
        );
    }

    #[test]
    fn reads_vm_state_and_bandwidth_groups() {
        let out = r#"
VMState="running"
nic1="nat"
nicbandwidthgroup1="none"
nicbandwidthgroup2="vmlab-nic2"
"#;
        assert!(show_vm_on(out));
        assert!(!show_vm_on("VMState=\"poweroff\""));
        assert!(group_is_assigned(out, 2, "vmlab-nic2"));
        assert!(!group_is_assigned(out, 1, "vmlab-nic2"));

        let groups = "Name:           Limit:\nvmlab-nic2     1000 kbit/s\n";
        assert!(list_has_group(groups, "vmlab-nic2"));
        assert!(!list_has_group(groups, "vmlab-nic1"));
    }

    #[test]
    fn parses_vdi_storage_and_picks_a_compatible_free_port() {
        let info = r#"
VMState="poweroff"
storagecontrollername0="IDE"
storagecontrollertype0="PIIX4"
storagecontrollerportcount0="2"
storagecontrollername1="SATA"
storagecontrollertype1="IntelAhci"
storagecontrollerportcount1="2"
storagecontrollername2="NVMe"
storagecontrollertype2="NVMe"
storagecontrollerportcount2="1"
"IDE-0-0"="C:\\VBox\\VBoxGuestAdditions.iso"
"SATA-0-0"="C:\\VMs\\sistema.vdi"
"SATA-0-1"="none"
"SATA-1-0"="none"
"NVMe-0-0"="none"
"#;
        let storage = parse_vm_storage(info);
        assert!(!storage.running);
        assert_eq!(storage.controllers.len(), 3);
        assert_eq!(storage.attachments.len(), 5);
        let slot = pick_vdi_free_slot(&storage).unwrap();
        assert_eq!(slot.controller, "SATA");
        assert_eq!((slot.port, slot.device), (1, 0));
        assert_eq!(storage.state, "poweroff");
    }

    #[test]
    fn storage_editable_only_allows_poweroff_and_aborted() {
        for state in ["poweroff", "POWEROFF", " aborted ", "Aborted"] {
            assert!(storage_editable(state), "debería permitir «{state}»");
        }
        for state in [
            "saved",
            "running",
            "paused",
            "starting",
            "saving",
            "aborted-saved",
        ] {
            assert!(!storage_editable(state), "no debería permitir «{state}»");
        }
    }

    #[test]
    fn storage_warning_depends_on_the_exact_state() {
        // Editables: ningún aviso, tanto al conectar como al desconectar.
        assert_eq!(storage_state_warning("Ubuntu", "poweroff", "conectar"), None);
        assert_eq!(storage_state_warning("Ubuntu", "aborted", "desconectar"), None);

        // Guardado: no se descarta; se explica cómo salir de ese estado.
        assert_eq!(
            storage_state_warning("Ubuntu", "saved", "conectar").as_deref(),
            Some("«Ubuntu» tiene un estado guardado. Inicia la máquina y apágala del todo, o descarta el estado guardado desde VirtualBox, y vuelve a aplicar.")
        );
        assert_eq!(
            storage_state_warning("Ubuntu", "SAVED", "desconectar").as_deref(),
            Some("«Ubuntu» tiene un estado guardado. Inicia la máquina y apágala del todo, o descarta el estado guardado desde VirtualBox, y vuelve a aplicar.")
        );

        // En marcha o en pausa: se mantiene el texto de siempre, según la acción.
        assert_eq!(
            storage_state_warning("Ubuntu", "running", "conectar").as_deref(),
            Some("apaga «Ubuntu» para conectar el disco")
        );
        assert_eq!(
            storage_state_warning("Ubuntu", "paused", "desconectar").as_deref(),
            Some("apaga «Ubuntu» para desconectar el disco")
        );

        // Cualquier otro estado: se muestra el estado exacto leído de VMState.
        for state in ["starting", "saving", "aborted-saved"] {
            assert_eq!(
                storage_state_warning("Ubuntu", state, "conectar").as_deref(),
                Some(format!(
                    "«Ubuntu» está en estado «{state}»: apágala del todo para cambiar sus discos."
                ))
                .as_deref()
            );
        }
    }

    #[test]
    fn no_compatible_controller_has_no_free_vdi_slot() {
        let info = r#"
storagecontrollername0="IDE"
storagecontrollertype0="PIIX4"
storagecontrollerportcount0="2"
storagecontrollername1="Floppy"
storagecontrollertype1="I82078"
storagecontrollerportcount1="1"
"IDE-0-0"="none"
"Floppy-0-0"="none"
"#;
        assert!(pick_vdi_free_slot(&parse_vm_storage(info)).is_none());
    }

    #[test]
    fn recognizes_an_already_attached_vdi() {
        let path = Path::new(r"C:\Users\lab\AppData\Roaming\VMLab\disks\vmlab-disk-datos.vdi");
        let storage = VmStorageInfo {
            attachments: vec![VmStorageAttachment {
                controller: "SATA".into(),
                port: 1,
                device: 0,
                medium: path.to_string_lossy().into_owned(),
            }],
            ..VmStorageInfo::default()
        };
        assert!(vdi_is_attached(&storage, path));
        assert!(!vdi_is_attached(&storage, Path::new(r"C:\Users\lab\AppData\Roaming\VMLab\disks\vmlab-disk-otro.vdi")));
    }

    #[test]
    fn grows_a_full_sata_controller_instead_of_adding_another() {
        let info = r#"
VMState="poweroff"
storagecontrollername0="IDE"
storagecontrollertype0="PIIX4"
storagecontrollerportcount0="2"
storagecontrollername1="SATA"
storagecontrollertype1="IntelAhci"
storagecontrollerportcount1="2"
"IDE-0-0"="none"
"IDE-1-0"="none"
"SATA-0-0"="C:\\VMs\\sistema.vdi"
"SATA-1-0"="C:\\VBox\\VBoxGuestAdditions.iso"
"#;
        let mut storage = parse_vm_storage(info);
        assert!(pick_vdi_free_slot(&storage).is_none());
        assert_eq!(sata_plan(&storage).unwrap(), SataPlan::Grow("SATA".into(), 3));

        if let Some(controller) = storage.controllers.iter_mut().find(|c| c.name == "SATA") {
            controller.ports = 3;
        }
        let slot = pick_vdi_free_slot(&storage).unwrap();
        assert_eq!(slot.controller, "SATA");
        assert_eq!((slot.port, slot.device), (2, 0));
    }

    #[test]
    fn a_full_sata_controller_at_its_cap_has_no_room() {
        let info = r#"
VMState="poweroff"
storagecontrollername0="SATA"
storagecontrollertype0="IntelAhci"
storagecontrollerportcount0="30"
storagecontrollermaxportcount0="30"
"#;
        let storage = parse_vm_storage(info);
        assert!(has_sata_controller(&storage));
        assert_eq!(grow_sata_controller(&storage), None);
        assert_eq!(
            sata_plan(&storage).unwrap_err().to_string(),
            "la VM no tiene puertos SATA libres"
        );
    }

    #[test]
    fn without_a_sata_controller_the_vmlab_one_is_created() {
        let info = r#"
VMState="poweroff"
storagecontrollername0="IDE"
storagecontrollertype0="PIIX4"
storagecontrollerportcount0="2"
storagecontrollername1="NVMe"
storagecontrollertype1="NVMe"
storagecontrollerportcount1="1"
"NVMe-0-0"="C:\\VMs\\sistema.vdi"
"#;
        let storage = parse_vm_storage(info);
        assert!(pick_vdi_free_slot(&storage).is_none());
        assert!(!has_sata_controller(&storage));
        assert_eq!(sata_plan(&storage).unwrap(), SataPlan::Create);
    }

    #[test]
    fn a_free_sata_port_is_used_without_growing() {
        let info = r#"
VMState="poweroff"
storagecontrollername0="SATA"
storagecontrollertype0="IntelAhci"
storagecontrollerportcount0="2"
"SATA-0-0"="C:\\VMs\\sistema.vdi"
"SATA-1-0"="none"
"#;
        let storage = parse_vm_storage(info);
        let slot = pick_vdi_free_slot(&storage).unwrap();
        assert_eq!(slot.controller, "SATA");
        assert_eq!((slot.port, slot.device), (1, 0));
    }

    #[test]
    fn maxportcount_is_the_growth_cap_and_defaults_to_thirty() {
        let info = r#"
VMState="poweroff"
storagecontrollername0="SATA"
storagecontrollertype0="IntelAhci"
storagecontrollerportcount0="1"
storagecontrollermaxportcount0="30"
"SATA-0-0"="C:\\VMs\\sistema.vdi"
"#;
        let mut storage = parse_vm_storage(info);
        assert_eq!(storage.controllers[0].max_ports, 30);
        assert_eq!(grow_sata_controller(&storage), Some(("SATA".into(), 2)));
        storage.controllers[0].ports = 2;
        let slot = pick_vdi_free_slot(&storage).unwrap();
        assert_eq!((slot.controller.as_str(), slot.port), ("SATA", 1));

        let default_cap = r#"
VMState="poweroff"
storagecontrollername0="SATA"
storagecontrollertype0="IntelAhci"
storagecontrollerportcount0="1"
"#;
        assert_eq!(parse_vm_storage(default_cap).controllers[0].max_ports, MAX_SATA_PORTS);

        let low_cap = r#"
VMState="poweroff"
storagecontrollername0="SATA"
storagecontrollertype0="IntelAhci"
storagecontrollerportcount0="2"
storagecontrollermaxportcount0="2"
"#;
        let low_cap = parse_vm_storage(low_cap);
        assert_eq!(grow_sata_controller(&low_cap), None);
        assert!(sata_plan(&low_cap).is_err());
    }

    #[test]
    fn storagectl_portcount_arguments_are_validated() {
        let expected: Vec<String> = ["storagectl", "Ubuntu", "--name", "SATA", "--portcount", "3"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(storagectl_portcount_args("Ubuntu", "SATA", 3).unwrap(), expected);
        assert!(storagectl_portcount_args("--vm", "SATA", 3).is_err());
        assert!(storagectl_portcount_args("Ubuntu", "--SATA", 3).is_err());
        assert!(storagectl_portcount_args("Ubuntu", "SATA", 0).is_err());
        assert!(storagectl_portcount_args("Ubuntu", "SATA", MAX_SATA_PORTS + 1).is_err());
    }

    #[test]
    fn too_many_storage_controllers_is_translated() {
        let e = CoreError::Message(
            "VBoxManage: error: Too many storage controllers of this type".into(),
        );
        let text = vdi_storagectl_error("crear el controlador VMLab SATA", e).to_string();
        assert!(text.contains("demasiados controladores de almacenamiento"), "{text}");
        assert!(text.contains("crear el controlador VMLab SATA"), "{text}");
    }

    #[test]
    fn builds_vdi_arguments_without_interpolating_values() {
        fn strings(items: &[&str]) -> Vec<String> {
            items.iter().map(|item| (*item).to_string()).collect()
        }
        let path = Path::new(r"C:\Users\lab\AppData\Roaming\VMLab\disks\vmlab-disk-datos.vdi");
        let path_s = path.to_string_lossy().into_owned();

        assert_eq!(
            createmedium_args(path, 10_240).unwrap(),
            strings(&[
                "createmedium", "disk", "--filename", path_s.as_str(), "--size", "10240", "--format",
                "VDI", "--variant", "Standard"
            ])
        );
        assert_eq!(
            showmediuminfo_args(path).unwrap(),
            strings(&["showmediuminfo", "disk", path_s.as_str()])
        );
        assert_eq!(
            storagectl_args("Ubuntu").unwrap(),
            strings(&[
                "storagectl", "Ubuntu", "--name", "VMLab SATA", "--add", "sata", "--controller",
                "IntelAhci", "--portcount", "8"
            ])
        );
        assert_eq!(
            storageattach_args("Ubuntu", "SATA", 3, 0, Some(path)).unwrap(),
            strings(&[
                "storageattach", "Ubuntu", "--storagectl", "SATA", "--port", "3", "--device", "0",
                "--type", "hdd", "--medium", path_s.as_str()
            ])
        );
        assert_eq!(
            storageattach_args("Ubuntu", "SATA", 3, 0, None).unwrap(),
            strings(&[
                "storageattach", "Ubuntu", "--storagectl", "SATA", "--port", "3", "--device", "0",
                "--type", "hdd", "--medium", "none"
            ])
        );

        assert!(createmedium_args(Path::new("--mal.vdi"), 10).is_err());
        assert!(createmedium_args(path, 0).is_err());
        assert!(storagectl_args("--vm").is_err());
        assert!(storageattach_args("Ubuntu", "--ctl", 0, 0, None).is_err());
        assert!(storageattach_args("Ubuntu", "SATA", 0, 0, Some(Path::new("--mal.vdi"))).is_err());
    }

    #[test]
    fn parses_medium_size_in_megabytes() {
        assert_eq!(parse_medium_size_mb("capacity=10737418240\n"), Some(10_240));
        assert_eq!(parse_medium_size_mb("Size=\"20480MB\"\n"), Some(20_480));
        assert_eq!(parse_medium_size_mb("Capacity=\"20GB\"\n"), Some(20_480));
        let realistic = "VDI Image Information:\n\
                         UUID:                 12345678-1234-1234-1234-123456789abc\n\
                         Format:               VDI\n\
                         Storage format:       Standard\n\
                         Capacity:             10240 MBytes\n\
                         Location:             C:\\VMs\\disco.vdi\n";
        assert_eq!(parse_medium_size_mb(realistic), Some(10_240));
        assert_eq!(parse_medium_size_mb("Capacity:       20 GBytes\n"), Some(20_480));
        assert_eq!(
            parse_medium_size_mb("UUID: 1234\nFormat: VDI\nLocation: C:\\VMs\\disco.vdi\n"),
            None
        );
        assert_eq!(parse_medium_size_mb("otro=1\n"), None);
    }

    #[test]
    fn warns_with_the_real_and_canvas_sizes_when_a_vdi_exists() {
        assert_eq!(vdi_gb_text(1024), "1");
        assert_eq!(vdi_gb_text(1536), "1.5");
        assert_eq!(
            vdi_size_warning("datos", 1024, 20_480),
            "Disco «datos»: el disco existente mide 1 GB y el lienzo indica 20 GB; \
             el tamaño no cambia (usa «Gestionar discos» para ampliarlo)."
        );
    }

    // ── C1: creación de VMs ──

    const REAL_OSTYPES_72: &str = r#"Supported guest OS types:

ID / Description: Other -- Other/Unknown
Family:           Other (Other)
Architecture:     x86

ID / Description: Other_64 -- Other/Unknown (64-bit)
Family:           Other (Other)
Architecture:     x86 (64-bit)

ID / Description: Other_arm64 -- Other/Unknown (ARM 64-bit)
Family:           Other (Other)
Architecture:     ARMv8 (64-bit)
"#;

    #[test]
    fn parses_the_real_vbox_72_ostypes_list() {
        let types = parse_ostypes_checked(REAL_OSTYPES_72).unwrap();
        assert_eq!(types.len(), 3);

        assert_eq!(types[0].id, "Other");
        assert_eq!(types[0].description, "Other/Unknown");
        assert_eq!(types[0].family, "other");
        assert_eq!(types[0].architecture, "x86");

        assert_eq!(types[1].id, "Other_64");
        assert_eq!(types[1].description, "Other/Unknown (64-bit)");
        assert_eq!(types[1].architecture, "x86 (64-bit)");

        assert_eq!(types[2].id, "Other_arm64");
        assert_eq!(types[2].description, "Other/Unknown (ARM 64-bit)");
        assert_eq!(types[2].architecture, "ARMv8 (64-bit)");
        assert!(ostype_is_arm(&types[2]));
        assert!(!ostype_is_arm(&types[0]));

        let selection = ostypes_for_selection(types);
        assert_eq!(selection.len(), 2);
        assert_eq!(selection[1].id, "Other_64");
        assert!(!selection.iter().any(ostype_is_arm));
    }

    #[test]
    fn old_ostypes_format_still_parses() {
        let out = "ID:          WindowsXP\n\
                   Description: Windows XP\n\
                   ID:          Ubuntu_64\n\
                   Description: Ubuntu (64-bit)\n";
        let types = parse_ostypes_checked(out).unwrap();
        assert_eq!(types.len(), 2);
        assert_eq!(types[0].id, "WindowsXP");
        assert_eq!(types[0].description, "Windows XP");
        assert_eq!(types[0].family, "windows");
        assert_eq!(types[0].architecture, "");
        assert_eq!(types[1].id, "Ubuntu_64");
        assert_eq!(types[1].family, "linux");
    }

    #[test]
    fn unrecognized_ostypes_output_is_reported() {
        let err = parse_ostypes_checked("Formato desconocido\n").unwrap_err().to_string();
        assert_eq!(err, OSTYPES_PARSE_ERROR);
        assert!(parse_ostypes_checked("").unwrap().is_empty());
        assert!(parse_ostypes("Description: huérfana\n").is_empty());
    }

    #[test]
    fn catalog_uses_every_ostype_id_while_selection_skips_arm() {
        let out = r#"Supported guest OS types:

ID / Description: WindowsXP -- Windows XP
Family:           Windows (Windows)
Architecture:     x86

ID / Description: Windows11_64 -- Windows 11 (64-bit)
Family:           Windows (Windows)
Architecture:     x86 (64-bit)

ID / Description: Ubuntu_64 -- Ubuntu (64-bit)
Family:           Linux (Linux)
Architecture:     x86 (64-bit)

ID / Description: Other_arm64 -- Other/Unknown (ARM 64-bit)
Family:           Other (Other)
Architecture:     ARMv8 (64-bit)
"#;
        let types = parse_ostypes_checked(out).unwrap();
        assert_eq!(types[0].family, "windows");
        assert_eq!(types[2].family, "linux");

        let ids: Vec<String> = types.iter().map(|entry| entry.id.clone()).collect();
        assert!(ids.iter().any(|id| id == "Other_arm64"));
        assert!(validate::vm_ostype("Other_arm64", &ids).is_ok());

        let catalog = available_os_catalog(&ids);
        let labels: Vec<&str> = catalog
            .iter()
            .map(|entry| entry.label.as_str())
            .collect();
        assert!(labels.contains(&"Windows XP"));
        assert!(labels.contains(&"Windows 11 (64 bits)"));
        assert!(labels.contains(&"Ubuntu"));
        assert!(labels.contains(&"Pop!_OS"));

        let selection = ostypes_for_selection(types);
        assert!(!selection.iter().any(|entry| entry.id == "Other_arm64"));
    }

    #[test]
    fn created_ostype_is_checked_against_id_or_readable_description() {
        let requested = VmOsType {
            id: "Ubuntu_64".into(),
            description: "Ubuntu (64-bit)".into(),
            family: "linux".into(),
            architecture: "x86 (64-bit)".into(),
        };

        // `OSTypeId` manda cuando existe, aunque `ostype` sea legible.
        let with_id = "UUID=\"abc\"\nCfgFile=\"C:\\vm\\vm.vbox\"\nOSTypeId=\"Ubuntu_64\"\nostype=\"Ubuntu (64-bit)\"\n";
        assert!(created_ostype_matches(&requested, with_id));

        // Sin ID, en 7.2 `ostype` trae la descripción legible.
        let readable = "ostype=\"Ubuntu (64-bit)\"\n";
        assert!(created_ostype_matches(&requested, readable));

        // Otras versiones ponen el ID directamente en `ostype`.
        let plain = "ostype=\"Ubuntu_64\"\n";
        assert!(created_ostype_matches(&requested, plain));

        // El fallo real: se pidió Ubuntu y VirtualBox puso Windows XP.
        let wrong_id = "OSTypeId=\"WindowsXP\"\nostype=\"Windows XP (32-bit)\"\n";
        assert!(!created_ostype_matches(&requested, wrong_id));
        let wrong_description = "ostype=\"Windows XP (32-bit)\"\n";
        assert!(!created_ostype_matches(&requested, wrong_description));
        assert!(!created_ostype_matches(&requested, "UUID=\"abc\"\n"));
    }

    #[test]
    fn reads_uuid_and_cfgfile_from_machine_readable_info() {
        let info = r#"
UUID="c0ffee00-1111-2222-3333-444455556666"
CfgFile="C:\\Users\\marc\\VirtualBox VMs\\Mi VM\\Mi VM.vbox"
VMState="poweroff"
"#;
        assert_eq!(
            parse_created_vm_info(info),
            Some((
                "c0ffee00-1111-2222-3333-444455556666".into(),
                "C:\\Users\\marc\\VirtualBox VMs\\Mi VM\\Mi VM.vbox".into()
            ))
        );
        assert_eq!(parse_created_vm_info("VMState=\"poweroff\"\n"), None);
    }

    #[test]
    fn createvm_arguments_are_separate_and_validated() {
        let args = createvm_args("Mi VM (lab)", "Windows10_64").unwrap();
        assert_eq!(
            args,
            ["createvm", "--name", "Mi VM (lab)", "--ostype", "Windows10_64", "--register"]
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<String>>()
        );
        assert!(createvm_args("-mal", "Windows10_64").is_err());
        assert!(createvm_args("Mi VM", "--mal").is_err());
        assert!(createvm_args("Mi VM", "").is_err());
    }

    #[test]
    fn modifyvm_arguments_follow_virtualbox_7_syntax() {
        let args = modifyvm_args("Mi VM", 4096, 2, 128, "vboxsvga", "efi", true).unwrap();
        let expected = [
            "modifyvm", "Mi VM", "--memory", "4096", "--cpus", "2", "--vram", "128",
            "--graphicscontroller", "vboxsvga", "--firmware", "efi", "--ioapic", "on",
            "--nic1", "nat", "--boot1", "dvd", "--boot2", "disk", "--boot3", "none",
            "--boot4", "none", "--tpm-type", "2.0",
        ];
        assert_eq!(args, expected.iter().map(|s| s.to_string()).collect::<Vec<String>>());

        let no_tpm = modifyvm_args("Mi VM", 1024, 1, 16, "vboxvga", "bios", false).unwrap();
        assert!(!no_tpm.iter().any(|a| a.contains("tpm")));
        assert!(no_tpm.iter().any(|a| a == "bios"));

        assert!(modifyvm_args("Mi VM", 4096, 2, 128, "vboxsvga", "bios", true).is_err());
        assert!(modifyvm_args("-Mi VM", 4096, 2, 128, "vboxsvga", "efi", true).is_err());
        assert!(modifyvm_args("Mi VM", 63, 2, 128, "vboxsvga", "efi", false).is_err());
    }

    #[test]
    fn new_controller_and_iso_arguments_are_validated() {
        let sata = storagectl_create_args("Mi VM", "SATA", "sata", Some(2)).unwrap();
        assert_eq!(
            sata,
            ["storagectl", "Mi VM", "--name", "SATA", "--add", "sata", "--controller", "IntelAhci", "--portcount", "2"]
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<String>>()
        );
        let ide = storagectl_create_args("Mi VM", "IDE", "ide", None).unwrap();
        assert_eq!(
            ide,
            ["storagectl", "Mi VM", "--name", "IDE", "--add", "ide", "--controller", "PIIX4"]
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<String>>()
        );
        assert!(storagectl_create_args("Mi VM", "-IDE", "ide", None).is_err());
        assert!(storagectl_create_args("Mi VM", "SATA", "sata", Some(0)).is_err());
        assert!(storagectl_create_args("Mi VM", "SCSI", "scsi", None).is_err());

        let iso = Path::new("C:\\ISOs\\windows xp.iso");
        let attach = storageattach_iso_args("Mi VM", "IDE", 1, 0, iso).unwrap();
        assert_eq!(
            attach,
            [
                "storageattach", "Mi VM", "--storagectl", "IDE", "--port", "1", "--device", "0",
                "--type", "dvddrive", "--medium", "C:\\ISOs\\windows xp.iso"
            ]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<String>>()
        );
        assert!(storageattach_iso_args("Mi VM", "IDE", 1, 0, Path::new("-mal.iso")).is_err());
        assert!(storageattach_iso_args("-Mi VM", "IDE", 1, 0, iso).is_err());

        assert_eq!(
            unregistervm_args("c0ffee00-1111-2222-3333-444455556666").unwrap(),
            ["unregistervm", "c0ffee00-1111-2222-3333-444455556666", "--delete"]
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<String>>()
        );
        assert!(unregistervm_args("--otra-vm").is_err());

        let uuid = "c0ffee00-1111-2222-3333-444455556666";
        assert_eq!(
            unregistervm_delete_args(uuid, false).unwrap(),
            ["unregistervm", uuid].iter().map(|s| s.to_string()).collect::<Vec<String>>()
        );
        assert_eq!(
            unregistervm_delete_args(uuid, true).unwrap(),
            ["unregistervm", uuid, "--delete"].iter().map(|s| s.to_string()).collect::<Vec<String>>()
        );
        assert!(unregistervm_delete_args("Mi VM", true).is_err());
        assert!(unregistervm_delete_args("--otra-vm", false).is_err());
    }

    #[test]
    fn delete_vm_only_allows_poweroff_or_aborted() {
        assert!(storage_editable("poweroff"));
        assert!(storage_editable("aborted"));
        assert!(!storage_editable("saved"));
        assert!(!storage_editable("running"));
        assert!(!storage_editable("paused"));
        assert!(!storage_editable("starting"));

        let saved = delete_vm_state_error("Mi VM", "saved");
        assert!(saved.contains("descarta el estado guardado o inicia y apaga la máquina del todo"));
        assert!(delete_vm_state_error("Mi VM", "running").contains("en marcha"));
        assert!(delete_vm_state_error("Mi VM", "paused").contains("en pausa"));
        assert!(delete_vm_state_error("Mi VM", "starting").contains("iniciando"));
        assert!(delete_vm_state_error("Mi VM", "other").contains("estado «other»"));
    }

    #[test]
    fn data_disk_name_only_accepts_vmlab_vdis() {
        assert_eq!(data_disk_name("C:\\VMs\\VMLab\\disks\\vmlab-disk-datos.vdi").as_deref(), Some("datos"));
        assert_eq!(data_disk_name("/tmp/disks/vmlab-disk-disco1.vdi").as_deref(), Some("disco1"));
        assert_eq!(data_disk_name("vmlab-disk-.vdi"), None);
        assert_eq!(data_disk_name("C:\\ISOs\\ubuntu.iso"), None);
        assert_eq!(data_disk_name("Mi VM.vdi"), None);
    }

    #[test]
    fn closemedium_arguments_are_restricted_to_the_vm_folder() {
        let dir = Path::new("C:\\VMs\\Mi VM");
        let vdi = dir.join("Mi VM.vdi");
        assert_eq!(
            closemedium_delete_args(&vdi, dir).unwrap(),
            ["closemedium", "disk", "C:\\VMs\\Mi VM\\Mi VM.vdi", "--delete"]
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<String>>()
        );

        // Extensión incorrecta.
        assert!(closemedium_delete_args(&dir.join("Mi VM.iso"), dir).is_err());
        // Nombre de fichero que empieza por '-': sería interpretado como opción.
        assert!(closemedium_delete_args(&dir.join("-mal.vdi"), dir).is_err());
        // Otra carpeta: nunca se borra un VDI ajeno.
        assert!(closemedium_delete_args(&Path::new("C:\\VMs\\Otra VM\\Mi VM.vdi"), dir).is_err());
        assert!(closemedium_delete_args(Path::new("-mal.vdi"), dir).is_err());
    }

    fn c1_params(bus: &str, iso: Option<&str>) -> VmCreateParams {
        VmCreateParams {
            name: "Mi VM".into(),
            ostype: "Windows10_64".into(),
            memory_mb: 4096,
            cpus: 2,
            disk_mb: 51_200,
            vram_mb: 128,
            firmware: "bios".into(),
            tpm: false,
            graphics: "vboxsvga".into(),
            disk_bus: bus.into(),
            iso: iso.map(str::to_string),
        }
    }

    #[test]
    fn xp_plan_uses_ide_and_win10_plan_uses_sata() {
        let vdi = Path::new("C:\\VMs\\Mi VM\\Mi VM.vdi");
        let iso = Path::new("C:\\ISOs\\instalador.iso");

        let mut xp = c1_params("ide", Some(iso.to_str().unwrap()));
        xp.ostype = "WindowsXP".into();
        xp.memory_mb = 1024;
        xp.cpus = 1;
        xp.disk_mb = 20 * 1024;
        xp.vram_mb = 16;
        xp.graphics = "vboxvga".into();
        let xp_steps = create_vm_steps(&xp, vdi, false).unwrap();
        assert_eq!(xp_steps.len(), 4);
        assert_eq!(xp_steps[0][0], "createmedium");
        assert_eq!(&xp_steps[1][..5], ["storagectl", "Mi VM", "--name", "IDE", "--add"]);
        assert_eq!(&xp_steps[2][..4], ["storageattach", "Mi VM", "--storagectl", "IDE"]);
        assert!(xp_steps[2].iter().any(|a| a == "hdd"));
        assert_eq!(&xp_steps[3][..4], ["storageattach", "Mi VM", "--storagectl", "IDE"]);
        assert!(xp_steps[3].iter().any(|a| a == "dvddrive"));
        assert_eq!(xp_steps[3][5], "1"); // puerto 1, mismo controlador IDE
        // El nombre y la ISO viajan como argumentos separados, sin interpolar.
        assert!(xp_steps.iter().flatten().any(|a| a == "C:\\ISOs\\instalador.iso"));
        assert!(xp_steps.iter().flatten().any(|a| a == vdi.to_str().unwrap()));

        let win = c1_params("sata", Some(iso.to_str().unwrap()));
        let win_steps = create_vm_steps(&win, vdi, false).unwrap();
        assert_eq!(win_steps.len(), 5);
        assert_eq!(&win_steps[1][..5], ["storagectl", "Mi VM", "--name", "SATA", "--add"]);
        assert!(win_steps[1].iter().any(|a| a == "IntelAhci"));
        assert_eq!(&win_steps[2][..4], ["storageattach", "Mi VM", "--storagectl", "SATA"]);
        assert_eq!(&win_steps[3][..5], ["storagectl", "Mi VM", "--name", "IDE", "--add"]);
        assert_eq!(&win_steps[4][..4], ["storageattach", "Mi VM", "--storagectl", "IDE"]);
        assert_eq!(win_steps[4][5], "1");

        // Con ISO y controlador IDE ya presente, no se crea otro.
        let win_ide = create_vm_steps(&win, vdi, true).unwrap();
        assert_eq!(win_ide.len(), 4);

        // Sin ISO: solo disco, sin unidad óptica conectada.
        let no_iso = create_vm_steps(&c1_params("sata", None), vdi, false).unwrap();
        assert_eq!(no_iso.len(), 3);
    }

    #[test]
    fn create_request_uses_camel_case_fields() {
        let json = serde_json::json!({
            "name": "Win11 (lab)",
            "ostype": "Windows11_64",
            "memoryMb": 4096,
            "cpus": 2,
            "diskGb": 64,
            "vramMb": 128,
            "firmware": "efi",
            "tpm": true,
            "graphics": "vboxsvga",
            "diskBus": "sata",
            "iso": "C:\\ISOs\\windows11.iso"
        });
        let req: CreateVmRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.name, "Win11 (lab)");
        assert_eq!(req.memory_mb, 4096);
        assert_eq!(req.disk_gb, Some(64));
        assert_eq!(req.vram_mb, Some(128));
        assert!(req.tpm);
        assert_eq!(req.disk_bus, "sata");
        assert!(req.iso.is_some());
    }

    #[test]
    fn optical_slot_prefers_a_free_ide_slot_without_touching_existing_drives() {
        let info = r#"
VMState="running"
storagecontrollername0="IDE"
storagecontrollertype0="PIIX4"
storagecontrollerportcount0="2"
storagecontrollername1="SATA"
storagecontrollertype1="IntelAhci"
storagecontrollerportcount1="2"
"IDE-1-0"="C:\\ISOs\\installer.iso"
"IDE-1-1"="none"
"SATA-0-0"="C:\\VMs\\system.vdi"
"#;
        let storage = parse_vm_storage(info);
        let placement = plan_optical_slot(&storage).unwrap();
        assert_eq!(
            placement.slot,
            OpticalSlot { controller: "IDE".into(), port: 0, device: 0 }
        );
        assert_eq!(placement.prepare, OpticalPrepare::None);

        // Un `emptydrive` es una unidad del usuario: no se reutiliza.
        let with_drive = info.replace("\"IDE-1-1\"=\"none\"", "\"IDE-0-0\"=\"emptydrive\"");
        let storage = parse_vm_storage(&with_drive);
        let placement = plan_optical_slot(&storage).unwrap();
        assert_eq!(placement.slot.device, 1);
    }

    #[test]
    fn optical_slot_uses_or_grows_sata_when_ide_is_full() {
        let full_ide = r#"
"IDE-0-0"="C:\\a.vdi"
"IDE-0-1"="C:\\b.vdi"
"IDE-1-0"="C:\\c.iso"
"IDE-1-1"="C:\\d.vdi"
"#;
        let info = format!(
            r#"
VMState="poweroff"
storagecontrollername0="IDE"
storagecontrollertype0="PIIX4"
storagecontrollerportcount0="2"
storagecontrollername1="SATA"
storagecontrollertype1="IntelAhci"
storagecontrollerportcount1="1"
storagecontrollermaxportcount1="30"
{full_ide}
"SATA-0-0"="none"
"#
        );
        let storage = parse_vm_storage(&info);
        let placement = plan_optical_slot(&storage).unwrap();
        assert_eq!(placement.slot.controller, "SATA");
        assert_eq!((placement.slot.port, placement.slot.device), (0, 0));
        assert_eq!(placement.prepare, OpticalPrepare::None);

        let grown = info.replace("\"SATA-0-0\"=\"none\"", "\"SATA-0-0\"=\"C:\\\\system.vdi\"");
        let storage = parse_vm_storage(&grown);
        let placement = plan_optical_slot(&storage).unwrap();
        assert_eq!(
            placement.prepare,
            OpticalPrepare::GrowSata { controller: "SATA".into(), ports: 2 }
        );
        assert_eq!(placement.slot.port, 1);
    }

    #[test]
    fn optical_slot_creates_ide_only_when_there_is_no_alternative() {
        let no_controller = "VMState=\"poweroff\"\n";
        let placement = plan_optical_slot(&parse_vm_storage(no_controller)).unwrap();
        assert_eq!(
            placement.slot,
            OpticalSlot { controller: VMLAB_IDE_CONTROLLER.into(), port: 0, device: 0 }
        );
        assert_eq!(placement.prepare, OpticalPrepare::CreateIde);

        // Hay IDE (lleno) y SATA agotado: no se crea otro controlador.
        let info = r#"
VMState="poweroff"
storagecontrollername0="IDE"
storagecontrollertype0="PIIX4"
storagecontrollerportcount0="2"
storagecontrollername1="SATA"
storagecontrollertype1="IntelAhci"
storagecontrollerportcount1="2"
storagecontrollermaxportcount1="2"
"IDE-0-0"="a"
"IDE-0-1"="b"
"IDE-1-0"="c"
"IDE-1-1"="d"
"SATA-0-0"="e"
"SATA-1-0"="f"
"#;
        let err = plan_optical_slot(&parse_vm_storage(info)).unwrap_err().to_string();
        assert!(err.contains("no queda ningún hueco"), "{err}");
    }

    #[test]
    fn optical_storageattach_arguments_are_closed() {
        let iso = std::env::temp_dir().join(format!("vmlab-vbox-{}.iso", std::process::id()));
        std::fs::write(&iso, b"ISO").unwrap();
        let iso_s = iso.to_string_lossy().into_owned();
        let slot = OpticalSlot { controller: "IDE".into(), port: 1, device: 0 };
        let base: Vec<String> = ["storageattach", "Ubuntu", "--storagectl", "IDE", "--port", "1",
            "--device", "0", "--type", "dvddrive", "--medium"]
            .iter().map(|s| s.to_string()).collect();

        let mut expected = base.clone();
        expected.push("emptydrive".into());
        assert_eq!(
            storageattach_optical_args("Ubuntu", &slot, OpticalMedium::Empty, false).unwrap(),
            expected
        );
        let mut expected = base.clone();
        expected.push("none".into());
        assert_eq!(
            storageattach_optical_args("Ubuntu", &slot, OpticalMedium::None, false).unwrap(),
            expected
        );
        let mut expected = base.clone();
        expected.push(iso_s.clone());
        expected.push("--forceunmount".into());
        assert_eq!(
            storageattach_optical_args("Ubuntu", &slot, OpticalMedium::Iso(&iso_s), true).unwrap(),
            expected
        );

        assert!(storageattach_optical_args("--vm", &slot, OpticalMedium::Empty, false).is_err());
        let bad_slot = OpticalSlot { controller: "--IDE".into(), port: 0, device: 0 };
        assert!(storageattach_optical_args("Ubuntu", &bad_slot, OpticalMedium::Empty, false).is_err());
        assert!(storageattach_optical_args("Ubuntu", &slot, OpticalMedium::Iso("relativa.iso"), false).is_err());
        // El `--forceunmount` nunca se construye automáticamente al insertar.
        let inserted = storageattach_optical_args("Ubuntu", &slot, OpticalMedium::Iso(&iso_s), false).unwrap();
        assert!(!inserted.iter().any(|a| a == "--forceunmount"));
        std::fs::remove_file(&iso).unwrap();
    }

    #[test]
    fn optical_extradata_arguments_and_parsing_are_symmetric() {
        let key = optical_extradata_key("n_abc").unwrap();
        assert_eq!(key, "VMLab/Optical/n_abc");
        assert!(optical_extradata_key("n|mal").is_err());

        let expected: Vec<String> = ["setextradata", "Ubuntu", "VMLab/Optical/n_abc", "IDE|1|0"]
            .iter().map(|s| s.to_string()).collect();
        assert_eq!(setextradata_args("Ubuntu", &key, "IDE|1|0").unwrap(), expected);
        assert!(setextradata_args("--vm", &key, "IDE|1|0").is_err());
        assert!(setextradata_args("Ubuntu", "--clave", "IDE|1|0").is_err());
        assert!(setextradata_args("Ubuntu", &key, "--mal").is_err());
        assert!(setextradata_args("Ubuntu", &key, "IDE|1\n0").is_err());
        assert_eq!(
            getextradata_args("Ubuntu", &key).unwrap(),
            ["getextradata", "Ubuntu", "VMLab/Optical/n_abc"]
                .iter().map(|s| s.to_string()).collect::<Vec<_>>()
        );
        assert_eq!(
            enumerate_extradata_args("Ubuntu").unwrap(),
            ["getextradata", "Ubuntu", "enumerate"]
                .iter().map(|s| s.to_string()).collect::<Vec<_>>()
        );

        assert_eq!(parse_extradata_value("Value: IDE|1|0\n"), Some("IDE|1|0".into()));
        assert_eq!(parse_extradata_value("Value: <no value>\n"), None);
        assert_eq!(parse_extradata_value(""), None);
        let slot = parse_optical_location("IDE|1|0").unwrap();
        assert_eq!(slot, OpticalSlot { controller: "IDE".into(), port: 1, device: 0 });
        assert_eq!(slot.location().unwrap(), "IDE|1|0");
        for bad in ["IDE|-1|0", "IDE|1|0|extra", "--evil|0|0", "IDE|31|0", "IDE|0|2", ""] {
            assert!(parse_optical_location(bad).is_none(), "{bad}");
        }

        let out = "Key: VMLab/Optical/n_abc, Value: IDE|1|0\nKey: Otra, Value: <no value>\n";
        assert_eq!(
            parse_extradata_enumeration(out),
            vec![
                ("VMLab/Optical/n_abc".into(), Some("IDE|1|0".into())),
                ("Otra".into(), None),
            ]
        );
    }

    #[test]
    fn optical_medium_comparison_ignores_the_rest_of_the_storage() {
        let info = r#"
VMState="poweroff"
storagecontrollername0="IDE"
storagecontrollertype0="PIIX4"
storagecontrollerportcount0="2"
"IDE-1-0"="C:\\ISOs\\ubuntu.iso"
"IDE-0-0"="emptydrive"
"#;
        let storage = parse_vm_storage(info);
        let iso_slot = OpticalSlot { controller: "IDE".into(), port: 1, device: 0 };
        assert!(optical_medium_is(&storage, &iso_slot, Some("C:\\ISOs\\ubuntu.iso")));
        assert!(!optical_medium_is(&storage, &iso_slot, Some("C:\\ISOs\\otra.iso")));
        assert!(!optical_medium_is(&storage, &iso_slot, None));
        let empty_slot = OpticalSlot { controller: "IDE".into(), port: 0, device: 0 };
        assert!(optical_medium_is(&storage, &empty_slot, None));
        assert!(optical_state_warning("Ubuntu", "poweroff", "crear").is_none());
        let saved = optical_state_warning("Ubuntu", "saved", "crear").unwrap();
        assert!(saved.contains("estado guardado"), "{saved}");
    }
}
