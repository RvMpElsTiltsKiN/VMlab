import type { Edge } from '@xyflow/react';
import type {
  GuestIpMode,
  GuestSystem,
  MachineData,
  MachineKind,
  MachineNode,
  NetMode,
  NodeNet,
} from '../types';

/** Nodo tal y como viaja dentro de un archivo `.vmlab`. */
export interface LabNode {
  id: string;
  position: { x: number; y: number };
  data: MachineData;
}

/** Arista tal y como viaja dentro de un archivo `.vmlab`. */
export interface LabEdge {
  source: string;
  target: string;
}

export type ParseLabResult =
  | { ok: true; nodes: MachineNode[]; edges: Edge[]; warnings: string[] }
  | { ok: false; error: string };

/** 5 MB. Se comprueba sobre `text.length` antes de llamar a `JSON.parse`. */
const MAX_TEXT = 5 * 1024 * 1024;
const MAX_NODES = 200;
const MAX_EDGES = 1000;
const COORD_LIMIT = 100_000;
const ID_RE = /^[A-Za-z0-9_-]{1,64}$/;

const NET_MODES: readonly NetMode[] = ['nat', 'bridged', 'intnet', 'hostonly', 'natnetwork', 'none'];
const GUEST_SYSTEMS: readonly GuestSystem[] = [
  'auto',
  'netplan',
  'networkManager',
  'systemdNetworkd',
  'ifupdown',
  'wicked',
  'dhcpcd',
  'connman',
  'networkScripts',
  'dhcpd',
];
const GUEST_MODES: readonly GuestIpMode[] = ['dhcp', 'static', 'server'];

const GUEST_STRING_KEYS = [
  'guestUser',
  'guestIface',
  'guestAddress',
  'guestGateway',
  'guestDns',
  'guestSubnet',
  'guestRangeStart',
  'guestRangeEnd',
] as const;

const ALL_DATA_KEYS = new Set<string>([
  'kind',
  'label',
  'target',
  'image',
  'net',
  'slot',
  ...GUEST_STRING_KEYS,
  'guestSystem',
  'guestMode',
  'guestPrefix',
]);

const GUEST_KEYS = new Set<string>([...GUEST_STRING_KEYS, 'guestSystem', 'guestMode', 'guestPrefix']);

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

const nameOf = (label: string, id: string): string => (label.trim() ? label : id);

function kindName(value: unknown): string {
  return typeof value === 'string' && value ? value : 'desconocido';
}

function droppedKeys(keys: string[]): string {
  const shown = keys.slice(0, 5).join(', ');
  return keys.length > 5 ? `${shown}, …` : shown;
}

/** Lista blanca de red para exportar. Nunca escribe claves desconocidas. */
function exportNet(net: NodeNet | undefined): NodeNet | undefined {
  if (!net || !NET_MODES.includes(net.mode)) return undefined;
  if (!Number.isInteger(net.adapter) || net.adapter < 1 || net.adapter > 4) return undefined;
  const out: NodeNet = { mode: net.mode, adapter: net.adapter };
  if (typeof net.name === 'string' && net.name && net.name.length <= 200) out.name = net.name;
  if (typeof net.bridge === 'string' && net.bridge && net.bridge.length <= 200) out.bridge = net.bridge;
  return out;
}

/** Lista blanca de `MachineData` para exportar. La contraseña de invitado nunca se guarda. */
function exportData(data: MachineData): Record<string, unknown> {
  const out: Record<string, unknown> = { kind: data.kind, label: data.label, target: data.target };

  if (data.kind === 'docker') {
    if (typeof data.image === 'string' && data.image && data.image.length <= 200) out.image = data.image;
    return out;
  }

  if (data.slot === true) out.slot = true;

  const net = exportNet(data.net);
  if (net) out.net = net;

  for (const key of GUEST_STRING_KEYS) {
    const value = data[key];
    if (typeof value === 'string' && value && value.length <= 200) out[key] = value;
  }
  if (typeof data.guestPrefix === 'number' && Number.isInteger(data.guestPrefix) && data.guestPrefix >= 0 && data.guestPrefix <= 32) {
    out.guestPrefix = data.guestPrefix;
  }
  if (typeof data.guestSystem === 'string' && GUEST_SYSTEMS.includes(data.guestSystem as GuestSystem)) {
    out.guestSystem = data.guestSystem;
  }
  if (typeof data.guestMode === 'string' && GUEST_MODES.includes(data.guestMode as GuestIpMode)) {
    out.guestMode = data.guestMode;
  }
  return out;
}

