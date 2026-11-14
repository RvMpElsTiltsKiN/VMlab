import { useEffect, useRef, useState, type ChangeEvent } from 'react';
import {
  formatMessage,
  resolveLang,
  technicalText,
  useT,
  type Lang,
  type Locale,
  type Message,
  type Translate,
} from '../i18n';
import { composeToLab, type ComposeLab } from '../lib/compose';
import { ipc } from '../lib/ipc';
import { TEMPLATES, type LabTemplate } from '../lib/templates';
import { checkUpdatesOnStartup, setCheckUpdatesOnStartup } from '../lib/updates';
import { useLab, type LabMode } from '../store/useLab';
import type { LabSnapInfo } from '../types';
import { AboutDialog } from './AboutDialog';

type Pending =
  | { kind: 'file'; text: string; name: string }
  | { kind: 'template'; template: LabTemplate };

type ComposePending = { name: string; lab: ComposeLab; warnings: Message[] };

type SnapConfirm = { kind: 'restore' | 'delete'; snap: LabSnapInfo };

const TWO_DIGITS = (n: number) => String(n).padStart(2, '0');

const errText = (e: unknown) => (typeof e === 'string' ? e : e instanceof Error ? e.message : JSON.stringify(e));

/** Fecha legible para ISO de VirtualBox y para el formato de `docker images`. */
function snapDate(value: string | null, t: Translate, locale: Locale): string {
  if (!value) return t('snaps.dateUnknown');
  const normalized = value.replace(' ', 'T').replace(' +0000 UTC', 'Z');
  const date = new Date(normalized);
  if (Number.isNaN(date.getTime())) return value;
  return `${date.toLocaleDateString(locale === 'es' ? 'es-ES' : 'en-US')} ${TWO_DIGITS(date.getHours())}:${TWO_DIGITS(date.getMinutes())}`;
}

/**
 * Menú único de laboratorio: plantillas, importar y exportar.
 *
 * Los modales son propios de la app (nada de `window.confirm`) y el menú se
 * cierra al hacer clic fuera o al pulsar Escape.
 */
