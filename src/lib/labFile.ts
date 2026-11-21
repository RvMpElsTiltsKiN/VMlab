import type { Message } from '../i18n';
import type {
  DiskMount,
  DiskSpec,
  DockerEnvVar,
  DockerPortMap,
  DockerRunOptions,
  Edge,
  EdgeKind,
  GuestIpMode,
  GuestSystem,
  LinkConditions,
  MachineData,
  MachineNode,
  NetMode,
  NodeNet,
  OpticalSpec,
} from '../types';
import { MAX_DISK_SIZE_MB, normalizeMountPath, syncDiskMounts, validMountPath } from './disk';
import { hasLinkConditions, LINK_LIMITS } from './link';
import { isoPathLooksValid } from './optical';

/** Nodo tal y como viaja dentro de un archivo `.vmlab`. */
export interface LabNode {
  id: string;
  position: { x: number; y: number };
  data: MachineData;
}

/** Arista tal y como viaja dentro de un archivo `.vmlab`. `kind` es opcional en archivos antiguos. */
export interface LabEdge {
  source: string;
  target: string;
  kind?: EdgeKind;
  /** Ruta del enlace de montaje (el dato canónico viaja en `disk.mounts`). */
  path?: string;
}

export type ParseLabResult =
  | { ok: true; nodes: MachineNode[]; edges: Edge[]; warnings: Message[] }
  | { ok: false; error: Message };

/** 5 MB. Se comprueba sobre `text.length` antes de llamar a `JSON.parse`. */
const MAX_TEXT = 5 * 1024 * 1024;
const MAX_NODES = 200;
const MAX_EDGES = 1000;
const COORD_LIMIT = 100_000;
const ID_RE = /^[A-Za-z0-9_-]{1,64}$/;

/** Límites de las opciones de Docker. Los mismos que `validate::docker_run_options`. */
export const RUN_LIMITS = {
  env: 32,
  ports: 32,
  keyLength: 128,
  valueLength: 1024,
} as const;

/** Nombre de variable de entorno aceptado por Docker (solo ASCII). */
export const ENV_KEY_RE = /^[A-Za-z_][A-Za-z0-9_]{0,127}$/;

/** `char::is_control` de Rust: categoría Unicode Cc. */
const CONTROL_RE = /[\p{Cc}]/u;

/** Longitud en caracteres (no en unidades UTF-16), como el backend. */
export function charLength(value: string): number {
  return [...value].length;
}

export function hasControlChars(value: string): boolean {
  return CONTROL_RE.test(value);
}

/** Puerto válido para Docker (el backend ya no admite 0 por el tipo `u16`). */
export function validPortNumber(value: unknown): value is number {
  return typeof value === 'number' && Number.isInteger(value) && value >= 1 && value <= 65_535;
}

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
  'run',
  'net',
  'link',
  'slot',
  'disk',
  'optical',
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

