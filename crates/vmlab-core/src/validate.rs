//! Validación estricta de todo texto que acabará como argumento de un comando.
//! Como no hay shell, el riesgo real es la *inyección de opciones* (un valor que
//! empieza por `-` y se interpreta como flag), por eso todos los validadores lo prohíben.
use crate::error::{CoreError, Result};
use crate::model::{DockerRunOptions, LinkConditions};
use std::collections::HashSet;
use std::net::Ipv4Addr;

fn bad<T>(what: &str, v: &str) -> Result<T> {
    Err(CoreError::Invalid(format!("{what} «{}»", v.chars().take(60).collect::<String>())))
}

/// Nombre de contenedor Docker: `[A-Za-z0-9][A-Za-z0-9_.-]{0,63}`.
pub fn container_name(s: &str) -> Result<&str> {
    let mut chars = s.chars();
    let ok = matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && s.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if ok { Ok(s) } else { bad("nombre de contenedor no válido", s) }
}

/// Referencia de imagen Docker (`repo/nombre:tag`, `nombre@sha256:...`).
pub fn docker_image(s: &str) -> Result<&str> {
    let mut chars = s.chars();
    let ok = matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && s.len() <= 200
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | ':' | '@' | '-'));
    if ok { Ok(s) } else { bad("imagen Docker no válida", s) }
}

/// Nombre o UUID de una VM de VirtualBox. Los nombres pueden llevar espacios y acentos.
pub fn vbox_target(s: &str) -> Result<&str> {
    let ok = !s.is_empty()
        && s.chars().count() <= 128
        && !s.starts_with('-')
        && !s.chars().any(|c| c.is_control());
    if ok { Ok(s) } else { bad("máquina virtual no válida", s) }
}

/// Interfaz de red del host o nombre de una red de VirtualBox. Puede llevar espacios
/// (p. ej. `VirtualBox Host-Only Ethernet Adapter`), pero no empezar por `-` ni llevar controles.
pub fn host_iface(s: &str) -> Result<&str> {
    let ok = !s.is_empty()
        && s.chars().count() <= 128
        && !s.starts_with('-')
        && !s.chars().any(|c| c.is_control());
    if ok { Ok(s) } else { bad("interfaz de red no válida", s) }
}

/// Nombre de interfaz dentro de Linux (`IFNAMSIZ` = 16, incluido el NUL).
pub fn linux_iface(s: &str) -> Result<&str> {
    let mut chars = s.chars();
    let ok = matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && s.len() <= 15
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if ok { Ok(s) } else { bad("interfaz Linux no válida", s) }
}

/// Usuario Linux para `VBoxManage guestcontrol`.
pub fn guest_user(s: &str) -> Result<&str> {
    let mut chars = s.chars();
    let ok = matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric() || c == '_')
        && s.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if ok { Ok(s) } else { bad("usuario de la VM no válido", s) }
}

/// Contraseña: no puede llevar controles (romperían el fichero temporal) ni ser enorme.
pub fn guest_password(s: &str) -> Result<&str> {
    let ok = s.chars().count() <= 1024 && !s.chars().any(|c| c.is_control());
    if ok { Ok(s) } else { bad("contraseña no válida", s) }
}

/// Etiqueta de nombre de host para `/etc/hosts`: `[a-z0-9-]`, 1-63 caracteres y
/// sin empezar ni terminar en guion.
pub fn hostname_label(s: &str) -> Result<&str> {
    let ok = !s.is_empty()
        && s.len() <= 63
        && !s.starts_with('-')
        && !s.ends_with('-')
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok { Ok(s) } else { bad("nombre de host no válido", s) }
}

/// IPv4 que se puede escribir en `/etc/hosts`. Se rechazan las direcciones que no
/// identifican a una máquina concreta del laboratorio.
pub fn hosts_ipv4(s: &str) -> Result<Ipv4Addr> {
    let Ok(addr) = s.parse::<Ipv4Addr>() else {
        return bad("dirección IPv4 no válida", s);
    };
    if addr.is_unspecified() || addr.is_multicast() || addr.is_broadcast() || addr.is_loopback() {
        return bad("dirección IPv4 no utilizable en /etc/hosts", s);
    }
    Ok(addr)
}

/// Redes gestionadas por VMLab: `vmlab-` + [a-z0-9-].
pub fn network_name(s: &str) -> Result<&str> {
    let ok = s.len() <= 40
        && s.strip_prefix("vmlab-").is_some_and(|r| {
            !r.is_empty() && r.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        });
    if ok { Ok(s) } else { bad("nombre de red no válido", s) }
}

