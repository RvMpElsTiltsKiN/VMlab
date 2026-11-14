//! Validación de ficheros `docker-compose.yml` con la CLI de Docker Compose.
//!
//! El texto llega de la UI y nunca se interpreta en Rust: se escribe en una
//! carpeta temporal nueva, se ejecuta `docker compose config` sin shell y se
//! devuelve el JSON tal cual. La carpeta se borra siempre, incluso si Docker
//! falla o se agota el tiempo.
use crate::error::{CoreError, Result};
use crate::runner::run;
use crate::tools::Tools;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Tamaño máximo del `docker-compose.yml` que acepta la UI.
const MAX_INPUT: usize = 1024 * 1024;
/// Tamaño máximo del JSON que se devuelve a la UI.
const MAX_OUTPUT: usize = 2 * 1024 * 1024;
/// Recorte de los errores de Docker antes de mostrarlos.
const MAX_ERROR: usize = 800;
const TIMEOUT: Duration = Duration::from_secs(60);

/// Carpeta temporal que se elimina al salir del ámbito, pase lo que pase.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Crea una carpeta temporal nueva y vacía con un nombre generado en el backend.
fn new_temp_dir() -> Result<TempDir> {
    let base = std::env::temp_dir();
    let pid = std::process::id();
    for attempt in 0..100u32 {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = base.join(format!("vmlab-compose-{pid}-{nanos}-{attempt}"));
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(TempDir(path)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(CoreError::Io(format!(
                    "no se pudo crear la carpeta temporal de Docker Compose: {e}"
                )))
            }
        }
    }
    Err(CoreError::Io(
        "no se pudo crear una carpeta temporal única para Docker Compose".into(),
    ))
}

fn check_input(text: &str) -> Result<()> {
    if text.trim().is_empty() {
        return Err(CoreError::Invalid(
            "el contenido de docker-compose.yml está vacío".into(),
        ));
    }
    if text.len() > MAX_INPUT {
        return Err(CoreError::Invalid(
            "el contenido de docker-compose.yml no puede superar 1 MB".into(),
        ));
    }
    Ok(())
}

fn check_output(bytes: usize) -> Result<()> {
    if bytes > MAX_OUTPUT {
        return Err(CoreError::Invalid(
            "la configuración generada por Docker Compose no puede superar 2 MB".into(),
        ));
    }
    Ok(())
}

/// `true` si el error de Docker indica que el subcomando `compose` no existe
/// (Docker Desktop antiguo o instalación sin el plugin).
fn missing_compose(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("is not a docker command")
        || m.contains("unknown command")
        || m.contains("not a docker command")
}

fn short_error(message: &str) -> String {
    message.trim().chars().take(MAX_ERROR).collect()
}

/// Valida `text` con `docker compose config --format json` y devuelve su salida.
pub async fn config(tools: &Tools, text: &str) -> Result<String> {
    check_input(text)?;
    let dir = new_temp_dir()?;
    let file = dir.0.join("docker-compose.yml");
    std::fs::write(&file, text).map_err(|e| {
        CoreError::Io(format!("no se pudo escribir docker-compose.yml en la carpeta temporal: {e}"))
    })?;

    let bin = tools.docker()?;
    let file_arg = file.to_string_lossy().into_owned();
    let dir_arg = dir.0.to_string_lossy().into_owned();
    let out = run(
        bin,
        &[
            "compose",
            "-f",
            file_arg.as_str(),
            "--project-directory",
            dir_arg.as_str(),
            "config",
            "--format",
            "json",
        ],
        TIMEOUT,
    )
    .await?;

    if out.code != 0 {
        let raw = if out.stderr.trim().is_empty() { &out.stdout } else { &out.stderr };
        let combined = format!("{}\n{}", out.stderr, out.stdout);
        if missing_compose(&combined) {
            return Err(CoreError::Message(
                "Docker Compose no está disponible. Actualiza Docker Desktop.".into(),
            ));
        }
        return Err(CoreError::Message(short_error(raw)));
    }

    check_output(out.stdout.len())?;
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enforces_the_compose_input_and_output_limits() {
        assert!(check_input("").is_err());
        assert!(check_input("   \n\t ").is_err());
        assert!(check_input("services: {}").is_ok());
        assert!(check_input(&"a".repeat(MAX_INPUT)).is_ok());
        assert!(check_input(&"a".repeat(MAX_INPUT + 1)).is_err());

        assert!(check_output(0).is_ok());
        assert!(check_output(MAX_OUTPUT).is_ok());
        assert!(check_output(MAX_OUTPUT + 1).is_err());
    }

    #[test]
    fn detects_a_missing_compose_command() {
        assert!(missing_compose(
            "docker: 'compose' is not a docker command.\nSee 'docker --help'"
        ));
        assert!(missing_compose("unknown command: docker compose"));
        assert!(missing_compose("docker: 'compose' is not a docker command"));
        assert!(!missing_compose(
            "yaml: line 3: could not find expected ':'"
        ));
        assert!(!missing_compose("services.web.ports contains an invalid type"));
    }

    #[test]
    fn truncates_docker_errors() {
        let long = "x".repeat(2000);
        assert_eq!(short_error(&long).chars().count(), MAX_ERROR);
        assert_eq!(short_error("  error breve  \n"), "error breve");
    }
}
