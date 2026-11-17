import type { Edge, MachineNode, NodeKind } from '../types';

/** Tipos del lienzo. `MachineKind` sigue siendo el contrato IPC de máquinas. */
export const DEFAULT_MOUNT_PATH = '/data';
export const VOLUME_PREFIX = 'vmlab-vol-';

export const isDisk = (node: MachineNode): boolean => node.data.kind === 'disk';

export function edgeKind(edge: Edge): 'network' | 'mount' {
  return edge.kind === 'mount' ? 'mount' : 'network';
}

/** La ruta de un enlace de montaje viaja en `edge.data.path`. */
export function mountPathOf(edge: Edge): string | undefined {
  const raw = edge.data?.path;
  return typeof raw === 'string' ? raw : undefined;
}

/**
 * Saneado aproximado del nombre visible: minúsculas, sin tildes, separadores
 * colapsados y máximo 40 caracteres. El backend es la autoridad final.
 */
export function diskName(raw: string): string {
  const folded = raw.normalize('NFD').replace(/[\u0300-\u036f]/g, '').toLowerCase();
  let out = '';
  for (const ch of folded) {
    const base = ch === 'ñ' ? 'n' : ch;
    if (/[a-z0-9._]/.test(base)) {
      out += base;
    } else if (out && !out.endsWith('-')) {
      out += '-';
    }
  }
  return out
    .replace(/^[^a-z0-9]+/, '')
    .replace(/[^a-z0-9]+$/, '')
    .slice(0, 40)
    .replace(/[^a-z0-9]+$/, '');
}

/** Nombre real del volumen Docker que crea el backend al aplicar la red. */
export function volumeName(raw: string): string {
  return `${VOLUME_PREFIX}${diskName(raw)}`;
}

export function normalizeMountPath(path: string): string {
  const trimmed = path.replace(/\/+$/, '');
  return trimmed || '/';
}

const DANGEROUS_PATHS = ['/', '/proc', '/sys', '/dev', '/etc', '/run', '/var/run'];

/** Misma validación aproximada que `validate::mount_path` del núcleo. */
export function validMountPath(path: string): boolean {
  const normalized = normalizeMountPath(path);
  return (
    path.startsWith('/') &&
    [...path].length <= 1024 &&
    !path.includes(':') &&
    !path.includes(',') &&
    !path.includes('\\') &&
    !/[\p{Cc}]/u.test(path) &&
    !path.split('/').includes('..') &&
    !DANGEROUS_PATHS.some((p) => normalized === p || normalized.startsWith(`${p}/`))
  );
}

/** Siguiente nombre libre: datos, datos2, datos3… */
export function nextDiskName(nodes: MachineNode[]): string {
  const used = new Set(nodes.filter(isDisk).map((n) => n.data.target));
  let n = 1;
  while (used.has(n === 1 ? 'datos' : `datos${n}`)) n += 1;
  return n === 1 ? 'datos' : `datos${n}`;
}

/** Rutas `/data`, `/data2`… ya ocupadas en un contenedor por otros discos. */
export function nextMountPath(containerId: string, edges: Edge[], exceptDiskId?: string): string {
  const used = new Set(
    edges
      .filter((e) => edgeKind(e) === 'mount' && mountPathOf(e))
      .filter((e) => {
        const touches = e.source === containerId || e.target === containerId;
        const other = e.source === containerId ? e.target : e.source;
        return touches && (!exceptDiskId || other !== exceptDiskId);
      })
      .map((e) => normalizeMountPath(mountPathOf(e)!)),
  );
  let n = 1;
  while (used.has(n === 1 ? DEFAULT_MOUNT_PATH : `${DEFAULT_MOUNT_PATH}${n}`)) n += 1;
  return n === 1 ? DEFAULT_MOUNT_PATH : `${DEFAULT_MOUNT_PATH}${n}`;
}

const sameMounts = (a: { container: string; path: string }[] | undefined, b: { container: string; path: string }[]): boolean =>
  !!a && a.length === b.length && a.every((m, i) => m.container === b[i].container && m.path === b[i].path);

/**
 * Mantiene `edges` y `disk.mounts` sincronizados en todo momento.
 *
 * La ruta canónica de cada enlace está en `edge.data.path`; `disk.mounts` se
 * reconstruye a partir de las aristas de montaje. Se descartan los enlaces
 * inválidos (disco-disco, disco-VM, montaje sin disco) para que el backend
 * nunca reciba una topología incoherente.
 */
export function syncDiskMounts(
  nodes: MachineNode[],
  edges: Edge[],
): { nodes: MachineNode[]; edges: Edge[] } {
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const diskIds = new Set(nodes.filter(isDisk).map((n) => n.id));
  const mountsByDisk = new Map<string, { container: string; path: string }[]>();
  const linkedContainers = new Set<string>();
  const outEdges: Edge[] = [];

  for (const edge of edges) {
    const a = byId.get(edge.source);
    const b = byId.get(edge.target);
    if (!a || !b || a.id === b.id) continue;
    const aDisk = diskIds.has(a.id);
    const bDisk = diskIds.has(b.id);

    if (!aDisk && !bDisk) {
      // Un enlace de montaje necesita un disco en uno de sus extremos.
      if (edgeKind(edge) === 'mount') continue;
      outEdges.push({ ...edge, kind: 'network' });
      continue;
    }
    if (aDisk && bDisk) continue;

    const diskId = aDisk ? a.id : b.id;
    const container = aDisk ? b : a;
    if (container.data.kind !== 'docker') continue;
    const pairKey = `${diskId}:${container.id}`;
    if (linkedContainers.has(pairKey)) continue;
    linkedContainers.add(pairKey);

    const existing = mountsByDisk.get(diskId)?.find((m) => m.container === container.id);
    const path = mountPathOf(edge) ?? existing?.path ?? DEFAULT_MOUNT_PATH;
    const list = mountsByDisk.get(diskId) ?? [];
    mountsByDisk.set(diskId, [...list, { container: container.id, path }]);
    outEdges.push({ ...edge, kind: 'mount', data: { ...(edge.data ?? {}), path } });
  }

  const outNodes = nodes.map((node) => {
    if (node.data.kind !== 'disk') {
      return node.data.disk === undefined ? node : { ...node, data: { ...node.data, disk: undefined } };
    }
    const mounts = mountsByDisk.get(node.id) ?? [];
    if (sameMounts(node.data.disk?.mounts, mounts)) return node;
    return {
      ...node,
      data: { ...node.data, disk: { shared: node.data.disk?.shared ?? false, mounts } },
    };
  });

  return { nodes: outNodes, edges: outEdges };
}

/** Tipo de nodo para el payload IPC: los discos nunca llegan a acciones de máquina. */
export function machineKindOf(kind: NodeKind): 'vbox' | 'docker' | null {
  return kind === 'disk' ? null : kind;
}
