//! Control de contenedores con la CLI de Docker.
//! Cada segmento de red de VMLab es una red *bridge* de Docker llamada `vmlab-xxxxxxxx`.
use crate::error::{CoreError, Result};
use crate::model::{Action, DockerRunOptions, LinkInfo, PowerState};
use crate::runner::{run, run_ok};
use crate::tools::Tools;
use crate::validate;
use std::collections::HashSet;
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

/// Argumentos `-e`/`-p` compartidos por `docker run` y `docker create`.
///
/// Cada `-e`/`-p` viaja como argumento propio y `KEY=VALUE` como **un solo**
/// argumento, de modo que ningún valor necesita comillas ni pasa por un shell.
/// Los puertos se publican siempre en `127.0.0.1`.
fn option_args(opts: Option<&DockerRunOptions>) -> Result<Vec<String>> {
    let mut args = Vec::new();
    if let Some(opts) = opts {
        validate::docker_run_options(opts)?;
        for e in &opts.env {
            args.push("-e".to_string());
            args.push(format!("{}={}", e.key, e.value));
        }
        for p in &opts.ports {
            args.push("-p".to_string());
            args.push(format!("127.0.0.1:{}:{}/{}", p.host, p.container, p.proto.as_str()));
        }
    }
    Ok(args)
}

/// Argumentos de `docker run` para crear un contenedor con las opciones del nodo.
pub fn run_args(name: &str, image: &str, opts: Option<&DockerRunOptions>) -> Result<Vec<String>> {
    validate::container_name(name)?;
    validate::docker_image(image)?;
    let mut args = vec![
        "run".to_string(),
        "-d".to_string(),
        "--name".to_string(),
        name.to_string(),
    ];
    args.extend(option_args(opts)?);
    args.push(image.to_string());
    Ok(args)
}

/// Argumentos de `docker create` para crear el contenedor **sin arrancarlo**
/// conectado ya a la red del segmento.
pub fn create_args(
    name: &str,
    image: &str,
    opts: Option<&DockerRunOptions>,
    network: &str,
) -> Result<Vec<String>> {
    validate::container_name(name)?;
    validate::docker_image(image)?;
    validate::network_name(network)?;
    let mut args = vec!["create".to_string(), "--name".to_string(), name.to_string()];
    args.extend(option_args(opts)?);
    args.push("--network".to_string());
    args.push(network.to_string());
    args.push(image.to_string());
    Ok(args)
}

