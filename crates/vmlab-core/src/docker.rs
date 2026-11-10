//! Control de contenedores con la CLI de Docker.
//! Cada segmento de red de VMLab es una red *bridge* de Docker llamada `vmlab-xxxxxxxx`.
use crate::error::{CoreError, Result};
use crate::model::{Action, LinkInfo, PowerState};
use crate::runner::{run, run_ok};
use crate::tools::Tools;
use crate::validate;
use std::time::Duration;

const SHORT: Duration = Duration::from_secs(20);
const LONG: Duration = Duration::from_secs(300); // `docker run` puede tener que descargar la imagen

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerInfo {
    pub name: String,
    pub image: String,
    pub state: PowerState,
}

pub fn map_state(s: &str) -> PowerState {
    match s.trim() {
        "running" | "restarting" => PowerState::Running,
        "paused" => PowerState::Paused,
        "exited" | "created" | "dead" | "removing" => PowerState::Stopped,
        _ => PowerState::Unknown,
    }
}

/// Parsea la salida de `docker ps -a --format "{{.Names}}\t{{.Image}}\t{{.State}}"`.
pub fn parse_ps(out: &str) -> Vec<ContainerInfo> {
    out.lines()
        .filter_map(|l| {
            let mut p = l.split('\t');
            let (name, image, state) = (p.next()?, p.next()?, p.next()?);
            (!name.is_empty()).then(|| ContainerInfo { name: name.into(), image: image.into(), state: map_state(state) })
        })
        .collect()
}

/// `true` si el mensaje de error significa que el motor no está escuchando
/// (Docker Desktop cerrado, WSL detenido o pipe/socket inexistente).
pub fn engine_is_down(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    [
        "failed to connect to the docker api",
        "cannot connect to the docker daemon",
        "is the docker daemon running",
        "error during connect",
        "dockerdesktoplinuxengine",
        "pipe/docker_engine",
        "docker.sock",
    ]
    .iter()
    .any(|needle| m.contains(needle))
}

pub async fn list(tools: &Tools) -> Result<Vec<ContainerInfo>> {
    let out = run_ok(tools.docker()?, &["ps", "-a", "--format", "{{.Names}}\t{{.Image}}\t{{.State}}"], SHORT).await?;
    Ok(parse_ps(&out.stdout))
}

/// Pares `red=IP` de `docker inspect` (separados por `;`).
fn parse_networks(out: &str) -> Vec<(String, String)> {
    out.split(';')
        .filter_map(|pair| {
            let (name, ip) = pair.trim().split_once('=')?;
            (!name.is_empty()).then(|| (name.to_string(), ip.trim().to_string()))
        })
        .collect()
}

fn vmlab_of(nets: &[(String, String)]) -> Option<(String, String)> {
    // Durante la migración puede haber redes antiguas `lablab-*`; se prefieren las nuevas.
    let pick = nets
        .iter()
        .find(|(net, ip)| net.starts_with("vmlab-") && !ip.is_empty())
        .or_else(|| nets.iter().find(|(net, ip)| net.starts_with("lablab-") && !ip.is_empty()))?;
    Some((format!("docker:{}", pick.0), pick.1.clone()))
}

/// Red `vmlab-*` e IPv4 del contenedor. Sin credenciales: solo consulta Docker.
pub async fn vmlab_link(tools: &Tools, name: &str) -> Result<LinkInfo> {
    validate::container_name(name)?;
    let out = run_ok(
        tools.docker()?,
        &[
            "inspect",
            "-f",
            "{{range $k, $v := .NetworkSettings.Networks}}{{$k}}={{$v.IPAddress}};{{end}}",
            name,
        ],
        SHORT,
    )
    .await?;
    Ok(match vmlab_of(&parse_networks(&out.stdout)) {
        Some((segment, ipv4)) => LinkInfo { segment: Some(segment), ipv4: Some(ipv4), note: None },
        None => LinkInfo {
            segment: None,
            ipv4: None,
            note: Some("sin red VMLab o sin IP asignada".into()),
        },
    })
}

