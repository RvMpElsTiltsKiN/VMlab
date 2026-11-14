import type { Edge } from '@xyflow/react';
import type { MachineNode, TopologyPayload } from '../types';

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
  const parent = new Map<string, string>(nodes.map((n) => [n.id, n.id]));
  const find = (x: string): string => {
    let r = x;
    while (parent.get(r) !== r) r = parent.get(r)!;
    parent.set(x, r);
    return r;
  };
  for (const e of edges) {
    if (parent.has(e.source) && parent.has(e.target)) parent.set(find(e.source), find(e.target));
  }
  const buckets = new Map<string, string[]>();
  for (const n of nodes) {
    const r = find(n.id);
    buckets.set(r, [...(buckets.get(r) ?? []), n.id]);
  }
  const byId = new Map(nodes.map((n) => [n.id, n]));
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

export function toPayload(nodes: MachineNode[], edges: Edge[]): TopologyPayload {
  return {
    nodes: nodes.map((n) => ({
      id: n.id,
      kind: n.data.kind,
      target: n.data.target,
      image: n.data.image || undefined,
      label: n.data.label,
      net: n.data.kind === 'vbox' ? n.data.net : undefined,
      link: n.data.link,
      options: n.data.kind === 'docker' ? n.data.run : undefined,
    })),
    edges: edges.map((e) => ({ from: e.source, to: e.target })),
  };
}

export const NET_COLORS = ['#38d6b0', '#f5b84b', '#c084fc', '#fb7185', '#60a5fa', '#a3e635'];