/**
 * Serializa el lienzo con el formato público de VMLab.
 *
 * Solo incluye los campos conocidos de `MachineData`; cualquier clave extra
 * (incluidas contraseñas de versiones antiguas) se queda fuera.
 */
export function serializeLab(nodes: readonly LabNode[], edges: readonly LabEdge[]): string {
  const ids = new Set(nodes.map((n) => n.id));
  const seen = new Set<string>();
  const outEdges: LabEdge[] = [];

  for (const edge of edges) {
    if (!ids.has(edge.source) || !ids.has(edge.target) || edge.source === edge.target) continue;
    const key = edge.source < edge.target ? `${edge.source}|${edge.target}` : `${edge.target}|${edge.source}`;
    if (seen.has(key)) continue;
    seen.add(key);
    outEdges.push({ source: edge.source, target: edge.target });
  }

  return JSON.stringify(
    {
      format: 'vmlab-lab',
      version: 1,
      exportedAt: new Date().toISOString(),
      nodes: nodes.map((n) => ({
        id: n.id,
        position: { x: n.position.x, y: n.position.y },
        data: exportData(n.data),
      })),
      edges: outEdges,
    },
    null,
    2,
  );
}

function parseNet(value: unknown, label: string, warnings: string[]): NodeNet | undefined {
  if (!isRecord(value)) {
    warnings.push(`Nodo «${label}»: la red no es válida; se descarta.`);
    return undefined;
  }

  const mode = value.mode;
  if (typeof mode !== 'string' || !NET_MODES.includes(mode as NetMode)) {
    warnings.push(`Nodo «${label}»: el modo de red no es válido; se descarta la red.`);
    return undefined;
  }

  const adapter = value.adapter;
  if (typeof adapter !== 'number' || !Number.isInteger(adapter) || adapter < 1 || adapter > 4) {
    warnings.push(`Nodo «${label}»: el adaptador de red debe estar entre 1 y 4; se descarta la red.`);
    return undefined;
  }

  const net: NodeNet = { mode: mode as NetMode, adapter };
  for (const key of ['name', 'bridge'] as const) {
    const raw = value[key];
    if (raw === undefined) continue;
    if (typeof raw === 'string' && raw.length <= 200) {
      if (raw) net[key] = raw;
    } else {
      warnings.push(`Nodo «${label}»: «${key}» de la red no es válido; se descarta.`);
    }
  }

  const unknown = Object.keys(value).filter((key) => key !== 'mode' && key !== 'adapter' && key !== 'name' && key !== 'bridge');
  if (unknown.length) {
    warnings.push(`Nodo «${label}»: se descartaron campos de red desconocidos (${droppedKeys(unknown)}).`);
  }
  return net;
}

