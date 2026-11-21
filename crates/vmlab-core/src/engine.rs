//! Orquestación: combina `vbox`, `docker` y `topology`. Es la única API que usa la capa Tauri.
use crate::error::{CoreError, Result};
use crate::model::*;
use crate::tools::Tools;
use crate::validate::DiskKind;
use crate::{connectivity, disks, docker, docker_desktop, guest, netem, optical, topology, vbox};
use crate::docker::MountInfo;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

fn vbox_state(running: bool) -> PowerState {
    if running { PowerState::Running } else { PowerState::Stopped }
}

/// Los fallos de una VM se devuelven como avisos y siempre nombran a la VM, para
/// que se pueda distinguir qué conexión falló cuando un nodo tiene varias.
fn optical_vm_warning(vm: &NodeSpec, detail: impl std::fmt::Display) -> String {
    format!("{}: {detail}", vm.display())
}

/// Unidades que cada VM debe conservar, calculadas desde los planes ya validados.
fn optical_wanted_by_vm(plans: &[optical::OpticalPlan]) -> HashMap<&str, HashSet<&str>> {
    let mut wanted: HashMap<&str, HashSet<&str>> = HashMap::new();
    for plan in plans {
        wanted
            .entry(plan.vm.target.as_str())
            .or_default()
            .insert(plan.node.id.as_str());
    }
    wanted
}

/// Solo las unidades del extradata de *esta* VM que ya no están en el lienzo.
fn optical_units_to_remove<'a>(
    entries: &'a [(String, vbox::OpticalSlot)],
    wanted: &HashSet<&str>,
) -> Vec<&'a (String, vbox::OpticalSlot)> {
    entries.iter().filter(|(id, _)| !wanted.contains(id.as_str())).collect()
}

/// Crea (sin arrancar) el contenedor de un nodo Docker del segmento cuando todavía
/// no existe y el nodo tiene imagen. Sin imagen no se toca nada.
/// Los montajes del plan se pasan a `docker create` con `-v`.
async fn create_missing(tools: &Tools, n: &NodeSpec, mounts: &BTreeSet<MountInfo>) -> Result<()> {
    let Some(image) = n.image.as_deref().filter(|i| !i.is_empty()) else {
        return Ok(());
    };
    if docker::exists(tools, &n.target).await? {
        return Ok(());
    }
    let mounts: Vec<MountInfo> = mounts.iter().cloned().collect();
    docker::create_container_with_mounts(tools, &n.target, image, n.options.as_ref(), &mounts).await
}

/// Si el contenedor ya existe pero sus montajes no coinciden con el plan, hay que
/// recrearlo: `docker create` no admite cambiar `-v` de un contenedor existente.
/// Devuelve `true` cuando se ha recreado. El volumen nunca se borra.
async fn sync_mounts(
    tools: &Tools,
    n: &NodeSpec,
    planned: &BTreeSet<MountInfo>,
) -> Result<bool> {
    if !docker::exists(tools, &n.target).await? {
        return Ok(false);
    }
    let current = docker::mounts(tools, &n.target).await?;
    if docker::mounts_match(&current, planned) {
        return Ok(false);
    }
    let image = match n.image.as_deref().filter(|i| !i.is_empty()) {
        Some(image) => image.to_string(),
        None => docker::container_image(tools, &n.target).await?,
    };
    let mounts: Vec<MountInfo> = planned.iter().cloned().collect();
    docker::remove_container(tools, &n.target).await?;
    docker::create_container_with_mounts(tools, &n.target, &image, n.options.as_ref(), &mounts).await?;
    Ok(true)
}

