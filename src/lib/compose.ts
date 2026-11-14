import type { Message, TKey, TParams } from '../i18n';
import type { DockerEnvVar, DockerPortMap, DockerPortProto, MachineData } from '../types';
import {
  charLength,
  ENV_KEY_RE,
  hasControlChars,
  parseLab,
  RUN_LIMITS,
  validPortNumber,
  type LabEdge,
  type LabNode,
} from './labFile';

/** Laboratorio en el formato público de VMLab, listo para `importLab`. */
export interface ComposeLab {
  format: 'vmlab-lab';
  version: 1;
  nodes: LabNode[];
  edges: LabEdge[];
}

export type ComposeResult = { lab: ComposeLab; warnings: Message[] } | { error: Message };

/** Aviso con la clave y los parámetros; el servicio se añade en `composeToLab`. */
type Warn = (key: TKey, params?: TParams) => void;

const MAX_SERVICES = 200;
const GRID = 220;
const COLUMNS = 4;
const ID_MAX = 64;
const IMAGE_MAX = 200;
const TARGET_RE = /^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$/;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/** `true` si una opción de Compose tiene contenido que VMLab no puede importar. */
function hasEntries(value: unknown): boolean {
  if (value === undefined || value === null) return false;
  if (typeof value === 'boolean') return value;
  if (Array.isArray(value)) return value.length > 0;
  if (typeof value === 'object') return Object.keys(value).length > 0;
  if (typeof value === 'string') return value.trim() !== '';
  return true;
}

/** Sanea un nombre de contenedor al formato que acepta Docker y VMLab. */
function sanitizeTarget(raw: string): string {
  const cleaned = raw
    .replace(/[^A-Za-z0-9_.-]/g, '-')
    .replace(/^[^A-Za-z0-9]+/, '')
    .slice(0, ID_MAX);
  return cleaned || 'vmlab';
}

/** Identificador de nodo único con el formato de `parseLab`. */
function uniqueId(name: string, used: Set<string>): string {
  const base = name.replace(/[^A-Za-z0-9_-]/g, '_').slice(0, ID_MAX) || 'servicio';
  let id = base;
  let suffix = 2;
  while (used.has(id)) {
    const tail = `_${suffix++}`;
    id = `${base.slice(0, ID_MAX - tail.length)}${tail}`;
  }
  used.add(id);
  return id;
}

function dependsOnOf(raw: unknown): string[] {
  if (Array.isArray(raw)) return raw.filter((item): item is string => typeof item === 'string' && item !== '');
  if (isRecord(raw)) return Object.keys(raw);
  return [];
}

function networksOf(raw: unknown, warn: Warn): string[] {
  if (raw === undefined || raw === null) return ['default'];
  if (Array.isArray(raw)) {
    const names = raw.map((item) => (typeof item === 'string' ? item.trim() : '')).filter(Boolean);
    return names.length ? [...new Set(names)] : ['default'];
  }
  if (isRecord(raw)) {
    const names = Object.keys(raw).filter(Boolean);
    return names.length ? names : ['default'];
  }
  warn('compose.warn.networksInvalid');
  return ['default'];
}

function environmentOf(raw: unknown, warn: Warn): DockerEnvVar[] {
  if (raw === undefined || raw === null) return [];

  const pairs: { key: string; value: unknown }[] = [];
  if (Array.isArray(raw)) {
    for (const item of raw) {
      if (typeof item !== 'string') {
        warn('compose.warn.envItemInvalid');
        continue;
      }
      const eq = item.indexOf('=');
      if (eq < 0) {
        warn('compose.warn.envNoValue', { key: item });
        continue;
      }
      pairs.push({ key: item.slice(0, eq), value: item.slice(eq + 1) });
    }
  } else if (isRecord(raw)) {
    for (const [key, value] of Object.entries(raw)) pairs.push({ key, value });
  } else {
    warn('compose.warn.envListInvalid');
    return [];
  }

  const env: DockerEnvVar[] = [];
  const keys = new Set<string>();
  for (const { key, value } of pairs) {
    if (value === null || value === undefined) {
      warn('compose.warn.envNoValue', { key });
      continue;
    }
    const text = typeof value === 'string' ? value : typeof value === 'number' || typeof value === 'boolean' ? String(value) : null;
    if (text === null) {
      warn('compose.warn.envNotText', { key });
      continue;
    }
    if (!ENV_KEY_RE.test(key)) {
      warn('compose.warn.envKeyInvalid', { key });
      continue;
    }
    if (charLength(text) > RUN_LIMITS.valueLength) {
      warn('compose.warn.envValueLong', { key, max: RUN_LIMITS.valueLength });
      continue;
    }
    if (hasControlChars(text)) {
      warn('compose.warn.envValueControl', { key });
      continue;
    }
    if (keys.has(key)) {
      warn('compose.warn.envDup', { key });
      continue;
    }
    if (env.length >= RUN_LIMITS.env) {
      warn('compose.warn.envExtra', { max: RUN_LIMITS.env });
      break;
    }
    keys.add(key);
    env.push({ key, value: text });
  }
  return env;
}

