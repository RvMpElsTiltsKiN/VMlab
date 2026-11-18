import { useEffect, useState, type DragEvent } from 'react';
import { technicalText, useT } from '../i18n';
import { ipc } from '../lib/ipc';
import { stateLabel } from '../lib/status';
import { useLab } from '../store/useLab';
import type { GuestAdditionsPrep, MachineInfo, PaletteItem } from '../types';
import { DRAG_MIME } from './Canvas';
import { NewVmDialog } from './NewVmDialog';
import { CheckIcon, ContainerIcon, CubeIcon, DiskIcon, PlayIcon, PlusIcon, RefreshIcon, WarnIcon } from './icons';

const NAME_RE = /^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$/;
const IMAGE_RE = /^[A-Za-z0-9][A-Za-z0-9._/:@-]{0,199}$/;
const errText = (e: unknown) => (typeof e === 'string' ? e : e instanceof Error ? e.message : JSON.stringify(e));

function GuestAdditions({ m }: { m: MachineInfo }) {
  const t = useT();
  const refresh = useLab((s) => s.refreshInventory);
  const toast = useLab((s) => s.toast);
  const labsnapBusy = useLab((s) => s.labsnapBusy);
  const [busy, setBusy] = useState(false);
  const [prep, setPrep] = useState<GuestAdditionsPrep | null>(null);
  const [error, setError] = useState<string | null>(null);
  const version = m.guestAdditions?.version ?? null;
  const known = m.guestAdditions?.known ?? false;

  // Al detectar una versión nueva (tras instalar y comprobar) se oculta la guía.
  useEffect(() => {
    if (version) {
      setPrep(null);
      setError(null);
    }
  }, [version]);

  const prepare = async () => {
    if (labsnapBusy) return;
    setBusy(true);
    setError(null);
    setPrep(null);
    try {
      const p = await ipc.prepareGuestAdditions(m.target);
      setPrep(p);
      toast('ok', t('ga.prepared'));
    } catch (e) {
      const text = technicalText(errText(e));
      setError(text);
      toast('error', text);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="ga-box">
      {version ? (
        <span className="ga-chip ok" title={t('ga.installedHint')}>
          <CheckIcon /> Guest Additions {version}
        </span>
      ) : known ? (
        <button
          className="ga-chip missing"
          onClick={prepare}
          onPointerDown={(e) => e.stopPropagation()}
          draggable={false}
          disabled={busy || labsnapBusy}
          title={t('ga.installHint')}
        >
          <WarnIcon /> {busy ? t('ga.preparing') : t('ga.install')}
        </button>
      ) : (
        <span className="ga-chip unknown" title={t('ga.unknownHint')}>
          <WarnIcon /> {t('ga.noData')}
        </span>
      )}

      {prep && (
        <div className="ga-guide" onPointerDown={(e) => e.stopPropagation()}>
          <p>
            <b>{prep.attached ? t('ga.isoMounted') : t('ga.isoNotMounted')}</b>
            {prep.started ? t('ga.vmOpen') : prep.alreadyRunning ? t('ga.vmRunning') : ''}
          </p>
          {prep.os === 'windows' ? (
            <p>
              {t('ga.windowsBefore')} <b>VBoxWindowsAdditions.exe</b> {t('ga.windowsAfter')}
            </p>
          ) : prep.os === 'linux' ? (
            <>
              <p>{t('ga.linux')}</p>
              <code>sudo mount /dev/cdrom /mnt</code>
              <code>sudo /mnt/VBoxLinuxAdditions.run</code>
              <p>
                {t('ga.linuxCompilerBefore')} <b>build-essential</b> {t('ga.linuxCompilerAfter')}
              </p>
            </>
          ) : (
            <p>{t('ga.other')}</p>
          )}
          {prep.warnings.map((w, i) => <p key={i} className="hint warn">{w}</p>)}
          <button className="btn" onClick={() => { refresh(); toast('info', t('ga.rechecking')); }}>
            {t('ga.recheck')}
          </button>
        </div>
      )}
      {error && <p className="hint err">{error}</p>}
    </div>
  );
}

function Item({ m, used }: { m: MachineInfo; used: boolean }) {
  const t = useT();
  const addMachine = useLab((s) => s.addMachine);
  const item: PaletteItem = { kind: m.kind, label: m.label, target: m.target, image: m.image ?? undefined };
  const fromControl = (target: EventTarget | null) =>
    target instanceof HTMLElement && !!target.closest('button, .ga-box');
  const onDragStart = (e: DragEvent) => {
    if (fromControl(e.target)) {
      e.preventDefault();
      return;
    }
    const payload = JSON.stringify(item);
    e.dataTransfer.setData(DRAG_MIME, payload);
    // Respaldo para WebViews que no conservan tipos MIME personalizados.
    e.dataTransfer.setData('text/plain', `vmlab:${payload}`);
    e.dataTransfer.effectAllowed = 'copy';
  };
  return (
    <li className={`pal-item ${m.kind} ${used ? 'used' : ''}`} draggable onDragStart={onDragStart}
      onDoubleClick={(e) => { if (!fromControl(e.target)) addMachine(item); }}
      title={t('sidebar.dragItemHint')}>
      <span className="pal-icon">{m.kind === 'vbox' ? <CubeIcon /> : <ContainerIcon />}</span>
      <span className="pal-text">
        <span className="pal-name">{m.label}</span>
        <span className="pal-sub">{m.image ?? m.target.slice(0, 13)}</span>
      </span>
      <span className="status" data-state={m.state} title={stateLabel(m.state, t)}><i className="dot" /></span>
      {m.kind === 'vbox' && <GuestAdditions m={m} />}
    </li>
  );
}

/** Paleta de discos: se arrastra al lienzo o se añade con doble clic. */
function DiskItem() {
  const t = useT();
  const addMachine = useLab((s) => s.addMachine);
  const item: PaletteItem = { kind: 'disk', label: t('sidebar.disk'), target: '' };
  const onDragStart = (e: DragEvent) => {
    const payload = JSON.stringify(item);
    e.dataTransfer.setData(DRAG_MIME, payload);
    e.dataTransfer.setData('text/plain', `vmlab:${payload}`);
    e.dataTransfer.effectAllowed = 'copy';
  };

  return (
    <li className="pal-item disk" draggable onDragStart={onDragStart}
      onDoubleClick={() => addMachine(item)} title={t('sidebar.dragItemHint')}>
      <span className="pal-icon"><DiskIcon /></span>
      <span className="pal-text">
        <span className="pal-name">{t('sidebar.disk')}</span>
        <span className="pal-sub">{t('sidebar.diskHint')}</span>
      </span>
    </li>
  );
}

export function Sidebar() {
  const t = useT();
  const tools = useLab((s) => s.tools);
  const inventory = useLab((s) => s.inventory);
  const nodes = useLab((s) => s.nodes);
  const refresh = useLab((s) => s.refreshInventory);
  const addMachine = useLab((s) => s.addMachine);
  const toast = useLab((s) => s.toast);
  const dockerStarting = useLab((s) => s.dockerStarting);
  const startDocker = useLab((s) => s.startDocker);
  const overSidebar = useLab((s) => s.overSidebar);
  const [name, setName] = useState('');
  const [image, setImage] = useState('');
  const [newVmOpen, setNewVmOpen] = useState(false);

  const used = (m: MachineInfo) => nodes.some((n) => n.data.kind === m.kind && n.data.target === m.target);

  const addContainer = () => {
    if (!NAME_RE.test(name)) return toast('error', t('sidebar.invalidName'));
    if (!IMAGE_RE.test(image)) return toast('error', t('sidebar.invalidImage'));
    addMachine({ kind: 'docker', label: name, target: name, image });
    setName('');
    setImage('');
  };

  return (
    <aside className={`sidebar ${overSidebar ? 'drop-active' : ''}`}>
      {overSidebar && <div className="sidebar-drop">{t('sidebar.drop')}</div>}
      <div className="side-section">
        <div className="side-title">
          <span>{t('sidebar.tools')}</span>
          <button className="icon-btn sm" onClick={refresh} title={t('sidebar.refreshHint')}><RefreshIcon /></button>
        </div>
        <div className="pills">
          <span className={`pill ${tools?.vboxmanage ? 'ok' : 'bad'}`}>VirtualBox</span>
          <span className={`pill ${tools?.docker && !inventory?.dockerError ? 'ok' : 'bad'}`}>
            Docker{dockerStarting ? t('sidebar.dockerStartingSuffix') : ''}
          </span>
        </div>
      </div>

      <div className="side-scroll">
        <div className="side-section">
          <div className="side-title"><span>{t('sidebar.vms')}</span></div>
          <span className="new-vm-btn-wrap" title={tools?.vboxmanage ? t('newvm.button') : t('newvm.disabledHint')}>
            <button
              className="btn new-vm-btn"
              onClick={() => setNewVmOpen(true)}
              disabled={!tools?.vboxmanage}
              aria-label={t('newvm.button')}
            >
              <PlusIcon /> {t('newvm.button')}
            </button>
          </span>
          {inventory?.vboxError && <p className="hint err">{inventory.vboxError}</p>}
          <ul className="pal-list">
            {inventory?.vbox.map((m) => <Item key={m.target} m={m} used={used(m)} />)}
          </ul>
          {inventory && !inventory.vboxError && inventory.vbox.length === 0 && <p className="hint">{t('sidebar.noVms')}</p>}
          <p className="hint">{t('sidebar.dragHint')}</p>
        </div>

        <div className="side-section">
          <div className="side-title"><span>{t('sidebar.containers')}</span></div>
          {inventory?.dockerError && (
            <div className="docker-alert">
              <p className="hint err">{inventory.dockerError}</p>
              {inventory.dockerEngineOff && (
                <button className="btn primary docker-start" onClick={() => void startDocker()} disabled={dockerStarting}>
                  <PlayIcon /> {dockerStarting ? t('sidebar.startingDocker') : t('sidebar.startDocker')}
                </button>
              )}
              {dockerStarting && (
                <p className="hint">{t('sidebar.dockerStartingHint')}</p>
              )}
              {inventory.dockerErrorDetail && (
                <details className="docker-detail">
                  <summary>{t('sidebar.details')}</summary>
                  <code>{inventory.dockerErrorDetail}</code>
                </details>
              )}
            </div>
          )}
          <ul className="pal-list">
            {inventory?.docker.map((m) => <Item key={m.target} m={m} used={used(m)} />)}
          </ul>
        </div>

        <div className="side-section">
          <div className="side-title"><span>{t('sidebar.newContainer')}</span></div>
          <div className="form">
            <input placeholder={t('sidebar.namePlaceholder')} value={name} onChange={(e) => setName(e.target.value)} spellCheck={false} />
            <input placeholder={t('sidebar.imagePlaceholder')} value={image} onChange={(e) => setImage(e.target.value)} spellCheck={false}
              onKeyDown={(e) => e.key === 'Enter' && addContainer()} />
            <button className="btn" onClick={addContainer}><PlusIcon /> {t('sidebar.add')}</button>
          </div>
          <p className="hint">{t('sidebar.runHintStart')} <code>docker run</code> {t('sidebar.runHintEnd')}</p>
        </div>

        <div className="side-section">
          <div className="side-title"><span>{t('sidebar.disks')}</span></div>
          <ul className="pal-list"><DiskItem /></ul>
        </div>
      </div>
      {newVmOpen && <NewVmDialog onClose={() => setNewVmOpen(false)} />}
    </aside>
  );
}
