import { t } from '../i18n';
import type { MachineData } from '../types';
import type { LabEdge, LabNode } from './labFile';

export interface LabTemplate {
  id: string;
  name: string;
  description: string;
  nodes: LabNode[];
  edges: LabEdge[];
}

function node(id: string, x: number, y: number, data: MachineData): LabNode {
  return { id, position: { x, y }, data };
}

function edge(source: string, target: string): LabEdge {
  return { source, target };
}

/** Enlace de montaje: el disco es `source` y la ruta se guarda también en `disk.mounts`. */
function mountEdge(source: string, target: string, path: string): LabEdge {
  return { source, target, kind: 'mount', path };
}

const webDatabase: LabTemplate = {
  id: 'web-database',
  get name() { return t('template.web.name'); },
  get description() { return t('template.web.description'); },
  nodes: [
    node('web', 80, 100, {
      kind: 'docker',
      get label() { return t('template.web.webLabel'); },
      target: 'vmlab-web',
      image: 'nginx:alpine',
      run: { env: [], ports: [{ host: 8080, container: 80, proto: 'tcp' }] },
    }),
    node('db', 300, 100, {
      kind: 'docker',
      get label() { return t('template.web.dbLabel'); },
      target: 'vmlab-db',
      image: 'mariadb:11',
      run: { env: [{ key: 'MARIADB_ALLOW_EMPTY_ROOT_PASSWORD', value: '1' }], ports: [] },
    }),
  ],
  edges: [edge('web', 'db')],
};

const internalVms: LabTemplate = {
  id: 'internal-vms',
  get name() { return t('template.internal.name'); },
  get description() { return t('template.internal.description'); },
  nodes: [
    node('vm1', 80, 100, { kind: 'vbox', label: 'VM 1', target: '', slot: true, net: { mode: 'intnet', adapter: 2 } }),
    node('vm2', 300, 100, { kind: 'vbox', label: 'VM 2', target: '', slot: true, net: { mode: 'intnet', adapter: 2 } }),
  ],
  edges: [edge('vm1', 'vm2')],
};

const containers: LabTemplate = {
  id: 'containers',
  get name() { return t('template.containers.name'); },
  get description() { return t('template.containers.description'); },
  nodes: [
    node('a', 80, 100, {
      kind: 'docker',
      get label() { return t('template.containers.label', { letter: 'A' }); },
      target: 'vmlab-a',
      image: 'alpine:3.20',
    }),
    node('b', 300, 100, {
      kind: 'docker',
      get label() { return t('template.containers.label', { letter: 'B' }); },
      target: 'vmlab-b',
      image: 'alpine:3.20',
    }),
    node('c', 520, 100, {
      kind: 'docker',
      get label() { return t('template.containers.label', { letter: 'C' }); },
      target: 'vmlab-c',
      image: 'alpine:3.20',
    }),
  ],
  edges: [edge('a', 'b'), edge('b', 'c'), edge('a', 'c')],
};

const attackerTarget: LabTemplate = {
  id: 'attacker-target',
  get name() { return t('template.attacker.name'); },
  get description() { return t('template.attacker.description'); },
  nodes: [
    node('attacker', 80, 100, {
      kind: 'vbox',
      get label() { return t('template.attacker.attackerLabel'); },
      target: '',
      slot: true,
      net: { mode: 'intnet', adapter: 2 },
    }),
    node('target', 300, 100, {
      kind: 'docker',
      get label() { return t('template.attacker.targetLabel'); },
      target: 'vmlab-target',
      image: 'nginx:alpine',
    }),
  ],
  edges: [edge('attacker', 'target')],
};

const serverWithDisk: LabTemplate = {
  id: 'server-disk',
  get name() { return t('template.serverDisk.name'); },
  get description() { return t('template.serverDisk.description'); },
  nodes: [
    node('web', 80, 100, {
      kind: 'docker',
      get label() { return t('template.serverDisk.webLabel'); },
      target: 'vmlab-web',
      image: 'nginx:alpine',
      run: { env: [], ports: [{ host: 8080, container: 80, proto: 'tcp' }] },
    }),
    node('datos', 300, 100, {
      kind: 'disk',
      get label() { return t('template.serverDisk.diskLabel'); },
      target: 'datos',
      disk: { shared: false, mounts: [{ container: 'web', path: '/usr/share/nginx/html' }] },
    }),
  ],
  edges: [mountEdge('datos', 'web', '/usr/share/nginx/html')],
};

export const TEMPLATES: LabTemplate[] = [webDatabase, internalVms, containers, attackerTarget, serverWithDisk];