pub async fn action(tools: &Tools, name: &str, image: Option<&str>, action: Action) -> Result<()> {
    validate::container_name(name)?;
    let bin = tools.docker()?;
    match action {
        Action::Start | Action::StartGui => {
            match run_ok(bin, &["start", name], SHORT).await {
                Ok(_) => Ok(()),
                // El contenedor no existe todavía: se crea desde la imagen del nodo.
                Err(CoreError::Failed { stderr, .. }) if stderr.contains("No such container") => {
                    let image = image
                        .filter(|i| !i.is_empty())
                        .ok_or_else(|| CoreError::Invalid("el contenedor no existe y el nodo no tiene imagen".into()))?;
                    validate::docker_image(image)?;
                    run_ok(bin, &["run", "-d", "--name", name, image], LONG).await.map(|_| ())
                }
                Err(e) => Err(e),
            }
        }
        Action::Stop => run_ok(bin, &["stop", name], LONG).await.map(|_| ()),
        Action::ForceStop => run_ok(bin, &["kill", name], SHORT).await.map(|_| ()),
        Action::Restart => run_ok(bin, &["restart", name], LONG).await.map(|_| ()),
        Action::Pause => run_ok(bin, &["pause", name], SHORT).await.map(|_| ()),
        Action::Resume => run_ok(bin, &["unpause", name], SHORT).await.map(|_| ()),
    }
}

pub async fn ensure_network(tools: &Tools, net: &str) -> Result<()> {
    validate::network_name(net)?;
    let bin = tools.docker()?;
    if run_ok(bin, &["network", "inspect", net], SHORT).await.is_ok() {
        return Ok(());
    }
    run_ok(bin, &["network", "create", "--driver", "bridge", net], SHORT).await.map(|_| ())
}

async fn vmlab_networks_of(tools: &Tools, container: &str) -> Result<Vec<String>> {
    let out = run_ok(
        tools.docker()?,
        &["inspect", "-f", "{{range $k, $v := .NetworkSettings.Networks}}{{$k}} {{end}}", container],
        SHORT,
    )
    .await?;
    Ok(out
        .stdout
        .split_whitespace()
        .filter(|n| validate::managed_network(n))
        .map(String::from)
        .collect())
}

/// Deja al contenedor conectado **solo** a `keep` entre las redes gestionadas
/// (`vmlab-*` y las antiguas `lablab-*`, que se desconectan al migrar).
/// Es idempotente.
pub async fn set_networks(tools: &Tools, container: &str, keep: Option<&str>) -> Result<()> {
    validate::container_name(container)?;
    let bin = tools.docker()?;
    let current = match vmlab_networks_of(tools, container).await {
        Ok(c) => c,
        // Contenedor aún sin crear: nada que desconectar, y tampoco se puede conectar todavía.
        Err(CoreError::Failed { stderr, .. }) if stderr.contains("No such") => {
            return match keep {
                Some(_) => Err(CoreError::Invalid("el contenedor aún no existe: inícialo una vez y vuelve a aplicar".into())),
                None => Ok(()),
            };
        }
        Err(e) => return Err(e),
    };
    for net in current.iter().filter(|n| Some(n.as_str()) != keep) {
        run_ok(bin, &["network", "disconnect", "--force", net, container], SHORT).await?;
    }
    if let Some(net) = keep {
        validate::network_name(net)?;
        if !current.iter().any(|n| n == net) {
            run_ok(bin, &["network", "connect", net, container], SHORT).await?;
        }
    }
    Ok(())
}

/// Borra las redes `vmlab-*` y las antiguas `lablab-*` que ya no tienen
/// contenedores (las que estén en uso fallan y se ignoran).
pub async fn prune_networks(tools: &Tools) -> Result<()> {
    let bin = tools.docker()?;
    for filter in ["name=vmlab-", "name=lablab-"] {
        let out = run_ok(bin, &["network", "ls", "--filter", filter, "--format", "{{.Name}}"], SHORT).await?;
        for net in out.stdout.lines().map(str::trim).filter(|n| validate::managed_network(n)) {
            let _ = run_ok(bin, &["network", "rm", net], SHORT).await;
        }
    }
    Ok(())
}

// ───────────────────── Instantáneas del laboratorio ─────────────────────

/// El nombre visible de un contenedor puede llevar mayúsculas, pero el repositorio
/// de Docker debe ir en minúsculas.
fn image_repo(container: &str) -> String {
    format!("vmlab-snap/{}", container.to_ascii_lowercase())
}

/// Referencia de la imagen que guarda la instantánea de un contenedor.
pub fn snapshot_image(container: &str, id: &str) -> Result<String> {
    validate::container_name(container)?;
    validate::labsnap_id(id)?;
    Ok(format!("{}:{}", image_repo(container), id))
}

/// Docker interpreta cada `--change` como una instrucción de Dockerfile. Un valor
/// con espacios o paréntesis debe ir entre comillas, o el parser lo parte en
/// varios tokens y rechaza la instrucción `LABEL`.
fn label_change(key: &str, value: &str) -> String {
    let plain = value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if plain {
        format!("LABEL {key}={value}")
    } else {
        format!("LABEL {key}=\"{value}\"")
    }
}

fn label_value(labels: &str, key: &str) -> Option<String> {
    labels.split(',').find_map(|item| {
        let (name, value) = item.trim().split_once('=')?;
        (name == key && !value.is_empty()).then(|| value.to_string())
    })
}

