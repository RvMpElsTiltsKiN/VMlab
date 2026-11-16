import { useCallback, useEffect, useMemo, useState } from 'react';
import { technicalText, useLocale, useT, type Locale, type Translate } from '../i18n';
import { groupLive, probeKey } from '../lib/connectivity';
import { ipc, isTauri } from '../lib/ipc';
import { charLength, ENV_KEY_RE, hasControlChars, RUN_LIMITS } from '../lib/labFile';
import { hasLinkConditions, LINK_LIMITS, sameLink } from '../lib/link';
import { resolveState, stateLabel } from '../lib/status';
import { computeGroups, NET_COLORS, uniqueHostSlug } from '../lib/topology';
import { useLab } from '../store/useLab';
import type {
  CaptureFilter,
  DockerPortProto,
  DockerRunOptions,
  GuestConfigRequest,
  GuestHostsRequest,
  GuestIpMode,
  GuestProfile,
  GuestReport,
  GuestSystem,
  LinkConditions,
  MachineData,
  MachineNode,
  NetMode,
  NodeNet,
} from '../types';
import { CameraIcon, TrashIcon } from './icons';

const DEFAULT_NET: NodeNet = { mode: 'intnet', adapter: 2 };

const NET_MODES_BASE: { value: NetMode; key: 'net.mode.intnet' | 'net.mode.nat' | 'net.mode.bridged' | 'net.mode.hostonly' | 'net.mode.natnetwork' | 'net.mode.none' }[] = [
  { value: 'intnet', key: 'net.mode.intnet' },
  { value: 'nat', key: 'net.mode.nat' },
  { value: 'bridged', key: 'net.mode.bridged' },
  { value: 'hostonly', key: 'net.mode.hostonly' },
  { value: 'natnetwork', key: 'net.mode.natnetwork' },
  { value: 'none', key: 'net.mode.none' },
];

/** Etiquetas del modo de red en el idioma activo (no se calculan al cargar el módulo). */
function netModes(t: Translate): { value: NetMode; label: string }[] {
  return NET_MODES_BASE.map(({ value, key }) => ({ value, label: t(key) }));
}

const CLIENT_SYSTEMS_BASE: { value: GuestSystem; key: 'guest.system.netplan' | 'guest.system.networkManager' | 'guest.system.systemdNetworkd' | 'guest.system.ifupdown' | 'guest.system.wicked' | 'guest.system.dhcpcd' | 'guest.system.connman' | 'guest.system.networkScripts' }[] = [
  { value: 'netplan', key: 'guest.system.netplan' },
  { value: 'networkManager', key: 'guest.system.networkManager' },
  { value: 'systemdNetworkd', key: 'guest.system.systemdNetworkd' },
  { value: 'ifupdown', key: 'guest.system.ifupdown' },
  { value: 'wicked', key: 'guest.system.wicked' },
  { value: 'dhcpcd', key: 'guest.system.dhcpcd' },
  { value: 'connman', key: 'guest.system.connman' },
  { value: 'networkScripts', key: 'guest.system.networkScripts' },
];

function clientSystems(t: Translate): { value: GuestSystem; label: string }[] {
  return CLIENT_SYSTEMS_BASE.map(({ value, key }) => ({ value, label: t(key) }));
}

interface LinkPreset {
  id: string;
  label: string;
  link?: LinkConditions;
}

/** Preajustes de condiciones de red; las etiquetas se traducen en cada render. */
function linkPresets(t: Translate): LinkPreset[] {
  return [
    { id: 'none', label: t('link.preset.none') },
    { id: 'lan', label: t('link.preset.lan'), link: { delayMs: 1, jitterMs: 0, lossTenths: 0 } },
    { id: 'wan', label: t('link.preset.wan'), link: { delayMs: 80, jitterMs: 0, lossTenths: 0 } },
    {
      id: '4g',
      label: t('link.preset.4g'),
      link: { delayMs: 60, jitterMs: 0, lossTenths: 10, rateKbit: 20_000 },
    },
    { id: 'sat', label: t('link.preset.sat'), link: { delayMs: 600, jitterMs: 0, lossTenths: 20 } },
    { id: 'bad', label: t('link.preset.bad'), link: { delayMs: 200, jitterMs: 0, lossTenths: 100 } },
  ];
}

const errText = (e: unknown) => (typeof e === 'string' ? e : e instanceof Error ? e.message : JSON.stringify(e));

/** Redondea y acota un valor numérico de un campo de red. */
function clampInt(value: number, min: number, max: number): number {
  if (!Number.isFinite(value)) return min;
  return Math.min(max, Math.max(min, Math.round(value)));
}

type CaptureFilterKind = 'all' | 'icmp' | 'tcp' | 'udp' | 'arp' | 'port';

const CAPTURE_FILTERS_BASE: { value: CaptureFilterKind; key: 'capture.filter.all' | 'capture.filter.icmp' | 'capture.filter.tcp' | 'capture.filter.udp' | 'capture.filter.arp' | 'capture.filter.port' }[] = [
  { value: 'all', key: 'capture.filter.all' },
  { value: 'icmp', key: 'capture.filter.icmp' },
  { value: 'tcp', key: 'capture.filter.tcp' },
  { value: 'udp', key: 'capture.filter.udp' },
  { value: 'arp', key: 'capture.filter.arp' },
  { value: 'port', key: 'capture.filter.port' },
];

function captureFilters(t: Translate): { value: CaptureFilterKind; label: string }[] {
  return CAPTURE_FILTERS_BASE.map(({ value, key }) => ({ value, label: t(key) }));
}

/** Tamaño legible con el separador decimal del idioma activo. */
function formatBytes(bytes: number, locale: Locale): string {
  const number = new Intl.NumberFormat(locale === 'es' ? 'es-ES' : 'en-US', {
    maximumFractionDigits: 1,
  });
  if (bytes < 1_024) return `${bytes} B`;
  if (bytes < 1_024 * 1_024) return `${number.format(bytes / 1_024)} KB`;
  return `${number.format(bytes / (1_024 * 1_024))} MB`;
}

function formatElapsed(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1_000));
  const hours = Math.floor(total / 3_600);
  const minutes = Math.floor((total % 3_600) / 60);
  const seconds = total % 60;
  const tail = `${String(minutes).padStart(2, '0')}:${String(seconds).padStart(2, '0')}`;
  return hours > 0 ? `${hours}:${tail}` : tail;
}

/** Hora de la captura en el idioma activo, no fijada a `es-ES`. */
function captureTime(iso: string, locale: Locale): string {
  const date = new Date(iso);
  return Number.isNaN(date.getTime())
    ? iso
    : date.toLocaleTimeString(locale === 'es' ? 'es-ES' : 'en-US');
}

function modeHint(mode: NetMode, t: Translate): string {
  switch (mode) {
    case 'intnet':
      return t('net.hint.intnet');
    case 'nat':
      return t('net.hint.nat');
    case 'bridged':
      return t('net.hint.bridged');
    case 'hostonly':
      return t('net.hint.hostonly');
    case 'natnetwork':
      return t('net.hint.natnetwork');
    case 'none':
      return t('net.hint.none');
  }
}

