//! Planificación pura de los discos del lienzo (sin I/O).
//!
//! Un disco enlazado a contenedores es un volumen Docker
//! `vmlab-vol-<nombre>`; uno enlazado a una VM es un VDI
//! `vmlab-disk-<nombre>.vdi` dentro de la carpeta que elige el backend.
//! La topología ya debe haber pasado por `validate::disk_topology` antes de
//! llegar aquí.
use crate::docker::MountInfo;
use crate::error::{CoreError, Result};
use crate::model::{NodeKind, NodeSpec, Topology};
use crate::validate::{self, DiskKind};
use std::collections::{BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};

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

/// VDI que debe existir y conectarse a una VM del lienzo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VdiPlan {
    pub node: NodeSpec,
    /// `id` del nodo VirtualBox del lienzo.
    pub vm_id: String,
    /// UUID o nombre real de la VM.
    pub vm: String,
    /// Ruta decidida por el backend, siempre dentro de su carpeta de discos.
    pub path: PathBuf,
    pub size_mb: u32,
}

/// Normaliza una ruta sin tocar el sistema de ficheros (para comparar prefijos).
fn lexical_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::Prefix(_) | Component::RootDir => out.push(c.as_os_str()),
            Component::Normal(part) => out.push(part),
        }
    }
    out
}

/// `true` solo si `path` es un fichero directo dentro de `disks_dir`.
pub fn path_in_disks_dir(disks_dir: &Path, path: &Path) -> bool {
    let dir = lexical_path(disks_dir);
    let file = lexical_path(path);
    file.parent() == Some(dir.as_path())
}

/// Comprueba que un medio pueda ser gestionado por VMLab: debe estar directamente
/// en `disks_dir` y llamarse `vmlab-disk-<nombre saneado>.vdi`.
pub fn vdi_path_allowed(disks_dir: &Path, path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    const PREFIX: &str = "vmlab-disk-";
    const SUFFIX: &str = ".vdi";
    if !name.starts_with(PREFIX) || !name.ends_with(SUFFIX) || name.len() <= PREFIX.len() + SUFFIX.len() {
        return false;
    }
    let base = &name[PREFIX.len()..name.len() - SUFFIX.len()];
    validate::disk_name(base).is_ok() && path_in_disks_dir(disks_dir, path)
}

/// Nombre del fichero VDI derivado del nombre visible del disco.
pub fn vdi_filename(name: &str) -> Result<String> {
    let clean = validate::sanitize_disk_name(name);
    validate::disk_name(&clean)?;
    Ok(format!("vmlab-disk-{clean}.vdi"))
}

/// Ruta completa del VDI. El frontend nunca aporta rutas: `disks_dir` lo decide
/// el backend y se comprueba que el resultado no pueda salir de esa carpeta.
pub fn vdi_path(disks_dir: &Path, name: &str) -> Result<PathBuf> {
    let path = disks_dir.join(vdi_filename(name)?);
    if !vdi_path_allowed(disks_dir, &path) {
        return Err(CoreError::Invalid("la ruta del disco VDI queda fuera de la carpeta de discos".into()));
    }
    Ok(path)
}

/// Traduce los discos de la topología a volúmenes y montajes Docker.
/// Los discos de VM y los discos sin enlaces no producen volumen.
pub fn plan(t: &Topology) -> Result<Vec<DiskPlan>> {
    let by_id: HashMap<&str, &NodeSpec> = t.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut plans = Vec::new();
    for n in t.nodes.iter().filter(|n| n.kind == NodeKind::Disk) {
        if validate::disk_kind(t, n)? != DiskKind::Docker {
            continue;
        }
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
                if container.kind != NodeKind::Docker {
                    return Err(CoreError::Invalid(format!(
                        "el disco «{}» no es un disco de contenedores",
                        n.display()
                    )));
                }
                let path = m.path.as_deref().filter(|p| !p.is_empty()).ok_or_else(|| {
                    CoreError::Invalid(format!(
                        "el disco «{}» necesita una ruta de montaje para «{}»",
                        n.display(),
                        container.display()
                    ))
                })?;
                mounts.push(PlannedMount {
                    container_id: m.container.clone(),
                    container: container.target.clone(),
                    path: validate::normalize_mount_path(path),
                });
            }
        }
        mounts.sort_by(|a, b| a.container.cmp(&b.container).then(a.path.cmp(&b.path)));
        plans.push(DiskPlan { node: n.clone(), volume, mounts });
    }
    plans.sort_by(|a, b| a.volume.cmp(&b.volume));
    Ok(plans)
}

