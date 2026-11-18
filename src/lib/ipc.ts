/**
 * Puente con el backend. En Tauri usa `invoke`/eventos; en un navegador normal
 * (npm run dev) cae a un simulador para poder desarrollar la interfaz sin VirtualBox ni Docker.
 */
import { t } from '../i18n';
import type {
  Action,
  ApplyReport,
  CaptureFilter,
  CaptureInfo,
  CreateVmRequest,
  CreatedVm,
  DockerRunOptions,
  DockerStartResult,
  GuestAdditionsPrep,
  GuestConfigRequest,
  GuestHostsRequest,
  GuestProfile,
  GuestReport,
  Inventory,
  LabSnapInfo,
  LabSnapNode,
  LabSnapReport,
  LinkConditions,
  LinkDiagnostics,
  LinkReport,
  MachineInfo,
  MachineKind,
  NodeProbe,
  ProbeNode,
  ScreenshotInfo,
  Snapshot,
  ToolsInfo,
  TopologyPayload,
  VmOsEntry,
  VmOsType,
} from '../types';

export const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

export const STATUS_EVENT = 'vmlab:status';

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import('@tauri-apps/api/core');
  return invoke<T>(cmd, args);
}

// ───────────────────────── API real ─────────────────────────

const real = {
  detectTools: () => invoke<ToolsInfo>('detect_tools'),
  listMachines: () => invoke<Inventory>('list_machines'),
  vmOsCatalog: () => invoke<VmOsEntry[]>('vm_os_catalog'),
  vmListOstypes: () => invoke<VmOsType[]>('vm_list_ostypes'),
  vmCreate: (request: CreateVmRequest) => invoke<CreatedVm>('vm_create', { request }),
  startDocker: () => invoke<DockerStartResult>('docker_start'),
  refreshStatus: () => invoke<Snapshot>('refresh_status'),
  machineAction: (kind: MachineKind, target: string, image: string | undefined, options: DockerRunOptions | undefined, action: Action) =>
    invoke<void>('machine_action', { kind, target, image: image ?? null, options: options ?? null, action }),
  composeConfig: (text: string) => invoke<string>('compose_config', { text }),
  openTerminal: (kind: MachineKind, target: string, user?: string, host?: string) =>
    invoke<void>('open_terminal', { kind, target, user: user ?? null, host: host ?? null }),
  applyTopology: (topology: TopologyPayload) => invoke<ApplyReport>('apply_topology', { topology }),
  linkApply: (kind: MachineKind, target: string, adapter: number | undefined, conditions: LinkConditions) =>
    invoke<LinkReport>('link_apply', { kind, target, adapter: adapter ?? null, conditions }),
  linkClear: (kind: MachineKind, target: string, adapter: number | undefined) =>
    invoke<LinkReport>('link_clear', { kind, target, adapter: adapter ?? null }),
  linkImagePresent: () => invoke<boolean>('link_image_present'),
  captureStart: (kind: MachineKind, target: string, adapter: number | undefined, filter: CaptureFilter) =>
    invoke<CaptureInfo>('capture_start', { kind, target, adapter: adapter ?? null, filter }),
  captureStop: (id: string) => invoke<CaptureInfo>('capture_stop', { id }),
  captureList: () => invoke<CaptureInfo[]>('capture_list'),
  captureOpen: (id: string) => invoke<void>('capture_open', { id }),
  captureOpenFolder: () => invoke<void>('capture_open_folder'),
  takeScreenshot: (target: string) => invoke<ScreenshotInfo>('take_screenshot', { target }),
  readScreenshot: async (path: string) => {
    const bytes = await invoke<ArrayBuffer>('read_screenshot', { path });
    return new Uint8Array(bytes);
  },
  openScreenshotsFolder: () => invoke<void>('open_screenshots_folder'),
  labsnapList: (nodes: LabSnapNode[]) => invoke<LabSnapInfo[]>('labsnap_list', { nodes }),
  labsnapTake: (nodes: LabSnapNode[], label: string) =>
    invoke<LabSnapReport>('labsnap_take', { nodes, label }),
  labsnapRestore: (nodes: LabSnapNode[], id: string) =>
    invoke<LabSnapReport>('labsnap_restore', { nodes, id }),
  labsnapDelete: (nodes: LabSnapNode[], id: string) =>
    invoke<LabSnapReport>('labsnap_delete', { nodes, id }),
  networkProbe: (nodes: ProbeNode[]) => invoke<NodeProbe[]>('network_probe', { nodes }),
  diagnoseLink: (a: NodeProbe, b: NodeProbe) => invoke<LinkDiagnostics>('diagnose_link', { a, b }),
  guestProbe: (target: string, username: string, password: string) =>
    invoke<GuestProfile>('guest_probe', { target, username, password }),
  guestApply: (request: GuestConfigRequest) => invoke<GuestReport>('guest_apply', { request }),
  guestHostsApply: (request: GuestHostsRequest) =>
    invoke<GuestReport>('guest_hosts_apply', { request }),
  prepareGuestAdditions: (target: string) =>
    invoke<GuestAdditionsPrep>('guest_additions_prepare', { target }),
  saveWorkspace: (json: string) => invoke<void>('save_workspace', { json }),
  loadWorkspace: () => invoke<string | null>('load_workspace'),
  async onStatus(cb: (s: Snapshot) => void): Promise<() => void> {
    const { listen } = await import('@tauri-apps/api/event');
    return listen<Snapshot>(STATUS_EVENT, (e) => cb(e.payload));
  },
};

