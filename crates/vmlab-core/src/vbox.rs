//! Control de VirtualBox mediante `VBoxManage`.
//!
//! Convención de red: VMLab es dueño del **adaptador 2** de cada VM (red interna
//! `vmlab-xxxxxxxx`). El adaptador 1 (NAT, internet) no se toca.
use crate::error::{CoreError, Result};
use crate::model::{Action, GuestAdditionsInfo, GuestAdditionsPrep, LinkInfo, NetMode, NodeNet};
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
                nets.push(GuestNetProp { index, mac: None, ipv4: None });
                nets.last_mut().expect("recién insertado")
            }
        };
        match field.trim() {
            "MAC" => slot.mac = Some(normalize_mac(value)),
            "V4/IP" => {
                let ip = value.split('/').next().unwrap_or(value).trim();
                if ip.parse::<std::net::Ipv4Addr>().is_ok() {
                    slot.ipv4 = Some(ip.to_string());
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
    let mode = vm_field(&info.stdout, &format!("nic{adapter}")).unwrap_or_else(|| "none".into());
    let note = |text: String| LinkInfo { segment: None, ipv4: None, note: Some(text) };

    if mode == "none" || mode == "null" {
        return Ok(note(format!("adaptador {adapter} desconectado")));
    }
    if mode == "nat" {
        return Ok(note("NAT aísla la VM: no comparte el segmento del lienzo".into()));
    }
    if vm_field(&info.stdout, &format!("cableconnected{adapter}")).as_deref() == Some("off") {
        return Ok(note(format!("cable del adaptador {adapter} desconectado")));
    }

    let field = |suffix: &str| vm_field(&info.stdout, &format!("{suffix}{adapter}"));
    let segment = match mode.as_str() {
        "intnet" => Some(format!("intnet:{}", field("intnet").unwrap_or_else(|| "default".into()))),
        "hostonly" => field("hostonlyadapter").map(|n| format!("hostonly:{n}")),
        "bridged" => field("bridgeadapter").map(|n| format!("bridged:{n}")),
        "natnetwork" => field("nat-network").map(|n| format!("natnetwork:{n}")),
        other => return Ok(note(format!("modo de red no reconocido: {other}"))),
    };
    let Some(segment) = segment else {
        return Ok(note(format!("no se pudo leer la red del adaptador {adapter}")));
    };
    let Some(mac) = field("macaddress") else {
        return Ok(LinkInfo { segment: Some(segment), ipv4: None, note: Some("el adaptador no tiene MAC".into()) });
    };

    let props = run_ok(bin, &["guestproperty", "enumerate", target], SHORT).await?;
    let wanted = normalize_mac(&mac);
    let ipv4 = parse_guest_networks(&props.stdout)
        .into_iter()
        .find(|n| n.mac.as_deref() == Some(wanted.as_str()))
        .and_then(|n| n.ipv4);
    let note = ipv4.is_none().then(|| {
        "sin IPv4 en ese adaptador (¿Guest Additions activas y DHCP/estática configurada?)".to_string()
    });
    Ok(LinkInfo { segment: Some(segment), ipv4, note })
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
            "maxportcount" if controllers[idx].ports == 0 => {
                if let Ok(n) = value.parse::<u32>() {
                    controllers[idx].ports = n;
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
"#;
        let nets = parse_guest_networks(out);
        assert_eq!(nets.len(), 2);
        assert_eq!(nets[0].mac.as_deref(), Some("080027D6CADD"));
        assert_eq!(nets[0].ipv4.as_deref(), Some("10.0.2.15"));
        assert_eq!(nets[1].mac.as_deref(), Some("080027AABBCC"));
        assert_eq!(nets[1].ipv4.as_deref(), Some("10.77.9.10"));
        assert!(parse_guest_networks("/VirtualBox/GuestAdd/Version = '7.2.22' @ x").is_empty());
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
}