/// Traduce los discos enlazados a VMs a VDI con su ruta y tamaño efectivos.
pub fn plan_vdi(t: &Topology, disks_dir: &Path) -> Result<Vec<VdiPlan>> {
    let by_id: HashMap<&str, &NodeSpec> = t.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut plans = Vec::new();
    for n in t.nodes.iter().filter(|n| n.kind == NodeKind::Disk) {
        if validate::disk_kind(t, n)? != DiskKind::Vdi {
            continue;
        }
        let spec = n.disk.as_ref().expect("disk_kind ya comprobó la configuración");
        let mount = spec.mounts.first().ok_or_else(|| {
            CoreError::Invalid(format!("el disco «{}» no declara su enlace con la VM", n.display()))
        })?;
        let vm = by_id.get(mount.container.as_str()).ok_or_else(|| {
            CoreError::Invalid(format!(
                "el disco «{}» enlaza con «{}», que no está en el lienzo",
                n.display(),
                mount.container
            ))
        })?;
        if vm.kind != NodeKind::Vbox {
            return Err(CoreError::Invalid(format!(
                "el disco «{}» no es un disco de máquina virtual",
                n.display()
            )));
        }
        validate::vbox_target(&vm.target)?;
        plans.push(VdiPlan {
            node: n.clone(),
            vm_id: mount.container.clone(),
            vm: vm.target.clone(),
            path: vdi_path(disks_dir, &n.target)?,
            size_mb: spec.size_mb.unwrap_or(validate::DEFAULT_VDI_SIZE_MB),
        });
    }
    plans.sort_by(|a, b| a.path.cmp(&b.path));
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
                        mounts: vec![DiskMount {
                            container: "web-id".into(),
                            path: Some("/data/".into()),
                        }],
                        size_mb: None,
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
    fn vdi_names_and_paths_stay_inside_the_disks_folder() {
        assert_eq!(vdi_filename("Datos DB").unwrap(), "vmlab-disk-datos-db.vdi");
        assert_eq!(vdi_filename("--Datos..DB--").unwrap(), "vmlab-disk-datos..db.vdi");
        assert!(vdi_filename("###").is_err());

        let dir = Path::new(r"C:\Users\lab\AppData\Roaming\VMLab\disks");
        let good = dir.join("vmlab-disk-datos.vdi");
        assert_eq!(vdi_path(dir, "Datos").unwrap(), good);
        assert!(vdi_path_allowed(dir, &good));
        assert!(!vdi_path_allowed(dir, &dir.join("../fuera.vdi")));
        assert!(!vdi_path_allowed(dir, &dir.join("otro.vdi")));
        assert!(!vdi_path_allowed(dir, &dir.join("vmlab-disk-datos.vmdk")));
        assert!(!vdi_path_allowed(dir, &dir.join("vmlab-disk-Datos.vdi")));
        assert!(!vdi_path_allowed(dir, &dir.join("sub/vmlab-disk-datos.vdi")));
        assert!(!vdi_path_allowed(Path::new(r"C:\Users\lab\AppData\Roaming\VMLab\disks2"), &good));
    }

    #[test]
    fn plans_only_the_storage_that_is_created() {
        let t = Topology {
            nodes: vec![
                NodeSpec {
                    id: "web-id".into(),
                    kind: NodeKind::Docker,
                    target: "web".into(),
                    ..NodeSpec::default()
                },
                NodeSpec {
                    id: "vm-id".into(),
                    kind: NodeKind::Vbox,
                    target: "Ubuntu Lab".into(),
                    ..NodeSpec::default()
                },
                NodeSpec {
                    id: "docker-disk".into(),
                    kind: NodeKind::Disk,
                    target: "Datos".into(),
                    disk: Some(DiskSpec {
                        mounts: vec![DiskMount { container: "web-id".into(), path: Some("/data".into()) }],
                        ..DiskSpec::default()
                    }),
                    ..NodeSpec::default()
                },
                NodeSpec {
                    id: "vdi-disk".into(),
                    kind: NodeKind::Disk,
                    target: "Sistema Extra".into(),
                    disk: Some(DiskSpec {
                        mounts: vec![DiskMount { container: "vm-id".into(), path: None }],
                        size_mb: Some(20_480),
                        ..DiskSpec::default()
                    }),
                    ..NodeSpec::default()
                },
                NodeSpec {
                    id: "empty-disk".into(),
                    kind: NodeKind::Disk,
                    target: "Sin enlaces".into(),
                    disk: Some(DiskSpec::default()),
                    ..NodeSpec::default()
                },
            ],
            edges: vec![
                Edge { from: "docker-disk".into(), to: "web-id".into(), kind: EdgeKind::Mount },
                Edge { from: "vdi-disk".into(), to: "vm-id".into(), kind: EdgeKind::Mount },
            ],
        };
        crate::validate::disk_topology(&t).unwrap();

        let plans = plan(&t).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].volume, "vmlab-vol-datos");

        let dir = Path::new(r"C:\Users\lab\AppData\Roaming\VMLab\disks");
        let vdis = plan_vdi(&t, dir).unwrap();
        assert_eq!(vdis.len(), 1);
        assert_eq!(vdis[0].vm_id, "vm-id");
        assert_eq!(vdis[0].vm, "Ubuntu Lab");
        assert_eq!(vdis[0].size_mb, 20_480);
        assert_eq!(vdis[0].path, dir.join("vmlab-disk-sistema-extra.vdi"));
    }

    #[test]
    fn a_vm_disk_without_size_uses_the_default() {
        let t = Topology {
            nodes: vec![
                NodeSpec { id: "vm".into(), kind: NodeKind::Vbox, target: "vm".into(), ..NodeSpec::default() },
                NodeSpec {
                    id: "d".into(),
                    kind: NodeKind::Disk,
                    target: "datos".into(),
                    disk: Some(DiskSpec {
                        mounts: vec![DiskMount { container: "vm".into(), path: None }],
                        ..DiskSpec::default()
                    }),
                    ..NodeSpec::default()
                },
            ],
            edges: vec![Edge { from: "d".into(), to: "vm".into(), kind: EdgeKind::Mount }],
        };
        let vdis = plan_vdi(&t, Path::new("/tmp/vmlab-disks")).unwrap();
        assert_eq!(vdis[0].size_mb, validate::DEFAULT_VDI_SIZE_MB);
    }

    #[test]
    fn a_topology_without_disks_has_no_plan() {
        let t = Topology::default();
        assert!(plan(&t).unwrap().is_empty());
        assert!(mounts_by_container(&plan(&t).unwrap()).is_empty());
        assert!(plan_vdi(&t, Path::new("/tmp/vmlab-disks")).unwrap().is_empty());
    }
}
