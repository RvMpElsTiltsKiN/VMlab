import type { Edge, MachineNode } from '../types';
import { edgeKind } from './disk';

/** Prefijo de los nombres visibles de las unidades ópticas nuevas. */
export const OPTICAL_PREFIX = 'cd';

/** Un disco o una unidad óptica no son máquinas: no tienen red, sondas ni acciones. */
export const isOptical = (node: MachineNode): boolean => node.data.kind === 'optical';

export function isPeripheral(node: MachineNode): boolean {
  return node.data.kind === 'disk' || isOptical(node);
}

export const isMachineNode = (node: MachineNode): boolean => !isPeripheral(node);

/** Siguiente nombre libre: cd, cd2, cd3… */
export function nextOpticalName(nodes: MachineNode[]): string {
  const used = new Set(nodes.filter(isOptical).map((n) => n.data.target));
  let n = 1;
  while (used.has(n === 1 ? OPTICAL_PREFIX : `${OPTICAL_PREFIX}${n}`)) n += 1;
  return n === 1 ? OPTICAL_PREFIX : `${OPTICAL_PREFIX}${n}`;
}

/** Nombre del fichero de una ISO, para mostrar en el nodo y en el inspector. */
export function isoFileName(iso?: string): string {
  if (!iso?.trim()) return '';
  return iso.split(/[\\/]/).pop() || iso;
}

/**
 * Validación aproximada en el frontend: no tenemos permisos de filesystem,
 * así que no podemos comprobar que el fichero exista. El backend valida la
 * existencia al aplicar la red.
 */
export function isoPathLooksValid(iso?: string): boolean {
  if (!iso?.trim()) return true; // unidad vacía
  const value = iso.trim();
  if (value.startsWith('-') || /[\p{Cc}]/u.test(value)) return false;
  const absolute = /^[A-Za-z]:[\\/]/.test(value) || value.startsWith('\\\\') || value.startsWith('/');
  return absolute && /\.iso$/i.test(value);
}

/** VMs enlazadas con una unidad por enlaces `Mount` válidos (una por VM). */
export function linkedVmsOf(node: MachineNode, nodes: MachineNode[], edges: Edge[]): MachineNode[] {
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const vms = new Map<string, MachineNode>();
  for (const edge of edges) {
    if (edgeKind(edge) !== 'mount') continue;
    if (edge.source !== node.id && edge.target !== node.id) continue;
    const otherId = edge.source === node.id ? edge.target : edge.source;
    const other = byId.get(otherId);
    if (other?.data.kind === 'vbox') vms.set(other.id, other);
  }
  return [...vms.values()];
}