// ───────────────────────── Simulador (solo navegador) ─────────────────────────

const mockStates: Snapshot['states'] = {
  'vbox:11111111-aaaa': 'running',
  'vbox:22222222-bbbb': 'stopped',
  'docker:web': 'running',
  'docker:db': 'stopped',
};
const delay = (ms: number) => new Promise((r) => setTimeout(r, ms));
const mockKey = (kind: MachineKind, target: string) => `${kind}:${target}`;

/** Catálogo de ejemplo para desarrollar el formulario sin VirtualBox. */
const MOCK_VM_OS_CATALOG: VmOsEntry[] = [
  { id: 'WindowsXP', label: 'Windows XP', group: 'windows', defaults: { memoryMb: 1024, cpus: 1, diskGb: 20, vramMb: 16, firmware: 'bios', tpm: false, graphics: 'vboxvga', diskBus: 'ide' } },
  { id: 'Windows7_64', label: 'Windows 7 (64 bits)', group: 'windows', defaults: { memoryMb: 2048, cpus: 2, diskGb: 40, vramMb: 128, firmware: 'bios', tpm: false, graphics: 'vboxvga', diskBus: 'sata' } },
  { id: 'Windows10_64', label: 'Windows 10 (64 bits)', group: 'windows', defaults: { memoryMb: 4096, cpus: 2, diskGb: 50, vramMb: 128, firmware: 'bios', tpm: false, graphics: 'vboxsvga', diskBus: 'sata' } },
  { id: 'Windows11_64', label: 'Windows 11 (64 bits)', group: 'windows', defaults: { memoryMb: 4096, cpus: 2, diskGb: 64, vramMb: 128, firmware: 'efi', tpm: true, graphics: 'vboxsvga', diskBus: 'sata' } },
  { id: 'Ubuntu_64', label: 'Ubuntu', group: 'linux', defaults: { memoryMb: 4096, cpus: 2, diskGb: 25, vramMb: 128, firmware: 'bios', tpm: false, graphics: 'vmsvga', diskBus: 'sata' } },
  { id: 'Ubuntu_64', label: 'Pop!_OS', group: 'linux', defaults: { memoryMb: 4096, cpus: 2, diskGb: 25, vramMb: 128, firmware: 'bios', tpm: false, graphics: 'vmsvga', diskBus: 'sata' } },
  { id: 'Debian_64', label: 'Debian', group: 'linux', defaults: { memoryMb: 2048, cpus: 2, diskGb: 20, vramMb: 128, firmware: 'bios', tpm: false, graphics: 'vmsvga', diskBus: 'sata' } },
  { id: 'ArchLinux_64', label: 'Arch Linux', group: 'linux', defaults: { memoryMb: 4096, cpus: 2, diskGb: 25, vramMb: 128, firmware: 'bios', tpm: false, graphics: 'vmsvga', diskBus: 'sata' } },
  { id: 'Linux26_64', label: 'Linux genérico', group: 'linux', defaults: { memoryMb: 2048, cpus: 2, diskGb: 20, vramMb: 128, firmware: 'bios', tpm: false, graphics: 'vmsvga', diskBus: 'sata' } },
];

