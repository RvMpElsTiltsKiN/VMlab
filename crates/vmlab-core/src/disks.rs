//! Planificación pura de los discos del lienzo (sin I/O).
//!
//! Cada disco es un volumen Docker `vmlab-vol-<nombre>` y declara en qué ruta se
//! monta en cada contenedor enlazado. La topología ya debe haber pasado por
//! `validate::disk_topology` antes de llegar aquí.
use crate::docker::MountInfo;
use crate::error::{CoreError, Result};
use crate::model::{NodeKind, NodeSpec, Topology};
use crate::validate;
use std::collections::{BTreeSet, HashMap};

/// Montaje de un disco en un contenedor concreto del lienzo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedMount {
    /// `id` del nodo Docker del lienzo.
    pub container_id: String,
    /// Nombre real del contenedor en Docker.
    pub container: String,
    /// Ruta normalizada dentro del contenedor.
    pub path: String,
}

/// Volumen y montajes derivados de un nodo Disco.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskPlan {
    pub node: NodeSpec,
    pub volume: String,
    pub mounts: Vec<PlannedMount>,
}

/// Traduce los discos de la topología a volúmenes y montajes.
pub fn plan(t: &Topology) -> Result<Vec<DiskPlan>> {
    let by_id: HashMap<&str, &NodeSpec> = t.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut plans = Vec::new();
    for n in t.nodes.iter().filter(|n| n.kind == NodeKind::Disk) {
        let volume = validate::volume_name(&n.target)?;
        let mut mounts = Vec::new();
        if let Some(spec) = n.disk.as_ref() {
            for m in &spec.mounts {
                let container = by_id.get(m.container.as_str()).ok_or_else(|| {
                    CoreError::Invalid(format!(
                        "el disco «{}» enlaza con «{}», que no está en el lienzo",
                        n.display(),
                        m.container
                    ))
                })?;
                mounts.push(PlannedMount {
                    container_id: m.container.clone(),
                    container: container.target.clone(),
                    path: validate::normalize_mount_path(&m.path),
                });
            }
        }
        mounts.sort_by(|a, b| a.container.cmp(&b.container).then(a.path.cmp(&b.path)));
        plans.push(DiskPlan { node: n.clone(), volume, mounts });
    }
    plans.sort_by(|a, b| a.volume.cmp(&b.volume));
    Ok(plans)
}

/// Montajes que debe tener cada contenedor, agrupados por nombre de contenedor.
pub fn mounts_by_container(plans: &[DiskPlan]) -> HashMap<&str, BTreeSet<MountInfo>> {
    let mut by_container: HashMap<&str, BTreeSet<MountInfo>> = HashMap::new();
    for p in plans {
        for m in &p.mounts {
            by_container.entry(m.container.as_str()).or_default().insert(MountInfo {
                volume: p.volume.clone(),
                path: m.path.clone(),
            });
        }
    }
    by_container
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DiskMount, DiskSpec, Edge, EdgeKind};

    #[test]
    fn plans_volumes_and_mounts_by_container_name() {
        let t = Topology {
            nodes: vec![
                NodeSpec {
                    id: "web-id".into(),
                    kind: NodeKind::Docker,
                    target: "web".into(),
                    ..NodeSpec::default()
                },
                NodeSpec {
                    id: "disk-id".into(),
                    kind: NodeKind::Disk,
                    target: "Datos DB".into(),
                    disk: Some(DiskSpec {
                        shared: true,
                        mounts: vec![DiskMount { container: "web-id".into(), path: "/data/".into() }],
                    }),
                    ..NodeSpec::default()
                },
            ],
            edges: vec![Edge {
                from: "disk-id".into(),
                to: "web-id".into(),
                kind: EdgeKind::Mount,
            }],
        };
        crate::validate::disk_topology(&t).unwrap();
        let plans = plan(&t).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].volume, "vmlab-vol-datos-db");
        assert_eq!(plans[0].mounts[0].container, "web");
        assert_eq!(plans[0].mounts[0].container_id, "web-id");
        assert_eq!(plans[0].mounts[0].path, "/data");

        let by_container = mounts_by_container(&plans);
        assert_eq!(
            by_container.get("web").unwrap().iter().next().unwrap(),
            &MountInfo { volume: "vmlab-vol-datos-db".into(), path: "/data".into() }
        );
    }

    #[test]
    fn a_topology_without_disks_has_no_plan() {
        let t = Topology::default();
        assert!(plan(&t).unwrap().is_empty());
        assert!(mounts_by_container(&plan(&t).unwrap()).is_empty());
    }
}
