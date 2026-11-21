//! Apertura de una consola del sistema ya conectada al nodo.
//!
//! Docker usa `docker exec -it`; VirtualBox usa el cliente OpenSSH de Windows.
//! El proceso se lanza sin shell y con una consola nueva, sin esperar a que termine.
use crate::error::{CoreError, Result};
use crate::model::NodeKind;
use crate::tools::Tools;
use crate::validate;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Comando exacto con el que se abre una shell dentro del contenedor.
const DOCKER_SHELL: &str = "exec $(command -v bash || echo sh)";

/// Mensaje que se muestra cuando Windows no tiene instalado el cliente OpenSSH.
pub const SSH_MISSING: &str =
    "Falta el cliente OpenSSH de Windows. Actívalo en Configuración → Aplicaciones → Características opcionales";

/// Argumentos de `docker exec` para el contenedor indicado.
pub fn docker_args(container: &str) -> Result<Vec<String>> {
    let name = validate::container_name(container)?;
    Ok(vec![
        "exec".into(),
        "-it".into(),
        name.into(),
        "sh".into(),
        "-c".into(),
        DOCKER_SHELL.into(),
    ])
}

/// Argumentos de `ssh` para entrar como `user` en `host`.
///
/// Nunca se concatena `user@host`: el usuario y la IP viajan en argumentos separados.
pub fn ssh_args(user: &str, host: &str) -> Result<Vec<String>> {
    let user = validate::guest_user(user)?;
    let ip: Ipv4Addr = host
        .parse()
        .map_err(|_| CoreError::Invalid(format!("dirección IP no válida: «{}»", host.chars().take(60).collect::<String>())))?;
    if ip.is_unspecified() || ip.is_multicast() || ip == Ipv4Addr::BROADCAST {
        return Err(CoreError::Invalid(format!("dirección IP no permitida: «{ip}»")));
    }
    Ok(vec!["-l".into(), user.into(), ip.to_string()])
}

/// Abre una ventana de consola conectada al nodo.
///
/// - Docker: `docker exec -it <contenedor> …`.
/// - VirtualBox: `ssh -l <usuario> <ip>`.
///
/// En Windows el proceso recibe `CREATE_NEW_CONSOLE` para tener su propia ventana.
/// No se espera a que termine ni se guarda el hijo: cerrar VMLab no cierra el terminal.
pub fn open(
    kind: NodeKind,
    target: &str,
    user: Option<&str>,
    host: Option<&str>,
    tools: &Tools,
) -> Result<()> {
    if !cfg!(windows) {
        return Err(CoreError::Message("Solo disponible en Windows".into()));
    }

    let (program, args) = match kind {
        NodeKind::Docker => {
            let args = docker_args(target)?;
            (tools.docker()?.to_path_buf(), args)
        }
        NodeKind::Vbox => {
            let user = user.ok_or_else(|| CoreError::Invalid("falta el usuario de la VM".into()))?;
            let host = host.ok_or_else(|| CoreError::Invalid("falta la dirección IP de la VM".into()))?;
            let args = ssh_args(user, host)?;
            (ssh_exe()?, args)
        }
        NodeKind::Disk => {
            return Err(CoreError::Invalid("un disco no tiene terminal".into()));
        }
        NodeKind::Optical => {
            return Err(CoreError::Invalid("una unidad óptica no tiene terminal".into()));
        }
    };

    launch(&program, &args)
}

/// `%SystemRoot%\System32\OpenSSH\ssh.exe` o, si no existe, `ssh.exe` en el PATH.
fn ssh_exe() -> Result<PathBuf> {
    if let Some(root) = std::env::var_os("SystemRoot") {
        let exe = PathBuf::from(root).join("System32").join("OpenSSH").join("ssh.exe");
        if exe.is_file() {
            return Ok(exe);
        }
    }
    if let Some(exe) = find_in_path("ssh.exe") {
        return Ok(exe);
    }
    Err(CoreError::Message(SSH_MISSING.into()))
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// Lanza el programa sin shell, con la consola del sistema y sin esperar a que termine.
fn launch(program: &Path, args: &[String]) -> Result<()> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NEW_CONSOLE: el terminal abre su propia ventana, no la de VMLab.
        cmd.creation_flags(0x0000_0010);
    }

    cmd.spawn()
        .map_err(|source| CoreError::Spawn { program: program.display().to_string(), source })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_accepts_a_valid_name() {
        assert_eq!(
            docker_args("web-01_a.b").expect("nombre válido"),
            vec!["exec", "-it", "web-01_a.b", "sh", "-c", DOCKER_SHELL]
        );
    }

    #[test]
    fn docker_rejects_names_with_spaces() {
        assert!(docker_args("web 01").is_err());
    }

    #[test]
    fn docker_rejects_names_starting_with_dash() {
        assert!(docker_args("-it").is_err());
        assert!(docker_args("--privileged").is_err());
    }

    #[test]
    fn ssh_accepts_a_valid_user_and_ip() {
        assert_eq!(
            ssh_args("ubuntu", "10.0.2.15").expect("usuario e IP válidos"),
            vec!["-l", "ubuntu", "10.0.2.15"]
        );
        assert_eq!(
            ssh_args("root", "10.0.0.1").expect("IPv4 válida"),
            vec!["-l", "root", "10.0.0.1"]
        );
    }

    #[test]
    fn ssh_rejects_option_injection_in_user() {
        assert!(ssh_args("-oProxyCommand=x", "10.0.2.15").is_err());
        assert!(ssh_args("-ubuntu", "10.0.2.15").is_err());
    }

    #[test]
    fn ssh_rejects_an_at_sign_in_user() {
        assert!(ssh_args("ubuntu@evil", "10.0.2.15").is_err());
        assert!(ssh_args("ubuntu root", "10.0.2.15").is_err());
    }

    #[test]
    fn ssh_rejects_an_invalid_ip() {
        assert!(ssh_args("ubuntu", "10.0.2.300").is_err());
        assert!(ssh_args("ubuntu", "10.0.2").is_err());
    }

    #[test]
    fn ssh_rejects_unspecified_multicast_and_broadcast_addresses() {
        assert!(ssh_args("ubuntu", "0.0.0.0").is_err());
        assert!(ssh_args("ubuntu", "224.0.0.1").is_err());
        assert!(ssh_args("ubuntu", "255.255.255.255").is_err());
    }

    #[test]
    fn ssh_rejects_ipv6() {
        assert!(ssh_args("ubuntu", "::1").is_err());
        assert!(ssh_args("ubuntu", "2001:db8::1").is_err());
    }

    #[test]
    fn ssh_rejects_hosts_with_text() {
        assert!(ssh_args("ubuntu", "example.com").is_err());
        assert!(ssh_args("ubuntu", "10.0.2.15:22").is_err());
    }
}
