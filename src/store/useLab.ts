import { create } from 'zustand';
import {
  applyEdgeChanges,
  applyNodeChanges,
  type Connection,
  type Edge,
  type EdgeChange,
  type NodeChange,
} from '@xyflow/react';
import { probeKey } from '../lib/connectivity';
import { ipc, setMockNotice } from '../lib/ipc';
import { parseLab, serializeLab, type LabEdge, type LabNode } from '../lib/labFile';
import { hasLinkConditions } from '../lib/link';
import { EMPTY_SNAPSHOT, resolveState } from '../lib/status';
import type { LabTemplate } from '../lib/templates';
import { toPayload } from '../lib/topology';
import { formatMessage, setI18nLang, t, technicalText, type Lang, type Message } from '../i18n';
import type {
  Action,
  ApplyReport,
  CaptureFilter,
  CaptureInfo,
  Inventory,
  LabSnapInfo,
  LabSnapNode,
  MachineData,
  MachineNode,
  NodeProbe,
  PaletteItem,
  Snapshot,
  ToolsInfo,
} from '../types';

export type Theme = 'dark' | 'light';
export type LabMode = 'replace' | 'append';
export interface Toast {
  id: number;
  kind: 'ok' | 'info' | 'error';
  text: string;
  /** Veces que se ha repetido el mismo mensaje de error consecutivo (empieza en 1). */
  count: number;
}

interface LabState {
  nodes: MachineNode[];
  edges: Edge[];
  snapshot: Snapshot;
  tools: ToolsInfo | null;
  inventory: Inventory | null;
  busy: Record<string, boolean>;
  applying: boolean;
  /** Docker Desktop se está arrancando en segundo plano. */
  dockerStarting: boolean;
  /** Instantáneas del laboratorio disponibles en los nodos del lienzo. */
  labsnaps: LabSnapInfo[];
  /** Hay una operación de instantáneas en curso (puede tardar minutos). */
  labsnapBusy: boolean;
  /** Capturas de paquetes de esta sesión (activas y terminadas). */
  captures: CaptureInfo[];
  /** Hay una operación de captura en curso (iniciar/detener). */
  captureBusy: boolean;
  report: ApplyReport | null;
  /** Último enlace observado por nodo (`kind:target`). */
  probes: Record<string, NodeProbe>;
  theme: Theme;
  /** Idioma de la interfaz: `auto` sigue el idioma del sistema. */
  lang: Lang;
  toasts: Toast[];
  /** Un nodo del lienzo se está arrastrando sobre el inventario. */
  overSidebar: boolean;
  /** El lienzo está bloqueado: no se desplaza ni hace zoom, pero los nodos siguen arrastrándose. */
  canvasLocked: boolean;

  init: () => Promise<void>;
  refreshInventory: () => Promise<void>;
  startDocker: () => Promise<void>;
  onNodesChange: (c: NodeChange<MachineNode>[]) => void;
  onEdgesChange: (c: EdgeChange[]) => void;
  onConnect: (c: Connection) => void;
  addMachine: (item: PaletteItem, position?: { x: number; y: number }) => void;
  updateNode: (id: string, patch: Partial<MachineData>) => void;
  removeNode: (id: string) => void;
  exportLab: () => void;
  importLab: (text: string, mode: LabMode) => Promise<void>;
  applyTemplate: (template: LabTemplate, mode: LabMode) => void;
  act: (id: string, action: Action) => Promise<void>;
  openTerminal: (id: string) => Promise<void>;
  actAll: (action: 'start' | 'stop') => Promise<void>;
  applyNetwork: () => Promise<void>;
  applyLink: (id: string) => Promise<void>;
  clearLink: (id: string) => Promise<void>;
  refreshCaptures: () => Promise<void>;
  startCapture: (id: string, filter: CaptureFilter) => Promise<void>;
  stopCapture: (id: string, open: boolean) => Promise<void>;
  openCapture: (id: string) => Promise<void>;
  openCaptureFolder: () => Promise<void>;
  refreshLabsnaps: () => Promise<void>;
  labsnapTake: (label: string) => Promise<void>;
  labsnapRestore: (id: string) => Promise<void>;
  labsnapDelete: (id: string) => Promise<void>;
  refreshProbes: () => Promise<void>;
  toggleTheme: () => void;
  setLang: (lang: Lang) => void;
  toggleCanvasLock: () => void;
  setOverSidebar: (over: boolean) => void;
  toast: (kind: Toast['kind'], text: string) => void;
  dismissToast: (id: number) => void;
}

