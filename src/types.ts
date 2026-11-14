import type { Node } from '@xyflow/react';

export type MachineKind = 'vbox' | 'docker';
export type PowerState = 'running' | 'stopped' | 'paused' | 'missing' | 'unknown';
export type Action = 'start' | 'startGui' | 'stop' | 'forceStop' | 'restart' | 'pause' | 'resume';

/** Modos de adaptador que entiende VirtualBox (los que ofrece VMLab). */
export type NetMode = 'nat' | 'bridged' | 'intnet' | 'hostonly' | 'natnetwork' | 'none';

export interface NodeNet {
  mode: NetMode;
  /** Adaptador 1..4. VMLab usa el 2 por convención. */
  adapter: number;
  /** Red interna, red NAT o adaptador solo-anfitrión. */
  name?: string;
  /** Interfaz física del host para el modo puente. */
  bridge?: string;
}

/** Condiciones de red que se aplican al tráfico saliente de un nodo. */
export interface LinkConditions {
  /** Retardo en milisegundos (0..10000). */
  delayMs: number;
  /** Variación del retardo (0..10000 y nunca mayor que `delayMs`). */
  jitterMs: number;
  /** Pérdida en décimas de porcentaje: 15 = 1,5 % (0..1000). */
  lossTenths: number;
  /** Límite de velocidad en kbit/s (8..1000000). Sin valor = sin límite. */
  rateKbit?: number;
}

/** Resultado de aplicar o quitar las condiciones de red de un nodo. */
export interface LinkReport {
  warnings: string[];
}

export type DockerPortProto = 'tcp' | 'udp';

/** Variable de entorno con la que se crea un contenedor Docker. */
export interface DockerEnvVar {
  key: string;
  value: string;
}

/** Puerto publicado por un contenedor. Siempre se enlaza a 127.0.0.1. */
export interface DockerPortMap {
  host: number;
  container: number;
  proto: DockerPortProto;
}

/** Opciones que solo se usan al crear el contenedor (nunca al arrancarlo). */
export interface DockerRunOptions {
  env: DockerEnvVar[];
  ports: DockerPortMap[];
}

/**
 * Filtro de captura de paquetes. Coincide con `CaptureFilter` del núcleo Rust:
 * las variantes sin datos viajan como cadena y el puerto como `{ port }`.
 */
export type CaptureFilter = 'all' | 'icmp' | 'tcp' | 'udp' | 'arp' | { port: number };

/** Captura de paquetes gestionada por el backend. La ruta del fichero nunca sale del backend. */
export interface CaptureInfo {
  id: string;
  kind: MachineKind;
  target: string;
  fileName: string;
  /** Fecha ISO UTC en la que empezó la captura. */
  startedAt: string;
  running: boolean;
  sizeBytes: number;
  /** Avisos no fatales (filtro ignorado en VirtualBox, imagen por descargar…). */
  warning?: string;
}

/** Datos que guarda cada nodo del lienzo. */
export interface MachineData extends Record<string, unknown> {
  kind: MachineKind;
  label: string;
  /** VirtualBox: UUID de la VM. Docker: nombre del contenedor. */
  target: string;
  /** Solo Docker: imagen con la que crear el contenedor si no existe. */
  image?: string;
  /** Solo Docker: variables y puertos que se aplican al crear el contenedor. */
  run?: DockerRunOptions;
  /** Solo VirtualBox: modo de red elegido en el panel derecho. */
  net?: NodeNet;
  /** Condiciones de red opcionales del nodo. */
  link?: LinkConditions;
  /** Hueco de VM de una plantilla: todavía no tiene una máquina asignada. */
  slot?: boolean;
  /** Solo VirtualBox/Linux: usuario para Guest Additions (la contraseña nunca se guarda). */
  guestUser?: string;
  guestSystem?: GuestSystem;
  guestIface?: string;
  guestMode?: GuestIpMode;
  guestAddress?: string;
  guestPrefix?: number;
  guestGateway?: string;
  guestDns?: string;
  guestSubnet?: string;
  guestRangeStart?: string;
  guestRangeEnd?: string;
}
export type MachineNode = Node<MachineData, 'machine'>;

// ---- Contrato con el backend Rust (camelCase, ver crates/vmlab-core/src/model.rs) ----

export interface ToolsInfo {
  vboxmanage: string | null;
  docker: string | null;
}

/** Resultado de arrancar Docker Desktop en segundo plano. */
export interface DockerStartResult {
  alreadyRunning: boolean;
  launched: boolean;
  engineReady: boolean;
  waitedSecs: number;
  desktop: string;
  message: string;
}

export interface MachineInfo {
  kind: MachineKind;
  target: string;
  label: string;
  state: PowerState;
  image: string | null;
  /** Solo VirtualBox: Guest Additions detectadas (la propiedad persiste con la VM apagada). */
  guestAdditions: GuestAdditionsInfo | null;
}