pub async fn inventory(tools: &Tools) -> Inventory {
    let (vb, dk) = tokio::join!(
        async { if tools.vboxmanage.is_some() { Some(vbox::inventory(tools).await) } else { None } },
        async { if tools.docker.is_some() { Some(docker::list(tools).await) } else { None } },
    );
    let mut inv = Inventory::default();
    match vb {
        Some(Ok(list)) => {
            inv.vbox = list
                .into_iter()
                .map(|v| MachineInfo {
                    kind: NodeKind::Vbox,
                    target: v.uuid,
                    label: v.name,
                    state: vbox_state(v.running),
                    image: None,
                    guest_additions: Some(v.guest_additions),
                })
                .collect()
        }
        Some(Err(e)) => inv.vbox_error = Some(e.to_string()),
        None => inv.vbox_error = Some(CoreError::ToolMissing("VBoxManage").to_string()),
    }
    match dk {
        Some(Ok(list)) => {
            inv.docker = list
                .into_iter()
                .map(|c| MachineInfo {
                    kind: NodeKind::Docker,
                    target: c.name.clone(),
                    label: c.name,
                    state: c.state,
                    image: Some(c.image),
                    guest_additions: None,
                })
                .collect()
        }
        Some(Err(e)) => {
            let raw = e.to_string();
            inv.docker_engine_off = docker::engine_is_down(&raw);
            inv.docker_error = Some(if inv.docker_engine_off {
                "Docker Desktop no está en marcha. Pulsa «Iniciar Docker» para arrancarlo en segundo plano.".into()
            } else {
                raw.clone()
            });
            inv.docker_error_detail = Some(raw);
        }
        None => {
            inv.docker_error = Some(CoreError::ToolMissing("docker").to_string());
            inv.docker_engine_off = false;
            inv.docker_error_detail = None;
        }
    }
    inv
}

/// Estado de todas las máquinas en 2 llamadas (una por tecnología), pensado para sondeo periódico.
pub async fn snapshot(tools: &Tools) -> Snapshot {
    // Refresco periódico: solo `list vms` + `list runningvms`. La versión de Guest
    // Additions se consulta bajo demanda en `inventory`, no cada 2 segundos.
    let (vb, dk) = tokio::join!(
        async { if tools.vboxmanage.is_some() { vbox::list_basic(tools).await.ok() } else { None } },
        async { if tools.docker.is_some() { docker::list(tools).await.ok() } else { None } },
    );
    let mut states: HashMap<String, PowerState> = HashMap::new();
    let (vbox_ok, docker_ok) = (vb.is_some(), dk.is_some());
    for vm in vb.unwrap_or_default() {
        let s = vbox_state(vm.running);
        states.insert(NodeKind::Vbox.key(&vm.uuid), s);
        states.insert(NodeKind::Vbox.key(&vm.name), s);
    }
    for c in dk.unwrap_or_default() {
        states.insert(NodeKind::Docker.key(&c.name), c.state);
    }
    Snapshot { states, vbox_ok, docker_ok }
}

pub async fn act(
    tools: &Tools,
    kind: NodeKind,
    target: &str,
    image: Option<&str>,
    options: Option<DockerRunOptions>,
    a: Action,
) -> Result<()> {
    match kind {
        NodeKind::Vbox => vbox::action(tools, target, a).await,
        NodeKind::Docker => docker::action(tools, target, image, options, a).await,
        NodeKind::Disk => Err(CoreError::Invalid(
            "un disco no se puede encender ni apagar; solo se crea el volumen".into(),
        )),
        NodeKind::Optical => Err(CoreError::Invalid(
            "una unidad óptica no se puede encender ni apagar; solo cambia su ISO".into(),
        )),
    }
}

/// Detecta el gestor de red Linux dentro de la VM (Guest Additions).
pub async fn guest_probe(tools: &Tools, target: &str, username: &str, password: &str) -> Result<GuestProfile> {
    guest::probe(tools, target, username, password).await
}

/// Escribe y aplica la configuración de red dentro de la VM Linux.
pub async fn guest_apply(tools: &Tools, request: GuestConfigRequest) -> Result<GuestReport> {
    guest::apply(tools, request).await
}

/// Escribe los nombres de la red en el bloque gestionado de `/etc/hosts`.
pub async fn guest_hosts_apply(tools: &Tools, request: GuestHostsRequest) -> Result<GuestReport> {
    guest::apply_hosts(tools, request).await
}

