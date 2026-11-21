import type { Edge, MachineNode, NodeKind } from '../types';
import { isOptical } from './optical';

/** Tipos del lienzo. `MachineKind` sigue siendo el contrato IPC de máquinas. */
export const DEFAULT_MOUNT_PATH = '/data';
export const VOLUME_PREFIX = 'vmlab-vol-';
export const VDI_PREFIX = 'vmlab-disk-';
/** Igual que `DEFAULT_VDI_SIZE_MB` del núcleo (10 GB). */
export const DEFAULT_DISK_SIZE_MB = 10_240;
/** Igual que `MAX_VDI_SIZE_MB` del núcleo. */
export const MAX_DISK_SIZE_MB = 2_000_000;

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

/** Nombre del fichero VDI que crea el backend en su carpeta de datos. */
export function vdiName(raw: string): string {
  return `${VDI_PREFIX}${diskName(raw)}.vdi`;
}

/** Etiqueta visible del tamaño de un VDI: 10240 MB → «10 GB». */
export function diskSizeLabel(sizeMb?: number): string {
  const mb = sizeMb ?? DEFAULT_DISK_SIZE_MB;
  const gb = mb / 1024;
  return `${Number.isInteger(gb) ? gb : Number(gb.toFixed(2))} GB`;
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

type SyncedMount = { container: string; path?: string };

const sameMounts = (a: SyncedMount[] | undefined, b: SyncedMount[]): boolean =>
  !!a && a.length === b.length && a.every((m, i) => m.container === b[i].container && m.path === b[i].path);

/** Elimina la ruta de montaje de una arista con máquina virtual o unidad óptica. */
function withoutPath(edge: Edge): Edge {
  const data: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(edge.data ?? {})) {
    if (key !== 'path') data[key] = value;
  }
  return { ...edge, kind: 'mount', data: Object.keys(data).length ? data : undefined };
}

/**
 * Mantiene `edges`, `disk.mounts` y los enlaces de las unidades ópticas
 * sincronizados en todo momento.
 *
 * La ruta canónica de cada enlace de contenedor está en `edge.data.path`;
 * `disk.mounts` se reconstruye a partir de las aristas de montaje. Los enlaces
 * con VM o unidad óptica no tienen ruta. Se descartan los enlaces inválidos
 * (periférico-periférico, montaje sin periférico, Docker+VM en un disco, más de
 * una VM por disco, o una unidad óptica que no apunte a VirtualBox). Una unidad
 * óptica sí admite varias VMs: cada par unidad/VM conserva su propia arista.
 */
export function syncDiskMounts(
  nodes: MachineNode[],
  edges: Edge[],
): { nodes: MachineNode[]; edges: Edge[] } {
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const diskIds = new Set(nodes.filter(isDisk).map((n) => n.id));
  const opticalIds = new Set(nodes.filter(isOptical).map((n) => n.id));
  const mountsByDisk = new Map<string, SyncedMount[]>();
  /** Tipo del primer enlace válido de cada disco: no se pueden mezclar. */
  const linkedKind = new Map<string, 'docker' | 'vbox'>();
  const linkedContainers = new Set<string>();
  const vmDisks = new Set<string>();
  const linkedOpticalPairs = new Set<string>();
  const outEdges: Edge[] = [];

  for (const edge of edges) {
    const a = byId.get(edge.source);
    const b = byId.get(edge.target);
    if (!a || !b || a.id === b.id) continue;
    const aDisk = diskIds.has(a.id);
    const bDisk = diskIds.has(b.id);
    const aOptical = opticalIds.has(a.id);
    const bOptical = opticalIds.has(b.id);

    // Dos periféricos nunca se montan entre sí.
    if ((aDisk || aOptical) && (bDisk || bOptical)) continue;

    if (!aDisk && !bDisk && !aOptical && !bOptical) {
      // Un enlace de montaje necesita un disco o una unidad óptica en un extremo.
      if (edgeKind(edge) === 'mount') continue;
      outEdges.push({ ...edge, kind: 'network' });
      continue;
    }

    if (aOptical || bOptical) {
      if (edgeKind(edge) !== 'mount') continue;
      const optical = aOptical ? a : b;
      const target = aOptical ? b : a;
      if (target.data.kind !== 'vbox') continue;
      // Varias VMs por unidad están permitidas; solo se evita el par duplicado.
      const pair = `${optical.id}:${target.id}`;
      if (linkedOpticalPairs.has(pair)) continue;
      linkedOpticalPairs.add(pair);
      outEdges.push(withoutPath(edge));
      continue;
    }

    const diskId = aDisk ? a.id : b.id;
    const target = aDisk ? b : a;
    if (target.data.kind !== 'docker' && target.data.kind !== 'vbox') continue;
    const kind = target.data.kind;
    const firstKind = linkedKind.get(diskId);
    if (firstKind && firstKind !== kind) continue; // mezcla: se descarta el último
    if (kind === 'vbox' && mountsByDisk.has(diskId)) continue; // una sola VM

    const pairKey = `${diskId}:${target.id}`;
    if (linkedContainers.has(pairKey)) continue;
    linkedContainers.add(pairKey);
    linkedKind.set(diskId, kind);

    const list = mountsByDisk.get(diskId) ?? [];
    if (kind === 'vbox') {
      vmDisks.add(diskId);
      mountsByDisk.set(diskId, [...list, { container: target.id }]);
      outEdges.push(withoutPath(edge));
      continue;
    }

    const existing = list.find((m) => m.container === target.id);
    const path = mountPathOf(edge) ?? existing?.path ?? DEFAULT_MOUNT_PATH;
    mountsByDisk.set(diskId, [...list, { container: target.id, path }]);
    outEdges.push({ ...edge, kind: 'mount', data: { ...(edge.data ?? {}), path } });
  }

  const outNodes = nodes.map((node) => {
    if (node.data.kind !== 'disk') {
      const stripDisk = node.data.disk !== undefined;
      const stripOptical = node.data.kind !== 'optical' && node.data.optical !== undefined;
      if (!stripDisk && !stripOptical) return node;
      return {
        ...node,
        data: {
          ...node.data,
          ...(stripDisk ? { disk: undefined } : {}),
          ...(stripOptical ? { optical: undefined } : {}),
        },
      };
    }
    const mounts = mountsByDisk.get(node.id) ?? [];
    const shared = vmDisks.has(node.id) ? false : node.data.disk?.shared ?? false;
    const sizeMb = node.data.disk?.sizeMb;
    if (sameMounts(node.data.disk?.mounts, mounts) && (node.data.disk?.shared ?? false) === shared) return node;
    return {
      ...node,
      data: {
        ...node.data,
        disk: { ...(sizeMb === undefined ? {} : { sizeMb }), shared, mounts },
      },
    };
  });

  return { nodes: outNodes, edges: outEdges };
}

/** Tipo de nodo para el payload IPC: discos y unidades ópticas no son máquinas. */
export function machineKindOf(kind: NodeKind): 'vbox' | 'docker' | null {
  return kind === 'disk' || kind === 'optical' ? null : kind;
}