export interface GuestAdditionsInfo {
  /** Versión instalada, o null si no se detectan. */
  version: string | null;
  /** false si VirtualBox no pudo consultar (VM bloqueada, etc.). */
  known: boolean;
}

export interface GuestAdditionsPrep {
  iso: string;
  attached: boolean;
  started: boolean;
  alreadyRunning: boolean;
  /** windows | linux | other */
  os: 'windows' | 'linux' | 'other';
  warnings: string[];
}

export interface Inventory {
  vbox: MachineInfo[];
  docker: MachineInfo[];
  vboxError: string | null;
  /** Mensaje listo para mostrar cuando falla Docker. */
  dockerError: string | null;
  /** El motor no está escuchando: se puede arrancar Docker Desktop desde la app. */
  dockerEngineOff: boolean;
  /** Error técnico original, para el desplegable de detalles. */
  dockerErrorDetail: string | null;
}

export interface Snapshot {
  states: Record<string, PowerState>;
  vboxOk: boolean;
  dockerOk: boolean;
}

/** Nodo de la topología que se envía a `apply_topology` (camelCase, ver model.rs). */
export interface NodeSpec {
  id: string;
  kind: MachineKind;
  target: string;
  image?: string;
  label?: string;
  net?: NodeNet;
  link?: LinkConditions;
  /** Solo Docker: opciones con las que crear el contenedor si no existe. */
  options?: DockerRunOptions;
}

export interface TopologyPayload {
  nodes: NodeSpec[];
  edges: { from: string; to: string }[];
}

export interface ApplyReport {
  networks: string[];
  applied: number;
  errors: string[];
  warnings: string[];
}

/** Nodo del lienzo sobre el que se guarda o restaura una instantánea del laboratorio. */
export interface LabSnapNode {
  kind: MachineKind;
  target: string;
  /** Nombre visible del nodo en el lienzo (para los informes). */
  label?: string;
}

export interface LabSnapInfo {
  id: string;
  label: string;
  createdAt: string | null;
  /** Nodos del lienzo que tienen esa instantánea. */
  present: string[];
  /** Nodos del lienzo que no la tienen. */
  missing: string[];
}

export interface LabSnapReport {
  done: number;
  errors: string[];
  warnings: string[];
}

/** Nodo del lienzo cuya comunicación se quiere comprobar. */
export interface ProbeNode {
  kind: MachineKind;
  target: string;
  /** Solo VirtualBox: adaptador que gestiona VMLab. */
  adapter: number | null;
}

/** Enlace observado dentro de una máquina o contenedor. */
export interface NodeProbe {
  kind: MachineKind;
  target: string;
  /** Red concreta: `intnet:<nombre>`, `hostonly:<interfaz>`, `docker:<red>`… */
  segment: string | null;
  ipv4: string | null;
  /** Motivo cuando todavía no hay comunicación. */
  note: string | null;
}

// ---- Configuración dentro de la VM Linux (Guest Additions) ----

export type GuestSystem =
  | 'auto'
  | 'netplan'
  | 'networkManager'
  | 'systemdNetworkd'
  | 'ifupdown'
  | 'wicked'
  | 'dhcpcd'
  | 'connman'
  | 'networkScripts'
  | 'dhcpd';

export type GuestSystemKind = 'client' | 'server';
export type GuestIpMode = 'dhcp' | 'static' | 'server';

export interface GuestSystemInfo {
  system: GuestSystem;
  label: string;
  /** Ruta exacta que usaría VMLab en esa VM. */
  configPath: string;
  active: boolean;
  recommended: boolean;
  kind: GuestSystemKind;
}

export interface GuestInterface {
  name: string;
  mac: string | null;
  state: string | null;
  ipv4: string | null;
}

export interface GuestProfile {
  os: string;
  osId: string;
  kernel: string;
  isLinux: boolean;
  guestAdditions: boolean;
  isRoot: boolean;
  sudo: boolean;
  systems: GuestSystemInfo[];
  interfaces: GuestInterface[];
}

export interface GuestConfigRequest {
  target: string;
  username: string;
  password: string;
  system: GuestSystem;
  interface: string;
  ipMode: GuestIpMode;
  address?: string;
  prefix?: number;
  gateway?: string;
  dns?: string[];
  subnet?: string;
  rangeStart?: string;
  rangeEnd?: string;
}

export interface GuestReport {
  system: GuestSystem;
  files: string[];
  messages: string[];
  warnings: string[];
}

/** Entrada del bloque gestionado de `/etc/hosts`. */
export interface GuestHostEntry {
  name: string;
  ipv4: string;
}

export interface GuestHostsRequest {
  target: string;
  username: string;
  password: string;
  entries: GuestHostEntry[];
}

/** Elemento arrastrable de la paleta. */
export interface PaletteItem {
  kind: MachineKind;
  label: string;
  target: string;
  image?: string;
}
