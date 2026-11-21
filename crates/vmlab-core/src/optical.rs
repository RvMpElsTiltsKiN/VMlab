//! Planificación pura de las unidades ópticas del lienzo (sin I/O).
//!
//! Una unidad no es una máquina: cada enlace `Mount` con una VM de VirtualBox
//! crea una unidad propia en esa VM. Un mismo nodo puede conectarse a varias VMs
//! y todas comparten la ISO del nodo (solo lectura). La ISO se valida antes de
//! llegar aquí.
use crate::error::{CoreError, Result};
use crate::model::{EdgeKind, NodeKind, NodeSpec, Topology};
use crate::validate;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpticalPlan {
    /// Nodo `Optical` del lienzo (su `id` es la clave de extradata).
    pub node: NodeSpec,
    /// `id` del nodo VM del lienzo.
    pub vm_id: String,
    /// VM real a la que pertenece la unidad.
    pub vm: NodeSpec,
    /// ISO deseada; `None` deja la unidad vacía.
    pub iso: Option<String>,
}

/// Extrae una unidad por cada enlace `Mount` con una VM. Las unidades sin enlace
/// no crean nada, pero `optical_topology` ya ha validado todas las ISOs y todos
/// los enlaces. Un mismo nodo puede producir varios planes (uno por VM).
pub fn plan(t: &Topology) -> Result<Vec<OpticalPlan>> {
    validate::optical_topology(t)?;
    let by_id: HashMap<&str, &NodeSpec> = t.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut planned = Vec::new();
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    for node in t.nodes.iter().filter(|n| n.kind == NodeKind::Optical) {
        let iso = node
            .optical
            .as_ref()
            .and_then(|spec| spec.iso.as_deref())
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string);
        for edge in t.edges.iter().filter(|e| {
            e.kind == EdgeKind::Mount && (e.from == node.id || e.to == node.id)
        }) {
            let vm_id = if edge.from == node.id { edge.to.as_str() } else { edge.from.as_str() };
            let vm = by_id.get(vm_id).ok_or_else(|| {
                CoreError::Invalid(format!(
                    "la unidad «{}» enlaza con «{vm_id}», que no está en el lienzo",
                    node.display()
                ))
            })?;
            // Un enlace duplicado no debe crear dos unidades en la misma VM.
            if !seen.insert((node.id.as_str(), vm.id.as_str())) {
                continue;
            }
            planned.push(OpticalPlan {
                node: node.clone(),
                vm_id: vm.id.clone(),
                vm: (*vm).clone(),
                iso: iso.clone(),
            });
        }
    }
    Ok(planned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Edge, OpticalSpec};

    fn optical(id: &str, iso: Option<&str>) -> NodeSpec {
        NodeSpec {
            id: id.into(),
            kind: NodeKind::Optical,
            target: id.into(),
            optical: Some(OpticalSpec { iso: iso.map(str::to_string) }),
            ..NodeSpec::default()
        }
    }
    fn machine(id: &str, kind: NodeKind) -> NodeSpec {
        NodeSpec { id: id.into(), kind, target: id.into(), ..NodeSpec::default() }
    }
    fn mount(a: &str, b: &str) -> Edge {
        Edge { from: a.into(), to: b.into(), kind: EdgeKind::Mount }
    }

    #[test]
    fn plans_one_drive_per_vm_and_ignores_unconnected_drives() {
        let t = Topology {
            nodes: vec![
                machine("vm", NodeKind::Vbox),
                optical("cd", None),
                optical("suelta", None),
            ],
            edges: vec![mount("cd", "vm")],
        };
        let plans = plan(&t).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].node.id, "cd");
        assert_eq!(plans[0].vm_id, "vm");
        assert_eq!(plans[0].iso, None);
    }

    #[test]
    fn connects_in_either_direction_and_shares_an_iso() {
        let iso = None; // La existencia del fichero se comprueba en `validate::vm_iso`.
        let t = Topology {
            nodes: vec![machine("vm1", NodeKind::Vbox), machine("vm2", NodeKind::Vbox),
                optical("cd1", iso), optical("cd2", iso)],
            edges: vec![mount("vm1", "cd1"), mount("cd2", "vm2")],
        };
        let plans = plan(&t).unwrap();
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].vm_id, "vm1");
        assert_eq!(plans[1].vm_id, "vm2");
    }

    #[test]
    fn the_same_iso_can_be_mounted_in_two_drives() {
        let path = std::env::temp_dir().join(format!("vmlab-shared-iso-{}.iso", std::process::id()));
        std::fs::write(&path, b"ISO").unwrap();
        let iso = path.to_string_lossy().into_owned();
        let t = Topology {
            nodes: vec![
                machine("vm1", NodeKind::Vbox),
                machine("vm2", NodeKind::Vbox),
                optical("cd1", Some(iso.as_str())),
                optical("cd2", Some(iso.as_str())),
            ],
            edges: vec![mount("cd1", "vm1"), mount("cd2", "vm2")],
        };
        let plans = plan(&t).unwrap();
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].iso.as_deref(), Some(iso.as_str()));
        assert_eq!(plans[1].iso.as_deref(), Some(iso.as_str()));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn rejects_docker_but_accepts_a_second_vm() {
        let docker = Topology {
            nodes: vec![machine("web", NodeKind::Docker), optical("cd", None)],
            edges: vec![mount("cd", "web")],
        };
        assert!(plan(&docker).is_err());

        let two_vms = Topology {
            nodes: vec![machine("vm1", NodeKind::Vbox), machine("vm2", NodeKind::Vbox),
                optical("cd", None)],
            edges: vec![mount("cd", "vm1"), mount("cd", "vm2")],
        };
        assert!(plan(&two_vms).is_ok());
    }

    #[test]
    fn plans_one_unit_per_vm_for_the_same_optical_node() {
        let t = Topology {
            nodes: vec![
                machine("vm1", NodeKind::Vbox),
                machine("vm2", NodeKind::Vbox),
                machine("vm3", NodeKind::Vbox),
                optical("cd", None),
            ],
            edges: vec![mount("cd", "vm1"), mount("vm2", "cd"), mount("cd", "vm3")],
        };
        let plans = plan(&t).unwrap();
        assert_eq!(plans.len(), 3);
        let vms: Vec<&str> = plans.iter().map(|p| p.vm_id.as_str()).collect();
        assert_eq!(vms, vec!["vm1", "vm2", "vm3"]);
        assert!(plans.iter().all(|p| p.node.id == "cd"));
    }

    #[test]
    fn duplicate_links_create_only_one_unit_per_pair() {
        let t = Topology {
            nodes: vec![machine("vm", NodeKind::Vbox), optical("cd", None)],
            edges: vec![mount("cd", "vm"), mount("vm", "cd")],
        };
        assert_eq!(plan(&t).unwrap().len(), 1);
    }

    #[test]
    fn rejects_network_links_and_links_between_drives() {
        let network = Topology {
            nodes: vec![machine("vm", NodeKind::Vbox), optical("cd", None)],
            edges: vec![Edge { from: "cd".into(), to: "vm".into(), kind: EdgeKind::Network }],
        };
        assert!(plan(&network).is_err());

        let two_drives = Topology {
            nodes: vec![machine("vm", NodeKind::Vbox), optical("cd1", None), optical("cd2", None)],
            edges: vec![mount("cd1", "cd2")],
        };
        assert!(plan(&two_drives).is_err());
    }
}
