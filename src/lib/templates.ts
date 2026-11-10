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

const webDatabase: LabTemplate = {
  id: 'web-database',
  name: 'Servidor web + base de datos',
  description: 'Nginx y MariaDB en contenedores, conectados entre sí.',
  nodes: [
    node('web', 80, 100, { kind: 'docker', label: 'Servidor web', target: 'vmlab-web', image: 'nginx:alpine' }),
    node('db', 300, 100, { kind: 'docker', label: 'Base de datos', target: 'vmlab-db', image: 'mariadb:11' }),
  ],
  edges: [edge('web', 'db')],
};

const internalVms: LabTemplate = {
  id: 'internal-vms',
  name: 'Dos máquinas en red interna',
  description: 'Dos huecos para tus VM VirtualBox, unidos por una red interna.',
  nodes: [
    node('vm1', 80, 100, { kind: 'vbox', label: 'VM 1', target: '', slot: true, net: { mode: 'intnet', adapter: 2 } }),
    node('vm2', 300, 100, { kind: 'vbox', label: 'VM 2', target: '', slot: true, net: { mode: 'intnet', adapter: 2 } }),
  ],
  edges: [edge('vm1', 'vm2')],
};

const containers: LabTemplate = {
  id: 'containers',
  name: 'Contenedores en red',
  description: 'Tres contenedores Alpine conectados en una red local de Docker.',
  nodes: [
    node('a', 80, 100, { kind: 'docker', label: 'Contenedor A', target: 'vmlab-a', image: 'alpine:3.20' }),
    node('b', 300, 100, { kind: 'docker', label: 'Contenedor B', target: 'vmlab-b', image: 'alpine:3.20' }),
    node('c', 520, 100, { kind: 'docker', label: 'Contenedor C', target: 'vmlab-c', image: 'alpine:3.20' }),
  ],
  edges: [edge('a', 'b'), edge('b', 'c'), edge('a', 'c')],
};

const attackerTarget: LabTemplate = {
  id: 'attacker-target',
  name: 'VM atacante + contenedor objetivo',
  description: 'Una VM VirtualBox y un Nginx en contenedor, unidos por una red interna.',
  nodes: [
    node('attacker', 80, 100, { kind: 'vbox', label: 'VM atacante', target: '', slot: true, net: { mode: 'intnet', adapter: 2 } }),
    node('target', 300, 100, { kind: 'docker', label: 'Objetivo', target: 'vmlab-target', image: 'nginx:alpine' }),
  ],
  edges: [edge('attacker', 'target')],
};

export const TEMPLATES: LabTemplate[] = [webDatabase, internalVms, containers, attackerTarget];