export function LabMenu() {
  const t = useT();
  const lang = useLab((s) => s.lang);
  const setLang = useLab((s) => s.setLang);
  const locale = resolveLang(lang);
  const exportLab = useLab((s) => s.exportLab);
  const importLab = useLab((s) => s.importLab);
  const applyTemplate = useLab((s) => s.applyTemplate);
  const toast = useLab((s) => s.toast);
  const nodeCount = useLab((s) => s.nodes.length);
  const hasEnvVars = useLab((s) =>
    s.nodes.some((n) => n.data.kind === 'docker' && (n.data.run?.env.length ?? 0) > 0),
  );
  const labsnaps = useLab((s) => s.labsnaps);
  const labsnapBusy = useLab((s) => s.labsnapBusy);
  const refreshLabsnaps = useLab((s) => s.refreshLabsnaps);
  const labsnapTake = useLab((s) => s.labsnapTake);
  const labsnapRestore = useLab((s) => s.labsnapRestore);
  const labsnapDelete = useLab((s) => s.labsnapDelete);
  const hasAssigned = useLab((s) => s.nodes.some((n) => n.data.slot !== true && n.data.target.trim() !== ''));
  const hasNodes = nodeCount > 0;

  const rootRef = useRef<HTMLDivElement>(null);
  const fileRef = useRef<HTMLInputElement>(null);
  const composeRef = useRef<HTMLInputElement>(null);
  const [menuOpen, setMenuOpen] = useState(false);
  const [templatesOpen, setTemplatesOpen] = useState(false);
  const [snapsOpen, setSnapsOpen] = useState(false);
  const [snapLabel, setSnapLabel] = useState('');
  const [snapConfirm, setSnapConfirm] = useState<SnapConfirm | null>(null);
  const [pending, setPending] = useState<Pending | null>(null);
  const [composePending, setComposePending] = useState<ComposePending | null>(null);
  const [composeBusy, setComposeBusy] = useState(false);
  const [exportWarn, setExportWarn] = useState(false);
  const [aboutOpen, setAboutOpen] = useState(false);
  const [checkUpdates, setCheckUpdates] = useState(checkUpdatesOnStartup);

  useEffect(() => {
    if (!menuOpen) return;
    const onPointerDown = (event: PointerEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setMenuOpen(false);
    };
    document.addEventListener('pointerdown', onPointerDown);
    return () => document.removeEventListener('pointerdown', onPointerDown);
  }, [menuOpen]);

  useEffect(() => {
    if (!menuOpen && !templatesOpen && !snapsOpen && !snapConfirm && !pending && !composePending && !exportWarn) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      setMenuOpen(false);
      setTemplatesOpen(false);
      setPending(null);
      setComposePending(null);
      setExportWarn(false);
      if (!labsnapBusy) {
        setSnapsOpen(false);
        setSnapConfirm(null);
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [menuOpen, templatesOpen, snapsOpen, snapConfirm, pending, composePending, exportWarn, labsnapBusy]);

  async function onFile(event: ChangeEvent<HTMLInputElement>) {
    const input = event.currentTarget;
    const file = input.files?.[0];
    input.value = '';
    if (!file) return;
    try {
      const text = await file.text();
      if (nodeCount > 0) setPending({ kind: 'file', text, name: file.name });
      else await importLab(text, 'replace');
    } catch {
      toast('error', t('sidebar.fileReadError'));
    }
  }

  async function onComposeFile(event: ChangeEvent<HTMLInputElement>) {
    const input = event.currentTarget;
    const file = input.files?.[0];
    input.value = '';
    if (!file) return;
    setComposeBusy(true);
    try {
      const text = await file.text();
      const json = await ipc.composeConfig(text);
      const result = composeToLab(json);
      if ('error' in result) {
        toast('error', formatMessage(result.error));
        return;
      }
      setComposePending({ name: file.name, lab: result.lab, warnings: result.warnings });
    } catch (e) {
      toast('error', technicalText(errText(e)));
    } finally {
      setComposeBusy(false);
    }
  }

  function chooseTemplate(template: LabTemplate) {
    setTemplatesOpen(false);
    if (nodeCount > 0) setPending({ kind: 'template', template });
    else applyTemplate(template, 'replace');
  }

  function runPending(mode: LabMode) {
    const action = pending;
    setPending(null);
    if (!action) return;
    if (action.kind === 'file') void importLab(action.text, mode);
    else applyTemplate(action.template, mode);
  }

  function runCompose(mode: LabMode) {
    const action = composePending;
    setComposePending(null);
    if (!action || action.lab.nodes.length === 0) return;
    void importLab(JSON.stringify(action.lab), mode);
  }

  function exportNow() {
    setMenuOpen(false);
    if (hasEnvVars) setExportWarn(true);
    else exportLab();
  }

  function openSnaps() {
    setMenuOpen(false);
    setSnapsOpen(true);
    setSnapLabel('');
    void refreshLabsnaps();
  }

  async function saveSnap() {
    if (labsnapBusy || !snapLabel.trim()) return;
    await labsnapTake(snapLabel);
    setSnapLabel('');
  }

  async function confirmSnap() {
    const action = snapConfirm;
    if (!action || labsnapBusy) return;
    setSnapConfirm(null);
    if (action.kind === 'restore') await labsnapRestore(action.snap.id);
    else await labsnapDelete(action.snap.id);
  }

  return (
    <div className="lab-menu" ref={rootRef}>
      <button
        className="btn"
        onClick={() => setMenuOpen((open) => !open)}
        aria-haspopup="menu"
        aria-expanded={menuOpen}
        title={t('lab.menuHint')}
      >
        {t('lab.title')} <span className="lab-caret" aria-hidden="true">▾</span>
      </button>

      {menuOpen && (
        <div className="lab-dropdown" role="menu">
          <button role="menuitem" onClick={() => { setMenuOpen(false); setTemplatesOpen(true); }}>
            {t('lab.templates')}
          </button>
          <button role="menuitem" onClick={openSnaps}>
            {t('lab.snapshots')}
          </button>
          <button role="menuitem" onClick={() => { setMenuOpen(false); fileRef.current?.click(); }}>
            {t('lab.import')}
          </button>
          <button
            role="menuitem"
            disabled={composeBusy}
            onClick={() => { setMenuOpen(false); composeRef.current?.click(); }}
          >
            {composeBusy ? t('lab.importComposeBusy') : t('lab.importCompose')}
          </button>
          <button role="menuitem" disabled={!hasNodes} onClick={exportNow}>
            {t('lab.export')}
          </button>
          <div className="lab-lang">
            <label htmlFor="lab-lang-select">{t('lab.lang')}</label>
            <select
              id="lab-lang-select"
              value={lang}
              onChange={(event) => setLang(event.currentTarget.value as Lang)}
            >
              <option value="auto">{t('lab.langAuto')}</option>
              <option value="es">Español</option>
              <option value="en">English</option>
            </select>
          </div>
          <label className="lab-check">
            <input
              type="checkbox"
              checked={checkUpdates}
              onChange={(event) => {
                const enabled = event.currentTarget.checked;
                setCheckUpdates(enabled);
                setCheckUpdatesOnStartup(enabled);
              }}
            />
            {t('lab.checkUpdates')}
          </label>
          <div className="lab-about">
            <button role="menuitem" onClick={() => { setMenuOpen(false); setAboutOpen(true); }}>
              {t('lab.about')}
            </button>
          </div>
        </div>
      )}

      <input ref={fileRef} type="file" accept=".vmlab,.json" hidden onChange={onFile} />
      <input ref={composeRef} type="file" accept=".yml,.yaml" hidden onChange={onComposeFile} />

      {templatesOpen && (
        <div
          className="modal-backdrop"
          onMouseDown={(event) => { if (event.target === event.currentTarget) setTemplatesOpen(false); }}
        >
          <div className="modal wide" role="dialog" aria-modal="true" aria-labelledby="templates-title">
            <h3 id="templates-title">{t('templates.title')}</h3>
            <p className="hint">{t('templates.hint')}</p>
            <div className="template-grid">
              {TEMPLATES.map((template) => {
                const slots = template.nodes.filter((node) => node.data.slot === true).length;
                return (
                  <button key={template.id} className="template-card" onClick={() => chooseTemplate(template)}>
                    <span className="template-name">{template.name}</span>
                    <span className="template-desc">{template.description}</span>
                    <span className="template-meta">
                      {t('templates.count', { count: template.nodes.length })} ·{' '}
                      {slots ? t('templates.slots', { count: slots }) : t('templates.noSlots')}
                    </span>
                  </button>
                );
              })}
            </div>
            <div className="modal-actions">
              <button className="btn" onClick={() => setTemplatesOpen(false)}>{t('common.cancel')}</button>
            </div>
          </div>
        </div>
      )}

      {snapsOpen && (
        <div
          className="modal-backdrop"
          onMouseDown={(event) => {
            if (event.target === event.currentTarget && !labsnapBusy) setSnapsOpen(false);
          }}
        >
          <div className="modal wide" role="dialog" aria-modal="true" aria-labelledby="snaps-title">
            <h3 id="snaps-title">{t('snaps.title')}</h3>
            <p className="hint">
              {t('snaps.hint')}
            </p>

            <div className="labsnap-form">
              <label className="field">{t('snaps.name')}
                <input
                  value={snapLabel}
                  maxLength={40}
                  disabled={labsnapBusy}
                  onChange={(event) => setSnapLabel(event.currentTarget.value)}
                  placeholder={t('snaps.placeholder')}
                />
              </label>
              <button
                className="btn primary"
                disabled={labsnapBusy || !snapLabel.trim() || !hasAssigned}
                onClick={() => void saveSnap()}
              >
                {labsnapBusy ? t('snaps.working') : t('snaps.save')}
              </button>
            </div>
            {!hasAssigned && <p className="hint warn">{t('snaps.needAssigned')}</p>}
            {labsnapBusy && <p className="hint warn">{t('snaps.busy')}</p>}

            <div className="labsnap-list">
              {labsnaps.length === 0 ? (
                <p className="hint">{t('snaps.empty')}</p>
              ) : (
                labsnaps.map((snap) => {
                  const total = snap.present.length + snap.missing.length;
                  return (
                    <div className="labsnap-item" key={snap.id}>
                      <div className="labsnap-main">
                        <div className="labsnap-name">{snap.label}</div>
                        <div className="labsnap-meta">
                          {snapDate(snap.createdAt, t, locale)} · {t('snaps.machineCount', { present: snap.present.length, total, count: total })}
                        </div>
                        {snap.missing.length > 0 && (
                          <div className="labsnap-missing">{t('snaps.missing', { names: snap.missing.join(', ') })}</div>
                        )}
                      </div>
                      <div className="labsnap-actions">
                        <button
                          className="btn"
                          disabled={labsnapBusy || snap.present.length === 0}
                          onClick={() => setSnapConfirm({ kind: 'restore', snap })}
                        >
                          {t('common.restore')}
                        </button>
                        <button
                          className="btn danger"
                          disabled={labsnapBusy}
                          onClick={() => setSnapConfirm({ kind: 'delete', snap })}
                        >
                          {t('common.delete')}
                        </button>
                      </div>
                    </div>
                  );
                })
              )}
            </div>

            <div className="labsnap-help">
              <p className="hint"><b>{t('snaps.note')}</b></p>
              <ul className="hint">
                <li>{t('snaps.noteVbox')}</li>
                <li>{t('snaps.noteDocker')}</li>
                <li>{t('snaps.noteCanvas')}</li>
              </ul>
            </div>

            <div className="modal-actions">
              <button className="btn" disabled={labsnapBusy} onClick={() => setSnapsOpen(false)}>{t('common.close')}</button>
            </div>
          </div>
        </div>
      )}

      {snapConfirm && (
        <div
          className="modal-backdrop"
          onMouseDown={(event) => {
            if (event.target === event.currentTarget && !labsnapBusy) setSnapConfirm(null);
          }}
        >
          <div className="modal" role="dialog" aria-modal="true" aria-labelledby="snap-confirm-title">
            <h3 id="snap-confirm-title">
              {snapConfirm.kind === 'restore' ? t('snaps.confirmRestoreTitle') : t('snaps.confirmDeleteTitle')}
            </h3>
            <p className="hint">
              {snapConfirm.kind === 'restore'
                ? t('snaps.confirmRestore', { label: snapConfirm.snap.label, count: snapConfirm.snap.present.length })
                : t('snaps.confirmDelete', { label: snapConfirm.snap.label, count: snapConfirm.snap.present.length })}
            </p>
            <div className="modal-actions">
              <button
                className={snapConfirm.kind === 'delete' ? 'btn danger' : 'btn primary'}
                disabled={labsnapBusy}
                onClick={() => void confirmSnap()}
              >
                {snapConfirm.kind === 'restore' ? t('common.restore') : t('common.delete')}
              </button>
              <button className="btn" disabled={labsnapBusy} onClick={() => setSnapConfirm(null)}>{t('common.cancel')}</button>
            </div>
          </div>
        </div>
      )}

      {composePending && (
        <div
          className="modal-backdrop"
          onMouseDown={(event) => { if (event.target === event.currentTarget) setComposePending(null); }}
        >
          <div className="modal wide" role="dialog" aria-modal="true" aria-labelledby="compose-title">
            <h3 id="compose-title">{t('compose.title')}</h3>
            <p className="hint">
              {composePending.lab.nodes.length === 0
                ? t('compose.none', { name: composePending.name })
                : t('compose.imported', { count: composePending.lab.nodes.length, name: composePending.name })}
            </p>
            {composePending.warnings.length > 0 && (
              <>
                <p className="hint warn">
                  {t('compose.warnings', { count: composePending.warnings.length })}
                </p>
                <ul className="compose-warnings">
                  {composePending.warnings.map((warning, index) => (
                    <li key={index}>{formatMessage(warning, t)}</li>
                  ))}
                </ul>
              </>
            )}
            <div className="modal-actions">
              <button
                className="btn primary"
                disabled={composePending.lab.nodes.length === 0}
                onClick={() => runCompose('replace')}
              >
                {t('common.replace')}
              </button>
              <button
                className="btn"
                disabled={composePending.lab.nodes.length === 0}
                onClick={() => runCompose('append')}
              >
                {t('common.append')}
              </button>
              <button className="btn" onClick={() => setComposePending(null)}>{t('common.cancel')}</button>
            </div>
          </div>
        </div>
      )}

      {exportWarn && (
        <div
          className="modal-backdrop"
          onMouseDown={(event) => { if (event.target === event.currentTarget) setExportWarn(false); }}
        >
          <div className="modal" role="dialog" aria-modal="true" aria-labelledby="export-warn-title">
            <h3 id="export-warn-title">{t('export.title')}</h3>
            <p className="hint">{t('export.warn')}</p>
            <div className="modal-actions">
              <button
                className="btn primary"
                onClick={() => {
                  setExportWarn(false);
                  exportLab();
                }}
              >
                {t('export.anyway')}
              </button>
              <button className="btn" onClick={() => setExportWarn(false)}>{t('common.cancel')}</button>
            </div>
          </div>
        </div>
      )}

      {pending && (
        <div
          className="modal-backdrop"
          onMouseDown={(event) => { if (event.target === event.currentTarget) setPending(null); }}
        >
          <div className="modal" role="dialog" aria-modal="true" aria-labelledby="lab-mode-title">
            <h3 id="lab-mode-title">{pending.kind === 'file' ? t('import.title') : t('import.addTemplate')}</h3>
            <p className="hint">
              {pending.kind === 'file'
                ? t('import.conflictFile', { count: nodeCount, name: pending.name })
                : t('import.conflictTemplate', { count: nodeCount, name: pending.template.name })}
            </p>
            <div className="modal-actions">
              <button className="btn primary" onClick={() => runPending('replace')}>{t('common.replace')}</button>
              <button className="btn" onClick={() => runPending('append')}>{t('common.append')}</button>
              <button className="btn" onClick={() => setPending(null)}>{t('common.cancel')}</button>
            </div>
          </div>
        </div>
      )}

      {aboutOpen && <AboutDialog onClose={() => setAboutOpen(false)} />}
    </div>
  );
}