const MOCK_VM_OSTYPES: VmOsType[] = [
  ...Array.from(new Set(MOCK_VM_OS_CATALOG.map((e) => e.id))).map((id) => ({
    id,
    description: MOCK_VM_OS_CATALOG.find((e) => e.id === id)!.label,
    family: MOCK_VM_OS_CATALOG.find((e) => e.id === id)!.group,
  })),
  { id: 'Windows2019_64', description: 'Windows Server 2019 (64-bit)', family: 'windows' },
  { id: 'FreeBSD_64', description: 'FreeBSD (64-bit)', family: 'other' },
  { id: 'DOS', description: 'DOS', family: 'other' },
];

const mockBaseVms: MachineInfo[] = [
  { kind: 'vbox', target: '11111111-aaaa', label: 'Kali Linux', state: mockStates['vbox:11111111-aaaa'], image: null, guestAdditions: { version: '7.2.22', known: true } },
  { kind: 'vbox', target: '22222222-bbbb', label: 'Windows Server 2022', state: mockStates['vbox:22222222-bbbb'], image: null, guestAdditions: { version: null, known: true } },
];
const mockCreatedVms: MachineInfo[] = [];
let mockVmSeq = 0;

/** IPv4 a entero sin signo, o `null` si el texto no tiene cuatro octetos válidos. */
function mockIpv4(text: string): number | null {
  const parts = text.split('.');
  if (parts.length !== 4) return null;
  let value = 0;
  for (const part of parts) {
    if (!/^\d{1,3}$/.test(part)) return null;
    const octet = Number(part);
    if (octet > 255) return null;
    value = (value << 8) | octet;
  }
  return value >>> 0;
}

/** Misma regla visible que `same_slash_24` del núcleo: los tres primeros octetos. */
function mockSame24(a: string, b: string): boolean {
  const pa = a.split('.');
  const pb = b.split('.');
  return pa.length === 4 && pb.length === 4 && pa[0] === pb[0] && pa[1] === pb[1] && pa[2] === pb[2];
}

interface MockSnap {
  id: string;
  label: string;
  createdAt: string;
  present: Set<string>;
}
const mockSnaps: MockSnap[] = [];
/** La imagen de `tc` se «descarga» la primera vez que se aplican condiciones. */
let mockTcImage = false;
const mockSnapKey = (node: LabSnapNode) => mockKey(node.kind, node.target);
const mockSnapName = (node: LabSnapNode) => node.label?.trim() || node.target;
/** Mismo criterio que `validate::labsnap_slug` del núcleo Rust. */
const mockSlug = (label: string) =>
  label
    .normalize('NFD')
    .replace(/[\u0300-\u036f]/g, '')
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+/, '')
    .slice(0, 40)
    .replace(/-+$/, '');

// ---- Capturas simuladas ----
interface MockCapture {
  info: CaptureInfo;
  startMs: number;
}

const mockCaptures: MockCapture[] = [];
let mockCaptureSeq = 0;

/** Marca `AAAAMMDDHHMMSS` en UTC, igual que el backend. */
const mockCaptureStamp = (ms: number) => new Date(ms).toISOString().replace(/[-:T]/g, '').slice(0, 14);