function kindName(value: unknown): string | null {
  return typeof value === 'string' && value ? value : null;
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

/** Lista blanca de las condiciones de red para exportar. */
function exportLink(link: LinkConditions | undefined): LinkConditions | undefined {
  if (!hasLinkConditions(link)) return undefined;
  if (!Number.isInteger(link.delayMs) || link.delayMs < 0 || link.delayMs > LINK_LIMITS.delayMs) return undefined;
  if (!Number.isInteger(link.jitterMs) || link.jitterMs < 0 || link.jitterMs > link.delayMs) return undefined;
  if (!Number.isInteger(link.lossTenths) || link.lossTenths < 0 || link.lossTenths > LINK_LIMITS.lossTenths) {
    return undefined;
  }

  const out: LinkConditions = {
    delayMs: link.delayMs,
    jitterMs: link.jitterMs,
    lossTenths: link.lossTenths,
  };
  if (link.rateKbit !== undefined) {
    if (
      !Number.isInteger(link.rateKbit) ||
      link.rateKbit < LINK_LIMITS.rateKbitMin ||
      link.rateKbit > LINK_LIMITS.rateKbitMax
    ) {
      return undefined;
    }
    out.rateKbit = link.rateKbit;
  }
  return out;
}

/** Lista blanca de `run` para exportar. Descarta filas inválidas y duplicadas. */
function exportRun(run: DockerRunOptions | undefined): DockerRunOptions | undefined {
  if (!run) return undefined;

  const env: DockerEnvVar[] = [];
  const keys = new Set<string>();
  const envIn = Array.isArray(run.env) ? run.env : [];
  for (const item of envIn) {
    if (env.length >= RUN_LIMITS.env) break;
    if (!item || typeof item.key !== 'string' || typeof item.value !== 'string') continue;
    if (!ENV_KEY_RE.test(item.key) || keys.has(item.key)) continue;
    if (charLength(item.value) > RUN_LIMITS.valueLength || hasControlChars(item.value)) continue;
    keys.add(item.key);
    env.push({ key: item.key, value: item.value });
  }

  const ports: DockerPortMap[] = [];
  const published = new Set<string>();
  const portsIn = Array.isArray(run.ports) ? run.ports : [];
  for (const port of portsIn) {
    if (ports.length >= RUN_LIMITS.ports) break;
    if (!port || !validPortNumber(port.host) || !validPortNumber(port.container)) continue;
    if (port.proto !== 'tcp' && port.proto !== 'udp') continue;
    const key = `${port.host}/${port.proto}`;
    if (published.has(key)) continue;
    published.add(key);
    ports.push({ host: port.host, container: port.container, proto: port.proto });
  }

  if (!env.length && !ports.length) return undefined;
  return { env, ports };
}

/** Lista blanca de `disk` para exportar. Los enlaces VM no llevan ruta. */
function exportDisk(disk: DiskSpec | undefined): DiskSpec | undefined {
  if (!disk) return undefined;
  const mounts: DiskMount[] = [];
  const seen = new Set<string>();
  for (const mount of disk.mounts ?? []) {
    if (!mount || typeof mount.container !== 'string' || !mount.container) continue;
    // Una ruta ausente es válida: es un enlace con máquina virtual.
    if (mount.path !== undefined && (typeof mount.path !== 'string' || !validMountPath(mount.path))) continue;
    const key = `${mount.container}|${mount.path === undefined ? '' : normalizeMountPath(mount.path)}`;
    if (seen.has(key)) continue;
    seen.add(key);
    mounts.push(mount.path === undefined ? { container: mount.container } : { container: mount.container, path: mount.path });
  }
  const sizeMb = disk.sizeMb;
  const validSize = typeof sizeMb === 'number' && Number.isInteger(sizeMb) && sizeMb >= 1 && sizeMb <= MAX_DISK_SIZE_MB;
  return {
    ...(validSize ? { sizeMb } : {}),
    shared: disk.shared === true,
    mounts,
  };
}

/** Lista blanca de `optical` para exportar. La ruta es local de este equipo. */
function exportOptical(spec: OpticalSpec | undefined): OpticalSpec | undefined {
  const iso = spec?.iso?.trim();
  if (!iso || !isoPathLooksValid(iso)) return undefined;
  return { iso };
}

/** Lista blanca de `MachineData` para exportar. La contraseña de invitado nunca se guarda. */
function exportData(data: MachineData): Record<string, unknown> {
  const out: Record<string, unknown> = { kind: data.kind, label: data.label, target: data.target };

  if (data.kind === 'disk') {
    const disk = exportDisk(data.disk);
    if (disk) out.disk = disk;
    return out;
  }

  if (data.kind === 'optical') {
    const optical = exportOptical(data.optical);
    if (optical) out.optical = optical;
    return out;
  }

  const link = exportLink(data.link);
  if (link) out.link = link;

  if (data.kind === 'docker') {
    if (typeof data.image === 'string' && data.image && data.image.length <= 200) out.image = data.image;
    const run = exportRun(data.run);
    if (run) out.run = run;
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
    const kind: EdgeKind = edge.kind === 'mount' ? 'mount' : 'network';
    outEdges.push({
      source: edge.source,
      target: edge.target,
      kind,
      ...(kind === 'mount' && edge.path ? { path: edge.path } : {}),
    });
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

function parseNet(value: unknown, label: string, warnings: Message[]): NodeNet | undefined {
  if (!isRecord(value)) {
    warnings.push({ key: 'labFile.warn.netInvalid', params: { label } });
    return undefined;
  }

  const mode = value.mode;
  if (typeof mode !== 'string' || !NET_MODES.includes(mode as NetMode)) {
    warnings.push({ key: 'labFile.warn.netModeInvalid', params: { label } });
    return undefined;
  }

  const adapter = value.adapter;
  if (typeof adapter !== 'number' || !Number.isInteger(adapter) || adapter < 1 || adapter > 4) {
    warnings.push({ key: 'labFile.warn.netAdapterInvalid', params: { label } });
    return undefined;
  }

  const net: NodeNet = { mode: mode as NetMode, adapter };
  for (const key of ['name', 'bridge'] as const) {
    const raw = value[key];
    if (raw === undefined) continue;
    if (typeof raw === 'string' && raw.length <= 200) {
      if (raw) net[key] = raw;
    } else {
      warnings.push({ key: 'labFile.warn.netFieldInvalid', params: { label, field: key } });
    }
  }

  const unknown = Object.keys(value).filter((key) => key !== 'mode' && key !== 'adapter' && key !== 'name' && key !== 'bridge');
  if (unknown.length) {
    warnings.push({ key: 'labFile.warn.netUnknown', params: { label, fields: droppedKeys(unknown) } });
  }
  return net;
}

/** Límites de las condiciones de red (los mismos que valida el backend). */
function parseLink(value: unknown, label: string, warnings: Message[]): LinkConditions | undefined {
  if (!isRecord(value)) {
    warnings.push({ key: 'labFile.warn.linkInvalid', params: { label } });
    return undefined;
  }

  const delayMs = value.delayMs;
  if (
    typeof delayMs !== 'number' ||
    !Number.isInteger(delayMs) ||
    delayMs < 0 ||
    delayMs > LINK_LIMITS.delayMs
  ) {
    warnings.push({ key: 'labFile.warn.linkDelay', params: { label } });
    return undefined;
  }

  const jitterMs = value.jitterMs;
  if (
    typeof jitterMs !== 'number' ||
    !Number.isInteger(jitterMs) ||
    jitterMs < 0 ||
    jitterMs > LINK_LIMITS.jitterMs ||
    jitterMs > delayMs
  ) {
    warnings.push({ key: 'labFile.warn.linkJitter', params: { label } });
    return undefined;
  }

  const lossTenths = value.lossTenths;
  if (
    typeof lossTenths !== 'number' ||
    !Number.isInteger(lossTenths) ||
    lossTenths < 0 ||
    lossTenths > LINK_LIMITS.lossTenths
  ) {
    warnings.push({ key: 'labFile.warn.linkLoss', params: { label } });
    return undefined;
  }

  const out: LinkConditions = { delayMs, jitterMs, lossTenths };
  const rateKbit = value.rateKbit;
  if (rateKbit !== undefined && rateKbit !== null) {
    if (
      typeof rateKbit !== 'number' ||
      !Number.isInteger(rateKbit) ||
      rateKbit < LINK_LIMITS.rateKbitMin ||
      rateKbit > LINK_LIMITS.rateKbitMax
    ) {
      warnings.push({ key: 'labFile.warn.linkRate', params: { label } });
    } else {
      out.rateKbit = rateKbit;
    }
  }

  const unknown = Object.keys(value).filter(
    (key) => key !== 'delayMs' && key !== 'jitterMs' && key !== 'lossTenths' && key !== 'rateKbit',
  );
  if (unknown.length) {
    warnings.push({ key: 'labFile.warn.linkUnknown', params: { label, fields: droppedKeys(unknown) } });
  }

  return hasLinkConditions(out) ? out : undefined;
}

/** Límites de `run` en un archivo importado (los mismos que el backend). */
function parseRun(value: unknown, label: string, warnings: Message[]): DockerRunOptions | undefined {
  if (!isRecord(value)) {
    warnings.push({ key: 'labFile.warn.runInvalid', params: { label } });
    return undefined;
  }

  const env: DockerEnvVar[] = [];
  const keys = new Set<string>();
  let envFull = false;
  if (value.env !== undefined) {
    if (!Array.isArray(value.env)) {
      warnings.push({ key: 'labFile.warn.envListInvalid', params: { label } });
    } else {
      for (const item of value.env) {
        if (!isRecord(item) || typeof item.key !== 'string' || typeof item.value !== 'string') {
          warnings.push({ key: 'labFile.warn.envInvalid', params: { label } });
          continue;
        }
        if (!ENV_KEY_RE.test(item.key)) {
          warnings.push({ key: 'labFile.warn.envKeyInvalid', params: { label, key: item.key } });
          continue;
        }
        if (charLength(item.value) > RUN_LIMITS.valueLength) {
          warnings.push({
            key: 'labFile.warn.envValueLong',
            params: { label, key: item.key, max: RUN_LIMITS.valueLength },
          });
          continue;
        }
        if (hasControlChars(item.value)) {
          warnings.push({ key: 'labFile.warn.envValueControl', params: { label, key: item.key } });
          continue;
        }
        if (keys.has(item.key)) {
          warnings.push({ key: 'labFile.warn.envDup', params: { label, key: item.key } });
          continue;
        }
        if (env.length >= RUN_LIMITS.env) {
          envFull = true;
          continue;
        }
        keys.add(item.key);
        env.push({ key: item.key, value: item.value });
      }
      if (envFull) {
        warnings.push({ key: 'labFile.warn.envExtra', params: { label, max: RUN_LIMITS.env } });
      }
    }
  }

  const ports: DockerPortMap[] = [];
  const published = new Set<string>();
  let portsFull = false;
  if (value.ports !== undefined) {
    if (!Array.isArray(value.ports)) {
      warnings.push({ key: 'labFile.warn.portListInvalid', params: { label } });
    } else {
      for (const item of value.ports) {
        if (!isRecord(item)) {
          warnings.push({ key: 'labFile.warn.portInvalid', params: { label } });
          continue;
        }
        if (!validPortNumber(item.host) || !validPortNumber(item.container)) {
          warnings.push({ key: 'labFile.warn.portRange', params: { label } });
          continue;
        }
        const proto = item.proto;
        if (proto !== 'tcp' && proto !== 'udp') {
          warnings.push({ key: 'labFile.warn.portProto', params: { label } });
          continue;
        }
        const key = `${item.host}/${proto}`;
        if (published.has(key)) {
          warnings.push({ key: 'labFile.warn.portDup', params: { label, port: item.host, proto } });
          continue;
        }
        if (ports.length >= RUN_LIMITS.ports) {
          portsFull = true;
          continue;
        }
        published.add(key);
        ports.push({ host: item.host, container: item.container, proto });
      }
      if (portsFull) {
        warnings.push({ key: 'labFile.warn.portExtra', params: { label, max: RUN_LIMITS.ports } });
      }
    }
  }

  const unknown = Object.keys(value).filter((key) => key !== 'env' && key !== 'ports');
  if (unknown.length) {
    warnings.push({ key: 'labFile.warn.runUnknown', params: { label, fields: droppedKeys(unknown) } });
  }

  if (!env.length && !ports.length) return undefined;
  return { env, ports };
}

function parseGuestFields(data: Record<string, unknown>, out: MachineData, label: string, warnings: Message[]): void {
  for (const key of GUEST_STRING_KEYS) {
    const value = data[key];
    if (value === undefined) continue;
    if (typeof value === 'string' && value.length <= 200) {
      if (value) out[key] = value;
    } else {
      warnings.push({ key: 'labFile.warn.guestFieldInvalid', params: { label, field: key } });
    }
  }

  if (data.guestPrefix !== undefined) {
    const value = data.guestPrefix;
    if (typeof value === 'number' && Number.isInteger(value) && value >= 0 && value <= 32) {
      out.guestPrefix = value;
    } else {
      warnings.push({ key: 'labFile.warn.guestPrefix', params: { label } });
    }
  }

  if (data.guestSystem !== undefined) {
    const value = data.guestSystem;
    if (typeof value === 'string' && GUEST_SYSTEMS.includes(value as GuestSystem)) {
      out.guestSystem = value as GuestSystem;
    } else {
      warnings.push({ key: 'labFile.warn.guestSystem', params: { label } });
    }
  }

  if (data.guestMode !== undefined) {
    const value = data.guestMode;
    if (typeof value === 'string' && GUEST_MODES.includes(value as GuestIpMode)) {
      out.guestMode = value as GuestIpMode;
    } else {
      warnings.push({ key: 'labFile.warn.guestMode', params: { label } });
    }
  }
}

/** Lista blanca de la configuración de disco importada. */
function parseDisk(value: unknown, label: string, warnings: Message[]): DiskSpec | undefined {
  if (!isRecord(value)) {
    warnings.push({ key: 'labFile.warn.diskInvalid', params: { label } });
    return undefined;
  }

  const shared = value.shared === true;
  if (value.shared !== undefined && typeof value.shared !== 'boolean') {
    warnings.push({ key: 'labFile.warn.diskShared', params: { label } });
  }

  let sizeMb: number | undefined;
  if (value.sizeMb !== undefined) {
    if (
      typeof value.sizeMb === 'number' &&
      Number.isInteger(value.sizeMb) &&
      value.sizeMb >= 1 &&
      value.sizeMb <= MAX_DISK_SIZE_MB
    ) {
      sizeMb = value.sizeMb;
    } else {
      warnings.push({ key: 'labFile.warn.diskSize', params: { label } });
    }
  }

  const mounts: DiskMount[] = [];
  const seen = new Set<string>();
  const rawMounts = value.mounts;
  if (rawMounts !== undefined && !Array.isArray(rawMounts)) {
    warnings.push({ key: 'labFile.warn.diskMountsInvalid', params: { label } });
  }
  for (const item of Array.isArray(rawMounts) ? rawMounts : []) {
    if (!isRecord(item) || typeof item.container !== 'string' || !item.container) {
      warnings.push({ key: 'labFile.warn.diskMountInvalid', params: { label } });
      continue;
    }
    // `path` ausente = enlace con VM. Presente debe ser una ruta válida.
    const noPath = item.path === undefined;
    if (!noPath && (typeof item.path !== 'string' || !validMountPath(item.path))) {
      warnings.push({ key: 'labFile.warn.diskMountInvalid', params: { label } });
      continue;
    }
    const path = noPath ? undefined : (item.path as string);
    const key = `${item.container}|${path === undefined ? '' : normalizeMountPath(path)}`;
    if (seen.has(key)) {
      warnings.push({ key: 'labFile.warn.diskMountDup', params: { label } });
      continue;
    }
    seen.add(key);
    mounts.push(path === undefined ? { container: item.container } : { container: item.container, path });
  }

  const unknown = Object.keys(value).filter((key) => key !== 'shared' && key !== 'mounts' && key !== 'sizeMb');
  if (unknown.length) {
    warnings.push({ key: 'labFile.warn.diskUnknown', params: { label, fields: droppedKeys(unknown) } });
  }
  return { ...(sizeMb === undefined ? {} : { sizeMb }), shared, mounts };
}

/** Lee el campo opcional `optical` de un laboratorio antiguo o reciente. */
function parseOptical(value: unknown, label: string, warnings: Message[]): OpticalSpec | undefined {
  if (value === undefined || value === null) return undefined;
  if (!isRecord(value)) {
    warnings.push({ key: 'labFile.warn.opticalInvalid', params: { label } });
    return undefined;
  }
  const iso = value.iso;
  if (iso === undefined || iso === null || iso === '') return undefined;
  if (typeof iso !== 'string' || charLength(iso) > 1024 || !isoPathLooksValid(iso)) {
    warnings.push({ key: 'labFile.warn.opticalIsoInvalid', params: { label } });
    return undefined;
  }
  const unknown = Object.keys(value).filter((key) => key !== 'iso');
  if (unknown.length) {
    warnings.push({ key: 'labFile.warn.opticalUnknown', params: { label, fields: droppedKeys(unknown) } });
  }
  return { iso };
}

function parseNode(raw: unknown, index: number, seen: Set<string>, warnings: Message[]): MachineNode | null {
  if (!isRecord(raw)) {
    warnings.push({ key: 'labFile.warn.nodeNotObject', params: { index: index + 1 } });
    return null;
  }

  const id = raw.id;
  if (typeof id !== 'string' || !ID_RE.test(id)) {
    warnings.push({ key: 'labFile.warn.nodeIdInvalid', params: { index: index + 1 } });
    return null;
  }
  if (seen.has(id)) {
    warnings.push({ key: 'labFile.warn.nodeIdDup', params: { id } });
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
    warnings.push({ key: 'labFile.warn.nodePosition', params: { id } });
    return null;
  }

  const data = raw.data;
  if (!isRecord(data)) {
    warnings.push({ key: 'labFile.warn.nodeData', params: { id } });
    return null;
  }

  const kind = data.kind;
  if (kind !== 'vbox' && kind !== 'docker' && kind !== 'disk' && kind !== 'optical') {
    const kindLabel = kindName(kind);
    warnings.push(
      kindLabel
        ? { key: 'labFile.warn.nodeKind', params: { id, kind: kindLabel } }
        : { key: 'labFile.warn.nodeKindUnknown', params: { id } },
    );
    return null;
  }

  const label = data.label;
  if (typeof label !== 'string' || label.length > 100) {
    warnings.push({ key: 'labFile.warn.nodeName', params: { id } });
    return null;
  }

  const target = data.target;
  if (typeof target !== 'string' || target.length > 200) {
    warnings.push({ key: 'labFile.warn.nodeTarget', params: { id } });
    return null;
  }

  const display = nameOf(label, id);
  const out: MachineData = { kind: kind as MachineData['kind'], label, target };

  const unknown = Object.keys(data).filter((key) => !ALL_DATA_KEYS.has(key));
  if (unknown.length) {
    warnings.push({ key: 'labFile.warn.nodeUnknown', params: { label: display, fields: droppedKeys(unknown) } });
  }

  if (kind !== 'disk' && kind !== 'optical' && data.link !== undefined) {
    const link = parseLink(data.link, display, warnings);
    if (link) out.link = link;
  }

  if (kind === 'disk') {
    if (data.disk !== undefined) {
      const disk = parseDisk(data.disk, display, warnings);
      if (disk) out.disk = disk;
    }
    const extra = ['image', 'run', 'net', 'slot', 'link', ...GUEST_KEYS].filter(
      (key) => data[key] !== undefined,
    );
    if (extra.length) {
      warnings.push({ key: 'labFile.warn.diskFields', params: { label: display, fields: droppedKeys(extra) } });
    }
    return { id, type: 'machine', position: { x: position.x, y: position.y }, data: out };
  }

  if (kind === 'optical') {
    if (data.optical !== undefined) {
      const optical = parseOptical(data.optical, display, warnings);
      if (optical) out.optical = optical;
    }
    const extra = ['image', 'run', 'net', 'slot', 'link', 'disk', ...GUEST_KEYS].filter(
      (key) => data[key] !== undefined,
    );
    if (extra.length) {
      warnings.push({ key: 'labFile.warn.opticalFields', params: { label: display, fields: droppedKeys(extra) } });
    }
    return { id, type: 'machine', position: { x: position.x, y: position.y }, data: out };
  }

  if (kind === 'docker') {
    if (data.image !== undefined) {
      if (typeof data.image === 'string' && data.image.length <= 200) {
        if (data.image) out.image = data.image;
      } else {
        warnings.push({ key: 'labFile.warn.imageInvalid', params: { label: display } });
      }
    }
    if (data.run !== undefined) {
      const run = parseRun(data.run, display, warnings);
      if (run) out.run = run;
    }
    if (data.net !== undefined) {
      warnings.push({ key: 'labFile.warn.dockerNet', params: { label: display } });
    }
    if (data.slot === true) {
      warnings.push({ key: 'labFile.warn.dockerSlot', params: { label: display } });
    } else if (data.slot !== undefined && data.slot !== false) {
      warnings.push({ key: 'labFile.warn.slotInvalid', params: { label: display } });
    }
    const guest = [...GUEST_KEYS].filter((key) => data[key] !== undefined);
    if (guest.length) {
      warnings.push({ key: 'labFile.warn.dockerGuest', params: { label: display } });
    }
  } else {
    if (data.image !== undefined) {
      warnings.push({ key: 'labFile.warn.vboxImage', params: { label: display } });
    }
    if (data.run !== undefined) {
      warnings.push({ key: 'labFile.warn.vboxRun', params: { label: display } });
    }
    if (data.slot === true && !target) {
      out.slot = true;
    } else if (data.slot === true) {
      warnings.push({ key: 'labFile.warn.slotTarget', params: { label: display } });
    } else if (data.slot !== undefined && data.slot !== false) {
      warnings.push({ key: 'labFile.warn.slotInvalid', params: { label: display } });
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
  if (typeof text !== 'string') return { ok: false, error: { key: 'labFile.error.noText' } };
  if (text.length > MAX_TEXT) return { ok: false, error: { key: 'labFile.error.tooLarge' } };

  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    return { ok: false, error: { key: 'labFile.error.json' } };
  }

  if (!isRecord(raw)) return { ok: false, error: { key: 'labFile.error.notLab' } };
  if (raw.format !== 'vmlab-lab') return { ok: false, error: { key: 'labFile.error.notVmlab' } };

  const version = raw.version;
  if (typeof version === 'number' && version > 1) {
    return { ok: false, error: { key: 'labFile.error.newerVersion' } };
  }
  if (version !== 1) {
    return { ok: false, error: { key: 'labFile.error.version' } };
  }

  if (!Array.isArray(raw.nodes)) return { ok: false, error: { key: 'labFile.error.nodes' } };
  if (!Array.isArray(raw.edges)) return { ok: false, error: { key: 'labFile.error.edges' } };
  if (raw.nodes.length > MAX_NODES) {
    return {
      ok: false,
      error: { key: 'labFile.error.tooManyNodes', params: { count: raw.nodes.length, max: MAX_NODES } },
    };
  }
  if (raw.edges.length > MAX_EDGES) {
    return {
      ok: false,
      error: { key: 'labFile.error.tooManyEdges', params: { count: raw.edges.length, max: MAX_EDGES } },
    };
  }

  const warnings: Message[] = [];
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
  const nodeKinds = new Map(nodes.map((n) => [n.id, n.data.kind]));

  raw.edges.forEach((item, index) => {
    if (!isRecord(item) || typeof item.source !== 'string' || typeof item.target !== 'string') {
      warnings.push({ key: 'labFile.warn.edgeInvalid', params: { index: index + 1 } });
      return;
    }
    const { source, target } = item;
    if (!ids.has(source) || !ids.has(target)) {
      warnings.push({ key: 'labFile.warn.edgeMissing', params: { source, target } });
      return;
    }
    if (source === target) {
      warnings.push({ key: 'labFile.warn.edgeSelf', params: { source } });
      return;
    }
    const kind: EdgeKind = item.kind === 'mount' ? 'mount' : 'network';
    const sourceKind = nodeKinds.get(source);
    const targetKind = nodeKinds.get(target);
    const peripheral = (k: string | undefined) => k === 'disk' || k === 'optical';
    if (kind === 'mount' && peripheral(sourceKind) === peripheral(targetKind)) {
      warnings.push({ key: 'labFile.warn.edgeMountInvalid', params: { source, target } });
      return;
    }
    if (sourceKind === 'optical' || targetKind === 'optical') {
      const opticalId = sourceKind === 'optical' ? source : target;
      const otherKind = sourceKind === 'optical' ? targetKind : sourceKind;
      if (kind !== 'mount' || otherKind !== 'vbox') {
        warnings.push({ key: 'labFile.warn.edgeOpticalInvalid', params: { source: opticalId, target } });
        return;
      }
    }
    const key = `${kind}|${source < target ? `${source}|${target}` : `${target}|${source}`}`;
    if (seen.has(key)) {
      warnings.push({ key: 'labFile.warn.edgeDup', params: { source, target } });
      return;
    }
    seen.add(key);
    edges.push({
      id: `e_${source}_${target}${kind === 'mount' ? '_mount' : ''}`,
      source,
      target,
      kind,
      ...(kind === 'mount' && typeof item.path === 'string' ? { data: { path: item.path } } : {}),
    });
  });

  const synced = syncDiskMounts(nodes, edges);
  return { ok: true, nodes: synced.nodes, edges: synced.edges, warnings };
}
