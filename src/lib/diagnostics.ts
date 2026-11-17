/** Sondeos del cable y diagnóstico puro compartido por el tooltip y el panel. */
import { probeKey } from './connectivity';
import { ipc } from './ipc';
import { resolveState } from './status';
import type { LinkDiagnostics, MachineKind, MachineNode, NodeProbe, Snapshot } from '../types';

/** Sondeo sintético de un nodo que no está en marcha. */
function notRunningProbe(node: MachineNode): NodeProbe {
  return {
    // Los discos nunca llegan aquí (no se sondean), pero el tipo del lienzo los incluye.
    kind: node.data.kind as MachineKind,
    target: node.data.target,
    segment: null,
    ipv4: null,
    netmask: null,
    issues: [{ code: 'notRunning' }],
    note: null,
  };
}

/**
 * Último sondeo del nodo o, si está apagado, uno sintético con `notRunning`.
 * Devuelve `null` mientras un nodo en marcha todavía no tiene sondeo.
 */
export function endpointProbe(
  node: MachineNode,
  probes: Record<string, NodeProbe>,
  snapshot: Snapshot,
): NodeProbe | null {
  if (resolveState(node.data, snapshot) !== 'running') return notRunningProbe(node);
  return probes[probeKey(node.data.kind, node.data.target)] ?? null;
}

/** Sondeos de los dos extremos del cable, o `null` si aún falta alguno. */
export function linkProbes(
  a: MachineNode,
  b: MachineNode,
  probes: Record<string, NodeProbe>,
  snapshot: Snapshot,
): { a: NodeProbe; b: NodeProbe } | null {
  const pa = endpointProbe(a, probes, snapshot);
  const pb = endpointProbe(b, probes, snapshot);
  return pa && pb ? { a: pa, b: pb } : null;
}

/** Últimos diagnósticos ya calculados: el hover no repite IPC con los mismos sondeos. */
const cache = new Map<string, LinkDiagnostics>();
const CACHE_MAX = 32;

function probeSignature(probe: NodeProbe): string {
  return JSON.stringify(probe);
}

function cacheKey(a: NodeProbe, b: NodeProbe): string {
  return `${probeKey(a.kind, a.target)}=${probeSignature(a)}|${probeKey(b.kind, b.target)}=${probeSignature(b)}`;
}

/** Llama a `diagnose_link` (cálculo puro en Rust) y reutiliza el último resultado. */
export async function diagnoseProbes(a: NodeProbe, b: NodeProbe): Promise<LinkDiagnostics> {
  const key = cacheKey(a, b);
  const hit = cache.get(key);
  if (hit) {
    // Refresca la entrada para que el límite descarte la menos usada.
    cache.delete(key);
    cache.set(key, hit);
    return hit;
  }
  const result = await ipc.diagnoseLink(a, b);
  cache.set(key, result);
  if (cache.size > CACHE_MAX) {
    const oldest = cache.keys().next().value;
    if (oldest !== undefined) cache.delete(oldest);
  }
  return result;
}
