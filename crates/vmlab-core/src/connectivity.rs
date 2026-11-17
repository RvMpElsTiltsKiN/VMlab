//! Sonda de comunicación del lienzo.
//!
//! El color de cada cable depende de datos reales, no solo de que la máquina esté
//! encendida: se lee la IPv4 del adaptador que gestiona VMLab (guest properties de
//! VirtualBox) o de la red `vmlab-*` del contenedor. Nunca se ejecuta nada dentro
//! del guest, así que no hacen falta usuario ni contraseña.
use crate::model::{LinkInfo, NodeIssue, NodeKind, NodeProbe, ProbeNode};
use crate::tools::Tools;
use crate::{docker, validate, vbox};
use serde::Serialize;
use std::net::Ipv4Addr;

const DEFAULT_ADAPTER: u8 = 2;

async fn probe_one(tools: &Tools, node: ProbeNode) -> NodeProbe {
    let link: crate::error::Result<LinkInfo> = match node.kind {
        NodeKind::Vbox => {
            vbox::guest_link(tools, &node.target, node.adapter.unwrap_or(DEFAULT_ADAPTER)).await
        }
        NodeKind::Docker => docker::vmlab_link(tools, &node.target).await,
        // Un disco gestiona un volumen: no participa en el sondeo de red.
        NodeKind::Disk => Err(crate::error::CoreError::Invalid(
            "un disco no tiene conexión de red".into(),
        )),
    };
    match link {
        Ok(l) => NodeProbe {
            kind: node.kind,
            target: node.target,
            segment: l.segment,
            ipv4: l.ipv4,
            netmask: l.netmask,
            issues: l.issues,
            note: l.note,
        },
        Err(e) => {
            let detail = e.to_string();
            NodeProbe {
                kind: node.kind,
                target: node.target,
                segment: None,
                ipv4: None,
                netmask: None,
                issues: vec![NodeIssue::ProbeFailed { detail: Some(detail.clone()) }],
                note: Some(detail),
            }
        }
    }
}

/// Diagnóstico de un cable a partir de la sonda de sus dos extremos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkDiagnostics {
    /// Equivale al color actual del cable: mismo segmento, ambos con IPv4 y en
    /// los mismos tres primeros octetos.
    pub live: bool,
    /// Problemas de los dos extremos, primero los de `a` y después los de `b`.
    pub issues: Vec<NodeIssue>,
    /// Motivo extra: ambos tienen IPv4 y máscara pero no comparten subred.
    pub different_subnet: bool,
}

/// Calcula si dos IPv4 con sus máscaras pertenecen a la misma subred.
///
/// Devuelve `None` si falta algún dato o no es válido. Si las máscaras no
/// coinciden se consideran subredes distintas: VMLab exige la misma
/// configuración en ambos extremos.
pub fn same_subnet(a: &str, a_mask: &str, b: &str, b_mask: &str) -> Option<bool> {
    let a = a.parse::<Ipv4Addr>().ok()?;
    let b = b.parse::<Ipv4Addr>().ok()?;
    let a_mask = validate::netmask(a_mask)?.parse::<Ipv4Addr>().ok()?;
    let b_mask = validate::netmask(b_mask)?.parse::<Ipv4Addr>().ok()?;
    if a_mask != b_mask {
        return Some(false);
    }
    let mask = u32::from(a_mask);
    Some(u32::from(a) & mask == u32::from(b) & mask)
}

/// Regla actual del cable verde: los tres primeros octetos coinciden.
fn same_slash_24(a: &str, b: &str) -> bool {
    let pa: Vec<&str> = a.split('.').collect();
    let pb: Vec<&str> = b.split('.').collect();
    pa.len() == 4 && pb.len() == 4 && pa[..3] == pb[..3]
}

/// Calcula el estado y los problemas de un cable ya sondeado.
pub fn diagnose_link(a: &NodeProbe, b: &NodeProbe) -> LinkDiagnostics {
    let mut issues = Vec::with_capacity(a.issues.len() + b.issues.len());
    issues.extend(a.issues.iter().cloned());
    issues.extend(b.issues.iter().cloned());

    let live = match (&a.segment, &b.segment, &a.ipv4, &b.ipv4) {
        (Some(sa), Some(sb), Some(ia), Some(ib)) => sa == sb && same_slash_24(ia, ib),
        _ => false,
    };
    let different_subnet = match (
        a.ipv4.as_deref(),
        a.netmask.as_deref(),
        b.ipv4.as_deref(),
        b.netmask.as_deref(),
    ) {
        (Some(ia), Some(ma), Some(ib), Some(mb)) => same_subnet(ia, ma, ib, mb) == Some(false),
        _ => false,
    };
    LinkDiagnostics { live, issues, different_subnet }
}

