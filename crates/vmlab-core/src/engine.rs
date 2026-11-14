//! Orquestación: combina `vbox`, `docker` y `topology`. Es la única API que usa la capa Tauri.
use crate::error::{CoreError, Result};
use crate::model::*;
use crate::tools::Tools;
use crate::{connectivity, docker, docker_desktop, guest, netem, topology, vbox};
use std::collections::{HashMap, HashSet};

fn vbox_state(running: bool) -> PowerState {
    if running { PowerState::Running } else { PowerState::Stopped }
}

/// Crea (sin arrancar) el contenedor de un nodo Docker del segmento cuando todavía
/// no existe y el nodo tiene imagen. Sin imagen no se toca nada.
async fn create_missing(tools: &Tools, n: &NodeSpec, network: &str) -> Result<()> {
    let Some(image) = n.image.as_deref().filter(|i| !i.is_empty()) else {
        return Ok(());
    };
    if docker::exists(tools, &n.target).await? {
        return Ok(());
    }
    docker::create_on_network(tools, &n.target, image, n.options.as_ref(), network).await
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
    let plan = topology::plan(t);
    let mut rep = ApplyReport { warnings: plan.warnings.clone(), ..Default::default() };
    let running: HashSet<String> = if tools.vboxmanage.is_some() {
        vbox::running_keys(tools).await.unwrap_or_default()
    } else {
        HashSet::new()
    };

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
                    let ready = if docker_net_ready { Ok(()) } else { docker::ensure_network(tools, &seg.name).await };
                    docker_net_ready = ready.is_ok();
                    match ready {
                        Ok(()) => match create_missing(tools, n, &seg.name).await {
                            Ok(()) => docker::set_networks(tools, &n.target, Some(&seg.name)).await,
                            Err(e) => Err(e),
                        },
                        Err(e) => Err(e),
                    }
                }
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
            NodeKind::Docker => docker::set_networks(tools, &n.target, None).await,
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