const THEME_KEY = 'vmlab.theme';
const LANG_KEY = 'vmlab.lang';
const CANVAS_LOCK_KEY = 'vmlab.canvasLocked';
const LEGACY_THEME_KEY = 'lablab.theme';
const LEGACY_CANVAS_LOCK_KEY = 'lablab.canvasLocked';
let toastSeq = 0;
let started = false;
let probeBusy = false;
let saveTimer: ReturnType<typeof setTimeout> | undefined;
let captureTimer: ReturnType<typeof setInterval> | undefined;

/** Refresca la lista de capturas cada 2 s solo mientras haya alguna en marcha. */
function syncCapturePolling(hasRunning: boolean) {
  if (!hasRunning) {
    if (captureTimer !== undefined) {
      clearInterval(captureTimer);
      captureTimer = undefined;
    }
    return;
  }
  if (captureTimer !== undefined) return;
  captureTimer = setInterval(() => {
    void useLab.getState().refreshCaptures();
  }, 2_000);
}

/** Migra una clave del nombre antiguo (`lablab.*`) al nuevo (`vmlab.*`). */
function storedValue(key: string, legacy: string): string | null {
  try {
    const current = localStorage.getItem(key);
    if (current !== null) return current;
    const old = localStorage.getItem(legacy);
    if (old === null) return null;
    localStorage.setItem(key, old);
    localStorage.removeItem(legacy);
    return old;
  } catch {
    return null;
  }
}