function parseGuestFields(data: Record<string, unknown>, out: MachineData, label: string, warnings: string[]): void {
  for (const key of GUEST_STRING_KEYS) {
    const value = data[key];
    if (value === undefined) continue;
    if (typeof value === 'string' && value.length <= 200) {
      if (value) out[key] = value;
    } else {
      warnings.push(`Nodo «${label}»: «${key}» no es válido; se descarta.`);
    }
  }

  if (data.guestPrefix !== undefined) {
    const value = data.guestPrefix;
    if (typeof value === 'number' && Number.isInteger(value) && value >= 0 && value <= 32) {
      out.guestPrefix = value;
    } else {
      warnings.push(`Nodo «${label}»: el prefijo de invitado debe ser un entero entre 0 y 32; se descarta.`);
    }
  }

  if (data.guestSystem !== undefined) {
    const value = data.guestSystem;
    if (typeof value === 'string' && GUEST_SYSTEMS.includes(value as GuestSystem)) {
      out.guestSystem = value as GuestSystem;
    } else {
      warnings.push(`Nodo «${label}»: el sistema de red del invitado no es válido; se descarta.`);
    }
  }

  if (data.guestMode !== undefined) {
    const value = data.guestMode;
    if (typeof value === 'string' && GUEST_MODES.includes(value as GuestIpMode)) {
      out.guestMode = value as GuestIpMode;
    } else {
      warnings.push(`Nodo «${label}»: el modo de IP del invitado no es válido; se descarta.`);
    }
  }
}

function parseNode(raw: unknown, index: number, seen: Set<string>, warnings: string[]): MachineNode | null {
  if (!isRecord(raw)) {
    warnings.push(`Nodo ${index + 1}: no es un objeto; se descarta.`);
    return null;
  }

  const id = raw.id;
  if (typeof id !== 'string' || !ID_RE.test(id)) {
    warnings.push(`Nodo ${index + 1}: el identificador no es válido; se descarta.`);
    return null;
  }
  if (seen.has(id)) {
    warnings.push(`Nodo «${id}»: identificador duplicado; se descarta.`);
    return null;
  }

  const position = raw.position;
  if (
    !isRecord(position) ||
    typeof position.x !== 'number' ||
    typeof position.y !== 'number' ||
    !Number.isFinite(position.x) ||
    !Number.isFinite(position.y) ||
    Math.abs(position.x) > COORD_LIMIT ||
    Math.abs(position.y) > COORD_LIMIT
  ) {
    warnings.push(`Nodo «${id}»: la posición no es válida; se descarta.`);
    return null;
  }

  const data = raw.data;
  if (!isRecord(data)) {
    warnings.push(`Nodo «${id}»: no tiene datos válidos; se descarta.`);
    return null;
  }

  const kind = data.kind;
  if (kind !== 'vbox' && kind !== 'docker') {
    warnings.push(`Nodo «${id}»: el tipo «${kindName(kind)}» no es válido; se descarta.`);
    return null;
  }

  const label = data.label;
  if (typeof label !== 'string' || label.length > 100) {
    warnings.push(`Nodo «${id}»: el nombre no es válido; se descarta.`);
    return null;
  }

  const target = data.target;
  if (typeof target !== 'string' || target.length > 200) {
    warnings.push(`Nodo «${id}»: el destino no es válido; se descarta.`);
    return null;
  }

  const display = nameOf(label, id);
  const out: MachineData = { kind: kind as MachineKind, label, target };

  const unknown = Object.keys(data).filter((key) => !ALL_DATA_KEYS.has(key));
  if (unknown.length) {
    warnings.push(`Nodo «${display}»: se descartaron campos desconocidos (${droppedKeys(unknown)}).`);
  }

  if (kind === 'docker') {
    if (data.image !== undefined) {
      if (typeof data.image === 'string' && data.image.length <= 200) {
        if (data.image) out.image = data.image;
      } else {
        warnings.push(`Nodo «${display}»: la imagen no es válida; se descarta.`);
      }
    }
    if (data.net !== undefined) {
      warnings.push(`Nodo «${display}»: la red de VirtualBox no se aplica a un contenedor; se descarta.`);
    }
    if (data.slot === true) {
      warnings.push(`Nodo «${display}»: un hueco de VM no se aplica a un contenedor; se descarta.`);
    } else if (data.slot !== undefined && data.slot !== false) {
      warnings.push(`Nodo «${display}»: «slot» no es válido; se descarta.`);
    }
    const guest = [...GUEST_KEYS].filter((key) => data[key] !== undefined);
    if (guest.length) {
      warnings.push(`Nodo «${display}»: se descartaron campos de invitado no aplicables a un contenedor.`);
    }
  } else {
    if (data.image !== undefined) {
      warnings.push(`Nodo «${display}»: la imagen no se usa en una máquina VirtualBox; se descarta.`);
    }
    if (data.slot === true && !target) {
      out.slot = true;
    } else if (data.slot === true) {
      warnings.push(`Nodo «${display}»: un hueco de VM no puede tener destino; se descarta la marca de hueco.`);
    } else if (data.slot !== undefined && data.slot !== false) {
      warnings.push(`Nodo «${display}»: «slot» no es válido; se descarta.`);
    }
    if (data.net !== undefined) {
      const net = parseNet(data.net, display, warnings);
      if (net) out.net = net;
    }
    parseGuestFields(data, out, display, warnings);
  }

  return { id, type: 'machine', position: { x: position.x, y: position.y }, data: out };
}