/// Prepara la instalación de Guest Additions: monta la ISO y abre la VM con ventana.
pub async fn guest_additions_prepare(tools: &Tools, target: &str) -> Result<GuestAdditionsPrep> {
    vbox::prepare_guest_additions(tools, target).await
}

/// Comprueba la comunicación real de los nodos del lienzo (red interna e IPv4).
pub async fn network_probe(tools: &Tools, nodes: Vec<ProbeNode>) -> Vec<NodeProbe> {
    connectivity::probe(tools, nodes).await
}

/// Arranca Docker Desktop en segundo plano (sin abrir el panel) y espera al motor.
pub async fn docker_start(tools: &Tools, wait_secs: u64) -> Result<DockerStartInfo> {
    docker_desktop::start(tools, wait_secs).await
}

/// Detiene Docker Desktop al cerrar la app (solo si VMLab lo había arrancado).
pub async fn docker_stop(tools: &Tools) -> Result<()> {
    docker_desktop::stop(tools).await
}

/// Aplica la topología del lienzo al sistema. Nunca aborta a la mitad: acumula errores por nodo.
pub async fn apply(tools: &Tools, t: &Topology) -> ApplyReport {
    // Discos y ópticos se validan antes de cualquier operación: un enlace
    // incorrecto no debe aplicar parte de la red ni tocar el almacenamiento.
    let mut early_errors: Vec<String> = Vec::new();
    if let Err(e) = crate::validate::disk_topology(t) {
        early_errors.push(e.to_string());
    }
    if let Err(e) = crate::validate::optical_topology(t) {
        early_errors.push(e.to_string());
    }
    if !early_errors.is_empty() {
        return ApplyReport { errors: early_errors, ..Default::default() };
    }
    let has_vdi = t
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Disk)
        .any(|d| matches!(crate::validate::disk_kind(t, d), Ok(DiskKind::Vdi)));
    if has_vdi && tools.disks_dir.is_none() {
        return ApplyReport {
            errors: vec!["no se ha configurado la carpeta de discos de VMLab".into()],
            ..Default::default()
        };
    }
    if has_vdi && tools.vboxmanage.is_none() {
        return ApplyReport {
            errors: vec![CoreError::ToolMissing("VBoxManage").to_string()],
            ..Default::default()
        };
    }
    let disks_dir: Option<&Path> = tools.disks_dir.as_deref();
    let disk_plans = match disks::plan(t) {
        Ok(plans) => plans,
        Err(e) => return ApplyReport { errors: vec![e.to_string()], ..Default::default() },
    };
    let vdi_plans: Vec<disks::VdiPlan> = match disks_dir {
        Some(dir) => match disks::plan_vdi(t, dir) {
            Ok(plans) => plans,
            Err(e) => return ApplyReport { errors: vec![e.to_string()], ..Default::default() },
        },
        None => Vec::new(),
    };
    let mounts_by_container = disks::mounts_by_container(&disk_plans);
    let no_mounts: BTreeSet<MountInfo> = BTreeSet::new();

    let plan = topology::plan(t);
    let mut rep = ApplyReport { warnings: plan.warnings.clone(), ..Default::default() };
    for d in &disk_plans {
        if let Err(e) = docker::volume_create(tools, &d.volume).await {
            rep.errors.push(format!("{}: {e}", d.node.display()));
        }
    }
    let running: HashSet<String> = if tools.vboxmanage.is_some() {
        vbox::running_keys(tools).await.unwrap_or_default()
    } else {
        HashSet::new()
    };

    // Los ficheros VDI se crean antes de tocar la red. Un VDI existente nunca se
    // modifica: como mucho se avisa de que el tamaño pedido no se aplicará.
    if let Some(dir) = disks_dir {
        for vdi in &vdi_plans {
            match vbox::ensure_vdi_file(tools, dir, &vdi.path, vdi.size_mb, vdi.node.display()).await {
                // El aviso ya incluye el nombre del disco y ambas cifras.
                Ok(Some(warning)) => rep.warnings.push(warning),
                Ok(None) => {}
                Err(e) => rep.errors.push(format!("{}: {e}", vdi.node.display())),
            }
        }
    }

    // Desconectar primero: permite mover un VDI de una VM a otra sin que la
    // segunda conexión falle por estar el medio todavía en la primera.
    if let (Some(dir), true) = (disks_dir, tools.vboxmanage.is_some()) {
        let vm_nodes: Vec<&NodeSpec> = t.nodes.iter().filter(|n| n.kind == NodeKind::Vbox).collect();
        let mut wanted_by_vm: HashMap<&str, Vec<PathBuf>> = HashMap::new();
        for vdi in &vdi_plans {
            wanted_by_vm.entry(vdi.vm_id.as_str()).or_default().push(vdi.path.clone());
        }
        let quiet = !t.nodes.iter().any(|n| n.kind == NodeKind::Disk);
        for vm in &vm_nodes {
            let wanted = wanted_by_vm.get(vm.id.as_str()).cloned().unwrap_or_default();
            if let Err(e) = crate::validate::vbox_target(&vm.target) {
                rep.errors.push(format!("{}: {e}", vm.display()));
                continue;
            }
            match vbox::detach_stale_vdis(tools, dir, &vm.target, &wanted, running.contains(&vm.target)).await {
                Ok(warnings) => rep.warnings.extend(warnings),
                Err(e) if !quiet => rep.errors.push(format!("{}: {e}", vm.display())),
                Err(_) => {}
            }
        }
        for vm in &vm_nodes {
            let wanted = wanted_by_vm.get(vm.id.as_str()).cloned().unwrap_or_default();
            if wanted.is_empty() {
                continue;
            }
            if let Err(e) = crate::validate::vbox_target(&vm.target) {
                rep.errors.push(format!("{}: {e}", vm.display()));
                continue;
            }
            match vbox::attach_wanted_vdis(tools, dir, &vm.target, &wanted, running.contains(&vm.target)).await {
                Ok(warnings) => rep.warnings.extend(warnings),
                Err(e) => rep.errors.push(format!("{}: {e}", vm.display())),
            }
        }
    }

    // Unidades ópticas: crear solo con la VM apagada; cambiar el medio también
    // con la VM en marcha; quitar las que ya no estén en el lienzo solo apagadas.
    // Cada arista se procesa por separado: un fallo en una VM no impide seguir
    // con las demás y se devuelve como aviso nombrando la VM.
    let optical_plans = match optical::plan(t) {
        Ok(plans) => plans,
        Err(e) => {
            rep.errors.push(e.to_string());
            Vec::new()
        }
    };
    if !optical_plans.is_empty() && tools.vboxmanage.is_none() {
        rep.errors.push(CoreError::ToolMissing("VBoxManage").to_string());
    } else if tools.vboxmanage.is_some() {
        for plan in &optical_plans {
            let detail = |msg: String| format!("unidad «{}»: {msg}", plan.node.id);
            if let Err(e) = crate::validate::vbox_target(&plan.vm.target) {
                rep.warnings.push(optical_vm_warning(&plan.vm, detail(e.to_string())));
                continue;
            }
            let storage = match vbox::vm_optical_storage(tools, &plan.vm.target).await {
                Ok(storage) => storage,
                Err(e) => {
                    rep.warnings.push(optical_vm_warning(&plan.vm, detail(e.to_string())));
                    continue;
                }
            };
            let existing =
                match vbox::read_optical_extradata(tools, &plan.vm.target, &plan.node.id).await {
                    Ok(slot) => slot,
                    Err(e) => {
                        rep.warnings.push(optical_vm_warning(&plan.vm, detail(e.to_string())));
                        continue;
                    }
                };
            match existing {
                Some(slot) => {
                    if vbox::optical_medium_is(&storage, &slot, plan.iso.as_deref()) {
                        continue;
                    }
                    let medium = plan
                        .iso
                        .as_deref()
                        .map_or(vbox::OpticalMedium::Empty, vbox::OpticalMedium::Iso);
                    if let Err(e) = vbox::set_optical_medium(
                        tools,
                        &plan.vm.target,
                        &slot,
                        medium,
                        plan.iso.is_none(),
                    )
                    .await
                    {
                        // El error de VirtualBox se muestra tal cual: no sabemos
                        // si el cambio en caliente funciona en cada versión.
                        rep.warnings.push(optical_vm_warning(
                            &plan.vm,
                            detail(format!("no se pudo cambiar la ISO en caliente: {e}")),
                        ));
                    }
                }
                None => {
                    if let Some(warning) =
                        vbox::optical_state_warning(&plan.vm.target, &storage.state, "crear")
                    {
                        rep.warnings.push(optical_vm_warning(&plan.vm, detail(warning)));
                        continue;
                    }
                    let placement = match vbox::plan_optical_slot(&storage) {
                        Ok(placement) => placement,
                        Err(e) => {
                            rep.warnings.push(optical_vm_warning(&plan.vm, detail(e.to_string())));
                            continue;
                        }
                    };
                    if let Err(e) = vbox::create_optical_drive(
                        tools,
                        &plan.vm.target,
                        &plan.node.id,
                        &placement,
                        plan.iso.as_deref(),
                    )
                    .await
                    {
                        rep.warnings.push(optical_vm_warning(&plan.vm, detail(e.to_string())));
                    }
                }
            }
        }

        // El extradata es la única fuente de verdad para retirar unidades: así
        // no se tocan nunca las unidades ópticas que el usuario pusiera a mano.
        // El conjunto deseado se calcula por VM, así que quitar una conexión no
        // afecta a la misma unidad montada en otras VMs.
        let wanted_by_vm = optical_wanted_by_vm(&optical_plans);
        let mut visited: HashSet<&str> = HashSet::new();
        for vm in t.nodes.iter().filter(|n| n.kind == NodeKind::Vbox) {
            if !visited.insert(vm.target.as_str()) {
                continue;
            }
            if let Err(e) = crate::validate::vbox_target(&vm.target) {
                rep.errors.push(format!("{}: {e}", vm.display()));
                continue;
            }
            let wanted = wanted_by_vm.get(vm.target.as_str()).cloned().unwrap_or_default();
            let entries = match vbox::list_optical_extradata(tools, &vm.target).await {
                Ok(entries) => entries,
                Err(e) => {
                    rep.warnings.push(format!("{}: {e}", vm.display()));
                    continue;
                }
            };
            let to_remove = optical_units_to_remove(&entries, &wanted);
            if to_remove.is_empty() {
                continue;
            }
            let storage = match vbox::vm_optical_storage(tools, &vm.target).await {
                Ok(storage) => storage,
                Err(e) => {
                    rep.warnings.push(format!("{}: {e}", vm.display()));
                    continue;
                }
            };
            for item in to_remove {
                let node_id = item.0.as_str();
                let slot = &item.1;
                if let Some(warning) = vbox::optical_state_warning(&vm.target, &storage.state, "quitar") {
                    rep.warnings.push(format!(
                        "unidad «{node_id}» de «{}»: {warning}",
                        vm.display()
                    ));
                    continue;
                }
                match vbox::remove_optical_drive(tools, &vm.target, node_id, slot).await {
                    Ok(()) => rep.warnings.push(format!(
                        "Se ha quitado la unidad óptica «{node_id}» de «{}» porque ya no está en el lienzo.",
                        vm.display()
                    )),
                    Err(e) => rep.warnings.push(format!(
                        "unidad «{node_id}» de «{}»: {e}",
                        vm.display()
                    )),
                }
            }
        }
    }

    for seg in &plan.segments {
        rep.networks.push(seg.name.clone());
        let mut docker_net_ready = false;
        for n in &seg.nodes {
            let net = n.net.clone().unwrap_or_default();
            if n.kind == NodeKind::Vbox && net.mode != NetMode::Intnet {
                rep.warnings.push(format!(
                    "{} usa {}: no se conectará a la red interna {} del lienzo.",
                    n.display(),
                    net.mode.label(),
                    seg.name
                ));
            }
            let res: Result<()> = match n.kind {
                NodeKind::Vbox if running.contains(&n.target) => Err(CoreError::Invalid(
                    "apaga la VM para poder cambiar su adaptador de red".into(),
                )),
                NodeKind::Vbox => vbox::set_nic(tools, &n.target, &net, Some(&seg.name)).await,
                NodeKind::Docker => {
                    let planned = mounts_by_container.get(n.target.as_str()).unwrap_or(&no_mounts);
                    let recreated = match sync_mounts(tools, n, planned).await {
                        Ok(value) => value,
                        Err(e) => {
                            rep.errors.push(format!("{}: {e}", n.display()));
                            continue;
                        }
                    };
                    if recreated {
                        rep.warnings.push(format!(
                            "«{}»: cambiaron los montajes, así que se ha recreado el contenedor. El volumen se ha conservado. Se pierde el estado del sistema de ficheros del contenedor que no esté en volúmenes (solo se conservan los volúmenes de VMLab).",
                            n.display()
                        ));
                    }
                    let ready = if docker_net_ready { Ok(()) } else { docker::ensure_network(tools, &seg.name).await };
                    docker_net_ready = ready.is_ok();
                    match ready {
                        Ok(()) => match create_missing(tools, n, planned).await {
                            Ok(()) => docker::set_networks(tools, &n.target, Some(&seg.name)).await,
                            Err(e) => Err(e),
                        },
                        Err(e) => Err(e),
                    }
                }
                // Los discos y las unidades ópticas no pertenecen a segmentos de red.
                NodeKind::Disk | NodeKind::Optical => Ok(()),
            };
            match res {
                Ok(()) => rep.applied += 1,
                Err(e) => rep.errors.push(format!("{}: {e}", n.display())),
            }
        }
    }

    // DHCP automático solo para los segmentos que realmente usan la red interna de VMLab.
    let vmlab_intnet = |nodes: &[NodeSpec]| {
        nodes
            .iter()
            .any(|n| n.kind == NodeKind::Vbox && n.net.as_ref().map_or(true, |c| c.mode == NetMode::Intnet))
    };
    for seg in plan.segments.iter().filter(|s| vmlab_intnet(&s.nodes)) {
        if let Err(e) = vbox::ensure_dhcp(tools, &seg.name).await {
            rep.warnings.push(format!("No se pudo crear el servidor DHCP de {}: {e}. Tendrás que asignar IPs a mano.", seg.name));
        }
    }

    for n in &plan.isolated {
        let net = n.net.clone().unwrap_or_default();

        // Una VM encendida no admite `modifyvm`. No se ignora en silencio: el
        // usuario debe ver que esa máquina quedó fuera de la red aplicada.
        if n.kind == NodeKind::Vbox && running.contains(&n.target) {
            rep.warnings.push(format!(
                "{}: apaga la VM para cambiar su red (se ha dejado sin modificar).",
                n.display()
            ));
            continue;
        }

        let res = match n.kind {
            NodeKind::Vbox => vbox::set_nic(tools, &n.target, &net, None).await,
            NodeKind::Docker => {
                let planned = mounts_by_container.get(n.target.as_str()).unwrap_or(&no_mounts);
                let recreated = match sync_mounts(tools, n, planned).await {
                    Ok(value) => value,
                    Err(e) => {
                        rep.errors.push(format!("{}: {e}", n.display()));
                        continue;
                    }
                };
                if recreated {
                    rep.warnings.push(format!(
                        "«{}»: cambiaron los montajes, así que se ha recreado el contenedor. El volumen se ha conservado. Se pierde el estado del sistema de ficheros del contenedor que no esté en volúmenes (solo se conservan los volúmenes de VMLab).",
                        n.display()
                    ));
                }
                // Un nodo aislado sí puede tener discos: se crea con sus `-v`
                // aunque no tenga red, y luego se desconecta de las gestionadas.
                match create_missing(tools, n, planned).await {
                    Ok(()) => docker::set_networks(tools, &n.target, None).await,
                    Err(e) => Err(e),
                }
            }
            NodeKind::Disk | NodeKind::Optical => Ok(()),
        };
        if let Err(e) = res {
            rep.errors.push(format!("{}: {e}", n.display()));
        }
    }

    // Condiciones de red por nodo, una vez aplicada la topología de red.
    for n in t.nodes.iter().filter(|n| n.link.is_some()) {
        let Some(conditions) = n.link.as_ref() else { continue };
        if let Err(e) = crate::validate::link_conditions(conditions) {
            rep.errors.push(format!("{}: {e}", n.display()));
            continue;
        }
        let is_on = match n.kind {
            NodeKind::Disk | NodeKind::Optical => false,
            NodeKind::Vbox => running.contains(&n.target),
            NodeKind::Docker => match netem::is_running(tools, &n.target).await {
                Ok(value) => value,
                Err(e) => {
                    rep.errors.push(format!("{}: {e}", n.display()));
                    continue;
                }
            },
        };
        if !is_on {
            rep.warnings.push(format!(
                "arranca {} y vuelve a aplicar las condiciones",
                n.display()
            ));
            continue;
        }

        let result = match n.kind {
            NodeKind::Vbox => {
                if conditions.delay_ms > 0
                    || conditions.jitter_ms > 0
                    || conditions.loss_tenths > 0
                {
                    rep.warnings.push(format!(
                        "{}: VirtualBox solo aplica el límite de velocidad; se ignoran la latencia y la pérdida.",
                        n.display()
                    ));
                }
                let adapter = n.net.as_ref().map(|net| net.adapter).unwrap_or_else(default_adapter);
                vbox::set_bandwidth(tools, &n.target, adapter, conditions.rate_kbit).await
            }
            NodeKind::Docker => netem::apply_docker(tools, &n.target, conditions)
                .await
                .map(|_| None),
            NodeKind::Disk | NodeKind::Optical => Ok(None),
        };
        match result {
            Ok(Some(warning)) => rep.warnings.push(format!("{}: {warning}", n.display())),
            Ok(None) => {}
            Err(e) => rep.errors.push(format!("{}: {e}", n.display())),
        }
    }

    // Limpieza: servidores DHCP de redes gestionadas que el plan ya no usa, y
    // redes Docker sin contenedores. `in_use` protege las redes del plan actual.
    let in_use: HashSet<String> = plan.segments.iter().map(|s| s.name.clone()).collect();
    if tools.vboxmanage.is_some() {
        let _ = vbox::prune_orphan_dhcp(tools, &in_use).await;
    }
    if tools.docker.is_some() {
        let _ = docker::prune_networks(tools, &in_use).await;
    }
    rep
}