function SlotPanel({ node }: { node: MachineNode }) {
  const t = useT();
  const inventory = useLab((s) => s.inventory);
  const nodes = useLab((s) => s.nodes);
  const updateNode = useLab((s) => s.updateNode);
  const assigned = new Set(
    nodes
      .filter((n) => n.id !== node.id && n.data.kind === 'vbox' && !!n.data.target)
      .map((n) => n.data.target),
  );
  const available = (inventory?.vbox ?? []).filter((vm) => !assigned.has(vm.target));

  return (
    <div className="slot-box">
      <div className="side-title"><span>{t('slot.title')}</span></div>
      <label className="field">{t('slot.assign')}
        <select
          value=""
          disabled={!available.length}
          onChange={(e) => {
            const vm = available.find((v) => v.target === e.target.value);
            if (vm) updateNode(node.id, { target: vm.target, label: vm.label, slot: false });
          }}
        >
          <option value="">{available.length ? t('slot.choose') : t('slot.noneAvailable')}</option>
          {available.map((vm) => <option key={vm.target} value={vm.target}>{vm.label}</option>)}
        </select>
      </label>
      {!available.length && (
        <p className="hint">
          {inventory ? t('slot.allOnCanvas') : t('slot.readError')}
        </p>
      )}
      {available.length > 0 && <p className="hint">{t('slot.assignHint')}</p>}
    </div>
  );
}

function NetworkModePanel({ node, hasLinks }: { node: MachineNode; hasLinks: boolean }) {
  const t = useT();
  const modes = netModes(t);
  const updateNode = useLab((s) => s.updateNode);
  const net = node.data.net ?? DEFAULT_NET;
  const set = (patch: Partial<NodeNet>) => updateNode(node.id, { net: { ...net, ...patch } });

  return (
    <div className="net-mode">
      <div className="side-title"><span>{t('net.title', { adapter: net.adapter })}</span></div>
      <div className="grid2">
        <label className="field">{t('net.modeLabel')}
          <select value={net.mode} onChange={(e) => set({ mode: e.target.value as NetMode })}>
            {modes.map((m) => <option key={m.value} value={m.value}>{m.label}</option>)}
          </select>
        </label>
        <label className="field">{t('net.adapterLabel')}
          <select value={net.adapter} onChange={(e) => set({ adapter: Number(e.target.value) })}>
            {[1, 2, 3, 4].map((n) => <option key={n} value={n}>{t('net.nic', { n })}</option>)}
          </select>
        </label>
      </div>

      {net.adapter === 1 && (
        <p className="hint warn">{t('net.adapter1Warn')}</p>
      )}

      {net.mode === 'bridged' && (
        <label className="field">{t('net.bridge')}
          <input
            value={net.bridge ?? ''}
            onChange={(e) => set({ bridge: e.target.value })}
            placeholder={t('net.bridgePlaceholder')}
            spellCheck={false}
            className="mono"
          />
        </label>
      )}
      {net.mode === 'intnet' && (
        <label className="field">{t('net.intnetName')} <span className="muted">{t('net.optional')}</span>
          <input
            value={net.name ?? ''}
            onChange={(e) => set({ name: e.target.value })}
            placeholder={hasLinks ? t('net.intnetNamePlaceholder') : 'lab1'}
            spellCheck={false}
            className="mono"
          />
        </label>
      )}
      {(net.mode === 'hostonly' || net.mode === 'natnetwork') && (
        <label className="field">{net.mode === 'hostonly' ? t('net.hostonlyName') : t('net.natnetworkName')}
          <input
            value={net.name ?? ''}
            onChange={(e) => set({ name: e.target.value })}
            placeholder={net.mode === 'hostonly' ? 'vboxnet0' : 'NatNetwork'}
            spellCheck={false}
            className="mono"
          />
        </label>
      )}

      <p className="hint">{modeHint(net.mode, t)}</p>
      {net.mode !== 'intnet' && hasLinks && (
        <p className="hint warn">{t('net.modeWarn', { mode: modes.find((m) => m.value === net.mode)?.label ?? '' })}</p>
      )}
      {net.mode === 'intnet' && hasLinks && net.name && (
        <p className="hint">{t('net.intnetIsolated')}</p>
      )}
      <p className="hint">{t('net.applyHintBefore')} <b>{t('titlebar.apply')}</b>{t('net.applyHintAfter')}</p>
    </div>
  );
}