/// Parsea la salida de `docker images --format "{{.Repository}}\t{{.Tag}}\t{{.CreatedAt}}\t{{.Labels}}"`.
pub fn parse_snapshot_images(out: &str, container: &str) -> Vec<(String, String, Option<String>)> {
    let repo = image_repo(container);
    out.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let repository = fields.next()?;
            let tag = fields.next()?;
            let created = fields.next().unwrap_or("").trim();
            let labels = fields.next().unwrap_or("");
            if repository != repo || validate::labsnap_id(tag).is_err() {
                return None;
            }
            let label = label_value(labels, "vmlab.label").unwrap_or_else(|| tag.to_string());
            Some((tag.to_string(), label, (!created.is_empty()).then(|| created.to_string())))
        })
        .collect()
}

pub async fn snapshot_list(tools: &Tools, container: &str) -> Result<Vec<(String, String, Option<String>)>> {
    validate::container_name(container)?;
    let filter = format!("reference={}", image_repo(container));
    let out = run_ok(
        tools.docker()?,
        &[
            "images",
            "--filter",
            filter.as_str(),
            "--format",
            "{{.Repository}}\t{{.Tag}}\t{{.CreatedAt}}\t{{.Labels}}",
        ],
        SHORT,
    )
    .await?;
    Ok(parse_snapshot_images(&out.stdout, container))
}

pub async fn snapshot_take(tools: &Tools, container: &str, id: &str, label: &str) -> Result<()> {
    validate::container_name(container)?;
    let image = snapshot_image(container, id)?;
    validate::labsnap_label(label)?;
    let bin = tools.docker()?;
    let change_snapshot = label_change("vmlab.snapshot", id);
    let change_label = label_change("vmlab.label", label);
    let change_container = label_change("vmlab.container", container);
    run_ok(
        bin,
        &[
            "commit",
            "--change",
            change_snapshot.as_str(),
            "--change",
            change_label.as_str(),
            "--change",
            change_container.as_str(),
            container,
            image.as_str(),
        ],
        LONG,
    )
    .await
    .map(|_| ())
}

pub async fn snapshot_delete(tools: &Tools, container: &str, id: &str) -> Result<()> {
    let image = snapshot_image(container, id)?;
    run_ok(tools.docker()?, &["rmi", image.as_str()], LONG).await.map(|_| ())
}

fn output_message(out: &crate::runner::Output) -> String {
    let raw = if out.stderr.trim().is_empty() { &out.stdout } else { &out.stderr };
    raw.trim().chars().take(500).collect()
}

fn no_such_container(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("no such container") || m.contains("no such object")
}

/// Nombre del contenedor original que se conserva antes de restaurar. Docker limita
/// el nombre a 64 caracteres, así que se recorta la parte original si hace falta.
pub fn prerestore_name(original: &str, stamp: &str) -> String {
    let suffix = format!("-prerestore-{stamp}");
    let max_original = 64usize.saturating_sub(suffix.len());
    let base: String = original.chars().take(max_original).collect();
    format!("{base}{suffix}")
}

