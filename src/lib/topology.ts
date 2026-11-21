import { DEFAULT_MOUNT_PATH, edgeKind, isDisk } from './disk';
import { isOptical } from './optical';
import type { DiskMount, DiskSpec, Edge, MachineNode, OpticalSpec, TopologyPayload } from '../types';

export interface NetGroup {
  index: number;
  nodeIds: string[];
  mixed: boolean;
}

/**
 * Espejo en TypeScript de `topology::plan` (Rust): cada componente conexo con ≥ 2 nodos
 * es una red local. Sirve para colorear aristas y mostrar el resumen sin ir al backend.
 */
export function computeGroups(nodes: MachineNode[], edges: Edge[]) {
  // Los periféricos no son máquinas: no forman segmentos de red ni se sondean.
  const machines = nodes.filter((n) => !isDisk(n) && !isOptical(n));
  const parent = new Map<string, string>(machines.map((n) => [n.id, n.id]));
  const find = (x: string): string => {
    let r = x;
    while (parent.get(r) !== r) r = parent.get(r)!;
    parent.set(x, r);
    return r;
  };
  for (const e of edges) {
    if (edgeKind(e) !== 'network') continue;
    if (parent.has(e.source) && parent.has(e.target)) parent.set(find(e.source), find(e.target));
  }
  const buckets = new Map<string, string[]>();
  for (const n of machines) {
    const r = find(n.id);
    buckets.set(r, [...(buckets.get(r) ?? []), n.id]);
  }
  const byId = new Map(machines.map((n) => [n.id, n]));
  const groups: NetGroup[] = [];
  const nodeGroup = new Map<string, number>();
  for (const ids of buckets.values()) {
    if (ids.length < 2) continue;
    const kinds = new Set(ids.map((id) => byId.get(id)!.data.kind));
    const index = groups.length;
    groups.push({ index, nodeIds: ids, mixed: kinds.size > 1 });
    ids.forEach((id) => nodeGroup.set(id, index));
  }
  return { groups, nodeGroup };
}

/** Convierte el nombre visible de un nodo en una etiqueta válida para `/etc/hosts`. */
export function hostSlug(label: string): string {
  const base = label
    .normalize('NFD')
    .replace(/[\u0300-\u036f]/g, '')
    .toLowerCase()
    .replace(/[^a-z0-9-]+/g, '-')
    .replace(/-+/g, '-')
    .replace(/^-|-$/g, '')
    .slice(0, 63)
    .replace(/-+$/g, '');
  return base || 'vm';
}

/** Añade `-2`, `-3`… si el nombre ya está ocupado, respetando el límite de 63. */
export function uniqueHostSlug(label: string, used: Set<string>): string {
  const base = hostSlug(label);
  let candidate = base;
  for (let n = 2; used.has(candidate); n++) {
    const suffix = `-${n}`;
    candidate = `${base.slice(0, 63 - suffix.length).replace(/-+$/g, '')}${suffix}`;
  }
  used.add(candidate);
  return candidate;
}

/** Adaptación de la ISO al contrato IPC: una unidad vacía viaja sin campo. */
function toOpticalSpec(spec: OpticalSpec | undefined): OpticalSpec | undefined {
  const iso = spec?.iso?.trim();
  return iso ? { iso } : undefined;
}

/** Adaptación del disco al contrato IPC: las VM no reciben ruta y sí tamaño. */
function toDiskSpec(spec: DiskSpec | undefined, byId: Map<string, MachineNode>): DiskSpec | undefined {
  if (!spec) return undefined;
  const mounts: DiskMount[] = spec.mounts.map((mount) => {
    const target = byId.get(mount.container);
    if (target?.data.kind === 'vbox') return { container: mount.container };
    return { container: mount.container, path: mount.path ?? DEFAULT_MOUNT_PATH };
  });
  const hasVm = mounts.some((m) => byId.get(m.container)?.data.kind === 'vbox');
  return {
    ...(spec.sizeMb === undefined ? {} : { sizeMb: spec.sizeMb }),
    shared: hasVm ? false : spec.shared,
    mounts,
  };
}

export function toPayload(nodes: MachineNode[], edges: Edge[]): TopologyPayload {
  const byId = new Map(nodes.map((n) => [n.id, n]));
  return {
    nodes: nodes.map((n) => ({
      id: n.id,
      kind: n.data.kind,
      target: n.data.target,
      image: n.data.image || undefined,
      label: n.data.label,
      net: n.data.kind === 'vbox' ? n.data.net : undefined,
      link: isDisk(n) || isOptical(n) ? undefined : n.data.link,
      options: n.data.kind === 'docker' ? n.data.run : undefined,
      disk: isDisk(n) ? toDiskSpec(n.data.disk, byId) : undefined,
      optical: isOptical(n) ? toOpticalSpec(n.data.optical) : undefined,
    })),
    edges: edges.map((e) => ({ from: e.source, to: e.target, kind: edgeKind(e) })),
  };
}

export const NET_COLORS = ['#38d6b0', '#f5b84b', '#c084fc', '#fb7185', '#60a5fa', '#a3e635'];
