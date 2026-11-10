import { useEffect, useRef, useState, type ChangeEvent } from 'react';
import { TEMPLATES, type LabTemplate } from '../lib/templates';
import { useLab, type LabMode } from '../store/useLab';
import type { LabSnapInfo } from '../types';

type Pending =
  | { kind: 'file'; text: string; name: string }
  | { kind: 'template'; template: LabTemplate };

type SnapConfirm = { kind: 'restore' | 'delete'; snap: LabSnapInfo };

const TWO_DIGITS = (n: number) => String(n).padStart(2, '0');

/** Fecha legible para ISO de VirtualBox y para el formato de `docker images`. */
function snapDate(value: string | null): string {
  if (!value) return 'fecha desconocida';
  const normalized = value.replace(' ', 'T').replace(' +0000 UTC', 'Z');
  const date = new Date(normalized);
  if (Number.isNaN(date.getTime())) return value;
  return `${date.toLocaleDateString('es-ES')} ${TWO_DIGITS(date.getHours())}:${TWO_DIGITS(date.getMinutes())}`;
}

/**
 * Menú único de laboratorio: plantillas, importar y exportar.
 *
 * Los modales son propios de la app (nada de `window.confirm`) y el menú se
 * cierra al hacer clic fuera o al pulsar Escape.
 */