/// `true` para redes nuevas (`vmlab-*`) y para las antiguas (`lablab-*`), que se
/// desconectan y limpian durante la migración. No se aceptan para crear redes.
pub fn managed_network(s: &str) -> bool {
    let valid = |r: &str| {
        !r.is_empty() && r.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    };
    s.len() <= 40
        && (s.strip_prefix("vmlab-").is_some_and(valid) || s.strip_prefix("lablab-").is_some_and(valid))
}

/// Identificador de una instantánea del laboratorio: `[a-z0-9-]`, 1-40 caracteres.
pub fn labsnap_id(s: &str) -> Result<&str> {
    let ok = !s.is_empty()
        && s.len() <= 40
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok { Ok(s) } else { bad("identificador de instantánea no válido", s) }
}

/// Nombre visible de una instantánea: 1-40 caracteres, letras, números, espacios
/// y `- _ . ( )`. Se admiten tildes y `ñ`; se rechazan comillas, barras y controles.
pub fn labsnap_label(s: &str) -> Result<&str> {
    let count = s.chars().count();
    let ok = count > 0
        && count <= 40
        && s.chars().all(|c| {
            c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '(' | ')')
        });
    if ok { Ok(s) } else { bad("nombre de instantánea no válido", s) }
}

/// Deriva el identificador de una instantánea a partir de su nombre visible.
///
/// Las tildes y la `ñ` se convierten a su letra base; cualquier otro carácter
/// que no sea una letra ASCII o un dígito se convierte en un guion. Los guiones
/// repetidos y los de los extremos se eliminan, y el resultado se limita a 40.
pub fn labsnap_slug(label: &str) -> String {
    let mut out = String::new();
    for c in label.chars() {
        let base = match c {
            'á' | 'à' | 'ä' | 'â' | 'ã' | 'å' | 'Á' | 'À' | 'Ä' | 'Â' | 'Ã' | 'Å' => 'a',
            'é' | 'è' | 'ë' | 'ê' | 'É' | 'È' | 'Ë' | 'Ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' | 'Í' | 'Ì' | 'Ï' | 'Î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' | 'õ' | 'Ó' | 'Ò' | 'Ö' | 'Ô' | 'Õ' => 'o',
            'ú' | 'ù' | 'ü' | 'û' | 'Ú' | 'Ù' | 'Ü' | 'Û' => 'u',
            'ñ' | 'Ñ' => 'n',
            'ç' | 'Ç' => 'c',
            c if c.is_ascii_alphanumeric() => c.to_ascii_lowercase(),
            _ => '-',
        };
        if base == '-' {
            if !out.is_empty() && !out.ends_with('-') {
                out.push('-');
            }
        } else {
            out.push(base);
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.len() > 40 {
        out.truncate(40);
        while out.ends_with('-') {
            out.pop();
        }
    }
    out
}

/// Límites de las condiciones de red de un nodo. Todos los valores llegan de la UI
/// y se comprueban antes de convertirse en argumentos de `tc` o `VBoxManage`.
pub fn link_conditions(c: &LinkConditions) -> Result<()> {
    if c.delay_ms > 10_000 {
        return Err(CoreError::Invalid("el retardo debe estar entre 0 y 10000 ms".into()));
    }
    if c.jitter_ms > 10_000 {
        return Err(CoreError::Invalid("el jitter debe estar entre 0 y 10000 ms".into()));
    }
    if c.jitter_ms > c.delay_ms {
        return Err(CoreError::Invalid("el jitter no puede superar el retardo".into()));
    }
    if c.loss_tenths > 1_000 {
        return Err(CoreError::Invalid("la pérdida debe estar entre 0 y 100 %".into()));
    }
    if let Some(rate) = c.rate_kbit {
        if !(8..=1_000_000).contains(&rate) {
            return Err(CoreError::Invalid(
                "la velocidad debe estar entre 8 y 1000000 kbit/s".into(),
            ));
        }
    }
    Ok(())
}

/// Límites de las opciones con las que se crea un contenedor Docker: variables de
/// entorno y puertos publicados. Los nombres de variable solo admiten ASCII para
/// que Docker los interprete igual en cualquier plataforma.
pub fn docker_run_options(o: &DockerRunOptions) -> Result<()> {
    if o.env.len() > 32 {
        return Err(CoreError::Invalid(
            "no se pueden definir más de 32 variables de entorno".into(),
        ));
    }
    let mut keys = HashSet::new();
    for e in &o.env {
        let mut chars = e.key.chars();
        let key_ok = matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
            && e.key.len() <= 128
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !key_ok {
            return bad("nombre de variable de entorno no válido", &e.key);
        }
        if e.value.chars().count() > 1024 {
            return Err(CoreError::Invalid(format!(
                "el valor de la variable de entorno «{}» no puede superar los 1024 caracteres",
                e.key
            )));
        }
        if e.value.chars().any(|c| c.is_control()) {
            return Err(CoreError::Invalid(format!(
                "el valor de la variable de entorno «{}» no puede contener caracteres de control",
                e.key
            )));
        }
        if !keys.insert(e.key.as_str()) {
            return Err(CoreError::Invalid(format!(
                "la variable de entorno «{}» está repetida",
                e.key
            )));
        }
    }

    if o.ports.len() > 32 {
        return Err(CoreError::Invalid("no se pueden publicar más de 32 puertos".into()));
    }
    let mut published = HashSet::new();
    for p in &o.ports {
        if p.host == 0 || p.container == 0 {
            return Err(CoreError::Invalid("los puertos deben estar entre 1 y 65535".into()));
        }
        if !published.insert((p.host, p.proto)) {
            return Err(CoreError::Invalid(format!(
                "el puerto {} ya está publicado en {}",
                p.host,
                p.proto.as_str()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EnvVar, PortMap, PortProto};

    #[test]
    fn validates_docker_run_options() {
        let ok = DockerRunOptions {
            env: vec![EnvVar { key: "MI_VAR".into(), value: "hola mundo".into() }],
            ports: vec![
                PortMap { host: 8080, container: 80, proto: PortProto::Tcp },
                PortMap { host: 5353, container: 53, proto: PortProto::Udp },
            ],
        };
        assert!(docker_run_options(&ok).is_ok());

        let too_many_env = DockerRunOptions {
            env: (0..33).map(|i| EnvVar { key: format!("V{i}"), value: String::new() }).collect(),
            ..Default::default()
        };
        assert!(docker_run_options(&too_many_env).is_err());

        for key in ["", "1VAR", "CON-GUION", "CON ESPACIO", &"a".repeat(129)] {
            let o = DockerRunOptions {
                env: vec![EnvVar { key: key.into(), value: String::new() }],
                ..Default::default()
            };
            assert!(docker_run_options(&o).is_err(), "{key:?}");
        }
        let longest = DockerRunOptions {
            env: vec![EnvVar { key: "a".repeat(128), value: String::new() }],
            ..Default::default()
        };
        assert!(docker_run_options(&longest).is_ok());

        let control = DockerRunOptions {
            env: vec![EnvVar { key: "OK".into(), value: "a\nb".into() }],
            ..Default::default()
        };
        assert!(docker_run_options(&control).is_err());

        let too_long = DockerRunOptions {
            env: vec![EnvVar { key: "OK".into(), value: "a".repeat(1025) }],
            ..Default::default()
        };
        assert!(docker_run_options(&too_long).is_err());

        let repeated = DockerRunOptions {
            env: vec![
                EnvVar { key: "A".into(), value: "1".into() },
                EnvVar { key: "A".into(), value: "2".into() },
            ],
            ..Default::default()
        };
        assert!(docker_run_options(&repeated).is_err());

        let too_many_ports = DockerRunOptions {
            ports: (1..=33)
                .map(|i| PortMap { host: i, container: i, proto: PortProto::Tcp })
                .collect(),
            ..Default::default()
        };
        assert!(docker_run_options(&too_many_ports).is_err());

        let zero_host = DockerRunOptions {
            ports: vec![PortMap { host: 0, container: 80, proto: PortProto::Tcp }],
            ..Default::default()
        };
        assert!(docker_run_options(&zero_host).is_err());

        let zero_container = DockerRunOptions {
            ports: vec![PortMap { host: 8080, container: 0, proto: PortProto::Tcp }],
            ..Default::default()
        };
        assert!(docker_run_options(&zero_container).is_err());

        let repeated_port = DockerRunOptions {
            ports: vec![
                PortMap { host: 8080, container: 80, proto: PortProto::Tcp },
                PortMap { host: 8080, container: 81, proto: PortProto::Tcp },
            ],
            ..Default::default()
        };
        assert!(docker_run_options(&repeated_port).is_err());

        let same_host_other_proto = DockerRunOptions {
            ports: vec![
                PortMap { host: 8080, container: 80, proto: PortProto::Tcp },
                PortMap { host: 8080, container: 80, proto: PortProto::Udp },
            ],
            ..Default::default()
        };
        assert!(docker_run_options(&same_host_other_proto).is_ok());
    }

    #[test]
    fn rejects_option_injection() {
        assert!(container_name("--privileged").is_err());
        assert!(docker_image("-v /:/host").is_err());
        assert!(vbox_target("--type").is_err());
    }

    #[test]
    fn rejects_shell_metacharacters() {
        for s in ["a;rm -rf /", "a b", "a$(id)", "a`id`", "a|b", "a&b", "a\nb"] {
            assert!(container_name(s).is_err(), "{s}");
        }
        assert!(docker_image("nginx; id").is_err());
    }

    #[test]
    fn accepts_normal_values() {
        assert!(container_name("web-01_a.b").is_ok());
        assert!(docker_image("ghcr.io/org/app:1.2@sha256").is_ok());
        assert!(vbox_target("Ubuntu Server 22.04 (lab)").is_ok());
        assert!(vbox_target("c0ffee00-1111-2222-3333-444455556666").is_ok());
        assert!(network_name("vmlab-0a1b2c3d").is_ok());
        assert!(network_name("bridge").is_err());
        assert!(network_name("vmlab-").is_err());
        assert!(network_name("lablab-0a1b2c3d").is_err(), "las redes antiguas no se crean");
        assert!(managed_network("vmlab-0a1b2c3d"));
        assert!(managed_network("lablab-0a1b2c3d"));
        assert!(!managed_network("bridge"));
    }

    #[test]
    fn validates_guest_inputs() {
        assert!(linux_iface("eth0").is_ok());
        assert!(linux_iface("enp0s3.100").is_ok());
        assert!(linux_iface("eth 0").is_err());
        assert!(linux_iface("-eth0").is_err());
        assert!(guest_user("ubuntu").is_ok());
        assert!(guest_user("--root").is_err());
        assert!(guest_password("una contraseña con espacios").is_ok());
        assert!(guest_password("a\nb").is_err());
        assert!(host_iface("VirtualBox Host-Only Ethernet Adapter").is_ok());
        assert!(host_iface("--evil").is_err());
    }

    #[test]
    fn validates_hosts_names_and_addresses() {
        assert!(hostname_label("web-01").is_ok());
        assert!(hostname_label("a").is_ok());
        assert!(hostname_label(&"a".repeat(63)).is_ok());
        for s in ["", "-web", "web-", "Web", "web_01", "web 01", "web;id", "ñandu", &"a".repeat(64)] {
            assert!(hostname_label(s).is_err(), "{s}");
        }

        assert_eq!(hosts_ipv4("10.77.9.11").unwrap(), Ipv4Addr::new(10, 77, 9, 11));
        for s in ["0.0.0.0", "224.0.0.1", "255.255.255.255", "127.0.0.1", "10.77.9", "10.77.9.999", "::1"] {
            assert!(hosts_ipv4(s).is_err(), "{s}");
        }
    }

    #[test]
    fn derives_labsnap_slugs() {
        assert_eq!(labsnap_slug("Añadir ü ÁÉÍÓÚ"), "anadir-u-aeiou");
        assert_eq!(labsnap_slug("Lab prueba #1 (copia)"), "lab-prueba-1-copia");
        assert_eq!(labsnap_slug("--Hola--  mundo--"), "hola-mundo");
        assert_eq!(labsnap_slug("¿¡###"), "");
        assert_eq!(labsnap_slug(&"a".repeat(60)), "a".repeat(40));
        let exact = "a".repeat(40);
        assert_eq!(labsnap_slug(&exact), exact);
    }

    #[test]
    fn validates_labsnap_ids_and_labels() {
        assert!(labsnap_id("lab-1").is_ok());
        assert!(labsnap_id("").is_err());
        assert!(labsnap_id("Lab-1").is_err());
        assert!(labsnap_id("lab_1").is_err());
        assert!(labsnap_id(&"a".repeat(41)).is_err());

        assert!(labsnap_label("Lab 1 (copia) - ñandú").is_ok());
        assert!(labsnap_label("").is_err());
        assert!(labsnap_label(&"a".repeat(41)).is_err());
        for s in ["a\"b", "a|b", "a/b", "a\\b", "a\nb", "a\u{7}b"] {
            assert!(labsnap_label(s).is_err(), "{s:?}");
        }
    }

    #[test]
    fn validates_link_conditions() {
        let ok = LinkConditions {
            delay_ms: 50,
            jitter_ms: 10,
            loss_tenths: 15,
            rate_kbit: Some(20_000),
        };
        assert!(link_conditions(&ok).is_ok());

        let jitter_over_delay = LinkConditions { jitter_ms: 51, ..ok.clone() };
        assert!(link_conditions(&jitter_over_delay).is_err());

        let too_much_loss = LinkConditions { loss_tenths: 1_001, ..ok.clone() };
        assert!(link_conditions(&too_much_loss).is_err());

        let too_slow = LinkConditions { rate_kbit: Some(7), ..ok.clone() };
        assert!(link_conditions(&too_slow).is_err());

        let too_fast = LinkConditions { rate_kbit: Some(1_000_001), ..ok.clone() };
        assert!(link_conditions(&too_fast).is_err());

        let delay_out_of_range = LinkConditions { delay_ms: 10_001, jitter_ms: 0, ..ok };
        assert!(link_conditions(&delay_out_of_range).is_err());
    }
}