/// Comprueba los nodos en serie: VirtualBox serializa el acceso a sus máquinas y
/// lanzar varias consultas a la vez provoca bloqueos temporales.
pub async fn probe(tools: &Tools, nodes: Vec<ProbeNode>) -> Vec<NodeProbe> {
    let mut out = Vec::with_capacity(nodes.len());
    for node in nodes {
        out.push(probe_one(tools, node).await);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(segment: &str, ipv4: &str, netmask: &str) -> NodeProbe {
        NodeProbe {
            kind: NodeKind::Vbox,
            target: "vm".into(),
            segment: Some(segment.into()),
            ipv4: Some(ipv4.into()),
            netmask: Some(netmask.into()),
            issues: Vec::new(),
            note: None,
        }
    }

    #[test]
    fn shares_a_subnet_only_with_the_same_mask() {
        assert_eq!(
            same_subnet("10.77.9.10", "255.255.255.0", "10.77.9.11", "255.255.255.0"),
            Some(true)
        );
        assert_eq!(
            same_subnet("10.77.9.10", "255.255.255.0", "10.77.10.11", "255.255.255.0"),
            Some(false)
        );
        // La misma /16 sí comparte subred.
        assert_eq!(
            same_subnet("10.77.9.10", "255.255.0.0", "10.77.9.11", "255.255.0.0"),
            Some(true)
        );
        // Máscaras distintas: no se considera la misma subred.
        assert_eq!(
            same_subnet("10.77.9.10", "255.255.255.0", "10.77.9.11", "255.255.0.0"),
            Some(false)
        );
        // Datos inválidos: no se puede afirmar nada.
        assert_eq!(same_subnet("10.77.9", "255.255.255.0", "10.77.9.11", "255.255.255.0"), None);
        assert_eq!(same_subnet("10.77.9.10", "", "10.77.9.11", "255.255.255.0"), None);
        assert_eq!(
            same_subnet("10.77.9.10", "255.0.255.0", "10.77.9.11", "255.255.255.0"),
            None
        );
    }

    #[test]
    fn diagnosis_keeps_the_visible_live_rule_and_flags_different_subnets() {
        let a = probe("intnet:vmlab-0a1b2c3d", "10.77.9.10", "255.255.255.0");
        let b = probe("intnet:vmlab-0a1b2c3d", "10.77.9.11", "255.255.255.0");
        let ok = diagnose_link(&a, &b);
        assert!(ok.live);
        assert!(ok.issues.is_empty());
        assert!(!ok.different_subnet);

        let other = probe("intnet:vmlab-0a1b2c3d", "10.77.10.11", "255.255.255.0");
        let diag = diagnose_link(&a, &other);
        assert!(!diag.live);
        assert!(diag.different_subnet);

        // El cable sigue verde porque la regla visible solo compara los tres
        // primeros octetos, aunque las máscaras no coincidan.
        let mismatched = probe("intnet:vmlab-0a1b2c3d", "10.77.9.11", "255.255.0.0");
        let diag = diagnose_link(&a, &mismatched);
        assert!(diag.live);
        assert!(diag.different_subnet);

        // Un segmento distinto nunca está vivo.
        let other_segment = probe("intnet:vmlab-ffffffff", "10.77.9.11", "255.255.255.0");
        assert!(!diagnose_link(&a, &other_segment).live);
    }

    #[test]
    fn diagnosis_collects_issues_from_both_ends() {
        let mut a = probe("intnet:vmlab-0a1b2c3d", "10.77.9.10", "255.255.255.0");
        a.issues.push(NodeIssue::NoGuestAdditions);
        let mut b = probe("intnet:vmlab-0a1b2c3d", "10.77.9.11", "255.255.255.0");
        b.issues.push(NodeIssue::AdapterNotOnSegment);
        let diag = diagnose_link(&a, &b);
        assert_eq!(
            diag.issues,
            vec![NodeIssue::NoGuestAdditions, NodeIssue::AdapterNotOnSegment]
        );

        let mut stopped = probe("intnet:vmlab-0a1b2c3d", "", "");
        stopped.ipv4 = None;
        stopped.netmask = None;
        stopped.issues = vec![NodeIssue::NotRunning];
        let diag = diagnose_link(&a, &stopped);
        assert!(!diag.live);
        assert!(!diag.different_subnet);
        assert_eq!(
            diag.issues,
            vec![NodeIssue::NoGuestAdditions, NodeIssue::NotRunning]
        );
    }

    #[test]
    fn diagnostics_serialize_with_camel_case_keys() {
        let diag = LinkDiagnostics {
            live: false,
            issues: vec![
                NodeIssue::NoIpv4,
                NodeIssue::ProbeFailed { detail: Some("sin respuesta".into()) },
            ],
            different_subnet: true,
        };
        let json = serde_json::to_value(&diag).unwrap();
        assert_eq!(json["live"], serde_json::json!(false));
        assert_eq!(json["differentSubnet"], serde_json::json!(true));
        assert_eq!(json["issues"][0]["code"], serde_json::json!("noIpv4"));
        assert_eq!(json["issues"][1]["code"], serde_json::json!("probeFailed"));
        assert_eq!(json["issues"][1]["detail"], serde_json::json!("sin respuesta"));
        assert!(json.get("different_subnet").is_none());
    }

    #[test]
    fn node_probe_accepts_payloads_without_the_new_fields() {
        let json = serde_json::json!({
            "kind": "vbox",
            "target": "vm",
            "segment": null,
            "ipv4": null,
            "note": null
        });
        let probe: NodeProbe = serde_json::from_value(json).unwrap();
        assert_eq!(probe.netmask, None);
        assert!(probe.issues.is_empty());
    }
}
