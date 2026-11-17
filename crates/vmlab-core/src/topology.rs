//! Planificación pura (sin I/O): agrupa los nodos conectados por aristas en segmentos de red.
//! Cada componente conexo con ≥ 2 nodos es una red local; el resto son nodos aislados.
use crate::model::{EdgeKind, NodeKind, NodeSpec, Topology};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub name: String,
    pub nodes: Vec<NodeSpec>,
}

#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub segments: Vec<Segment>,
    pub isolated: Vec<NodeSpec>,
    pub warnings: Vec<String>,
}

fn find(parent: &mut [usize], x: usize) -> usize {
    let mut root = x;
    while parent[root] != root {
        root = parent[root];
    }
    let mut cur = x;
    while parent[cur] != root {
        let next = parent[cur];
        parent[cur] = root;
        cur = next;
    }
    root
}

/// Nombre determinista: depende solo del conjunto de nodos, no del orden de entrada.
fn segment_name(ids: &mut Vec<&str>) -> String {
    ids.sort_unstable();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a
    for id in ids.iter() {
        for b in id.bytes().chain(std::iter::once(0xff)) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("vmlab-{:08x}", (h & 0xffff_ffff) as u32)
}

pub fn plan(t: &Topology) -> Plan {
    let index: HashMap<&str, usize> = t.nodes.iter().enumerate().map(|(i, n)| (n.id.as_str(), i)).collect();
    // Un disco no es un nodo de red: sus enlaces son montajes y no deben unir
    // contenedores ni cambiar el nombre (hash) del segmento.
    let disks: HashSet<&str> = t
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Disk)
        .map(|n| n.id.as_str())
        .collect();
    let mut parent: Vec<usize> = (0..t.nodes.len()).collect();
    for e in &t.edges {
        if e.kind != EdgeKind::Network || disks.contains(e.from.as_str()) || disks.contains(e.to.as_str()) {
            continue;
        }
        if let (Some(&a), Some(&b)) = (index.get(e.from.as_str()), index.get(e.to.as_str())) {
            let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
            if ra != rb {
                parent[ra] = rb;
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..t.nodes.len() {
        if disks.contains(t.nodes[i].id.as_str()) {
            continue;
        }
        let r = find(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }

    let mut plan = Plan::default();
    for members in groups.into_values() {
        let nodes: Vec<NodeSpec> = members.iter().map(|&i| t.nodes[i].clone()).collect();
        if nodes.len() < 2 {
            plan.isolated.extend(nodes);
            continue;
        }
        let name = segment_name(&mut nodes.iter().map(|n| n.id.as_str()).collect());
        let has_vbox = nodes.iter().any(|n| n.kind == NodeKind::Vbox);
        let has_docker = nodes.iter().any(|n| n.kind == NodeKind::Docker);
        if has_vbox && has_docker {
            plan.warnings.push(format!(
                "La red {name} mezcla VirtualBox y Docker: cada tecnología se conecta a su propia red, pero entre ellas no hay enlace automático."
            ));
        }
        plan.segments.push(Segment { name, nodes });
    }
    plan.segments.sort_by(|a, b| a.name.cmp(&b.name));
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Edge;

    fn n(id: &str, kind: NodeKind) -> NodeSpec {
        NodeSpec { id: id.into(), kind, target: id.into(), ..NodeSpec::default() }
    }
    fn e(a: &str, b: &str) -> Edge {
        Edge { from: a.into(), to: b.into(), kind: EdgeKind::Network }
    }
    fn mount(a: &str, b: &str) -> Edge {
        Edge { from: a.into(), to: b.into(), kind: EdgeKind::Mount }
    }

    #[test]
    fn groups_components_and_isolates() {
        let t = Topology {
            nodes: vec![n("a", NodeKind::Vbox), n("b", NodeKind::Vbox), n("c", NodeKind::Docker), n("d", NodeKind::Docker), n("e", NodeKind::Docker)],
            edges: vec![e("a", "b"), e("c", "d"), e("x", "a"), e("e", "e")],
        };
        let p = plan(&t);
        assert_eq!(p.segments.len(), 2);
        assert_eq!(p.isolated.len(), 1);
        assert_eq!(p.isolated[0].id, "e");
        assert!(p.warnings.is_empty());
    }

    #[test]
    fn transitive_links_merge() {
        let t = Topology {
            nodes: vec![n("a", NodeKind::Docker), n("b", NodeKind::Docker), n("c", NodeKind::Docker)],
            edges: vec![e("a", "b"), e("c", "b")],
        };
        let p = plan(&t);
        assert_eq!(p.segments.len(), 1);
        assert_eq!(p.segments[0].nodes.len(), 3);
    }

    #[test]
    fn name_is_order_independent_and_valid() {
        let t1 = Topology { nodes: vec![n("a", NodeKind::Docker), n("b", NodeKind::Docker)], edges: vec![e("a", "b")] };
        let t2 = Topology { nodes: vec![n("b", NodeKind::Docker), n("a", NodeKind::Docker)], edges: vec![e("b", "a")] };
        let (p1, p2) = (plan(&t1), plan(&t2));
        assert_eq!(p1.segments[0].name, p2.segments[0].name);
        assert!(crate::validate::network_name(&p1.segments[0].name).is_ok());
    }

    #[test]
    fn mixed_segment_warns() {
        let t = Topology { nodes: vec![n("a", NodeKind::Vbox), n("b", NodeKind::Docker)], edges: vec![e("a", "b")] };
        assert_eq!(plan(&t).warnings.len(), 1);
    }

    #[test]
    fn a_disk_does_not_join_two_containers() {
        let t = Topology {
            nodes: vec![
                n("web", NodeKind::Docker),
                n("db", NodeKind::Docker),
                n("datos", NodeKind::Disk),
            ],
            edges: vec![mount("datos", "web"), mount("datos", "db")],
        };
        let p = plan(&t);
        assert!(p.segments.is_empty(), "un montaje no crea red");
        assert_eq!(p.isolated.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), ["web", "db"]);
        assert!(p.warnings.is_empty());
    }

    #[test]
    fn a_disk_does_not_change_the_segment_name() {
        let base = Topology {
            nodes: vec![n("web", NodeKind::Docker), n("db", NodeKind::Docker)],
            edges: vec![e("web", "db")],
        };
        let with_disk = Topology {
            nodes: vec![
                n("web", NodeKind::Docker),
                n("db", NodeKind::Docker),
                n("datos", NodeKind::Disk),
            ],
            edges: vec![e("web", "db"), mount("datos", "web"), mount("datos", "db")],
        };
        let (a, b) = (plan(&base), plan(&with_disk));
        assert_eq!(a.segments.len(), 1);
        assert_eq!(b.segments.len(), 1);
        assert_eq!(a.segments[0].name, b.segments[0].name);
        assert_eq!(b.segments[0].nodes.len(), 2, "el disco no entra en el segmento");
        assert_eq!(b.isolated.len(), 0);
    }

    #[test]
    fn network_edges_through_a_disk_are_ignored() {
        let t = Topology {
            nodes: vec![n("a", NodeKind::Docker), n("b", NodeKind::Docker), n("d", NodeKind::Disk)],
            edges: vec![e("a", "d"), e("d", "b")],
        };
        let p = plan(&t);
        assert!(p.segments.is_empty());
        assert_eq!(p.isolated.len(), 2);
    }
}
