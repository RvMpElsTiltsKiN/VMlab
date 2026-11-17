//! Instantáneas del laboratorio: guardan el estado de **todas** las máquinas del
//! lienzo bajo un mismo nombre y permiten restaurarlo o borrarlo.
//!
//! No confundir con `engine::snapshot`, que es el sondeo periódico del estado de
//! las máquinas. Aquí cada nodo aporta su propia instantánea (VirtualBox o Docker)
//! y se acumulan los resultados por nodo sin abortar la operación completa.
use crate::error::{CoreError, Result};
use crate::model::{LabSnapInfo, LabSnapNode, LabSnapReport, NodeKind};
use crate::tools::Tools;
use crate::{docker, validate, vbox};
use std::collections::{HashMap, HashSet};

// ───────────────────── Fecha UTC sin dependencias externas ─────────────────────

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Convierte días desde la época a `(año, mes, día)` (algoritmo de Hinnant).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { y + 1 } else { y }, month, day)
}

/// Inversa de [`civil_from_days`]: días desde la época para una fecha civil.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub(crate) fn parts_from_unix(secs: u64) -> (i64, u32, u32, u64, u64, u64) {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    (year, month, day, rem / 3_600, (rem % 3_600) / 60, rem % 60)
}

fn iso_from_unix(secs: u64) -> String {
    let (year, month, day, hour, minute, second) = parts_from_unix(secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn stamp_from_unix(secs: u64) -> String {
    let (year, month, day, hour, minute, second) = parts_from_unix(secs);
    format!("{year:04}{month:02}{day:02}{hour:02}{minute:02}{second:02}")
}

/// Convierte el `CreatedAt` de Docker (`2026-11-09 10:00:00 +0000 UTC`) a ISO UTC.
/// Si el formato no se reconoce, devuelve `None` para no inventar una fecha.
fn docker_created_at_iso(value: &str) -> Option<String> {
    let mut parts = value.split_whitespace();
    let date = parts.next()?;
    let time = parts.next()?;
    let offset = parts.next()?;

    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let mut time_parts = time.split(':');
    let hour: u32 = time_parts.next()?.parse().ok()?;
    let minute: u32 = time_parts.next()?.parse().ok()?;
    let seconds = time_parts.next()?.split('.').next()?;
    let second: u32 = seconds.parse().ok()?;
    if time_parts.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let sign = match offset.chars().next()? {
        '+' => 1i64,
        '-' => -1i64,
        _ => return None,
    };
    let digits: String = offset[1..].chars().filter(|c| *c != ':').collect();
    if digits.len() != 4 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let offset_hour: i64 = digits[0..2].parse().ok()?;
    let offset_minute: i64 = digits[2..4].parse().ok()?;
    if offset_hour > 23 || offset_minute > 59 {
        return None;
    }

    let local = days_from_civil(year, month, day) * 86_400
        + hour as i64 * 3_600
        + minute as i64 * 60
        + second as i64;
    let utc = local - sign * (offset_hour * 3_600 + offset_minute * 60);
    (utc >= 0).then(|| iso_from_unix(utc as u64))
}

/// Fecha ISO UTC que se guarda en la descripción de VirtualBox.
pub(crate) fn utc_now_iso() -> String {
    iso_from_unix(now_secs())
}

/// Marca de tiempo `AAAAMMDDHHMMSS` para el nombre del contenedor original.
pub(crate) fn utc_now_stamp() -> String {
    stamp_from_unix(now_secs())
}

// ───────────────────── Nodos y consultas ─────────────────────

fn node_key(node: &LabSnapNode) -> String {
    node.kind.key(&node.target)
}

fn assigned_nodes(nodes: &[LabSnapNode]) -> Vec<LabSnapNode> {
    nodes
        .iter()
        .filter(|n| !n.target.trim().is_empty())
        .cloned()
        .collect()
}

/// Instantáneas de un nodo. Un fallo de la herramienta se trata como «no hay»
/// para que el listado pueda seguir con el resto de nodos del lienzo.
async fn snapshots_of(tools: &Tools, node: &LabSnapNode) -> Vec<(String, String, Option<String>)> {
    match node.kind {
        NodeKind::Vbox if tools.vboxmanage.is_some() => {
            vbox::snapshot_list(tools, &node.target).await.unwrap_or_default()
        }
        NodeKind::Docker if tools.docker.is_some() => {
            docker::snapshot_list(tools, &node.target).await.unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

struct Accumulated {
    label: String,
    created_at: Option<String>,
    present: HashSet<String>,
}

/// Lista las instantáneas del laboratorio agrupadas por id, indicando qué nodos
/// del lienzo la tienen y cuáles no.
pub async fn list(tools: &Tools, nodes: &[LabSnapNode]) -> Vec<LabSnapInfo> {
    let mut universe: Vec<(String, &LabSnapNode)> = Vec::new();
    let mut seen = HashSet::new();
    for node in nodes.iter().filter(|n| !n.target.trim().is_empty()) {
        let key = node_key(node);
        if seen.insert(key.clone()) {
            universe.push((key, node));
        }
    }

    let mut by_id: HashMap<String, Accumulated> = HashMap::new();
    for node in nodes.iter().filter(|n| !n.target.trim().is_empty()) {
        let key = node_key(node);
        for (id, label, created_at) in snapshots_of(tools, node).await {
            let created_at = match node.kind {
                NodeKind::Docker => created_at.and_then(|v| docker_created_at_iso(&v)),
                NodeKind::Vbox | NodeKind::Disk => created_at,
            };
            let entry = by_id.entry(id).or_insert_with(|| Accumulated {
                label: label.clone(),
                created_at: created_at.clone(),
                present: HashSet::new(),
            });
            if entry.label.is_empty() {
                entry.label = label;
            }
            if entry.created_at.is_none() {
                entry.created_at = created_at;
            }
            entry.present.insert(key.clone());
        }
    }

    let mut infos: Vec<LabSnapInfo> = by_id
        .into_iter()
        .map(|(id, acc)| {
            let mut present = Vec::new();
            let mut missing = Vec::new();
            for (key, node) in &universe {
                if acc.present.contains(key) {
                    present.push(node.display().to_string());
                } else {
                    missing.push(node.display().to_string());
                }
            }
            LabSnapInfo { id, label: acc.label, created_at: acc.created_at, present, missing }
        })
        .collect();

    // Las más recientes primero; las que no tienen fecha, al final.
    infos.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
            .then_with(|| a.id.cmp(&b.id))
    });
    infos
}

/// Guarda una instantánea con el mismo id en todos los nodos del lienzo.
///
/// El id se deriva del nombre visible. Antes de tocar nada se comprueba que no
/// exista ya en ningún nodo.
pub async fn take(tools: &Tools, nodes: &[LabSnapNode], label: &str) -> Result<LabSnapReport> {
    let label = label.trim();
    validate::labsnap_label(label)?;
    let id = validate::labsnap_slug(label);
    validate::labsnap_id(&id)
        .map_err(|_| CoreError::Invalid("el nombre debe incluir al menos una letra o un número".into()))?;

    let active = assigned_nodes(nodes);
    if active.is_empty() {
        return Err(CoreError::Invalid("no hay máquinas asignadas en el lienzo".into()));
    }
    if list(tools, nodes).await.iter().any(|snap| snap.id == id) {
        return Err(CoreError::Message("ya existe una instantánea con ese nombre".into()));
    }

    let mut report = LabSnapReport::default();
    if active.iter().any(|n| n.kind == NodeKind::Docker) {
        report
            .warnings
            .push("Los volúmenes de Docker no se incluyen en la instantánea.".into());
    }

    for node in &active {
        let result = match node.kind {
            NodeKind::Vbox => vbox::snapshot_take(tools, &node.target, &id, label).await,
            NodeKind::Docker => docker::snapshot_take(tools, &node.target, &id, label).await,
            NodeKind::Disk => Err(CoreError::Invalid("los discos no admiten instantáneas".into())),
        };
        match result {
            Ok(()) => report.done += 1,
            Err(e) => report.errors.push(format!("{}: {e}", node.display())),
        }
    }
    Ok(report)
}

/// Decide qué nodos se restauran, cuáles están bloqueados por estar su VM en
/// marcha y cuáles no tienen la instantánea. Es una función pura para poder
/// probar la planificación sin lanzar procesos.
pub fn plan_restore(
    running: &HashSet<String>,
    nodes: &[LabSnapNode],
    available: &HashSet<String>,
) -> (Vec<LabSnapNode>, Vec<LabSnapNode>, Vec<LabSnapNode>) {
    let mut restore = Vec::new();
    let mut blocked = Vec::new();
    let mut missing = Vec::new();

    for node in nodes.iter().filter(|n| !n.target.trim().is_empty()) {
        if !available.contains(&node_key(node)) {
            missing.push(node.clone());
        } else if node.kind == NodeKind::Vbox && running.contains(&node.target) {
            blocked.push(node.clone());
        } else {
            restore.push(node.clone());
        }
    }
    (restore, blocked, missing)
}

/// Mensajes de error para las VM que impiden restaurar por estar encendidas.
fn blocked_errors(blocked: &[LabSnapNode]) -> Vec<String> {
    blocked
        .iter()
        .map(|node| format!("apaga la VM «{}» para restaurar", node.display()))
        .collect()
}

/// Restaura la instantánea en todos los nodos que la tengan. Las VM encendidas
/// no se tocan; en Docker se para y renombra el contenedor original.
pub async fn restore(tools: &Tools, nodes: &[LabSnapNode], id: &str) -> Result<LabSnapReport> {
    validate::labsnap_id(id)?;
    let active = assigned_nodes(nodes);
    if active.is_empty() {
        return Err(CoreError::Invalid("no hay máquinas asignadas en el lienzo".into()));
    }

    let running = if tools.vboxmanage.is_some() {
        vbox::running_keys(tools).await.unwrap_or_default()
    } else {
        HashSet::new()
    };
    let mut available = HashSet::new();
    for node in &active {
        if snapshots_of(tools, node).await.iter().any(|(snap_id, _, _)| snap_id == id) {
            available.insert(node_key(node));
        }
    }

    let (to_restore, blocked, missing) = plan_restore(&running, &active, &available);
    let mut report = LabSnapReport::default();
    for node in &missing {
        report
            .warnings
            .push(format!("«{}» no tiene la instantánea «{id}»", node.display()));
    }
    // Si hay alguna VM encendida, no se toca ningún nodo: el usuario debe apagarla
    // y volver a lanzar la restauración.
    if !blocked.is_empty() {
        report.errors.extend(blocked_errors(&blocked));
        return Ok(report);
    }

    let mut docker_restored = false;
    for node in &to_restore {
        let result = match node.kind {
            NodeKind::Vbox => vbox::snapshot_restore(tools, &node.target, id).await.map(|_| None),
            NodeKind::Docker => docker::snapshot_restore(tools, &node.target, id).await,
            NodeKind::Disk => Err(CoreError::Invalid("los discos no admiten instantáneas".into())),
        };
        match result {
            Ok(renamed) => {
                report.done += 1;
                if node.kind == NodeKind::Docker {
                    docker_restored = true;
                    if let Some(name) = renamed {
                        report.warnings.push(format!(
                            "El contenedor original «{}» se conserva como «{name}».",
                            node.display()
                        ));
                    }
                }
            }
            Err(e) => report.errors.push(format!("{}: {e}", node.display())),
        }
    }

    if docker_restored {
        report
            .warnings
            .push("Pulsa «Aplicar red» para reconectar los contenedores restaurados".into());
    }
    if report.done > 0 {
        report
            .warnings
            .push("Vuelve a aplicar la topología: hay que reaplicar las condiciones de red.".into());
    }
    Ok(report)
}

/// Borra la instantánea de todos los nodos que la tengan.
pub async fn delete(tools: &Tools, nodes: &[LabSnapNode], id: &str) -> Result<LabSnapReport> {
    validate::labsnap_id(id)?;
    let active = assigned_nodes(nodes);
    if active.is_empty() {
        return Err(CoreError::Invalid("no hay máquinas asignadas en el lienzo".into()));
    }

    let mut report = LabSnapReport::default();
    for node in &active {
        if !snapshots_of(tools, node).await.iter().any(|(snap_id, _, _)| snap_id == id) {
            report
                .warnings
                .push(format!("«{}» no tiene la instantánea «{id}»", node.display()));
            continue;
        }
        let result = match node.kind {
            NodeKind::Vbox => vbox::snapshot_delete(tools, &node.target, id).await,
            NodeKind::Docker => docker::snapshot_delete(tools, &node.target, id).await,
            NodeKind::Disk => Err(CoreError::Invalid("los discos no admiten instantáneas".into())),
        };
        match result {
            Ok(()) => report.done += 1,
            Err(e) => report.errors.push(format!("{}: {e}", node.display())),
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(kind: NodeKind, target: &str, label: &str) -> LabSnapNode {
        LabSnapNode { kind, target: target.into(), label: Some(label.into()) }
    }

    #[test]
    fn formats_utc_dates_without_external_crates() {
        assert_eq!(iso_from_unix(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_from_unix(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(stamp_from_unix(1_700_000_000), "20231114221320");
    }

    #[test]
    fn plans_restore_and_skips_running_vms_and_missing_snapshots() {
        let nodes = vec![
            node(NodeKind::Vbox, "vm-on", "VM encendida"),
            node(NodeKind::Vbox, "vm-off", "VM apagada"),
            node(NodeKind::Docker, "web", "web"),
            node(NodeKind::Vbox, "vm-missing", "VM sin instantánea"),
        ];
        let running: HashSet<String> = ["vm-on".to_string()].into_iter().collect();
        let available: HashSet<String> = [
            "vbox:vm-on".to_string(),
            "vbox:vm-off".to_string(),
            "docker:web".to_string(),
        ]
        .into_iter()
        .collect();

        let (restore, blocked, missing) = plan_restore(&running, &nodes, &available);
        assert_eq!(
            restore.iter().map(LabSnapNode::display).collect::<Vec<_>>(),
            vec!["VM apagada", "web"]
        );
        assert_eq!(
            blocked.iter().map(LabSnapNode::display).collect::<Vec<_>>(),
            vec!["VM encendida"]
        );
        assert_eq!(
            missing.iter().map(LabSnapNode::display).collect::<Vec<_>>(),
            vec!["VM sin instantánea"]
        );
    }

    #[test]
    fn aborts_the_restore_plan_when_any_vm_is_running() {
        let nodes = vec![
            node(NodeKind::Vbox, "vm-on", "VM encendida"),
            node(NodeKind::Docker, "web", "web"),
        ];
        let running: HashSet<String> = ["vm-on".to_string()].into_iter().collect();
        let available: HashSet<String> = [
            "vbox:vm-on".to_string(),
            "docker:web".to_string(),
        ]
        .into_iter()
        .collect();

        let (_, blocked, missing) = plan_restore(&running, &nodes, &available);
        assert_eq!(missing.len(), 0);
        assert_eq!(
            blocked_errors(&blocked),
            vec!["apaga la VM «VM encendida» para restaurar".to_string()]
        );
    }

    #[test]
    fn converts_docker_dates_to_utc_iso() {
        assert_eq!(
            docker_created_at_iso("2026-11-09 10:00:00 +0000 UTC"),
            Some("2026-11-09T10:00:00Z".to_string())
        );
        assert_eq!(
            docker_created_at_iso("2026-11-09 08:00:00.123456789 -0200 UTC"),
            Some("2026-11-09T10:00:00Z".to_string())
        );
        assert_eq!(
            docker_created_at_iso("2026-11-09 00:30:00 +0530 UTC"),
            Some("2026-11-08T19:00:00Z".to_string())
        );
        assert_eq!(docker_created_at_iso("2026-11-09 10:00:00"), None);
        assert_eq!(docker_created_at_iso("ayer"), None);
        assert_eq!(docker_created_at_iso("2026-13-09 10:00:00 +0000 UTC"), None);
        assert_eq!(docker_created_at_iso("2026-11-09 10:00:00 +0000 UTC extra"), Some("2026-11-09T10:00:00Z".to_string()));
    }
}