type OpticalChange = Pin<Box<dyn Future<Output = Result<()>> + Send>>;

/// Ejecuta los cambios en orden y devuelve un resultado por VM. Un fallo no
/// detiene los demás; por eso cada intento lleva ya nombre y UUID de su VM.
async fn collect_optical_iso_results(
    attempts: Vec<(String, String, OpticalChange)>,
) -> Vec<OpticalIsoResult> {
    let mut results = Vec::with_capacity(attempts.len());
    for (name, uuid, change) in attempts {
        let result = change.await;
        results.push(OpticalIsoResult {
            ok: result.is_ok(),
            error: result.err().map(|e| e.to_string()),
            name,
            uuid,
        });
    }
    results
}

/// Cambia o expulsa la ISO del nodo en TODAS las VMs que tengan esa unidad
/// gestionada (descubierta por el extradata `VMLab/Optical/<nodeId>`). `iso: None`
/// expulsa el medio sin borrar el fichero ni la unidad. Devuelve un resultado por
/// VM con nombre, UUID y éxito o error; un fallo en una VM no afecta a las demás.
pub async fn set_optical_iso(
    tools: &Tools,
    node_id: &str,
    iso: Option<&str>,
) -> Result<Vec<OpticalIsoResult>> {
    crate::validate::optical_node_id(node_id)?;
    let wanted = iso.filter(|s| !s.trim().is_empty());
    if let Some(iso) = wanted {
        crate::validate::vm_iso(iso)?;
    }

    let mut attempts: Vec<(String, String, OpticalChange)> = Vec::new();
    let mut results = Vec::new();
    for vm in vbox::list_basic(tools).await? {
        let slot = match vbox::read_optical_extradata(tools, &vm.uuid, node_id).await {
            Ok(slot) => slot,
            Err(e) => {
                results.push(OpticalIsoResult {
                    name: vm.name,
                    uuid: vm.uuid,
                    ok: false,
                    error: Some(e.to_string()),
                });
                continue;
            }
        };
        let Some(slot) = slot else { continue };
        let target = vm.uuid.clone();
        let tools = tools.clone();
        let desired = wanted.map(str::to_string);
        attempts.push((
            vm.name,
            vm.uuid,
            Box::pin(async move {
                let medium = desired
                    .as_deref()
                    .map_or(vbox::OpticalMedium::Empty, vbox::OpticalMedium::Iso);
                vbox::set_optical_medium(&tools, &target, &slot, medium, false).await
            }),
        ));
    }
    results.extend(collect_optical_iso_results(attempts).await);
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Edge, EdgeKind, OpticalSpec};

    fn attempt(name: &str, uuid: &str, ok: bool) -> (String, String, OpticalChange) {
        (
            name.into(),
            uuid.into(),
            Box::pin(async move {
                if ok {
                    Ok(())
                } else {
                    Err(CoreError::Message("fallo de prueba".into()))
                }
            }),
        )
    }

    #[tokio::test]
    async fn iso_change_reports_every_vm_and_continues_after_a_failure() {
        let results = collect_optical_iso_results(vec![
            attempt("una", "uuid-1", true),
            attempt("dos", "uuid-2", false),
            attempt("tres", "uuid-3", true),
        ])
        .await;
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].name, "una");
        assert_eq!(results[0].uuid, "uuid-1");
        assert!(results[0].ok);
        assert_eq!(results[0].error, None);
        assert!(!results[1].ok);
        assert!(results[1].error.as_deref().unwrap().contains("fallo de prueba"));
        assert!(results[2].ok);
    }

    #[test]
    fn removing_one_optical_link_does_not_affect_other_vms() {
        let optical = |id: &str| NodeSpec {
            id: id.into(),
            kind: NodeKind::Optical,
            target: id.into(),
            optical: Some(OpticalSpec { iso: None }),
            ..NodeSpec::default()
        };
        let machine = |id: &str| NodeSpec {
            id: id.into(),
            kind: NodeKind::Vbox,
            target: id.into(),
            ..NodeSpec::default()
        };
        let mount = |from: &str, to: &str| Edge {
            from: from.into(),
            to: to.into(),
            kind: EdgeKind::Mount,
        };

        // Desconectamos vm1: el plan conservado solo es el de vm2.
        let after = Topology {
            nodes: vec![machine("vm1"), machine("vm2"), optical("cd")],
            edges: vec![mount("cd", "vm2")],
        };
        let plans = optical::plan(&after).unwrap();
        let wanted_by_vm = optical_wanted_by_vm(&plans);
        let slot = vbox::OpticalSlot { controller: "SATA".into(), port: 0, device: 0 };
        let entries_vm1 = vec![("cd".to_string(), slot.clone())];
        let entries_vm2 = vec![("cd".to_string(), slot)];

        let none = HashSet::new();
        let remove_vm1 = optical_units_to_remove(&entries_vm1, &none);
        assert_eq!(remove_vm1.len(), 1);
        assert_eq!(remove_vm1[0].0, "cd");

        let wanted_vm2 = wanted_by_vm.get("vm2").cloned().unwrap_or_default();
        assert!(optical_units_to_remove(&entries_vm2, &wanted_vm2).is_empty());
    }

    #[test]
    fn optical_warnings_name_the_affected_vm() {
        let vm = NodeSpec {
            target: "uuid-1".into(),
            label: Some("Mi VM".into()),
            ..NodeSpec::default()
        };
        assert_eq!(optical_vm_warning(&vm, "no se pudo crear"), "Mi VM: no se pudo crear");
    }
}