function initialTheme(): Theme {
  const saved = storedValue(THEME_KEY, LEGACY_THEME_KEY);
  if (saved === 'light' || saved === 'dark') return saved;
  return window.matchMedia?.('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
}

function initialCanvasLocked(): boolean {
  return storedValue(CANVAS_LOCK_KEY, LEGACY_CANVAS_LOCK_KEY) === '1';
}

function initialLang(): Lang {
  try {
    const saved = localStorage.getItem(LANG_KEY);
    if (saved === 'auto' || saved === 'es' || saved === 'en') return saved;
  } catch {
    /* sin almacenamiento */
  }
  return 'auto';
}

function applyLang(lang: Lang) {
  setI18nLang(lang);
}

function applyTheme(t: Theme) {
  document.documentElement.dataset.theme = t;
  try { localStorage.setItem(THEME_KEY, t); } catch { /* sin almacenamiento */ }
}

const errText = (e: unknown) => (typeof e === 'string' ? e : e instanceof Error ? e.message : JSON.stringify(e));

/** Errores de la CLI cuando Docker Desktop está cerrado o su motor aún no escucha. */
const DOCKER_DOWN_RE = /failed to connect to the docker api|cannot connect to the docker daemon|is the docker daemon running|error during connect/i;

const dockerDown = (e: unknown) => DOCKER_DOWN_RE.test(errText(e));
const newId = () => `n_${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;

/** Nodos que todavía no apuntan a una máquina real (huecos de plantilla o destino vacío). */
function unassignedNodes(nodes: MachineNode[]): MachineNode[] {
  return nodes.filter((n) => n.data.slot === true || n.data.target.trim() === '');
}

/** Nodos que viajan al backend de instantáneas: nunca incluye huecos de plantilla. */
function labsnapNodes(nodes: MachineNode[]): LabSnapNode[] {
  return nodes
    .filter((n) => n.data.slot !== true && n.data.target.trim() !== '')
    .map((n) => ({ kind: n.data.kind, target: n.data.target, label: n.data.label }));
}

/** Adaptador de VirtualBox para el límite de ancho de banda (Docker no lo usa). */
function linkAdapter(node: MachineNode): number | undefined {
  return node.data.kind === 'vbox' ? node.data.net?.adapter ?? 2 : undefined;
}

/** Mensaje exacto que bloquea aplicar la red y arrancar/detener todo mientras haya huecos. */
function slotsError(nodes: MachineNode[]): string | null {
  const names = unassignedNodes(nodes).map((n) => n.data.label.trim() || t('toast.unnamed'));
  if (!names.length) return null;
  return t('toast.slots', { names: names.join('», «') });
}

/** Aviso ya traducido (backend) o pendiente de traducir (validador/store). */
type ToastNote = string | Message;

/** Añade como máximo 3 avisos y resume el resto, en líneas separadas. */
function toastSummary(line: string, warnings: ToastNote[]): string {
  const lines = [line, ...warnings.slice(0, 3).map((w) => (typeof w === 'string' ? w : formatMessage(w)))];
  if (warnings.length > 3) lines.push(t('toast.moreWarnings', { count: warnings.length - 3 }));
  return lines.join('\n');
}

interface MergeResult {
  nodes: MachineNode[];
  edges: Edge[];
  added: number;
}

/** Resuelve un UUID local por nombre cuando el archivo viene de otro equipo. */
function resolveVboxTarget(data: MachineData, inventory: Inventory | null, warnings: Message[]): MachineData {
  if (data.kind !== 'vbox' || data.slot === true || !data.target) return data;
  const vms = inventory?.vbox ?? [];
  if (vms.some((vm) => vm.target === data.target)) return data;
  const byName = vms.find((vm) => vm.label === data.label);
  if (byName) return { ...data, target: byName.target };
  warnings.push({ key: 'toast.vmMissing', params: { name: data.label || data.target } });
  return data;
}

/**
 * Integra nodos y aristas de un archivo o plantilla en el lienzo.
 *
 * - Genera ids nuevos y reasigna las aristas.
 * - En modo `append` desplaza el bloque a la derecha para no solapar.
 * - En modo `append` omite kind+target ya presentes (las plantillas Docker
 *   reciben un sufijo numérico en el destino, porque son funcionales).
 */
function mergeLab(
  incomingNodes: readonly LabNode[],
  incomingEdges: readonly LabEdge[],
  mode: LabMode,
  currentNodes: MachineNode[],
  currentEdges: Edge[],
  inventory: Inventory | null,
  template: boolean,
  warnings: Message[],
): MergeResult {
  const baseNodes = mode === 'replace' ? [] : currentNodes;
  const baseEdges = mode === 'replace' ? [] : currentEdges;
  const taken = new Set<string>();
  for (const node of baseNodes) {
    if (node.data.target) taken.add(`${node.data.kind}:${node.data.target}`);
  }

  const usedIds = new Set(baseNodes.map((node) => node.id));
  const idMap = new Map<string, string>();
  const kept: MachineNode[] = [];
  const stamp = Date.now().toString(36);
  const minX = incomingNodes.length ? Math.min(...incomingNodes.map((node) => node.position.x)) : 0;
  const maxX = currentNodes.length ? Math.max(...currentNodes.map((node) => node.position.x)) : 0;
  const shift = mode === 'append' && currentNodes.length && incomingNodes.length ? maxX + 260 - minX : 0;

  incomingNodes.forEach((input, index) => {
    let data = input.data;
    if (!template) data = resolveVboxTarget(data, inventory, warnings);

    if (data.target) {
      const key = `${data.kind}:${data.target}`;
      if (taken.has(key)) {
        if (template && data.kind === 'docker') {
          let suffix = 2;
          let candidate = `${data.target}-${suffix}`;
          while (taken.has(`docker:${candidate}`)) {
            suffix += 1;
            candidate = `${data.target}-${suffix}`;
          }
          data = { ...data, target: candidate };
        } else {
          warnings.push({ key: 'toast.alreadyOnCanvasSkip', params: { label: data.label || data.target } });
          return;
        }
      }
      taken.add(`${data.kind}:${data.target}`);
    }

    let id = `n_${stamp}_${index}`;
    let extra = 1;
    while (usedIds.has(id)) {
      id = `n_${stamp}_${index}_${extra}`;
      extra += 1;
    }
    usedIds.add(id);
    idMap.set(input.id, id);
    kept.push({
      id,
      type: 'machine',
      position: { x: input.position.x + shift, y: input.position.y },
      data,
    });
  });

  const edges: Edge[] = [];
  const seenEdges = new Set<string>();
  for (const input of incomingEdges) {
    const source = idMap.get(input.source);
    const target = idMap.get(input.target);
    if (!source || !target || source === target) continue;
    const key = source < target ? `${source}|${target}` : `${target}|${source}`;
    if (seenEdges.has(key)) continue;
    seenEdges.add(key);
    edges.push({ id: `e_${source}_${target}`, source, target });
  }

  return { nodes: [...baseNodes, ...kept], edges: [...baseEdges, ...edges], added: kept.length };
}

export const useLab = create<LabState>()((set, get) => ({
  nodes: [],
  edges: [],
  snapshot: EMPTY_SNAPSHOT,
  tools: null,
  inventory: null,
  busy: {},
  applying: false,
  dockerStarting: false,
  labsnaps: [],
  labsnapBusy: false,
  captures: [],
  captureBusy: false,
  report: null,
  probes: {},
  theme: initialTheme(),
  lang: initialLang(),
  toasts: [],
  overSidebar: false,
  canvasLocked: initialCanvasLocked(),

  async init() {
    if (started) return; // React.StrictMode monta dos veces en desarrollo
    started = true;
    applyTheme(get().theme);
    applyLang(get().lang);

    try {
      const raw = await ipc.loadWorkspace();
      if (raw) {
        const ws = JSON.parse(raw) as { nodes?: MachineNode[]; edges?: Edge[] };
        if (Array.isArray(ws.nodes) && Array.isArray(ws.edges)) set({ nodes: ws.nodes, edges: ws.edges });
      }
    } catch (e) {
      get().toast('error', technicalText(t('toast.workspaceLoad', { detail: errText(e) })));
    }

    // Autoguardado (con debounce) solo cuando cambia el contenido real del lienzo.
    useLab.subscribe((s, prev) => {
      if (s.nodes === prev.nodes && s.edges === prev.edges) return;
      clearTimeout(saveTimer);
      saveTimer = setTimeout(() => {
        const { nodes, edges } = useLab.getState();
        const slim = nodes.map(({ id, type, position, data }) => ({ id, type, position, data }));
        ipc.saveWorkspace(JSON.stringify({ version: 1, nodes: slim, edges })).catch(() => {});
      }, 600);
    });

    try { set({ tools: await ipc.detectTools() }); } catch { /* se mostrará como no detectado */ }
    await get().refreshInventory();
    try { set({ snapshot: await ipc.refreshStatus() }); } catch { /* el monitor lo reintenta */ }
    await ipc.onStatus((snapshot) => set({ snapshot }));
    void get().refreshCaptures();
  },

  async refreshInventory() {
    try {
      set({ inventory: await ipc.listMachines() });
    } catch (e) {
      get().toast('error', technicalText(errText(e)));
    }
  },

  async startDocker() {
    if (get().dockerStarting) return;
    set({ dockerStarting: true });
    get().toast('info', t('toast.dockerStarting'));
    try {
      const result = await ipc.startDocker();
      get().toast(result.engineReady ? 'ok' : 'error', result.engineReady ? result.message : technicalText(result.message));
      try { set({ tools: await ipc.detectTools() }); } catch { /* se mantiene lo detectado */ }
      await get().refreshInventory();
      try { set({ snapshot: await ipc.refreshStatus() }); } catch { /* el monitor lo reintenta */ }
      void get().refreshProbes();
    } catch (e) {
      get().toast('error', technicalText(t('toast.dockerStartFailed', { detail: errText(e) })));
    } finally {
      set({ dockerStarting: false });
    }
  },

  onNodesChange: (c) => set((s) => ({ nodes: applyNodeChanges(c, s.nodes) })),
  onEdgesChange: (c) => set((s) => ({ edges: applyEdgeChanges(c, s.edges) })),

  onConnect(c) {
    if (!c.source || !c.target || c.source === c.target) return;
    const exists = get().edges.some(
      (e) => (e.source === c.source && e.target === c.target) || (e.source === c.target && e.target === c.source),
    );
    if (exists) return;
    const edge: Edge = {
      id: `e_${c.source}_${c.target}`,
      source: c.source,
      target: c.target,
      sourceHandle: c.sourceHandle,
      targetHandle: c.targetHandle,
    };
    set((s) => ({ edges: [...s.edges, edge] }));
  },

  addMachine(item, position) {
    const { nodes, toast } = get();
    if (nodes.some((n) => n.data.kind === item.kind && n.data.target === item.target)) {
      toast('info', t('toast.alreadyOnCanvas', { label: item.label }));
      return;
    }
    const n = nodes.length;
    const node: MachineNode = {
      id: newId(),
      type: 'machine',
      position: position ?? { x: 80 + (n % 4) * 230, y: 80 + Math.floor(n / 4) * 150 },
      data: { kind: item.kind, label: item.label, target: item.target, image: item.image },
    };
    set((s) => ({ nodes: [...s.nodes, node] }));
  },

  updateNode: (id, patch) =>
    set((s) => ({ nodes: s.nodes.map((n) => (n.id === id ? { ...n, data: { ...n.data, ...patch } } : n)) })),

  removeNode: (id) =>
    set((s) => ({
      nodes: s.nodes.filter((n) => n.id !== id),
      edges: s.edges.filter((e) => e.source !== id && e.target !== id),
    })),

  exportLab() {
    const { nodes, edges, toast } = get();
    if (!nodes.length) {
      toast('info', t('toast.exportEmpty'));
      return;
    }
    try {
      const blob = new Blob([serializeLab(nodes, edges)], { type: 'application/json' });
      const url = URL.createObjectURL(blob);
      const link = document.createElement('a');
      link.href = url;
      link.download = `vmlab-lab-${new Date().toISOString().slice(0, 10)}.vmlab`;
      document.body.appendChild(link);
      link.click();
      link.remove();
      window.setTimeout(() => URL.revokeObjectURL(url), 1000);
      toast('ok', t('toast.exported', { nodes: nodes.length, edges: edges.length }));
    } catch (e) {
      toast('error', technicalText(t('toast.exportFailed', { detail: errText(e) })));
    }
  },

  async importLab(text, mode) {
    let inventory = get().inventory;
    if (!inventory) {
      await get().refreshInventory();
      inventory = get().inventory;
    }

    const parsed = parseLab(text);
    if (!parsed.ok) {
      get().toast('error', formatMessage(parsed.error));
      return;
    }

    const warnings = [...parsed.warnings];
    const result = mergeLab(parsed.nodes, parsed.edges, mode, get().nodes, get().edges, inventory, false, warnings);
    set({ nodes: result.nodes, edges: result.edges, probes: {}, report: null });
    const summary = mode === 'replace'
      ? t('toast.importedReplace', { count: result.added })
      : t('toast.importedAppend', { count: result.added });
    get().toast(warnings.length ? 'info' : 'ok', toastSummary(summary, warnings));
  },

  applyTemplate(template, mode) {
    const warnings: Message[] = [];
    const result = mergeLab(template.nodes, template.edges, mode, get().nodes, get().edges, get().inventory, true, warnings);
    set({ nodes: result.nodes, edges: result.edges, probes: {}, report: null });
    const summary = mode === 'replace'
      ? t('toast.templateReplace', { name: template.name, count: result.added })
      : t('toast.templateAppend', { name: template.name, count: result.added });
    get().toast(warnings.length ? 'info' : 'ok', toastSummary(summary, warnings));
  },

  async act(id, action) {
    if (get().labsnapBusy) return;
    const node = get().nodes.find((n) => n.id === id);
    if (!node || node.data.slot === true || !node.data.target.trim() || get().busy[id]) return;
    set((s) => ({ busy: { ...s.busy, [id]: true } }));
    try {
      // `run` solo se envía para contenedores y solo se usa si hay que crearlos.
      const options = node.data.kind === 'docker' ? node.data.run : undefined;
      await ipc.machineAction(node.data.kind, node.data.target, node.data.image, options, action);
    } catch (e) {
      if (node.data.kind === 'docker' && dockerDown(e)) {
        get().toast('error', t('toast.dockerDown', { label: node.data.label }));
        // Trae `dockerEngineOff` para que aparezca el botón de arranque.
        void get().refreshInventory();
      } else {
        get().toast('error', technicalText(t('toast.details', { label: node.data.label, detail: errText(e) })));
      }
    } finally {
      set((s) => ({ busy: { ...s.busy, [id]: false } }));
      try { set({ snapshot: await ipc.refreshStatus() }); } catch { /* el monitor lo reintenta */ }
      void get().refreshProbes();
    }
  },

  async openTerminal(id) {
    if (get().labsnapBusy) return;
    const node = get().nodes.find((n) => n.id === id);
    if (!node) return;
    const { data } = node;
    if (data.slot === true || !data.target.trim()) {
      get().toast('info', t('toast.slotTerminal'));
      return;
    }
    if (resolveState(data, get().snapshot) !== 'running') {
      get().toast('error', t(data.kind === 'docker' ? 'toast.containerNotRunning' : 'toast.vmNotRunning'));
      return;
    }
    try {
      if (data.kind === 'docker') {
        await ipc.openTerminal('docker', data.target);
        return;
      }
      const user = (data.guestUser ?? '').trim();
      if (!user) {
        get().toast('error', t('toast.missingUser'));
        return;
      }
      const ipv4 = get().probes[probeKey('vbox', data.target)]?.ipv4;
      if (!ipv4) {
        get().toast('error', t('toast.noIp'));
        return;
      }
      await ipc.openTerminal('vbox', data.target, user, ipv4);
    } catch (e) {
      get().toast('error', technicalText(errText(e)));
    }
  },

  async actAll(action) {
    if (get().labsnapBusy) return;
    const error = slotsError(get().nodes);
    if (error) {
      get().toast('error', error);
      return;
    }
    await Promise.allSettled(get().nodes.map((n) => get().act(n.id, action)));
  },

  async applyNetwork() {
    if (get().applying || get().labsnapBusy) return;
    const error = slotsError(get().nodes);
    if (error) {
      get().toast('error', error);
      return;
    }
    // Mientras se reconfigura, los cables vuelven a rojo hasta confirmar la nueva red.
    set({ applying: true, probes: {} });
    try {
      const { nodes, edges } = get();
      const report = await ipc.applyTopology(toPayload(nodes, edges));
      set({ report });
      if (report.errors.length) get().toast('error', t('toast.applyNetworkErrors', { count: report.errors.length }));
      else {
        get().toast(
          'ok',
          report.networks.length ? t('toast.networkApplied', { count: report.applied }) : t('toast.noConnections'),
        );
      }
      void get().refreshProbes();
    } catch (e) {
      get().toast('error', technicalText(errText(e)));
    } finally {
      set({ applying: false });
    }
  },

  async applyLink(id) {
    if (get().labsnapBusy) return;
    const node = get().nodes.find((n) => n.id === id);
    if (!node || node.data.slot === true || !node.data.target.trim() || get().busy[id]) return;
    const conditions = node.data.link;
    if (!hasLinkConditions(conditions)) {
      get().toast('info', t('toast.noLinkConditions'));
      return;
    }

    set((s) => ({ busy: { ...s.busy, [id]: true } }));
    try {
      const report = await ipc.linkApply(
        node.data.kind,
        node.data.target,
        linkAdapter(node),
        conditions,
      );
      const warnings = report.warnings ?? [];
      get().toast(
        warnings.length ? 'info' : 'ok',
        toastSummary(t('toast.linkApplied', { label: node.data.label }), warnings),
      );
    } catch (e) {
      if (node.data.kind === 'docker' && dockerDown(e)) {
        get().toast('error', t('toast.dockerDown', { label: node.data.label }));
        void get().refreshInventory();
      } else {
        get().toast('error', technicalText(t('toast.details', { label: node.data.label, detail: errText(e) })));
      }
    } finally {
      set((s) => ({ busy: { ...s.busy, [id]: false } }));
    }
  },

  async clearLink(id) {
    if (get().labsnapBusy) return;
    const node = get().nodes.find((n) => n.id === id);
    if (!node || node.data.slot === true || !node.data.target.trim() || get().busy[id]) return;

    set((s) => ({ busy: { ...s.busy, [id]: true } }));
    try {
      const report = await ipc.linkClear(node.data.kind, node.data.target, linkAdapter(node));
      get().updateNode(id, { link: undefined });
      const warnings = report.warnings ?? [];
      get().toast(
        warnings.length ? 'info' : 'ok',
        toastSummary(t('toast.linkCleared', { label: node.data.label }), warnings),
      );
    } catch (e) {
      if (node.data.kind === 'docker' && dockerDown(e)) {
        get().toast('error', t('toast.dockerDown', { label: node.data.label }));
        void get().refreshInventory();
      } else {
        get().toast('error', technicalText(t('toast.details', { label: node.data.label, detail: errText(e) })));
      }
    } finally {
      set((s) => ({ busy: { ...s.busy, [id]: false } }));
    }
  },

  async refreshCaptures() {
    try {
      const captures = await ipc.captureList();
      set({ captures });
      syncCapturePolling(captures.some((c) => c.running));
    } catch {
      // Un fallo puntual conserva la última lista; el siguiente ciclo lo reintenta.
    }
  },

  async startCapture(id, filter) {
    if (get().captureBusy || get().labsnapBusy) return;
    const node = get().nodes.find((n) => n.id === id);
    if (!node || node.data.slot === true || !node.data.target.trim()) return;
    if (resolveState(node.data, get().snapshot) !== 'running') {
      get().toast(
        'error',
        t(node.data.kind === 'docker' ? 'toast.containerNotRunning' : 'toast.vmNotRunningLong'),
      );
      return;
    }

    set({ captureBusy: true });
    try {
      const info = await ipc.captureStart(node.data.kind, node.data.target, linkAdapter(node), filter);
      set((s) => ({ captures: [info, ...s.captures.filter((c) => c.id !== info.id)] }));
      syncCapturePolling(true);
      get().toast(
        info.warning ? 'info' : 'ok',
        toastSummary(t('toast.captureStarted', { label: node.data.label }), info.warning ? [info.warning] : []),
      );
    } catch (e) {
      if (node.data.kind === 'docker' && dockerDown(e)) {
        get().toast('error', t('toast.dockerDown', { label: node.data.label }));
        void get().refreshInventory();
      } else {
        get().toast('error', technicalText(t('toast.details', { label: node.data.label, detail: errText(e) })));
      }
    } finally {
      set({ captureBusy: false });
    }
  },

  async stopCapture(id, open) {
    if (get().captureBusy) return;
    set({ captureBusy: true });
    try {
      const stopped = await ipc.captureStop(id);
      set((s) => ({ captures: s.captures.map((c) => (c.id === id ? stopped : c)) }));
      syncCapturePolling(get().captures.some((c) => c.running));
      if (open) {
        try {
          await ipc.captureOpen(id);
          get().toast('ok', t('toast.captureStoppedOpen', { file: stopped.fileName }));
        } catch (e) {
          get().toast('error', technicalText(errText(e)));
        }
      } else {
        get().toast('ok', t('toast.captureSaved', { file: stopped.fileName }));
      }
    } catch (e) {
      get().toast('error', technicalText(t('toast.captureStopFailed', { detail: errText(e) })));
    } finally {
      set({ captureBusy: false });
    }
  },

  async openCapture(id) {
    try {
      await ipc.captureOpen(id);
    } catch (e) {
      get().toast('error', technicalText(errText(e)));
    }
  },

  async openCaptureFolder() {
    try {
      await ipc.captureOpenFolder();
    } catch (e) {
      get().toast('error', technicalText(t('toast.captureFolderFailed', { detail: errText(e) })));
    }
  },

  async refreshLabsnaps() {
    try {
      set({ labsnaps: await ipc.labsnapList(labsnapNodes(get().nodes)) });
    } catch (e) {
      get().toast('error', technicalText(t('toast.snapsReadFailed', { detail: errText(e) })));
    }
  },

  async labsnapTake(label) {
    if (get().labsnapBusy) return;
    const clean = label.trim();
    if (!clean) {
      get().toast('error', t('toast.snapNeedName'));
      return;
    }
    const nodes = labsnapNodes(get().nodes);
    if (!nodes.length) {
      get().toast('info', t('toast.snapNoMachines'));
      return;
    }
    set({ labsnapBusy: true });
    get().toast('info', t('toast.snapSaving', { label: clean }));
    try {
      const report = await ipc.labsnapTake(nodes, clean);
      const notes = [...report.errors, ...report.warnings];
      get().toast(
        report.errors.length ? 'error' : 'ok',
        toastSummary(t('toast.snapSaved', { label: clean, count: report.done }), notes),
      );
    } catch (e) {
      get().toast('error', technicalText(errText(e)));
    } finally {
      set({ labsnapBusy: false });
      await get().refreshLabsnaps();
      void get().refreshInventory();
      try { set({ snapshot: await ipc.refreshStatus() }); } catch { /* el monitor lo reintenta */ }
    }
  },

  async labsnapRestore(id) {
    if (get().labsnapBusy) return;
    const nodes = labsnapNodes(get().nodes);
    if (!nodes.length) return;
    const snap = get().labsnaps.find((s) => s.id === id);
    set({ labsnapBusy: true });
    get().toast('info', t('toast.snapRestoring', { label: snap?.label ?? id }));
    try {
      const report = await ipc.labsnapRestore(nodes, id);
      const notes = [...report.errors, ...report.warnings];
      get().toast(
        report.errors.length ? 'error' : 'ok',
        toastSummary(t('toast.snapRestored', { label: snap?.label ?? id, count: report.done }), notes),
      );
    } catch (e) {
      get().toast('error', technicalText(errText(e)));
    } finally {
      set({ labsnapBusy: false });
      await get().refreshLabsnaps();
      void get().refreshInventory();
      try { set({ snapshot: await ipc.refreshStatus() }); } catch { /* el monitor lo reintenta */ }
      void get().refreshProbes();
    }
  },

  async labsnapDelete(id) {
    if (get().labsnapBusy) return;
    const nodes = labsnapNodes(get().nodes);
    if (!nodes.length) return;
    const snap = get().labsnaps.find((s) => s.id === id);
    set({ labsnapBusy: true });
    try {
      const report = await ipc.labsnapDelete(nodes, id);
      const notes = [...report.errors, ...report.warnings];
      get().toast(
        report.errors.length ? 'error' : 'ok',
        toastSummary(t('toast.snapDeleted', { label: snap?.label ?? id, count: report.done }), notes),
      );
    } catch (e) {
      get().toast('error', technicalText(errText(e)));
    } finally {
      set({ labsnapBusy: false });
      await get().refreshLabsnaps();
      void get().refreshInventory();
      try { set({ snapshot: await ipc.refreshStatus() }); } catch { /* el monitor lo reintenta */ }
    }
  },

  async refreshProbes() {
    const { nodes, edges, snapshot } = get();
    const linked = new Set(edges.flatMap((e) => [e.source, e.target]));
    const targets = nodes.filter(
      (n) => linked.has(n.id) && n.data.slot !== true && !!n.data.target && resolveState(n.data, snapshot) === 'running',
    );
    if (targets.length === 0) {
      if (Object.keys(get().probes).length > 0) set({ probes: {} });
      return;
    }
    if (probeBusy) return;
    probeBusy = true;
    try {
      const list = await ipc.networkProbe(
        targets.map((n) => ({
          kind: n.data.kind,
          target: n.data.target,
          adapter: n.data.kind === 'vbox' ? (n.data.net?.adapter ?? 2) : null,
        })),
      );
      const live = new Set(get().nodes.map((n) => probeKey(n.data.kind, n.data.target)));
      const probes: Record<string, NodeProbe> = {};
      for (const p of list) {
        const key = probeKey(p.kind, p.target);
        if (live.has(key)) probes[key] = p;
      }
      set({ probes });
    } catch {
      // Un fallo puntual (VirtualBox bloqueado, Docker arrancando…) conserva la última lectura.
    } finally {
      probeBusy = false;
    }
  },

  toggleTheme() {
    const theme: Theme = get().theme === 'dark' ? 'light' : 'dark';
    applyTheme(theme);
    set({ theme });
  },

  setLang(lang) {
    applyLang(lang);
    set({ lang });
  },

  toggleCanvasLock() {
    const canvasLocked = !get().canvasLocked;
    try { localStorage.setItem(CANVAS_LOCK_KEY, canvasLocked ? '1' : '0'); } catch { /* sin almacenamiento */ }
    set({ canvasLocked });
  },

  setOverSidebar: (overSidebar) => set({ overSidebar }),

  toast(kind, text) {
    const id = ++toastSeq;
    if (kind !== 'error') {
      // Éxito e información siguen desapareciendo solos.
      set((s) => ({
        toasts: [
          ...s.toasts.filter((t) => t.kind === 'error'),
          ...[...s.toasts.filter((t) => t.kind !== 'error'), { id, kind, text, count: 1 }].slice(-3),
        ],
      }));
      setTimeout(() => get().dismissToast(id), 3500);
      return;
    }

    // Los errores permanecen hasta que el usuario los cierre. Un mensaje
    // idéntico al último error no se apila: solo incrementa su contador.
    set((s) => {
      const errors = s.toasts.filter((t) => t.kind === 'error');
      const last = errors[errors.length - 1];
      if (last && last.text === text) {
        return { toasts: s.toasts.map((t) => (t.id === last.id ? { ...t, count: t.count + 1 } : t)) };
      }
      // Máximo 5 errores visibles: se descarta el más antiguo.
      const next = [...errors, { id, kind, text, count: 1 }].slice(-5);
      return { toasts: [...s.toasts.filter((t) => t.kind !== 'error'), ...next] };
    });
  },
  dismissToast: (id) => set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })),
}));

// El mock del navegador reutiliza los avisos del store.
setMockNotice((text) => useLab.getState().toast('info', text));
