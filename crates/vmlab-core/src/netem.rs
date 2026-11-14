//! Condiciones de red por nodo: `netem` dentro de contenedores Docker.
//!
//! El host nunca ejecuta un shell: se lanza un único `docker run` con una imagen
//! que trae `tc`, y los valores de las condiciones viajan como argumentos
//! posicionales de `sh -c`. El texto del script es siempre una constante.
use crate::error::{CoreError, Result};
use crate::model::LinkConditions;
use crate::runner::{run, run_ok};
use crate::tools::Tools;
use crate::validate;
use std::time::Duration;

/// Imagen usada para ejecutar `tc` dentro de la red del contenedor.
pub const TC_IMAGE: &str = "nicolaka/netshoot:latest";

const SHORT: Duration = Duration::from_secs(20);
const LONG: Duration = Duration::from_secs(300); // la primera vez puede descargar la imagen

/// Recorre todas las interfaces del contenedor (menos `lo`) y aplica netem.
/// Los valores llegan en `$@`; nunca se interpolan en este texto.
pub const SCRIPT_APPLY: &str = r#"for i in /sys/class/net/*; do
  i=${i##*/}
  [ "$i" = lo ] && continue
  tc qdisc replace dev "$i" root netem "$@" || exit 1
done"#;

/// Quita la disciplina netem de todas las interfaces del contenedor.
pub const SCRIPT_CLEAR: &str = r#"for i in /sys/class/net/*; do
  i=${i##*/}
  [ "$i" = lo ] && continue
  tc qdisc del dev "$i" root 2>/dev/null; true
done"#;

/// Formatea las décimas de porcentaje sin usar coma flotante: 15 -> `1.5%`.
fn loss_percent(tenths: u32) -> String {
    if tenths % 10 == 0 {
        format!("{}%", tenths / 10)
    } else {
        format!("{}.{}%", tenths / 10, tenths % 10)
    }
}

/// Argumentos de `netem` a partir de las condiciones del nodo.
///
/// El orden es el que acepta `tc`: retardo (+ jitter), pérdida y velocidad.
pub fn tc_args(c: &LinkConditions) -> Result<Vec<String>> {
    let mut args = Vec::new();

    if c.delay_ms > 0 {
        args.push("delay".to_string());
        args.push(format!("{}ms", c.delay_ms));
        if c.jitter_ms > 0 {
            args.push(format!("{}ms", c.jitter_ms));
        }
    }

    if c.loss_tenths > 0 {
        args.push("loss".to_string());
        args.push(loss_percent(c.loss_tenths));
    }

    if let Some(rate) = c.rate_kbit {
        args.push("rate".to_string());
        args.push(format!("{rate}kbit"));
    }

    if args.is_empty() {
        return Err(CoreError::Message("no hay condiciones que aplicar".into()));
    }
    Ok(args)
}

/// Construye el argv de `docker run` sin incluir valores de las condiciones en el
/// script: después de `sh -c <script> sh` solo van los argumentos de `tc`.
fn tc_script_args(container: &str, name: &str, script: &str, tc: &[String]) -> Vec<String> {
    let mut args = vec![
        "run".to_string(),
        "--rm".to_string(),
        "--name".to_string(),
        name.to_string(),
        "--network".to_string(),
        format!("container:{container}"),
        "--cap-add".to_string(),
        "NET_ADMIN".to_string(),
        TC_IMAGE.to_string(),
        "sh".to_string(),
        "-c".to_string(),
        script.to_string(),
        "sh".to_string(),
    ];
    args.extend(tc.iter().cloned());
    args
}

/// Nombre del contenedor auxiliar de `tc`, generado aquí: solo `[a-z0-9-]`.
fn helper_name() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("vmlab-tc-{nanos}")
}

fn output_message(out: &crate::runner::Output) -> String {
    let raw = if out.stderr.trim().is_empty() { &out.stdout } else { &out.stderr };
    raw.trim().chars().take(500).collect()
}

fn download_failed(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    [
        "unable to find image",
        "pull access denied",
        "manifest unknown",
        "failed to resolve reference",
        "error pulling image",
        "failed to pull",
        "no such image",
    ]
    .iter()
    .any(|needle| m.contains(needle))
}

/// Convierte un fallo de descarga en un mensaje accionable.
fn download_error(error: CoreError) -> CoreError {
    let failed = matches!(&error, CoreError::Failed { stderr, .. } if download_failed(stderr));
    if failed {
        CoreError::Message(format!(
            "no se pudo descargar la imagen «{TC_IMAGE}»; comprueba la conexión a internet y vuelve a intentarlo"
        ))
    } else {
        error
    }
}

/// `true` si el contenedor existe y está en marcha.
pub async fn is_running(tools: &Tools, container: &str) -> Result<bool> {
    validate::container_name(container)?;
    let out = run(
        tools.docker()?,
        &["inspect", "-f", "{{.State.Running}}", container],
        SHORT,
    )
    .await?;
    if out.code == 0 {
        return Ok(out.stdout.trim().eq_ignore_ascii_case("true"));
    }
    let message = output_message(&out);
    if message.to_ascii_lowercase().contains("no such") {
        Ok(false)
    } else {
        Err(CoreError::Failed {
            program: "docker".into(),
            code: out.code,
            stderr: message,
        })
    }
}

async fn run_tc(tools: &Tools, container: &str, script: &str, tc: &[String]) -> Result<()> {
    let bin = tools.docker()?;
    let name = helper_name();
    validate::container_name(&name)?;
    let args = tc_script_args(container, &name, script, tc);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match run_ok(bin, &refs, LONG).await {
        Ok(_) => Ok(()),
        Err(error) => {
            // Si el cliente se quedó colgado, el contenedor auxiliar puede seguir
            // vivo: se elimina por nombre antes de devolver el error.
            if matches!(error, CoreError::Timeout { .. }) {
                let _ = run_ok(bin, &["rm", "-f", name.as_str()], SHORT).await;
            }
            Err(download_error(error))
        }
    }
}

/// Aplica las condiciones netem dentro de un contenedor en marcha.
pub async fn apply_docker(tools: &Tools, container: &str, c: &LinkConditions) -> Result<()> {
    validate::container_name(container)?;
    validate::link_conditions(c)?;
    let tc = tc_args(c)?;
    if !is_running(tools, container).await? {
        return Err(CoreError::Message("arranca el contenedor primero".into()));
    }
    run_tc(tools, container, SCRIPT_APPLY, &tc).await
}

/// Quita las condiciones netem de un contenedor en marcha.
pub async fn clear_docker(tools: &Tools, container: &str) -> Result<()> {
    validate::container_name(container)?;
    if !is_running(tools, container).await? {
        return Err(CoreError::Message("arranca el contenedor primero".into()));
    }
    run_tc(tools, container, SCRIPT_CLEAR, &[]).await
}

/// Comprueba si la imagen de `tc` ya está descargada, para pedir confirmación antes
/// de descargarla por primera vez.
pub async fn image_present(tools: &Tools) -> bool {
    let Ok(bin) = tools.docker() else { return false };
    run(bin, &["image", "inspect", TC_IMAGE], SHORT)
        .await
        .map(|out| out.code == 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conditions(delay_ms: u32, jitter_ms: u32, loss_tenths: u32, rate_kbit: Option<u32>) -> LinkConditions {
        LinkConditions { delay_ms, jitter_ms, loss_tenths, rate_kbit }
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn builds_tc_arguments_for_each_condition() {
        assert_eq!(
            tc_args(&conditions(50, 0, 0, None)).unwrap(),
            strings(&["delay", "50ms"])
        );
        assert_eq!(
            tc_args(&conditions(50, 10, 0, None)).unwrap(),
            strings(&["delay", "50ms", "10ms"])
        );
        assert_eq!(
            tc_args(&conditions(0, 0, 15, None)).unwrap(),
            strings(&["loss", "1.5%"])
        );
        assert_eq!(
            tc_args(&conditions(0, 0, 1000, None)).unwrap(),
            strings(&["loss", "100%"])
        );
        assert_eq!(
            tc_args(&conditions(0, 0, 0, Some(20_000))).unwrap(),
            strings(&["rate", "20000kbit"])
        );
        assert_eq!(
            tc_args(&conditions(50, 10, 15, Some(20_000))).unwrap(),
            strings(&[
                "delay", "50ms", "10ms", "loss", "1.5%", "rate", "20000kbit"
            ])
        );
    }

    #[test]
    fn rejects_empty_conditions() {
        let error = tc_args(&conditions(0, 0, 0, None)).unwrap_err();
        assert_eq!(error.to_string(), "no hay condiciones que aplicar");
    }

    #[test]
    fn keeps_values_out_of_the_scripts() {
        assert!(SCRIPT_APPLY.contains(r#"tc qdisc replace dev "$i" root netem "$@""#));
        assert!(SCRIPT_CLEAR.contains(r#"tc qdisc del dev "$i" root 2>/dev/null; true"#));

        for script in [SCRIPT_APPLY, SCRIPT_CLEAR] {
            assert!(!script.contains("50ms"));
            assert!(!script.contains("1.5%"));
            assert!(!script.contains("20000"));
            assert!(!script.contains("delay "));
        }

        let tc = tc_args(&conditions(50, 10, 15, Some(20_000))).unwrap();
        let args = tc_script_args("web", "vmlab-tc-1700000000000000000", SCRIPT_APPLY, &tc);
        assert_eq!(&args[..5], ["run", "--rm", "--name", "vmlab-tc-1700000000000000000", "--network"]);
        let script_pos = args.iter().position(|arg| arg == SCRIPT_APPLY).unwrap();
        assert_eq!(args[script_pos - 1], "-c");
        assert_eq!(args[script_pos + 1], "sh");
        assert_eq!(&args[script_pos + 2..], &tc[..]);
    }

    #[test]
    fn helper_names_are_docker_safe() {
        let name = helper_name();
        assert!(name.starts_with("vmlab-tc-"));
        assert!(name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
        assert!(validate::container_name(&name).is_ok());
    }

    #[test]
    fn recognizes_image_download_failures() {
        assert!(download_failed("Unable to find image 'nicolaka/netshoot:latest' locally"));
        assert!(download_failed("pull access denied for nicolaka/netshoot"));
        assert!(download_failed("manifest unknown"));
        assert!(!download_failed("no hay condiciones que aplicar"));
    }
}