function LinkPanel({ node, state }: { node: MachineNode; state: string }) {
  const t = useT();
  const presets = linkPresets(t);
  const updateNode = useLab((s) => s.updateNode);
  const applyLink = useLab((s) => s.applyLink);
  const clearLink = useLab((s) => s.clearLink);
  const toast = useLab((s) => s.toast);
  const busy = useLab((s) => !!s.busy[node.id]);
  const labsnapBusy = useLab((s) => s.labsnapBusy);
  const [downloadOpen, setDownloadOpen] = useState(false);

  const isDocker = node.data.kind === 'docker';
  const link = node.data.link;
  const effective = hasLinkConditions(link);
  const disabled = busy || labsnapBusy;
  const running = state === 'running' || state === 'paused';
  const delayMs = link?.delayMs ?? 0;
  const jitterMs = link?.jitterMs ?? 0;
  const lossTenths = link?.lossTenths ?? 0;
  const rateKbit = link?.rateKbit;
  const preset = presets.find((p) => sameLink(p.link, link))?.id ?? 'custom';

  useEffect(() => setDownloadOpen(false), [node.id]);

  useEffect(() => {
    if (!downloadOpen) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && !busy) setDownloadOpen(false);
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [downloadOpen, busy]);

  const setLink = (patch: Partial<LinkConditions>) => {
    const next: LinkConditions = { delayMs, jitterMs, lossTenths, rateKbit, ...patch };
    next.delayMs = clampInt(next.delayMs, 0, LINK_LIMITS.delayMs);
    next.jitterMs = clampInt(next.jitterMs, 0, Math.min(next.delayMs, LINK_LIMITS.jitterMs));
    next.lossTenths = clampInt(next.lossTenths, 0, LINK_LIMITS.lossTenths);
    if (next.rateKbit === undefined || !Number.isFinite(next.rateKbit) || next.rateKbit <= 0) {
      delete next.rateKbit;
    } else {
      next.rateKbit = clampInt(next.rateKbit, LINK_LIMITS.rateKbitMin, LINK_LIMITS.rateKbitMax);
    }
    updateNode(node.id, { link: hasLinkConditions(next) ? next : undefined });
  };

  const apply = async () => {
    if (!effective || disabled) return;
    if (isDocker && running) {
      try {
        if (!(await ipc.linkImagePresent())) {
          setDownloadOpen(true);
          return;
        }
      } catch (e) {
        toast('error', technicalText(errText(e)));
        return;
      }
    }
    await applyLink(node.id);
  };

  const rateInput = (
    <label className="field">{t('link.rate')}
      <input
        type="number"
        min={LINK_LIMITS.rateKbitMin / 1000}
        max={LINK_LIMITS.rateKbitMax / 1000}
        step="any"
        value={rateKbit ? rateKbit / 1000 : ''}
        placeholder={t('link.ratePlaceholder')}
        disabled={disabled}
        onChange={(e) => setLink({ rateKbit: e.target.value === '' ? undefined : Number(e.target.value) * 1000 })}
      />
    </label>
  );

  return (
    <div className="link-box">
      <div className="side-title"><span>{t('link.title')}</span></div>
      <p className="hint">{t('link.intro')}</p>

      {isDocker ? (
        <>
          <label className="field">{t('link.preset')}
            <select
              value={preset}
              disabled={disabled}
              onChange={(e) => {
                const chosen = presets.find((p) => p.id === e.target.value);
                updateNode(node.id, { link: chosen?.link ? { ...chosen.link } : undefined });
              }}
            >
              {preset === 'custom' && <option value="custom" disabled>{t('link.preset.custom')}</option>}
              {presets.map((p) => <option key={p.id} value={p.id}>{p.label}</option>)}
            </select>
          </label>
          <div className="grid2">
            <label className="field">{t('link.delay')}
              <input
                type="number"
                min={0}
                max={LINK_LIMITS.delayMs}
                step={1}
                value={delayMs || ''}
                disabled={disabled}
                onChange={(e) => setLink({ delayMs: Number(e.target.value) })}
              />
            </label>
            <label className="field">{t('link.jitter')}
              <input
                type="number"
                min={0}
                max={delayMs}
                step={1}
                value={jitterMs || ''}
                disabled={disabled || delayMs === 0}
                onChange={(e) => setLink({ jitterMs: Number(e.target.value) })}
              />
            </label>
          </div>
          <div className="grid2">
            <label className="field">{t('link.loss')}
              <input
                type="number"
                min={0}
                max={100}
                step={0.1}
                value={lossTenths ? lossTenths / 10 : ''}
                disabled={disabled}
                onChange={(e) => setLink({ lossTenths: Number(e.target.value) * 10 })}
              />
            </label>
            {rateInput}
          </div>
        </>
      ) : (
        <>
          {rateInput}
          <p className="hint">{t('link.vboxOnlyRate')}</p>
        </>
      )}

      <div className="btn-grid">
        <button className="btn primary" disabled={disabled || !effective} onClick={() => void apply()}>
          {busy ? t('link.applying') : t('link.apply')}
        </button>
        <button className="btn" disabled={disabled} onClick={() => void clearLink(node.id)}>{t('link.remove')}</button>
      </div>

      {isDocker && (
        <>
          <p className="hint">{t('link.dockerLost')}</p>
          <p className="hint">{t('link.imageFirstBefore')} <span className="mono">nicolaka/netshoot</span>{t('link.imageFirstAfter')}</p>
          {!running && <p className="hint warn">{t('link.dockerStart')}</p>}
        </>
      )}

      {downloadOpen && (
        <div
          className="modal-backdrop"
          onMouseDown={(event) => {
            if (event.target === event.currentTarget && !busy) setDownloadOpen(false);
          }}
        >
          <div className="modal" role="dialog" aria-modal="true" aria-labelledby={`link-image-title-${node.id}`}>
            <h3 id={`link-image-title-${node.id}`}>{t('link.imageTitle')}</h3>
            <p className="hint">
              {t('link.imageBodyBefore')} <span className="mono">nicolaka/netshoot</span>{t('link.imageBodyAfter')}
            </p>
            <div className="modal-actions">
              <button
                className="btn primary"
                disabled={busy}
                onClick={() => {
                  setDownloadOpen(false);
                  void applyLink(node.id);
                }}
              >
                {t('link.imageApply')}
              </button>
              <button className="btn" disabled={busy} onClick={() => setDownloadOpen(false)}>{t('common.cancel')}</button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

function CapturePanel({ node, state }: { node: MachineNode; state: string }) {
  const t = useT();
  const locale = useLocale();
  const filters = captureFilters(t);
  const captures = useLab((s) => s.captures);
  const captureBusy = useLab((s) => s.captureBusy);
  const labsnapBusy = useLab((s) => s.labsnapBusy);
  const startCapture = useLab((s) => s.startCapture);
  const stopCapture = useLab((s) => s.stopCapture);
  const openCapture = useLab((s) => s.openCapture);
  const openCaptureFolder = useLab((s) => s.openCaptureFolder);
  const toast = useLab((s) => s.toast);

  const isDocker = node.data.kind === 'docker';
  const running = state === 'running';
  const [filterKind, setFilterKind] = useState<CaptureFilterKind>('all');
  const [port, setPort] = useState('80');
  const [imageOpen, setImageOpen] = useState(false);
  const [now, setNow] = useState(() => Date.now());

  const effectiveKind: CaptureFilterKind = isDocker ? filterKind : 'all';
  const portNumber = Number(port);
  const portValid =
    effectiveKind !== 'port' ||
    (port.trim() !== '' && Number.isInteger(portNumber) && portNumber >= 1 && portNumber <= 65_535);
  const filter: CaptureFilter =
    effectiveKind === 'port' ? { port: clampInt(portNumber, 1, 65_535) } : effectiveKind;

  const mine = captures.filter((c) => c.kind === node.data.kind && c.target === node.data.target);
  const active = mine.find((c) => c.running);
  const recent = mine.filter((c) => !c.running).slice(0, 5);

  useEffect(() => setImageOpen(false), [node.id]);

  useEffect(() => {
    if (!active) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, [active?.id]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    if (!imageOpen) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && !captureBusy) setImageOpen(false);
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [imageOpen, captureBusy]);

  const begin = async () => {
    if (!running || captureBusy || labsnapBusy) return;
    if (!portValid) {
      toast('error', t('capture.portError'));
      return;
    }
    if (isDocker) {
      try {
        if (!(await ipc.linkImagePresent())) {
          setImageOpen(true);
          return;
        }
      } catch (e) {
        toast('error', technicalText(errText(e)));
        return;
      }
    }
    await startCapture(node.id, filter);
  };

  const elapsed = active ? now - Date.parse(active.startedAt) : 0;

  return (
    <div className="capture-box">
      <div className="side-title"><span>{t('capture.title')}</span></div>
      <p className="hint">
        {t('capture.introBefore')} <span className="mono">.pcap</span> {t('capture.introAfter')}
      </p>

      {!running && (
        <p className="hint warn">
          {isDocker ? t('capture.startDocker') : t('capture.startVbox')}
        </p>
      )}

      <div className="grid2">
        <label className="field">{t('capture.filter')}
          <select
            value={effectiveKind}
            disabled={!isDocker || !running || captureBusy || !!active}
            onChange={(e) => setFilterKind(e.target.value as CaptureFilterKind)}
          >
            {isDocker ? (
              filters.map((f) => <option key={f.value} value={f.value}>{f.label}</option>)
            ) : (
              <option value="all">{t('capture.filter.all')}</option>
            )}
          </select>
        </label>
        {effectiveKind === 'port' && (
          <label className="field">{t('capture.port')}
            <input
              type="number"
              min={1}
              max={65_535}
              step={1}
              value={port}
              disabled={!running || captureBusy || !!active}
              onChange={(e) => setPort(e.target.value)}
            />
          </label>
        )}
      </div>
      {!isDocker && (
        <p className="hint">{t('capture.vboxNoBpf')}</p>
      )}

      {active ? (
        <div className="capture-live">
          <div className="capture-live-head">
            <span className="capture-rec"><i />REC</span>
            <span className="mono">{formatElapsed(elapsed)}</span>
            <span className="mono">{formatBytes(active.sizeBytes, locale)}</span>
          </div>
          <p className="hint mono capture-file" title={active.fileName}>{active.fileName}</p>
          {active.warning && <p className="hint warn">{active.warning}</p>}
          <div className="capture-actions">
            <button className="btn primary" disabled={captureBusy} onClick={() => void stopCapture(active.id, true)}>
              {t('capture.stopOpen')}
            </button>
            <button className="btn" disabled={captureBusy} onClick={() => void stopCapture(active.id, false)}>
              {captureBusy ? t('capture.stopping') : t('capture.stop')}
            </button>
          </div>
        </div>
      ) : (
        <button
          className="btn primary"
          disabled={!running || captureBusy || labsnapBusy || !portValid}
          onClick={() => void begin()}
        >
          {captureBusy ? t('capture.starting') : t('capture.start')}
        </button>
      )}

      {recent.length > 0 && (
        <div className="capture-recent">
          <div className="capture-recent-title">{t('capture.recent')}</div>
          <ul className="capture-list">
            {recent.map((c) => (
              <li key={c.id}>
                <span className="mono capture-file" title={c.fileName}>{c.fileName}</span>
                <div className="capture-recent-foot">
                  <span className="muted">{captureTime(c.startedAt, locale)} · {formatBytes(c.sizeBytes, locale)}</span>
                  <button className="btn" disabled={captureBusy} onClick={() => void openCapture(c.id)}>
                    {t('capture.open')}
                  </button>
                </div>
              </li>
            ))}
          </ul>
        </div>
      )}

      <button className="btn ghost capture-folder" onClick={() => void openCaptureFolder()}>
        {t('capture.openFolder')}
      </button>
      <p className="hint">
        {t('capture.needsWiresharkBefore')}{' '}
        <span className="mono">winget install WiresharkFoundation.Wireshark</span>{t('capture.needsWiresharkAfter')}
      </p>

      {imageOpen && (
        <div
          className="modal-backdrop"
          onMouseDown={(event) => {
            if (event.target === event.currentTarget && !captureBusy) setImageOpen(false);
          }}
        >
          <div className="modal" role="dialog" aria-modal="true" aria-labelledby={`capture-image-title-${node.id}`}>
            <h3 id={`capture-image-title-${node.id}`}>{t('capture.imageTitle')}</h3>
            <p className="hint">
              {t('capture.imageBodyBefore')} <span className="mono">nicolaka/netshoot</span>{' '}
              {t('capture.imageBodyMiddle')} <span className="mono">tcpdump</span>{t('capture.imageBodyAfter')}
            </p>
            <div className="modal-actions">
              <button
                className="btn primary"
                disabled={captureBusy}
                onClick={() => {
                  setImageOpen(false);
                  void startCapture(node.id, filter);
                }}
              >
                {t('capture.imageApply')}
              </button>
              <button className="btn" disabled={captureBusy} onClick={() => setImageOpen(false)}>{t('common.cancel')}</button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

function ScreenshotPanel({ node, state }: { node: MachineNode; state: string }) {
  const t = useT();
  const toast = useLab((s) => s.toast);
  const [busy, setBusy] = useState(false);
  const running = state === 'running';

  const openFolder = useCallback(() => {
    void ipc.openScreenshotsFolder().catch((e) => {
      toast('error', technicalText(t('screenshot.folderError', { detail: errText(e) })));
    });
  }, [t, toast]);

  const capture = useCallback(async () => {
    if (!running || busy) return;
    setBusy(true);
    try {
      const info = await ipc.takeScreenshot(node.data.target);
      const bytes = await ipc.readScreenshot(info.path);
      let copied = false;
      if (isTauri) {
        try {
          const { writeImage } = await import('@tauri-apps/plugin-clipboard-manager');
          await writeImage(bytes);
          copied = true;
        } catch {
          // La captura ya está guardada: se avisa de que no se pudo copiar.
        }
      }
      toast(
        'ok',
        copied ? t('screenshot.ok') : t('screenshot.okNoClipboard'),
        { label: t('screenshot.openFolder'), onClick: openFolder },
      );
    } catch (e) {
      toast('error', technicalText(t('screenshot.error', { detail: errText(e) })));
    } finally {
      setBusy(false);
    }
  }, [busy, node.data.target, openFolder, running, t, toast]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (!event.ctrlKey || !event.shiftKey || event.altKey || event.metaKey) return;
      if (event.repeat) return;
      if (event.key.toLowerCase() !== 'p') return;
      const target = event.target as HTMLElement | null;
      const tag = target?.tagName;
      if (target?.isContentEditable || tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return;
      if (!running || busy) return;
      event.preventDefault();
      void capture();
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [busy, capture, running]);

  return (
    <div className="screenshot-box">
      <button
        className="btn primary"
        disabled={!running || busy}
        title={running ? t('screenshot.shortcut') : t('screenshot.disabled')}
        onClick={() => void capture()}
      >
        <CameraIcon /> {busy ? t('screenshot.busy') : t('screenshot.button')}
      </button>
    </div>
  );
}

function NetworkInfoPanel({ node }: { node: MachineNode }) {
  const t = useT();
  const toast = useLab((s) => s.toast);
  const probes = useLab((s) => s.probes);
  const probe = probes[probeKey(node.data.kind, node.data.target)];

  const copy = async (text: string, message: string) => {
    try {
      await navigator.clipboard.writeText(text);
      toast('ok', message);
    } catch {
      toast('error', t('info.copyError'));
    }
  };

  return (
    <div className="net-info">
      <div className="side-title"><span>{t('info.title')}</span></div>
      <div className="kv">
        <span>{t('info.ip')}</span>
        <b className="mono">{probe?.ipv4 ?? t('info.unavailable')}</b>
        {probe?.ipv4 && (
          <button className="copy-mini" onClick={() => void copy(probe.ipv4!, t('info.ipCopied'))}>{t('common.copy')}</button>
        )}
      </div>
      <div className="kv">
        <span>{t('info.segment')}</span>
        <b className="mono">{probe?.segment ?? t('info.unavailable')}</b>
      </div>
      {node.data.kind === 'docker' && (
        <>
          <div className="kv">
            <span>{t('info.dnsName')}</span>
            <b className="mono">{node.data.target}</b>
            <button className="copy-mini" onClick={() => void copy(node.data.target, t('info.dnsCopied'))}>{t('common.copy')}</button>
          </div>
          <p className="hint">{t('info.dnsHint')}</p>
        </>
      )}
      {!probe?.ipv4 && <p className="hint">{t('info.noIp')}</p>}
    </div>
  );
}

function GuestPanel({ node, state }: { node: MachineNode; state: string }) {
  const t = useT();
  const updateNode = useLab((s) => s.updateNode);
  const toast = useLab((s) => s.toast);
  const labsnapBusy = useLab((s) => s.labsnapBusy);
  const nodes = useLab((s) => s.nodes);
  const edges = useLab((s) => s.edges);
  const probes = useLab((s) => s.probes);
  const [user, setUser] = useState(node.data.guestUser ?? '');
  const [password, setPassword] = useState('');
  const [profile, setProfile] = useState<GuestProfile | null>(null);
  const [detecting, setDetecting] = useState(false);
  const [applying, setApplying] = useState(false);
  const [hostsBusy, setHostsBusy] = useState(false);
  const [report, setReport] = useState<GuestReport | null>(null);
  const [hostsReport, setHostsReport] = useState<GuestReport | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Solo se reinicia al cambiar de nodo: no se borra la contraseña al escribir el usuario.
  useEffect(() => {
    setUser(node.data.guestUser ?? '');
    setPassword('');
    setProfile(null);
    setReport(null);
    setHostsReport(null);
    setError(null);
  }, [node.id]); // eslint-disable-line react-hooks/exhaustive-deps

  const hosts = useMemo(() => {
    const { groups, nodeGroup } = computeGroups(nodes, edges);
    const group = groups[nodeGroup.get(node.id) ?? -1];
    if (!group) return [];
    const self = probes[probeKey('vbox', node.data.target)];
    const used = new Set<string>();
    return group.nodeIds
      .filter((id) => id !== node.id)
      .map((id) => nodes.find((n) => n.id === id))
      .filter((n): n is MachineNode => !!n && n.data.kind === 'vbox' && n.data.slot !== true)
      .map((n) => ({ node: n, probe: probes[probeKey('vbox', n.data.target)] }))
      .filter((x) => !!x.probe?.ipv4 && (!self?.segment || x.probe.segment === self.segment))
      .map((x) => ({ name: uniqueHostSlug(x.node.data.label, used), ipv4: x.probe!.ipv4! }));
  }, [nodes, edges, probes, node.id, node.data.target]);

  const mode: GuestIpMode = node.data.guestMode ?? 'dhcp';
  const system: GuestSystem = node.data.guestSystem ?? 'auto';
  const iface = node.data.guestIface ?? '';
  const canProbe = state === 'running' && !!user && !!password && !detecting && !applying && !labsnapBusy;
  const canApply = state === 'running' && !!user && !!password && !!iface && !detecting && !applying && !labsnapBusy;
  const canApplyHosts = state === 'running' && !!user && !!password && !detecting && !applying && !hostsBusy && !labsnapBusy;

  const setData = (patch: Partial<MachineData>) => updateNode(node.id, patch);

  const systems = profile
    ? profile.systems.filter((s) => (mode === 'server' ? s.kind === 'server' : s.kind === 'client'))
    : [];
  const serverAvailable = !profile || profile.systems.some((s) => s.kind === 'server');

  const resolveSystem = (): GuestSystem => {
    if (mode === 'server') return 'dhcpd';
    if (system !== 'auto' && system !== 'dhcpd') return system;
    const recommended = profile?.systems.find((s) => s.kind === 'client' && s.recommended)
      ?? profile?.systems.find((s) => s.kind === 'client');
    return recommended?.system ?? 'auto';
  };

  const detect = async () => {
    if (!user || !password) return toast('error', t('guest.needCredentials'));
    setDetecting(true);
    setError(null);
    setReport(null);
    try {
      const p = await ipc.guestProbe(node.data.target, user, password);
      setProfile(p);
      const detectedIface = p.interfaces.some((i) => i.name === iface) ? iface : p.interfaces[0]?.name ?? '';
      const prefs = p.systems.filter((s) => s.kind === 'client');
      const detectedSystem = prefs.find((s) => s.recommended)?.system ?? prefs[0]?.system ?? 'auto';
      updateNode(node.id, { guestUser: user, guestIface: detectedIface, guestSystem: detectedSystem });
      if (!p.guestAdditions) setError(t('guest.noGa'));
    } catch (e) {
      setError(technicalText(errText(e)));
    } finally {
      setDetecting(false);
    }
  };

  const apply = async () => {
    if (!user || !password) return toast('error', t('guest.needCredentials'));
    if (!iface) return toast('error', t('guest.needIface'));
    const resolved = resolveSystem();
    if (mode === 'server' && resolved !== 'dhcpd') return toast('error', t('guest.needDhcpd'));

    setApplying(true);
    setError(null);
    setReport(null);
    try {
      const dns = (node.data.guestDns ?? '').split(/[\s,;]+/).filter(Boolean);
      const req: GuestConfigRequest = {
        target: node.data.target,
        username: user,
        password,
        system: resolved,
        interface: iface,
        ipMode: mode,
        address: node.data.guestAddress || undefined,
        prefix: node.data.guestPrefix ?? undefined,
        gateway: node.data.guestGateway || undefined,
        dns,
        subnet: node.data.guestSubnet || undefined,
        rangeStart: node.data.guestRangeStart || undefined,
        rangeEnd: node.data.guestRangeEnd || undefined,
      };
      const r = await ipc.guestApply(req);
      setReport(r);
      toast('ok', t('guest.applied', { system: r.system }));
    } catch (e) {
      setError(technicalText(errText(e)));
      toast('error', technicalText(errText(e)));
    } finally {
      setApplying(false);
    }
  };

  const applyHosts = async () => {
    if (!user || !password) return toast('error', t('guest.needCredentials'));
    setHostsBusy(true);
    setError(null);
    setHostsReport(null);
    try {
      const req: GuestHostsRequest = {
        target: node.data.target,
        username: user,
        password,
        entries: hosts.map((h) => ({ name: h.name, ipv4: h.ipv4 })),
      };
      const r = await ipc.guestHostsApply(req);
      setHostsReport(r);
      toast('ok', hosts.length ? t('guest.hostsApplied', { count: hosts.length }) : t('guest.hostsCleared'));
    } catch (e) {
      setError(technicalText(errText(e)));
      toast('error', technicalText(errText(e)));
    } finally {
      setHostsBusy(false);
    }
  };

  return (
    <div className="guest-box">
      <div className="side-title"><span>{t('guest.title')}</span></div>
      <p className="hint">{t('guest.introBefore')} <b>Guest Additions</b> {t('guest.introAfter')}</p>

      <div className="grid2">
        <label className="field">{t('guest.user')}
          <input value={user} onChange={(e) => setUser(e.target.value)} spellCheck={false} autoComplete="off" className="mono" />
        </label>
        <label className="field">{t('guest.password')}
          <input
            type="password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            autoComplete="off"
            placeholder={t('guest.passwordPlaceholder')}
          />
        </label>
      </div>

      <button className="btn" disabled={!canProbe} onClick={detect}>
        {detecting ? t('guest.detecting') : t('guest.detect')}
      </button>
      {state !== 'running' && <p className="hint warn">{t('guest.startToDetect')}</p>}

      {profile && (
        <div className="guest-detected">
          <p className="hint"><b>{profile.os}</b> · {profile.kernel}</p>
          <p className="hint">
            {profile.guestAdditions ? t('guest.gaOk') : t('guest.gaMissing')}
            {' · '}{profile.isRoot ? t('guest.root') : profile.sudo ? t('guest.sudo') : t('guest.noSudo')}
          </p>
          <p className="hint mono">
            {profile.systems.filter((s) => s.kind === 'client').map((s) => `${s.label}${s.active ? ' ●' : ''} → ${s.configPath}`).join('  |  ')}
          </p>
        </div>
      )}

      <div className="grid2">
        <label className="field">{t('guest.system')}
          <select
            value={mode === 'server' ? 'dhcpd' : system}
            onChange={(e) => setData({ guestSystem: e.target.value as GuestSystem })}
            disabled={mode === 'server'}
          >
            {mode === 'server' ? (
              <option value="dhcpd">{t('guest.dhcpd')}</option>
            ) : (
              <>
                <option value="auto">{t('guest.auto')}</option>
                {(systems.length ? systems : clientSystems(t).map((s) => ({ system: s.value, label: s.label }))).map((s) => (
                  <option key={s.system} value={s.system}>{s.label}</option>
                ))}
              </>
            )}
          </select>
        </label>
        <label className="field">{t('guest.iface')}
          <input
            list="vmlab-ifaces"
            value={iface}
            onChange={(e) => setData({ guestIface: e.target.value })}
            placeholder="eth0"
            spellCheck={false}
            className="mono"
          />
          <datalist id="vmlab-ifaces">
            {profile?.interfaces.map((i) => <option key={i.name} value={i.name}>{i.ipv4 ?? i.state ?? ''}</option>)}
          </datalist>
        </label>
      </div>

      <div className="seg" role="group" aria-label={t('guest.ipMode')}>
        <button className={mode === 'dhcp' ? 'on' : ''} onClick={() => setData({ guestMode: 'dhcp', guestSystem: system === 'dhcpd' ? 'auto' : system })}>{t('guest.dhcp')}</button>
        <button className={mode === 'static' ? 'on' : ''} onClick={() => setData({ guestMode: 'static', guestSystem: system === 'dhcpd' ? 'auto' : system })}>{t('guest.static')}</button>
        <button
          className={mode === 'server' ? 'on' : ''}
          disabled={!serverAvailable}
          title={serverAvailable ? t('guest.serverHint') : t('guest.serverUnavailable')}
          onClick={() => setData({ guestMode: 'server', guestSystem: 'dhcpd' })}
        >
          {t('guest.server')}
        </button>
      </div>

      {mode === 'static' && (
        <div className="grid3">
          <label className="field">IP
            <input value={node.data.guestAddress ?? ''} onChange={(e) => setData({ guestAddress: e.target.value })} placeholder="10.0.0.5" spellCheck={false} className="mono" />
          </label>
          <label className="field">{t('guest.prefix')}
            <input type="number" min={1} max={32} value={node.data.guestPrefix ?? 24} onChange={(e) => setData({ guestPrefix: Number(e.target.value) })} />
          </label>
          <label className="field">{t('guest.gateway')}
            <input value={node.data.guestGateway ?? ''} onChange={(e) => setData({ guestGateway: e.target.value })} placeholder="10.0.0.1" spellCheck={false} className="mono" />
          </label>
        </div>
      )}

      {mode === 'server' && (
        <>
          <div className="grid2">
            <label className="field">{t('guest.subnet')}
              <input value={node.data.guestSubnet ?? ''} onChange={(e) => setData({ guestSubnet: e.target.value })} placeholder="10.77.5.0/24" spellCheck={false} className="mono" />
            </label>
            <label className="field">{t('guest.gateway')}
              <input value={node.data.guestGateway ?? ''} onChange={(e) => setData({ guestGateway: e.target.value })} placeholder="10.77.5.1" spellCheck={false} className="mono" />
            </label>
          </div>
          <div className="grid2">
            <label className="field">{t('guest.rangeStart')}
              <input value={node.data.guestRangeStart ?? ''} onChange={(e) => setData({ guestRangeStart: e.target.value })} placeholder="10.77.5.100" spellCheck={false} className="mono" />
            </label>
            <label className="field">{t('guest.rangeEnd')}
              <input value={node.data.guestRangeEnd ?? ''} onChange={(e) => setData({ guestRangeEnd: e.target.value })} placeholder="10.77.5.200" spellCheck={false} className="mono" />
            </label>
          </div>
          <p className="hint warn">{t('guest.dhcpdWarnBefore')} <span className="mono">dhcpd.conf</span>{t('guest.dhcpdWarnAfter')}</p>
        </>
      )}

      {mode !== 'server' && (
        <label className="field">DNS <span className="muted">{t('guest.dnsHint')}</span>
          <input value={node.data.guestDns ?? ''} onChange={(e) => setData({ guestDns: e.target.value })} placeholder="1.1.1.1, 8.8.8.8" spellCheck={false} className="mono" />
        </label>
      )}

      <button className="btn primary" disabled={!canApply} onClick={apply}>
        {applying ? t('guest.writing') : t('guest.apply')}
      </button>

      {error && <p className="hint err">{error}</p>}
      {report && (
        <div className="report">
          <p className="hint"><b>{report.system}</b> · {t('guest.files', { count: report.files.length })}</p>
          {report.files.map((f) => <p key={f} className="hint mono">✓ {f}</p>)}
          {report.messages.map((m, i) => <p key={`m${i}`} className="hint">{m}</p>)}
          {report.warnings.map((w, i) => <p key={`w${i}`} className="hint warn">{w}</p>)}
        </div>
      )}

      <div className="hosts-box">
        <div className="side-title"><span>{t('guest.hostsTitle')}</span></div>
        <p className="hint">{t('guest.hostsHint')}</p>
        {hosts.length > 0 ? (
          <ul className="hosts-list">
            {hosts.map((h) => (
              <li key={h.name}>
                <span className="mono">{h.name}</span>
                <span className="arrow">→</span>
                <span className="mono">{h.ipv4}</span>
              </li>
            ))}
          </ul>
        ) : (
          <p className="hint">
            {t('guest.hostsEmptyBefore')} <span className="mono">vmlab-hosts</span>{t('guest.hostsEmptyAfter')}
          </p>
        )}
        <p className="hint warn">{t('guest.hostsWarn')}</p>
        {state !== 'running' && <p className="hint warn">{t('guest.hostsStartBefore')} <span className="mono">/etc/hosts</span>{t('guest.hostsStartAfter')}</p>}
        <button className="btn" disabled={!canApplyHosts} onClick={applyHosts}>
          {hostsBusy ? t('guest.writing') : t('guest.hostsApply')}
        </button>
        {hostsReport && (
          <div className="report">
            {hostsReport.messages.map((m, i) => <p key={`hm${i}`} className="hint">{m}</p>)}
            {hostsReport.warnings.map((w, i) => <p key={`hw${i}`} className="hint warn">{w}</p>)}
          </div>
        )}
      </div>
    </div>
  );
}

function TerminalPanel({ node, state }: { node: MachineNode; state: string }) {
  const t = useT();
  const openTerminal = useLab((s) => s.openTerminal);
  const updateNode = useLab((s) => s.updateNode);
  const probes = useLab((s) => s.probes);
  const labsnapBusy = useLab((s) => s.labsnapBusy);
  const isDocker = node.data.kind === 'docker';
  const ipv4 = isDocker ? null : probes[probeKey('vbox', node.data.target)]?.ipv4 ?? null;
  const running = state === 'running';

  return (
    <div className="terminal-box">
      <div className="side-title"><span>{t('terminal.title')}</span></div>
      {isDocker ? (
        <>
          <p className="hint">{t('terminal.dockerHintBefore')} <span className="mono">docker exec -it</span>{t('terminal.dockerHintAfter')}</p>
          <button className="btn" disabled={!running || labsnapBusy} onClick={() => openTerminal(node.id)}>{t('terminal.open')}</button>
          {!running && <p className="hint warn">{t('terminal.dockerStart')}</p>}
        </>
      ) : (
        <>
          <label className="field">{t('guest.user')}
            <input
              value={node.data.guestUser ?? ''}
              onChange={(e) => updateNode(node.id, { guestUser: e.target.value })}
              placeholder="ubuntu"
              spellCheck={false}
              autoComplete="off"
              className="mono"
            />
          </label>
          <p className="hint">{t('terminal.ipDetected')} {ipv4 ? <span className="mono">{ipv4}</span> : <b>{t('info.unavailable')}</b>}</p>
          {!ipv4 && <p className="hint warn">{t('terminal.noIp')}</p>}
          <button className="btn" disabled={!running || labsnapBusy} onClick={() => openTerminal(node.id)}>{t('terminal.openSsh')}</button>
          {!running && <p className="hint warn">{t('terminal.vmStart')}</p>}
          <p className="hint">{t('terminal.sshHint')}</p>
        </>
      )}
    </div>
  );
}

function NetworkSummary() {
  const t = useT();
  const nodes = useLab((s) => s.nodes);
  const edges = useLab((s) => s.edges);
  const report = useLab((s) => s.report);
  const probes = useLab((s) => s.probes);
  const snapshot = useLab((s) => s.snapshot);
  const { groups } = useMemo(() => computeGroups(nodes, edges), [nodes, edges]);
  const byId = new Map(nodes.map((n) => [n.id, n]));

  return (
    <>
      <div className="side-title"><span>{t('summary.title')}</span></div>
      {groups.length === 0 && <p className="hint">{t('summary.empty')}</p>}
      <ul className="net-list">
        {groups.map((g) => {
          const members = g.nodeIds.map((id) => byId.get(id)).filter((n): n is MachineNode => !!n);
          const off = members.filter((n) => n.data.kind === 'vbox' && (n.data.net?.mode ?? 'intnet') !== 'intnet');
          const live = groupLive(members, probes, snapshot);
          const notes = [
            ...new Set(
              members.flatMap((n) => {
                const note = probes[probeKey(n.data.kind, n.data.target)]?.note;
                return note ? [note] : [];
              }),
            ),
          ];
          const allRunning = members.every((n) => resolveState(n.data, snapshot) === 'running');
          const segments = new Set(
            members.map((n) => probes[probeKey(n.data.kind, n.data.target)]?.segment).filter(Boolean),
          );
          const reason = !allRunning
            ? t('summary.reasonOff')
            : segments.size > 1
              ? t('summary.reasonSegments')
              : t('summary.reasonNoIp');
          return (
            <li key={g.index}>
              <div className="net-name"><i style={{ background: NET_COLORS[g.index % NET_COLORS.length] }} />{t('summary.network', { index: g.index + 1 })}<small>{t('summary.nodes', { count: g.nodeIds.length })}</small></div>
              <div className="net-members">{members.map((n) => n.data.label).join(' · ')}</div>
              <span className={`net-live ${live ? 'ok' : 'bad'}`}><i />{live ? t('summary.live') : t('summary.noLive')}</span>
              {!live && <div className="hint">{notes[0] ?? reason}</div>}
              {g.mixed && <div className="hint warn">{t('summary.mixed')}</div>}
              {off.length > 0 && <div className="hint warn">{t('summary.adapterOutside', { names: off.map((n) => n.data.label).join(', ') })}</div>}
            </li>
          );
        })}
      </ul>
      {report && (
        <div className="report">
          <div className="side-title"><span>{t('summary.lastApply')}</span></div>
          <p className="hint">{t('summary.applied', { applied: report.applied, networks: report.networks.length })}</p>
          {report.warnings.map((w, i) => <p key={`w${i}`} className="hint warn">{w}</p>)}
          {report.errors.map((e, i) => <p key={`e${i}`} className="hint err">{technicalText(e)}</p>)}
        </div>
      )}
      <p className="hint foot">{t('summary.legend')}</p>
      <p className="hint foot">{t('summary.selectHint')}</p>
    </>
  );
}

interface EnvRow {
  key: string;
  value: string;
}

interface PortRow {
  host: string;
  container: string;
  proto: DockerPortProto;
}

const PORT_MAX = 65_535;

function envRowsOf(run: DockerRunOptions | undefined): EnvRow[] {
  return (run?.env ?? []).map((item) => ({ key: item.key, value: item.value }));
}

function portRowsOf(run: DockerRunOptions | undefined): PortRow[] {
  return (run?.ports ?? []).map((item) => ({
    host: String(item.host),
    container: String(item.container),
    proto: item.proto,
  }));
}

function validPortText(value: string): boolean {
  if (!/^\d+$/.test(value.trim())) return false;
  const port = Number(value);
  return Number.isInteger(port) && port >= 1 && port <= PORT_MAX;
}

/** Solo guarda las filas válidas: el backend rechazaría el resto. */
function runFromRows(env: EnvRow[], ports: PortRow[]): DockerRunOptions | undefined {
  const envOut: DockerRunOptions['env'] = [];
  const keys = new Set<string>();
  for (const row of env) {
    if (envOut.length >= RUN_LIMITS.env) break;
    if (!ENV_KEY_RE.test(row.key) || keys.has(row.key)) continue;
    if (charLength(row.value) > RUN_LIMITS.valueLength || hasControlChars(row.value)) continue;
    keys.add(row.key);
    envOut.push({ key: row.key, value: row.value });
  }

  const portsOut: DockerRunOptions['ports'] = [];
  const published = new Set<string>();
  for (const row of ports) {
    if (portsOut.length >= RUN_LIMITS.ports) break;
    if (!validPortText(row.host) || !validPortText(row.container)) continue;
    const host = Number(row.host);
    const key = `${host}/${row.proto}`;
    if (published.has(key)) continue;
    published.add(key);
    portsOut.push({ host, container: Number(row.container), proto: row.proto });
  }

  return envOut.length || portsOut.length ? { env: envOut, ports: portsOut } : undefined;
}

function RunOptionsPanel({ node }: { node: MachineNode }) {
  const t = useT();
  const updateNode = useLab((s) => s.updateNode);
  const [env, setEnv] = useState<EnvRow[]>(() => envRowsOf(node.data.run));
  const [ports, setPorts] = useState<PortRow[]>(() => portRowsOf(node.data.run));

  useEffect(() => {
    setEnv(envRowsOf(node.data.run));
    setPorts(portRowsOf(node.data.run));
  }, [node.id]); // eslint-disable-line react-hooks/exhaustive-deps

  const commit = (nextEnv: EnvRow[], nextPorts: PortRow[]) => {
    setEnv(nextEnv);
    setPorts(nextPorts);
    updateNode(node.id, { run: runFromRows(nextEnv, nextPorts) });
  };

  const patchEnv = (index: number, patch: Partial<EnvRow>) =>
    commit(env.map((row, i) => (i === index ? { ...row, ...patch } : row)), ports);
  const patchPort = (index: number, patch: Partial<PortRow>) =>
    commit(env, ports.map((row, i) => (i === index ? { ...row, ...patch } : row)));

  const keyCounts = new Map<string, number>();
  for (const row of env) {
    if (row.key) keyCounts.set(row.key, (keyCounts.get(row.key) ?? 0) + 1);
  }
  const portCounts = new Map<string, number>();
  for (const row of ports) {
    if (!validPortText(row.host)) continue;
    const key = `${Number(row.host)}/${row.proto}`;
    portCounts.set(key, (portCounts.get(key) ?? 0) + 1);
  }

  return (
    <div className="run-box">
      <div className="side-title"><span>{t('run.title')}</span></div>
      <p className="hint">
        {t('run.introBefore')} <span className="mono">docker rm</span> {t('run.introMiddle')}{' '}
        <span className="mono">127.0.0.1</span> {t('run.introAfter')}
      </p>

      <div className="run-subtitle">{t('run.env')}</div>
      <div className="run-rows">
        {env.map((row, index) => {
          const errors: string[] = [];
          if (!row.key) errors.push(t('run.errKeyRequired'));
          else if (!ENV_KEY_RE.test(row.key)) errors.push(t('run.errKeyInvalid'));
          else if ((keyCounts.get(row.key) ?? 0) > 1) errors.push(t('run.errKeyDup'));
          if (charLength(row.value) > RUN_LIMITS.valueLength) {
            errors.push(t('run.errValueLong', { max: RUN_LIMITS.valueLength }));
          }
          if (hasControlChars(row.value)) errors.push(t('run.errValueControl'));
          return (
            <div className={errors.length ? 'run-row has-error' : 'run-row'} key={`env-${index}`}>
              <div className="run-fields">
                <input
                  aria-label={t('run.envKeyAria')}
                  value={row.key}
                  maxLength={RUN_LIMITS.keyLength}
                  placeholder={t('run.envKeyPlaceholder')}
                  spellCheck={false}
                  className="mono"
                  onChange={(e) => patchEnv(index, { key: e.target.value })}
                />
                <input
                  aria-label={t('run.envValueAria')}
                  value={row.value}
                  placeholder={t('run.envValuePlaceholder')}
                  spellCheck={false}
                  className="mono"
                  onChange={(e) => patchEnv(index, { value: e.target.value })}
                />
                <button
                  type="button"
                  className="run-remove"
                  title={t('run.removeEnv')}
                  aria-label={t('run.removeEnv')}
                  onClick={() => commit(env.filter((_, i) => i !== index), ports)}
                >
                  <TrashIcon />
                </button>
              </div>
              {errors.length > 0 && <p className="hint err">{errors.join(' · ')}</p>}
            </div>
          );
        })}
      </div>
      <button
        className="btn"
        disabled={env.length >= RUN_LIMITS.env}
        onClick={() => commit([...env, { key: '', value: '' }], ports)}
      >
        {t('run.addEnv')}
      </button>
      <p className="hint">{t('run.envHint', { max: RUN_LIMITS.env })}</p>

      <div className="run-subtitle">{t('run.ports')}</div>
      <div className="run-rows">
        {ports.map((row, index) => {
          const errors: string[] = [];
          if (!row.host) errors.push(t('run.errHostRequired'));
          else if (!validPortText(row.host)) errors.push(t('run.errHostInvalid'));
          if (!row.container) errors.push(t('run.errContainerRequired'));
          else if (!validPortText(row.container)) errors.push(t('run.errContainerInvalid'));
          if (validPortText(row.host) && (portCounts.get(`${Number(row.host)}/${row.proto}`) ?? 0) > 1) {
            errors.push(t('run.errPortDup', { port: row.host, proto: row.proto }));
          }
          return (
            <div className={errors.length ? 'run-row has-error' : 'run-row'} key={`port-${index}`}>
              <div className="run-fields port">
                <input
                  aria-label={t('run.hostAria')}
                  type="number"
                  min={1}
                  max={PORT_MAX}
                  value={row.host}
                  placeholder="8080"
                  onChange={(e) => patchPort(index, { host: e.target.value })}
                />
                <span className="run-arrow" aria-hidden="true">→</span>
                <input
                  aria-label={t('run.containerAria')}
                  type="number"
                  min={1}
                  max={PORT_MAX}
                  value={row.container}
                  placeholder="80"
                  onChange={(e) => patchPort(index, { container: e.target.value })}
                />
                <select
                  aria-label={t('run.protoAria')}
                  value={row.proto}
                  onChange={(e) => patchPort(index, { proto: e.target.value as DockerPortProto })}
                >
                  <option value="tcp">TCP</option>
                  <option value="udp">UDP</option>
                </select>
                <button
                  type="button"
                  className="run-remove"
                  title={t('run.removePort')}
                  aria-label={t('run.removePort')}
                  onClick={() => commit(env, ports.filter((_, i) => i !== index))}
                >
                  <TrashIcon />
                </button>
              </div>
              {errors.length > 0 && <p className="hint err">{errors.join(' · ')}</p>}
            </div>
          );
        })}
      </div>
      <button
        className="btn"
        disabled={ports.length >= RUN_LIMITS.ports}
        onClick={() => commit(env, [...ports, { host: '', container: '', proto: 'tcp' }])}
      >
        {t('run.addPort')}
      </button>
      <p className="hint">{t('run.portsHint', { max: RUN_LIMITS.ports })}</p>
    </div>
  );
}

export function Inspector() {
  const t = useT();
  const node = useLab((s) => s.nodes.find((n) => n.selected));
  const state = useLab((s) => (node ? resolveState(node.data, s.snapshot) : 'unknown'));
  const busy = useLab((s) => (node ? !!s.busy[node.id] : false));
  const labsnapBusy = useLab((s) => s.labsnapBusy);
  const hasLinks = useLab((s) => (node ? s.edges.some((e) => e.source === node.id || e.target === node.id) : false));
  const { updateNode, removeNode, act } = useLab.getState();
  const isSlot = node?.data.kind === 'vbox' && node.data.slot === true;

  return (
    <aside className="inspector">
      {!node ? (
        <NetworkSummary />
      ) : (
        <>
          <div className="side-title"><span>{node.data.kind === 'vbox' ? t('insp.vm') : t('insp.container')}</span></div>
          {isSlot && <SlotPanel node={node} />}
          <label className="field">{t('insp.label')}
            <input value={node.data.label} onChange={(e) => updateNode(node.id, { label: e.target.value })} />
          </label>
          {!isSlot && (
            <label className="field">{node.data.kind === 'vbox' ? t('insp.uuid') : t('insp.containerName')}
              <input value={node.data.target} readOnly className="mono" />
            </label>
          )}
          {node.data.kind === 'docker' && (
            <label className="field">{t('insp.image')}
              <input value={node.data.image ?? ''} onChange={(e) => updateNode(node.id, { image: e.target.value })} spellCheck={false} className="mono" />
            </label>
          )}
          <div className="status big" data-state={isSlot ? 'missing' : busy ? 'busy' : state}>
            <i className="dot" />{isSlot ? t('node.unassigned') : busy ? t('node.working') : stateLabel(state, t)}
          </div>

          {!isSlot && <NetworkInfoPanel node={node} />}

          <div className="btn-grid">
            <button className="btn" disabled={busy || labsnapBusy || isSlot} onClick={() => act(node.id, 'start')}>{t('action.start')}</button>
            {node.data.kind === 'vbox' && <button className="btn" disabled={busy || labsnapBusy || isSlot} onClick={() => act(node.id, 'startGui')}>{t('action.startGui')}</button>}
            <button className="btn" disabled={busy || labsnapBusy || isSlot} onClick={() => act(node.id, 'stop')}>{t('action.stop')}</button>
            <button className="btn" disabled={busy || labsnapBusy || isSlot} onClick={() => act(node.id, 'restart')}>{t('action.restart')}</button>
            <button className="btn" disabled={busy || labsnapBusy || isSlot} onClick={() => act(node.id, state === 'paused' ? 'resume' : 'pause')}>{state === 'paused' ? t('action.resume') : t('action.pause')}</button>
            <button className="btn danger" disabled={busy || labsnapBusy || isSlot} onClick={() => act(node.id, 'forceStop')}>{t('action.forceStop')}</button>
          </div>

          {node.data.kind === 'vbox' && <NetworkModePanel node={node} hasLinks={hasLinks} />}

          {node.data.kind === 'docker' && <RunOptionsPanel node={node} />}

          {!isSlot && <LinkPanel node={node} state={state} />}

          {!isSlot && <CapturePanel node={node} state={state} />}

          {node.data.kind === 'vbox' && !isSlot && <ScreenshotPanel node={node} state={state} />}

          {node.data.kind === 'vbox' && !isSlot && <GuestPanel node={node} state={state} />}

          {!isSlot && <TerminalPanel node={node} state={state} />}

          <button className="btn ghost danger-text" disabled={labsnapBusy} onClick={() => removeNode(node.id)}><TrashIcon /> {t('insp.remove')}</button>
          <p className="hint">{t('insp.removeHint')}</p>
        </>
      )}
    </aside>
  );
}