/// Indica si el contenedor ya existe (aunque esté parado).
pub async fn exists(tools: &Tools, name: &str) -> Result<bool> {
    validate::container_name(name)?;
    match run_ok(tools.docker()?, &["inspect", "-f", "{{.Id}}", name], SHORT).await {
        Ok(_) => Ok(true),
        Err(CoreError::Failed { stderr, .. }) if no_such_container(&stderr) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Crea el contenedor sin arrancarlo y lo deja conectado a `network`.
pub async fn create_on_network(
    tools: &Tools,
    name: &str,
    image: &str,
    options: Option<&DockerRunOptions>,
    network: &str,
) -> Result<()> {
    let args = create_args(name, image, options, network)?;
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_ok(tools.docker()?, &refs, LONG)
        .await
        .map(|_| ())
        .map_err(|e| create_failure(name, e))
}

/// Error de creación con contexto: el nodo debe decir que no se pudo crear.
fn create_failure(name: &str, error: CoreError) -> CoreError {
    CoreError::Message(format!("no se pudo crear el contenedor «{name}»: {error}"))
}

pub async fn action(
    tools: &Tools,
    name: &str,
    image: Option<&str>,
    options: Option<DockerRunOptions>,
    action: Action,
) -> Result<()> {
    validate::container_name(name)?;
    let bin = tools.docker()?;
    match action {
        Action::Start | Action::StartGui => {
            match run_ok(bin, &["start", name], SHORT).await {
                Ok(_) => Ok(()),
                // El contenedor no existe todavía: se crea desde la imagen del nodo.
                Err(CoreError::Failed { stderr, .. }) if no_such_container(&stderr) => {
                    let image = image
                        .filter(|i| !i.is_empty())
                        .ok_or_else(|| CoreError::Invalid("el contenedor no existe y el nodo no tiene imagen".into()))?;
                    let args = run_args(name, image, options.as_ref())?;
                    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
                    run_ok(bin, &refs, LONG).await.map(|_| ())
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
        Err(CoreError::Failed { stderr, .. }) if no_such_container(&stderr) => {
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
            // Un contenedor `created` puede no listar sus redes en `inspect` aunque
            // se creara con `--network`; Docker responde "already exists in network".
            match run_ok(bin, &["network", "connect", net, container], SHORT).await {
                Ok(_) => {}
                Err(CoreError::Failed { stderr, .. }) if already_in_network(&stderr) => {}
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}

/// `true` si el error de `network connect` dice que el contenedor ya está conectado.
fn already_in_network(message: &str) -> bool {
    message.to_ascii_lowercase().contains("already exists in network")
}

/// `true` si la red es gestionada por VMLab y no pertenece al plan actual.
fn should_prune(name: &str, keep: &HashSet<String>) -> bool {
    validate::managed_network(name) && !keep.contains(name)
}

/// Borra las redes `vmlab-*` y las antiguas `lablab-*` que ya no tienen
/// contenedores (las que estén en uso fallan y se ignoran). Nunca borra una red
/// del plan actual, aunque todavía no tenga contenedores arrancados.
pub async fn prune_networks(tools: &Tools, keep: &HashSet<String>) -> Result<()> {
    let bin = tools.docker()?;
    for filter in ["name=vmlab-", "name=lablab-"] {
        let out = run_ok(bin, &["network", "ls", "--filter", filter, "--format", "{{.Name}}"], SHORT).await?;
        for net in out.stdout.lines().map(str::trim).filter(|n| should_prune(n, keep)) {
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
    // `docker commit` puede sobrescribir una etiqueta existente; se comprueba antes
    // para no perder una instantánea anterior con el mismo nombre.
    let inspected = run(bin, &["image", "inspect", image.as_str()], SHORT).await?;
    if inspected.code == 0 {
        return Err(CoreError::Message("ya existe una instantánea con ese nombre".into()));
    }
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
    match run_ok(tools.docker()?, &["rmi", image.as_str()], LONG).await {
        Ok(_) => Ok(()),
        Err(CoreError::Failed { stderr, .. }) if image_in_use(&stderr) => Err(CoreError::Message(
            "la imagen está en uso por el contenedor restaurado; elimina o renombra ese contenedor y vuelve a intentarlo"
                .into(),
        )),
        Err(e) => Err(e),
    }
}

/// Detecta el conflicto de Docker al borrar una imagen que usa un contenedor.
fn image_in_use(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("image is being used by") || m.contains("image is in use by")
}

fn output_message(out: &crate::runner::Output) -> String {
    let raw = if out.stderr.trim().is_empty() { &out.stdout } else { &out.stderr };
    raw.trim().chars().take(500).collect()
}

pub(crate) fn no_such_container(message: &str) -> bool {
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

/// Añade al error original si se pudo deshacer el renombrado del contenedor.
fn restore_failure(error: &CoreError, undone: bool, original: &str, preserved: &str) -> CoreError {
    let note = if undone {
        "se deshizo el renombrado y el contenedor original volvió a su estado anterior".to_string()
    } else {
        format!(
            "no se pudo deshacer el renombrado por completo; revisa los contenedores «{original}» y «{preserved}»"
        )
    };
    CoreError::Message(format!("{error}. {note}"))
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
    if let Some(was_running) = running {
        if was_running {
            run_ok(bin, &["stop", container], LONG).await?;
        }
        let name = prerestore_name(container, &crate::labsnap::utc_now_stamp());
        run_ok(bin, &["rename", container, name.as_str()], SHORT).await?;
        renamed = Some(name);
    }

    // TODO: restaurar los puertos publicados (y las variables de entorno) del
    // contenedor original. La instantánea actual solo guarda el sistema de
    // ficheros, así que el contenedor restaurado no vuelve a publicar puertos
    // en 127.0.0.1 y hay que añadirlos a mano.
    if let Err(error) = run_ok(bin, &["run", "-d", "--name", container, image.as_str()], LONG).await {
        if let Some(name) = &renamed {
            let undo = match run_ok(bin, &["rename", name.as_str(), container], SHORT).await {
                Ok(_) if running == Some(true) => run_ok(bin, &["start", container], LONG).await.is_ok(),
                Ok(_) => true,
                Err(_) => false,
            };
            return Err(restore_failure(&error, undo, container, name));
        }
        return Err(error);
    }
    Ok(renamed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EnvVar, PortMap, PortProto};

    #[test]
    fn builds_run_args_without_options() {
        let expected = vec!["run", "-d", "--name", "web", "nginx:latest"];
        assert_eq!(run_args("web", "nginx:latest", None).unwrap(), expected);
        let empty = DockerRunOptions::default();
        assert_eq!(run_args("web", "nginx:latest", Some(&empty)).unwrap(), expected);
    }

    #[test]
    fn builds_run_args_with_env_and_ports() {
        let opts = DockerRunOptions {
            env: vec![
                EnvVar { key: "MYSQL_ROOT_PASSWORD".into(), value: "s3creto".into() },
                EnvVar { key: "MESSAGE".into(), value: "hola mundo = 1 $USER \"x\"".into() },
            ],
            ports: vec![
                PortMap { host: 8080, container: 80, proto: PortProto::Tcp },
                PortMap { host: 5353, container: 53, proto: PortProto::Udp },
            ],
        };
        assert_eq!(
            run_args("db", "postgres:16", Some(&opts)).unwrap(),
            vec![
                "run",
                "-d",
                "--name",
                "db",
                "-e",
                "MYSQL_ROOT_PASSWORD=s3creto",
                "-e",
                "MESSAGE=hola mundo = 1 $USER \"x\"",
                "-p",
                "127.0.0.1:8080:80/tcp",
                "-p",
                "127.0.0.1:5353:53/udp",
                "postgres:16",
            ]
        );
    }

    #[test]
    fn rejects_invalid_run_options() {
        let bad_key = DockerRunOptions {
            env: vec![EnvVar { key: "2BAD".into(), value: "x".into() }],
            ..Default::default()
        };
        assert!(run_args("web", "nginx:latest", Some(&bad_key)).is_err());

        let duplicated = DockerRunOptions {
            ports: vec![
                PortMap { host: 8080, container: 80, proto: PortProto::Tcp },
                PortMap { host: 8080, container: 81, proto: PortProto::Tcp },
            ],
            ..Default::default()
        };
        assert!(run_args("web", "nginx:latest", Some(&duplicated)).is_err());

        let zero = DockerRunOptions {
            ports: vec![PortMap { host: 0, container: 80, proto: PortProto::Tcp }],
            ..Default::default()
        };
        assert!(run_args("web", "nginx:latest", Some(&zero)).is_err());

        assert!(run_args("--privileged", "nginx:latest", None).is_err());
        assert!(run_args("web", "--rm", None).is_err());
    }

    #[test]
    fn builds_create_args_with_env_ports_and_network() {
        let opts = DockerRunOptions {
            env: vec![EnvVar { key: "MYSQL_ROOT_PASSWORD".into(), value: "s3creto".into() }],
            ports: vec![PortMap { host: 8080, container: 80, proto: PortProto::Tcp }],
        };
        assert_eq!(
            create_args("db", "postgres:16", Some(&opts), "vmlab-abcd1234").unwrap(),
            vec![
                "create",
                "--name",
                "db",
                "-e",
                "MYSQL_ROOT_PASSWORD=s3creto",
                "-p",
                "127.0.0.1:8080:80/tcp",
                "--network",
                "vmlab-abcd1234",
                "postgres:16",
            ]
        );
        assert_eq!(
            create_args("web", "nginx:latest", None, "vmlab-abcd1234").unwrap(),
            vec!["create", "--name", "web", "--network", "vmlab-abcd1234", "nginx:latest"]
        );
    }

    #[test]
    fn rejects_invalid_create_args() {
        assert!(create_args("--privileged", "nginx:latest", None, "vmlab-abcd1234").is_err());
        assert!(create_args("web", "--rm", None, "vmlab-abcd1234").is_err());
        assert!(create_args("web", "nginx:latest", None, "lab1").is_err());
        assert!(create_args("web", "nginx:latest", None, "--net").is_err());
    }

    #[test]
    fn recognizes_missing_containers_regardless_of_case_and_wording() {
        for message in [
            "Error: No such object: x",
            "error: no such object: x",
            "Error response from daemon: No such container: x",
            "ERROR: NO SUCH CONTAINER: x",
        ] {
            assert!(no_such_container(message), "{message}");
        }
        assert!(!no_such_container(
            "permission denied while trying to connect to the Docker daemon socket"
        ));
    }

    #[test]
    fn creation_failures_mention_the_container() {
        let error = create_failure(
            "vmlab-web",
            CoreError::Failed {
                program: "docker".into(),
                code: 1,
                stderr: "error: no such object: vmlab-web".into(),
            },
        );
        let text = error.to_string();
        assert!(text.contains("no se pudo crear el contenedor «vmlab-web»"), "{text}");
        assert!(text.contains("no such object"), "{text}");
    }

    #[test]
    fn only_prunes_unused_managed_networks() {
        let keep: HashSet<String> = ["vmlab-en-uso".to_string()].into_iter().collect();
        assert!(!should_prune("vmlab-en-uso", &keep));
        assert!(should_prune("vmlab-huerfana", &keep));
        assert!(should_prune("lablab-vieja", &keep));
        assert!(!should_prune("bridge", &keep));
        assert!(!should_prune("otra-red", &keep));
        assert!(!should_prune("vmlab-", &keep));
    }

    #[test]
    fn recognizes_already_connected_networks() {
        assert!(already_in_network(
            "Error response from daemon: endpoint with name vmlab-web already exists in network vmlab-abcd1234"
        ));
        assert!(already_in_network("ALREADY EXISTS IN NETWORK vmlab-abcd1234"));
        assert!(!already_in_network("permission denied while trying to connect to the Docker daemon socket"));
    }

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
    fn detects_images_in_use_when_deleting() {
        assert!(image_in_use(
            "Error response from daemon: conflict: unable to delete abc (cannot be forced) - image is being used by stopped container def"
        ));
        assert!(image_in_use("conflict: image is in use by container abc"));
        assert!(!image_in_use("Error response from daemon: No such image: vmlab-snap/web:lab"));
    }

    #[test]
    fn reports_whether_the_restore_undo_worked() {
        let error = CoreError::Failed {
            program: "docker".into(),
            code: 125,
            stderr: "no such image".into(),
        };
        let undone =
            restore_failure(&error, true, "web", "web-prerestore-20261109103000").to_string();
        assert!(undone.contains("no such image"));
        assert!(undone.contains("se deshizo el renombrado"));

        let failed =
            restore_failure(&error, false, "web", "web-prerestore-20261109103000").to_string();
        assert!(failed.contains("no such image"));
        assert!(failed.contains("no se pudo deshacer"));
        assert!(failed.contains("web-prerestore-20261109103000"));
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