function portsOf(raw: unknown, warn: Warn): DockerPortMap[] {
  if (raw === undefined || raw === null) return [];
  if (!Array.isArray(raw)) {
    warn('compose.warn.portListInvalid');
    return [];
  }

  const ports: DockerPortMap[] = [];
  const published = new Set<string>();
  for (const item of raw) {
    if (!isRecord(item)) {
      warn('compose.warn.portLongForm');
      continue;
    }
    const target = item.target;
    const rawPublished = item.published;
    const host =
      typeof rawPublished === 'number'
        ? rawPublished
        : typeof rawPublished === 'string' && /^\d+$/.test(rawPublished.trim())
          ? Number(rawPublished.trim())
          : Number.NaN;
    const label = `${String(rawPublished ?? '?')}:${String(target ?? '?')}`;
    if (!validPortNumber(target) || !validPortNumber(host)) {
      warn('compose.warn.portNotNumeric', { port: label });
      continue;
    }
    const rawProto = item.protocol;
    const proto: DockerPortProto = rawProto === undefined || rawProto === null ? 'tcp' : rawProto === 'udp' ? 'udp' : 'tcp';
    if (rawProto !== undefined && rawProto !== null && rawProto !== 'tcp' && rawProto !== 'udp') {
      warn('compose.warn.portProto', { port: label });
      continue;
    }
    const key = `${host}/${proto}`;
    if (published.has(key)) {
      warn('compose.warn.portDup', { host, proto });
      continue;
    }
    if (ports.length >= RUN_LIMITS.ports) {
      warn('compose.warn.portExtra', { max: RUN_LIMITS.ports });
      break;
    }
    published.add(key);
    // `host_ip` se ignora: VMLab siempre publica en 127.0.0.1.
    ports.push({ host, container: target, proto });
  }
  return ports;
}

const IGNORED_OPTIONS = [
  ['volumes'],
  ['privileged'],
  ['cap_add'],
  ['devices'],
  ['command'],
  ['entrypoint'],
  ['healthcheck'],
  ['deploy'],
] as const;

interface ParsedService {
  name: string;
  id: string;
  label: string;
  target: string;
  image: string;
  env: DockerEnvVar[];
  ports: DockerPortMap[];
  dependsOn: string[];
  networks: string[];
  networkMode: string;
}

/** Une redes que Compose mantiene separadas cuando un servicio pertenece a varias. */
class Dsu {
  private parent = new Map<string, string>();

  find(value: string): string {
    const path: string[] = [];
    let current = value;
    while (this.parent.has(current) && this.parent.get(current) !== current) {
      path.push(current);
      current = this.parent.get(current)!;
    }
    for (const node of path) this.parent.set(node, current);
    return current;
  }

  union(a: string, b: string): void {
    const rootA = this.find(a);
    const rootB = this.find(b);
    if (rootA !== rootB) this.parent.set(rootB, rootA);
  }
}

/**
 * Convierte la salida de `docker compose config --format json` en un laboratorio
 * VMLab. Es una función pura: no toca el estado ni el sistema.
 *
 * Los servicios que VMLab no puede recrear (solo `build`, volúmenes, privilegios…)
 * se importan igualmente cuando tienen imagen y se avisa de lo que se pierde.
 */