/// Restaura la instantánea como un contenedor nuevo. Si el contenedor original
/// existe, se para (solo si está en marcha) y se renombra; nunca se borra.
/// Devuelve el nombre con el que quedó el contenedor original, si existía.
pub async fn snapshot_restore(tools: &Tools, container: &str, id: &str) -> Result<Option<String>> {
    validate::container_name(container)?;
    let image = snapshot_image(container, id)?;
    let bin = tools.docker()?;

    let running = match run(bin, &["inspect", "-f", "{{.State.Running}}", container], SHORT).await? {
        out if out.code == 0 => Some(out.stdout.trim().eq_ignore_ascii_case("true")),
        out => {
            let message = output_message(&out);
            if no_such_container(&message) {
                None
            } else {
                return Err(CoreError::Failed { program: "docker".into(), code: out.code, stderr: message });
            }
        }
    };

    let mut renamed = None;
    if let Some(running) = running {
        if running {
            run_ok(bin, &["stop", container], LONG).await?;
        }
        let name = prerestore_name(container, &crate::labsnap::utc_now_stamp());
        run_ok(bin, &["rename", container, name.as_str()], SHORT).await?;
        renamed = Some(name);
    }

    run_ok(bin, &["run", "-d", "--name", container, image.as_str()], LONG).await?;
    Ok(renamed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_an_engine_that_is_not_running() {
        let pipe = "docker terminó con error (1): failed to connect to the docker API at npipe:////./pipe/dockerDesktopLinuxEngine; check if the path is correct and if the daemon is running: open //./pipe/dockerDesktopLinuxEngine: El sistema no puede encontrar el archivo especificado.";
        assert!(engine_is_down(pipe));
        assert!(engine_is_down(
            "Cannot connect to the Docker daemon at unix:///var/run/docker.sock. Is the docker daemon running?"
        ));
        assert!(!engine_is_down("docker terminó con error (125): No such container: web"));
        assert!(!engine_is_down("permission denied while trying to connect to the Docker daemon socket"));
    }

    #[test]
    fn parses_ps() {
        let out = "web\tnginx:latest\trunning\ndb\tpostgres:16\texited\n\nbroken line\n";
        let c = parse_ps(out);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].state, PowerState::Running);
        assert_eq!(c[1].state, PowerState::Stopped);
        assert_eq!(c[1].image, "postgres:16");
    }

    #[test]
    fn picks_the_vmlab_network_and_ignores_the_default_bridge() {
        let nets = parse_networks("bridge=172.17.0.2;vmlab-0a1b2c3d=172.18.0.2;");
        assert_eq!(nets.len(), 2);
        assert_eq!(
            vmlab_of(&nets),
            Some(("docker:vmlab-0a1b2c3d".into(), "172.18.0.2".into()))
        );
        assert_eq!(vmlab_of(&parse_networks("bridge=172.17.0.2;")), None);
        assert_eq!(vmlab_of(&parse_networks("vmlab-empty=;")), None);
    }

    #[test]
    fn recognizes_legacy_networks_during_migration_but_prefers_new_ones() {
        let legacy = parse_networks("bridge=172.17.0.2;lablab-0a1b2c3d=172.18.0.2;");
        assert_eq!(
            vmlab_of(&legacy),
            Some(("docker:lablab-0a1b2c3d".into(), "172.18.0.2".into()))
        );
        let both = parse_networks("lablab-0a1b2c3d=172.18.0.2;vmlab-0a1b2c3d=172.19.0.2;");
        assert_eq!(
            vmlab_of(&both),
            Some(("docker:vmlab-0a1b2c3d".into(), "172.19.0.2".into()))
        );
    }

    #[test]
    fn builds_snapshot_image_references() {
        assert_eq!(
            snapshot_image("Web-01", "lab-uno").unwrap(),
            "vmlab-snap/web-01:lab-uno"
        );
        assert_eq!(
            snapshot_image("web", "Lab-uno").unwrap_err().to_string(),
            "entrada no válida: identificador de instantánea no válido «Lab-uno»"
        );
        assert!(snapshot_image("--privileged", "lab").is_err());
    }

    #[test]
    fn quotes_dockerfile_labels_only_when_needed() {
        assert_eq!(
            label_change("vmlab.snapshot", "lab-uno"),
            "LABEL vmlab.snapshot=lab-uno"
        );
        assert_eq!(
            label_change("vmlab.label", "Lab uno"),
            "LABEL vmlab.label=\"Lab uno\""
        );
        assert_eq!(
            label_change("vmlab.label", "Lab (1)"),
            "LABEL vmlab.label=\"Lab (1)\""
        );
    }

    #[test]
    fn parses_snapshot_images_with_labels() {
        let out = "\
vmlab-snap/web\tlab-uno\t2026-11-09 10:00:00 +0000 UTC\tvmlab.container=web,vmlab.label=Lab uno,vmlab.snapshot=lab-uno
vmlab-snap/web\tlab-dos\t2026-11-09 11:00:00 +0000 UTC\tvmlab.snapshot=lab-dos
vmlab-snap/otro\tlab-uno\t2026-11-09 10:00:00 +0000 UTC\tvmlab.label=Ajeno
vmlab-snap/web\tinvalida!\t2026-11-09 12:00:00 +0000 UTC\tvmlab.label=Mala
";
        assert_eq!(
            parse_snapshot_images(out, "web"),
            vec![
                (
                    "lab-uno".to_string(),
                    "Lab uno".to_string(),
                    Some("2026-11-09 10:00:00 +0000 UTC".to_string())
                ),
                (
                    "lab-dos".to_string(),
                    "lab-dos".to_string(),
                    Some("2026-11-09 11:00:00 +0000 UTC".to_string())
                ),
            ]
        );
    }

    #[test]
    fn builds_prerestore_names_within_docker_limit() {
        let short = prerestore_name("web", "20261109103000");
        assert_eq!(short, "web-prerestore-20261109103000");
        assert!(short.len() <= 64);

        let original = "a".repeat(64);
        let long = prerestore_name(&original, "20261109103000");
        assert_eq!(long.len(), 64);
        assert!(long.starts_with(&"a".repeat(38)));
        assert!(long.ends_with("-prerestore-20261109103000"));
    }
}