/**
 * Lee y valida un archivo `.vmlab`.
 *
 * Los nodos, redes y campos inválidos se descartan con un aviso; solo los
 * problemas estructurales (JSON roto, formato o versión, límites) son fatales.
 */
export function parseLab(text: string): ParseLabResult {
  if (typeof text !== 'string') return { ok: false, error: 'El archivo no contiene texto válido.' };
  if (text.length > MAX_TEXT) return { ok: false, error: 'El archivo supera el tamaño máximo de 5 MB.' };

  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    return { ok: false, error: 'El archivo no es un JSON válido.' };
  }

  if (!isRecord(raw)) return { ok: false, error: 'El archivo no contiene un laboratorio válido.' };
  if (raw.format !== 'vmlab-lab') return { ok: false, error: 'El archivo no es un laboratorio de VMLab.' };

  const version = raw.version;
  if (typeof version === 'number' && version > 1) {
    return { ok: false, error: 'El laboratorio fue creado con una versión más nueva de VMLab.' };
  }
  if (version !== 1) {
    return { ok: false, error: 'La versión del archivo no es compatible con esta versión de VMLab.' };
  }

  if (!Array.isArray(raw.nodes)) return { ok: false, error: 'El archivo no contiene una lista de nodos válida.' };
  if (!Array.isArray(raw.edges)) return { ok: false, error: 'El archivo no contiene una lista de conexiones válida.' };
  if (raw.nodes.length > MAX_NODES) {
    return { ok: false, error: `El laboratorio tiene ${raw.nodes.length} nodos; el máximo es ${MAX_NODES}.` };
  }
  if (raw.edges.length > MAX_EDGES) {
    return { ok: false, error: `El laboratorio tiene ${raw.edges.length} conexiones; el máximo es ${MAX_EDGES}.` };
  }

  const warnings: string[] = [];
  const nodes: MachineNode[] = [];
  const ids = new Set<string>();

  raw.nodes.forEach((item, index) => {
    const node = parseNode(item, index, ids, warnings);
    if (!node) return;
    ids.add(node.id);
    nodes.push(node);
  });

  const edges: Edge[] = [];
  const seen = new Set<string>();

  raw.edges.forEach((item, index) => {
    if (!isRecord(item) || typeof item.source !== 'string' || typeof item.target !== 'string') {
      warnings.push(`Conexión ${index + 1}: no es válida; se descarta.`);
      return;
    }
    const { source, target } = item;
    if (!ids.has(source) || !ids.has(target)) {
      warnings.push(`Conexión ${source} → ${target}: hace referencia a un nodo que no existe; se descarta.`);
      return;
    }
    if (source === target) {
      warnings.push(`Conexión «${source}»: une un nodo consigo mismo; se descarta.`);
      return;
    }
    const key = source < target ? `${source}|${target}` : `${target}|${source}`;
    if (seen.has(key)) {
      warnings.push(`Conexión duplicada entre «${source}» y «${target}»; se descarta.`);
      return;
    }
    seen.add(key);
    edges.push({ id: `e_${source}_${target}`, source, target });
  });

  return { ok: true, nodes, edges, warnings };
}