export function composeToLab(json: string): ComposeResult {
  let raw: unknown;
  try {
    raw = JSON.parse(json);
  } catch {
    return { error: { key: 'compose.error.json' } };
  }
  if (!isRecord(raw)) return { error: { key: 'compose.error.object' } };
  if (!isRecord(raw.services)) return { error: { key: 'compose.error.noServices' } };

  const serviceNames = Object.keys(raw.services);
  if (serviceNames.length > MAX_SERVICES) {
    return { error: { key: 'compose.error.tooMany', params: { count: serviceNames.length, max: MAX_SERVICES } } };
  }

  const warnings: Message[] = [];
  if (serviceNames.length === 0) warnings.push({ key: 'compose.warn.empty' });

  const project = sanitizeTarget(typeof raw.name === 'string' && raw.name.trim() ? raw.name.trim() : 'vmlab');
  const parsed: ParsedService[] = [];
  const usedIds = new Set<string>();

  for (const name of serviceNames) {
    const service = raw.services[name];
    const warn: Warn = (key, params) => warnings.push({ key, params: { service: name, ...params } });
    if (!isRecord(service)) {
      warn('compose.warn.notObject');
      continue;
    }

    const image = typeof service.image === 'string' ? service.image.trim() : '';
    const hasBuild = service.build !== undefined && service.build !== null;
    if (!image) {
      if (hasBuild) warn('compose.warn.onlyBuild');
      else warn('compose.warn.noImage');
      continue;
    }
    if (hasBuild) warn('compose.warn.imageAndBuild');
    if (image.length > IMAGE_MAX) {
      warn('compose.warn.imageTooLong', { max: IMAGE_MAX });
      continue;
    }

    const containerName = typeof service.container_name === 'string' ? service.container_name.trim() : '';
    let target: string;
    if (containerName && TARGET_RE.test(containerName)) {
      target = containerName;
    } else {
      target = `${project}-${sanitizeTarget(name)}`.slice(0, ID_MAX);
      if (!TARGET_RE.test(target)) target = sanitizeTarget(target);
      if (containerName) warn('compose.warn.containerInvalid', { name: containerName, target });
    }

    const ignored = IGNORED_OPTIONS.filter(([key]) => hasEntries(service[key])).map(([key]) => key);
    if (ignored.length) warn('compose.warn.ignoredOptions', { options: ignored.join(', ') });

    const networkMode = typeof service.network_mode === 'string' ? service.network_mode.trim() : '';
    if (networkMode) {
      if (networkMode === 'host') warn('compose.warn.networkModeHost');
      else warn('compose.warn.networkMode', { mode: networkMode });
    }

    const dependsOn = dependsOnOf(service.depends_on);
    const env = environmentOf(service.environment, warn);
    const ports = portsOf(service.ports, warn);
    const networks = networkMode ? [] : networksOf(service.networks, warn);

    parsed.push({
      name,
      id: uniqueId(name, usedIds),
      label: name,
      target,
      image,
      env,
      ports,
      dependsOn,
      networks,
      networkMode,
    });
  }

  // Redes: las que un servicio comparte se fusionan (y se avisa).
  const networks = new Dsu();
  for (const service of parsed) {
    if (!service.networks.length) continue;
    if (new Set(service.networks).size > 1) {
      warnings.push({
        key: 'compose.warn.multiNetwork',
        params: { service: service.name, networks: service.networks.join(', ') },
      });
    }
    for (let i = 1; i < service.networks.length; i++) {
      networks.union(service.networks[0], service.networks[i]);
    }
  }

  // Cada red (o grupo fusionado) se conecta como un árbol que prioriza depends_on.
  const groups = new Map<string, ParsedService[]>();
  for (const service of parsed) {
    if (!service.networks.length) continue;
    const root = networks.find(service.networks[0]);
    const group = groups.get(root) ?? [];
    group.push(service);
    groups.set(root, group);
  }

  const edges: LabEdge[] = [];
  for (const group of groups.values()) {
    const earlier = new Set<string>([group[0].name]);
    for (let i = 1; i < group.length; i++) {
      const service = group[i];
      const dependency = service.dependsOn.find((dep) => earlier.has(dep));
      const parent = group.find((member) => member.name === (dependency ?? group[0].name))!;
      edges.push({ source: service.id, target: parent.id });
      earlier.add(service.name);
    }
  }

  const nodes: LabNode[] = parsed.map((service, index) => {
    const data: MachineData = { kind: 'docker', label: service.label, target: service.target, image: service.image };
    if (service.env.length || service.ports.length) {
      data.run = { env: service.env, ports: service.ports };
    }
    return {
      id: service.id,
      position: {
        x: 80 + (index % COLUMNS) * GRID,
        y: 80 + Math.floor(index / COLUMNS) * GRID,
      },
      data,
    };
  });

  // La validación de siempre decide qué llega al lienzo.
  const checked = parseLab(JSON.stringify({ format: 'vmlab-lab', version: 1, nodes, edges }));
  if (!checked.ok) return { error: checked.error };

  const lab: ComposeLab = {
    format: 'vmlab-lab',
    version: 1,
    nodes: checked.nodes.map((node) => ({
      id: node.id,
      position: { x: node.position.x, y: node.position.y },
      data: node.data,
    })),
    edges: checked.edges.map((edge) => ({ source: edge.source, target: edge.target })),
  };

  return { lab, warnings: [...warnings, ...checked.warnings] };
}