/** Simula el crecimiento del `.pcap` mientras la captura sigue en marcha. */
function mockCaptureInfo(capture: MockCapture, now: number): CaptureInfo {
  if (capture.info.running) {
    capture.info.sizeBytes = Math.floor(Math.max(0, now - capture.startMs) / 1000) * 64_000;
  }
  return { ...capture.info };
}

const mockCapture = (id: string): MockCapture => {
  const capture = mockCaptures.find((c) => c.info.id === id);
  if (!capture) throw new Error(t('mock.captureUnknown'));
  return capture;
};

/** PNG 1×1 transparente válido para probar la captura de pantalla en el navegador. */
const MOCK_SCREENSHOT_PNG = Uint8Array.from(
  atob('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII='),
  (char) => char.charCodeAt(0),
);

const mock: typeof real = {
  detectTools: async () => ({ vboxmanage: 'C:/Program Files/Oracle/VirtualBox/VBoxManage.exe', docker: 'docker' }),
  listMachines: async () => ({
    vbox: [...mockBaseVms, ...mockCreatedVms],
    docker: [
      { kind: 'docker', target: 'web', label: 'web', state: mockStates['docker:web'], image: 'nginx:latest', guestAdditions: null },
      { kind: 'docker', target: 'db', label: 'db', state: mockStates['docker:db'], image: 'postgres:16', guestAdditions: null },
    ],
    vboxError: null,
    dockerError: null,
    dockerEngineOff: false,
    dockerErrorDetail: null,
  }),
  vmOsCatalog: async () => {
    await delay(120);
    return MOCK_VM_OS_CATALOG.map((entry) => ({ ...entry, defaults: { ...entry.defaults } }));
  },
  vmListOstypes: async () => {
    await delay(150);
    return MOCK_VM_OSTYPES.map((entry) => ({ ...entry }));
  },
  vmCreate: async (request) => {
    await delay(900);
    const name = request.name.trim();
    const existing = [...mockBaseVms, ...mockCreatedVms].map((vm) => vm.label.toLowerCase());
    if (existing.includes(name.toLowerCase())) {
      throw new Error(t('mock.vmAlreadyExists', { name }));
    }
    const uuid = `00000000-0000-4000-8000-${String(++mockVmSeq).padStart(12, '0')}`;
    const folder = `C:/Users/demo/VirtualBox VMs/${name}`;
    mockStates[mockKey('vbox', uuid)] = 'stopped';
    mockCreatedVms.push({
      kind: 'vbox',
      target: uuid,
      label: name,
      state: 'stopped',
      image: null,
      guestAdditions: { version: null, known: true },
    });
    return {
      uuid,
      name,
      cfgFile: `${folder}/${name}.vbox`,
      vdiPath: `${folder}/${name}.vdi`,
    };
  },
  startDocker: async () => {
    await delay(900);
    return {
      alreadyRunning: false,
      launched: true,
      engineReady: true,
      waitedSecs: 3,
      desktop: 'C:/Program Files/Docker/Docker/Docker Desktop.exe',
      message: t('mock.dockerStarted'),
    };
  },
  refreshStatus: async () => ({ states: { ...mockStates }, vboxOk: true, dockerOk: true }),
  machineAction: async (kind, target, _image, _options, action) => {
    await delay(700);
    const next = action === 'stop' || action === 'forceStop' ? 'stopped' : action === 'pause' ? 'paused' : 'running';
    mockStates[mockKey(kind, target)] = next;
  },
  composeConfig: async (text) => {
    await delay(500);
    if (!text.trim()) throw new Error(t('mock.composeEmpty'));
    // Solo en el simulador: un compose de ejemplo válido para poder probar la importación.
    return JSON.stringify({
      name: 'vmlab-demo',
      services: {
        web: {
          image: 'nginx:alpine',
          ports: [{ target: 80, published: '8080', protocol: 'tcp' }],
        },
        db: {
          image: 'mariadb:11',
          environment: { MARIADB_ALLOW_EMPTY_ROOT_PASSWORD: '1' },
        },
      },
    });
  },
  openTerminal: async (kind, target) => {
    mockNotice?.(
      t('mock.terminalDesktop', {
        command: kind === 'docker' ? `docker exec -it ${target}` : `ssh ${target}`,
      }),
    );
  },
  applyTopology: async (t) => ({
    networks: t.edges.length ? ['vmlab-demo0001'] : [],
    applied: t.nodes.length,
    errors: [],
    warnings: [],
  }),
  linkApply: async (kind, target, _adapter, conditions) => {
    await delay(700);
    if (kind === 'docker') {
      if (mockStates[mockKey(kind, target)] !== 'running') throw new Error(t('mock.startContainerFirst'));
      mockTcImage = true;
      return { warnings: [] };
    }
    const warnings: string[] = [];
    if (conditions.delayMs > 0 || conditions.jitterMs > 0 || conditions.lossTenths > 0) {
      warnings.push(t('mock.vboxRateOnly'));
    }
    return { warnings };
  },
  linkClear: async (kind, target) => {
    await delay(500);
    if (kind === 'docker' && mockStates[mockKey(kind, target)] !== 'running') {
      throw new Error(t('mock.startContainerFirst'));
    }
    return { warnings: [] };
  },
  linkImagePresent: async () => mockTcImage,
  captureStart: async (kind, target, _adapter, filter) => {
    await delay(450);
    const state = mockStates[mockKey(kind, target)];
    if (state !== 'running') {
      throw new Error(t(kind === 'docker' ? 'mock.startContainerFirst' : 'mock.startVmFirst'));
    }
    if (mockCaptures.some((c) => c.info.running && c.info.kind === kind && c.info.target === target)) {
      throw new Error(t('mock.captureAlreadyRunning'));
    }
    const startMs = Date.now();
    const info: CaptureInfo = {
      id: `cap-${++mockCaptureSeq}`,
      kind,
      target,
      fileName: `${mockSlug(target) || 'nodo'}-${mockCaptureStamp(startMs)}.pcap`,
      startedAt: new Date(startMs).toISOString(),
      running: true,
      sizeBytes: 0,
    };
    if (kind === 'docker' && !mockTcImage) {
      info.warning = t('mock.netshootNotDownloaded');
      mockTcImage = true;
    }
    if (kind === 'vbox' && filter !== 'all') {
      info.warning = t('mock.vboxNoBpf');
    }
    mockCaptures.push({ info, startMs });
    return { ...info };
  },
  captureStop: async (id) => {
    await delay(350);
    const capture = mockCapture(id);
    if (capture.info.running) {
      capture.info.running = false;
      capture.info.sizeBytes = Math.floor(Math.max(0, Date.now() - capture.startMs) / 1000) * 64_000;
    }
    return { ...capture.info };
  },
  captureList: async () => {
    await delay(120);
    const now = Date.now();
    return mockCaptures
      .map((c) => mockCaptureInfo(c, now))
      .sort((a, b) => b.startedAt.localeCompare(a.startedAt));
  },
  captureOpen: async (id) => {
    const capture = mockCapture(id);
    mockNotice?.(t('mock.wiresharkDesktop', { file: capture.info.fileName }));
  },
  captureOpenFolder: async () => {
    mockNotice?.(t('mock.captureFolder'));
  },
  takeScreenshot: async (target) => {
    await delay(600);
    if (mockStates[mockKey('vbox', target)] !== 'running') {
      throw new Error(t('mock.startVmFirst'));
    }
    const stamp = mockCaptureStamp(Date.now());
    return { path: `C:/Users/demo/Pictures/VMLab/${mockSlug(target) || 'vm'}-${stamp}.png` };
  },
  readScreenshot: async () => {
    await delay(150);
    return new Uint8Array(MOCK_SCREENSHOT_PNG);
  },
  openScreenshotsFolder: async () => {
    mockNotice?.(t('mock.screenshotFolder'));
  },
  labsnapList: async (nodes) => {
    await delay(250);
    return mockSnaps
      .map((snap) => ({
        id: snap.id,
        label: snap.label,
        createdAt: snap.createdAt,
        present: nodes.filter((n) => snap.present.has(mockSnapKey(n))).map(mockSnapName),
        missing: nodes.filter((n) => !snap.present.has(mockSnapKey(n))).map(mockSnapName),
      }))
      .sort((a, b) => b.createdAt.localeCompare(a.createdAt));
  },
  labsnapTake: async (nodes, label) => {
    await delay(1200);
    const clean = label.trim();
    const id = mockSlug(clean);
    if (!id) throw new Error(t('mock.snapNameNeedsAlnum'));
    if (mockSnaps.some((snap) => snap.id === id)) {
      throw new Error(t('mock.snapAlreadyExists'));
    }
    mockSnaps.push({
      id,
      label: clean,
      createdAt: new Date().toISOString(),
      present: new Set(nodes.map(mockSnapKey)),
    });
    return {
      done: nodes.length,
      errors: [],
      warnings: nodes.some((n) => n.kind === 'docker') ? [t('mock.snapVolumes')] : [],
    };
  },
  labsnapRestore: async (nodes, id) => {
    await delay(1400);
    const snap = mockSnaps.find((s) => s.id === id);
    if (!snap) throw new Error(t('mock.snapMissing'));
    const restored = nodes.filter((n) => snap.present.has(mockSnapKey(n)));
    const warnings = restored
      .filter((n) => n.kind === 'docker')
      .map((n) =>
        t('mock.snapRestoredOriginal', {
          name: mockSnapName(n),
          target: n.target,
        }),
      );
    if (warnings.length) warnings.push(t('mock.snapApplyNetwork'));
    return { done: restored.length, errors: [], warnings };
  },
  labsnapDelete: async (nodes, id) => {
    await delay(800);
    const index = mockSnaps.findIndex((snap) => snap.id === id);
    if (index < 0) throw new Error(t('mock.snapMissing'));
    const [snap] = mockSnaps.splice(index, 1);
    const done = nodes.filter((n) => snap.present.has(mockSnapKey(n))).length || snap.present.size;
    return { done, errors: [], warnings: [] };
  },
  networkProbe: async (nodes) => {
    await delay(250);
    return nodes.map((n) => {
      const host = n.target === '11111111-aaaa' || n.target === 'web' ? 11 : 12;
      return n.kind === 'vbox'
        ? {
            kind: n.kind,
            target: n.target,
            segment: 'intnet:vmlab-demo0001',
            ipv4: `10.77.9.${host}`,
            netmask: '255.255.255.0',
            issues: [],
            note: null,
          }
        : {
            kind: n.kind,
            target: n.target,
            segment: 'docker:vmlab-demo0001',
            ipv4: `172.18.0.${host}`,
            netmask: '255.255.255.0',
            issues: [],
            note: null,
          };
    });
  },
  diagnoseLink: async (a, b) => {
    const live = !!(
      a.segment &&
      b.segment &&
      a.segment === b.segment &&
      a.ipv4 &&
      b.ipv4 &&
      mockSame24(a.ipv4, b.ipv4)
    );
    let differentSubnet = false;
    if (a.ipv4 && a.netmask && b.ipv4 && b.netmask) {
      const ipA = mockIpv4(a.ipv4);
      const ipB = mockIpv4(b.ipv4);
      const maskA = mockIpv4(a.netmask);
      const maskB = mockIpv4(b.netmask);
      if (ipA !== null && ipB !== null && maskA !== null && maskB !== null) {
        differentSubnet = maskA !== maskB || ((ipA & maskA) >>> 0) !== ((ipB & maskB) >>> 0);
      }
    }
    return { live, issues: [...a.issues, ...b.issues], differentSubnet };
  },
  guestProbe: async (_target, _username, _password) => {
    await delay(500);
    return {
      os: 'Ubuntu 22.04.4 LTS',
      osId: 'ubuntu',
      kernel: 'Linux 6.5.0 x86_64',
      isLinux: true,
      guestAdditions: true,
      isRoot: false,
      sudo: true,
      systems: [
        {
          system: 'netplan',
          label: 'Netplan',
          configPath: '/etc/netplan/90-vmlab.yaml',
          active: true,
          recommended: true,
          kind: 'client',
        },
        {
          system: 'networkManager',
          label: 'NetworkManager',
          configPath: '/etc/NetworkManager/system-connections/vmlab-eth0.nmconnection',
          active: true,
          recommended: false,
          kind: 'client',
        },
        {
          system: 'dhcpd',
          label: t('mock.dhcpdLabel'),
          configPath: '/etc/dhcp/dhcpd.conf',
          active: false,
          recommended: true,
          kind: 'server',
        },
      ],
      interfaces: [
        { name: 'eth0', mac: '08:00:27:aa:bb:cc', state: 'UP', ipv4: '10.0.2.15/24' },
        { name: 'eth1', mac: '08:00:27:dd:ee:ff', state: 'DOWN', ipv4: null },
      ],
    };
  },
  guestApply: async (request) => {
    await delay(900);
    return {
      system: request.system === 'auto' ? 'netplan' : request.system,
      files: request.ipMode === 'server' ? ['/etc/dhcp/dhcpd.conf'] : [`/etc/netplan/90-vmlab.yaml`],
      messages: [
        t('mock.guestConfigWritten', { iface: request.interface ?? t('mock.guestIfaceFallback') }),
      ],
      warnings: [],
    };
  },
  guestHostsApply: async (request) => {
    await delay(900);
    return {
      system: 'auto',
      files: ['/etc/hosts'],
      messages: [
        request.entries.length
          ? t('mock.hostsWritten', { count: request.entries.length })
          : t('mock.hostsCleared'),
      ],
      warnings: [],
    };
  },
  prepareGuestAdditions: async (target) => {
    await delay(700);
    return {
      iso: 'C:/Program Files/Oracle/VirtualBox/VBoxGuestAdditions.iso',
      attached: true,
      started: true,
      alreadyRunning: false,
      os: target === '22222222-bbbb' ? 'windows' : 'linux',
      warnings: [],
    };
  },
  saveWorkspace: async (json) => {
    try {
      localStorage.setItem('vmlab.workspace', json);
      localStorage.removeItem('lablab.workspace');
    } catch { /* sin almacenamiento */ }
  },
  loadWorkspace: async () => {
    try {
      const current = localStorage.getItem('vmlab.workspace');
      if (current !== null) return current;
      const old = localStorage.getItem('lablab.workspace');
      if (old !== null) {
        localStorage.setItem('vmlab.workspace', old);
        localStorage.removeItem('lablab.workspace');
      }
      return old;
    } catch { return null; }
  },
  async onStatus(cb) {
    const timer = setInterval(() => cb({ states: { ...mockStates }, vboxOk: true, dockerOk: true }), 1500);
    return () => clearInterval(timer);
  },
};

// El simulador no puede importar el store (ciclo): la app registra aquí cómo mostrar avisos.
let mockNotice: ((text: string) => void) | null = null;
export function setMockNotice(fn: ((text: string) => void) | null) {
  mockNotice = fn;
}

export const ipc = isTauri ? real : mock;

// ───────────────────────── Ventana ─────────────────────────

export const windowControls = {
  async minimize() { if (isTauri) (await import('@tauri-apps/api/window')).getCurrentWindow().minimize(); },
  async toggleMaximize() { if (isTauri) (await import('@tauri-apps/api/window')).getCurrentWindow().toggleMaximize(); },
  async close() { if (isTauri) (await import('@tauri-apps/api/window')).getCurrentWindow().close(); },
  async isMaximized(): Promise<boolean> {
    return isTauri ? (await import('@tauri-apps/api/window')).getCurrentWindow().isMaximized() : false;
  },
  async onResized(cb: () => void): Promise<() => void> {
    if (!isTauri) return () => {};
    return (await import('@tauri-apps/api/window')).getCurrentWindow().onResized(cb);
  },
};