export function LabMenu() {
  const exportLab = useLab((s) => s.exportLab);
  const importLab = useLab((s) => s.importLab);
  const applyTemplate = useLab((s) => s.applyTemplate);
  const toast = useLab((s) => s.toast);
  const nodeCount = useLab((s) => s.nodes.length);
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
  const [menuOpen, setMenuOpen] = useState(false);
  const [templatesOpen, setTemplatesOpen] = useState(false);
  const [snapsOpen, setSnapsOpen] = useState(false);
  const [snapLabel, setSnapLabel] = useState('');
  const [snapConfirm, setSnapConfirm] = useState<SnapConfirm | null>(null);
  const [pending, setPending] = useState<Pending | null>(null);

  useEffect(() => {
    if (!menuOpen) return;
    const onPointerDown = (event: PointerEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setMenuOpen(false);
    };
    document.addEventListener('pointerdown', onPointerDown);
    return () => document.removeEventListener('pointerdown', onPointerDown);
  }, [menuOpen]);

  useEffect(() => {
    if (!menuOpen && !templatesOpen && !snapsOpen && !snapConfirm && !pending) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      setMenuOpen(false);
      setTemplatesOpen(false);
      setPending(null);
      if (!labsnapBusy) {
        setSnapsOpen(false);
        setSnapConfirm(null);
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [menuOpen, templatesOpen, snapsOpen, snapConfirm, pending, labsnapBusy]);

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
      toast('error', 'No se pudo leer el archivo.');
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
        title="Plantillas, importar y exportar laboratorios"
      >
        Laboratorio <span className="lab-caret" aria-hidden="true">▾</span>
      </button>

      {menuOpen && (
        <div className="lab-dropdown" role="menu">
          <button role="menuitem" onClick={() => { setMenuOpen(false); setTemplatesOpen(true); }}>
            Plantillas…
          </button>
          <button role="menuitem" onClick={openSnaps}>
            Instantáneas…
          </button>
          <button role="menuitem" onClick={() => { setMenuOpen(false); fileRef.current?.click(); }}>
            Importar lab…
          </button>
          <button role="menuitem" disabled={!hasNodes} onClick={() => { setMenuOpen(false); exportLab(); }}>
            Exportar lab…
          </button>
        </div>
      )}

      <input ref={fileRef} type="file" accept=".vmlab,.json" hidden onChange={onFile} />

      {templatesOpen && (
        <div
          className="modal-backdrop"
          onMouseDown={(event) => { if (event.target === event.currentTarget) setTemplatesOpen(false); }}
        >
          <div className="modal wide" role="dialog" aria-modal="true" aria-labelledby="templates-title">
            <h3 id="templates-title">Plantillas de laboratorio</h3>
            <p className="hint">Elige una plantilla para crear el lienzo. Los huecos de VM se asignan después desde el panel derecho.</p>
            <div className="template-grid">
              {TEMPLATES.map((template) => {
                const slots = template.nodes.filter((node) => node.data.slot === true).length;
                return (
                  <button key={template.id} className="template-card" onClick={() => chooseTemplate(template)}>
                    <span className="template-name">{template.name}</span>
                    <span className="template-desc">{template.description}</span>
                    <span className="template-meta">
                      {template.nodes.length} nodo(s) · {slots ? `${slots} hueco(s) de VM` : 'sin huecos de VM'}
                    </span>
                  </button>
                );
              })}
            </div>
            <div className="modal-actions">
              <button className="btn" onClick={() => setTemplatesOpen(false)}>Cancelar</button>
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
            <h3 id="snaps-title">Instantáneas del laboratorio</h3>
            <p className="hint">
              Guarda el estado de todas las máquinas del lienzo bajo un nombre y vuelve a él cuando quieras.
            </p>

            <div className="labsnap-form">
              <label className="field">Nombre
                <input
                  value={snapLabel}
                  maxLength={40}
                  disabled={labsnapBusy}
                  onChange={(event) => setSnapLabel(event.currentTarget.value)}
                  placeholder="p. ej. Antes de instalar el servidor"
                />
              </label>
              <button
                className="btn primary"
                disabled={labsnapBusy || !snapLabel.trim() || !hasAssigned}
                onClick={() => void saveSnap()}
              >
                {labsnapBusy ? 'Trabajando…' : 'Guardar estado del lab'}
              </button>
            </div>
            {!hasAssigned && <p className="hint warn">Asigna al menos una VM o contenedor antes de guardar.</p>}
            {labsnapBusy && <p className="hint warn">Trabajando… puede tardar minutos.</p>}

            <div className="labsnap-list">
              {labsnaps.length === 0 ? (
                <p className="hint">Todavía no hay instantáneas guardadas.</p>
              ) : (
                labsnaps.map((snap) => {
                  const total = snap.present.length + snap.missing.length;
                  return (
                    <div className="labsnap-item" key={snap.id}>
                      <div className="labsnap-main">
                        <div className="labsnap-name">{snap.label}</div>
                        <div className="labsnap-meta">
                          {snapDate(snap.createdAt)} · {snap.present.length}/{total} máquina(s)
                        </div>
                        {snap.missing.length > 0 && (
                          <div className="labsnap-missing">Faltan: {snap.missing.join(', ')}</div>
                        )}
                      </div>
                      <div className="labsnap-actions">
                        <button
                          className="btn"
                          disabled={labsnapBusy || snap.present.length === 0}
                          onClick={() => setSnapConfirm({ kind: 'restore', snap })}
                        >
                          Restaurar
                        </button>
                        <button
                          className="btn danger"
                          disabled={labsnapBusy}
                          onClick={() => setSnapConfirm({ kind: 'delete', snap })}
                        >
                          Eliminar
                        </button>
                      </div>
                    </div>
                  );
                })
              )}
            </div>

            <div className="labsnap-help">
              <p className="hint"><b>Ten en cuenta</b></p>
              <ul className="hint">
                <li>Para restaurar una VM de VirtualBox, apágala antes.</li>
                <li>En Docker se conserva el contenedor original renombrado. Los volúmenes y los puertos no se guardan.</li>
                <li>El lienzo no se guarda en la instantánea: usa «Exportar lab».</li>
              </ul>
            </div>

            <div className="modal-actions">
              <button className="btn" disabled={labsnapBusy} onClick={() => setSnapsOpen(false)}>Cerrar</button>
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
              {snapConfirm.kind === 'restore' ? 'Restaurar instantánea' : 'Eliminar instantánea'}
            </h3>
            <p className="hint">
              {snapConfirm.kind === 'restore'
                ? `Se restaurará «${snapConfirm.snap.label}» en ${snapConfirm.snap.present.length} máquina(s). Las VM encendidas no se tocarán.`
                : `Se borrará «${snapConfirm.snap.label}» de ${snapConfirm.snap.present.length} máquina(s). Esta acción no se puede deshacer.`}
            </p>
            <div className="modal-actions">
              <button
                className={snapConfirm.kind === 'delete' ? 'btn danger' : 'btn primary'}
                disabled={labsnapBusy}
                onClick={() => void confirmSnap()}
              >
                {snapConfirm.kind === 'restore' ? 'Restaurar' : 'Eliminar'}
              </button>
              <button className="btn" disabled={labsnapBusy} onClick={() => setSnapConfirm(null)}>Cancelar</button>
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
            <h3 id="lab-mode-title">{pending.kind === 'file' ? 'Importar laboratorio' : 'Añadir plantilla'}</h3>
            <p className="hint">
              El lienzo ya tiene {nodeCount} nodo(s). ¿Quieres reemplazarlo o añadir{' '}
              {pending.kind === 'file' ? `el contenido de «${pending.name}»` : `la plantilla «${pending.template.name}»`}?
            </p>
            <div className="modal-actions">
              <button className="btn primary" onClick={() => runPending('replace')}>Reemplazar</button>
              <button className="btn" onClick={() => runPending('append')}>Añadir</button>
              <button className="btn" onClick={() => setPending(null)}>Cancelar</button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
