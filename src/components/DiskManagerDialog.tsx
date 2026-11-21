import { useEffect, useRef, useState } from 'react';
import { technicalText, useT } from '../i18n';
import { diskName, diskSizeLabel, MAX_DISK_SIZE_MB, volumeName } from '../lib/disk';
import { ipc } from '../lib/ipc';
import { useLab } from '../store/useLab';
import type { ManagedDisk, ManagedDiskKind } from '../types';
import { PlusIcon, RefreshIcon, TrashIcon } from './icons';

const errText = (e: unknown) => (typeof e === 'string' ? e : e instanceof Error ? e.message : JSON.stringify(e));
const GB_MB = 1024;

type DiskAction =
  | { type: 'delete'; disk: ManagedDisk }
  | { type: 'resize'; disk: ManagedDisk };

/** Motivo visible cuando el borrado no es seguro. El backend vuelve a validarlo. */
function deleteReason(disk: ManagedDisk, t: ReturnType<typeof useT>): string | null {
  if (disk.inUseBy.length) {
    return disk.note ? `${t('diskManager.inUse')} ${disk.note}` : t('diskManager.inUse');
  }
  if (!disk.deletable) return disk.note ?? t('diskManager.notDeletable');
  return null;
}

function sizeText(disk: ManagedDisk): string {
  if (disk.capacityMb !== null) return diskSizeLabel(disk.capacityMb);
  if (disk.sizeOnDiskMb !== null) return diskSizeLabel(disk.sizeOnDiskMb);
  return '—';
}

/** Un disco del lienzo ya no existe: avisamos, pero no tocamos los nodos. */
function diskOnCanvas(disk: ManagedDisk): boolean {
  return useLab.getState().nodes.some((node) => {
    if (node.data.kind !== 'disk') return false;
    const name = disk.kind === 'vdi' ? diskName(node.data.target) : volumeName(node.data.target);
    return name === disk.name;
  });
}

/**
 * Lista de VDI y volúmenes creados por VMLab. El backend es el único que decide
 * qué se puede borrar o ampliar; aquí solo mostramos sus banderas y motivos.
 */
export function DiskManagerDialog({ onClose }: { onClose: () => void }) {
  const t = useT();
  const closeRef = useRef<HTMLButtonElement>(null);
  const confirmRef = useRef<HTMLButtonElement>(null);
  const [busy, setBusy] = useState(false);
  const [action, setAction] = useState<DiskAction | null>(null);
  const [sizeGb, setSizeGb] = useState('');

  const disks = useLab((s) => s.managedDisks);
  const tools = useLab((s) => s.tools);
  const refreshDisks = useLab((s) => s.refreshDisks);
  const toast = useLab((s) => s.toast);
  const noTools = !tools?.vboxmanage && !tools?.docker;

  useEffect(() => {
    closeRef.current?.focus();
  }, []);

  useEffect(() => {
    if (action) confirmRef.current?.focus();
  }, [action]);

  useEffect(() => {
    if (!noTools) void refreshDisks();
  }, [noTools, refreshDisks]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || busy) return;
      if (action) setAction(null);
      else onClose();
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [action, busy, onClose]);

  const requestClose = () => {
    if (!busy) onClose();
  };

  const openResize = (disk: ManagedDisk) => {
    setSizeGb(disk.capacityMb === null ? '' : String(disk.capacityMb / GB_MB));
    setAction({ type: 'resize', disk });
  };

  const currentMb = action?.type === 'resize' ? action.disk.capacityMb ?? 0 : 0;
  const gb = Number(sizeGb.replace(',', '.'));
  const newMb = Math.round(gb * GB_MB);
  const invalidSize = !Number.isFinite(gb) || newMb <= currentMb || newMb > MAX_DISK_SIZE_MB;

  async function confirmDelete(disk: ManagedDisk) {
    if (busy) return;
    setBusy(true);
    try {
      await ipc.diskDelete(disk.name, disk.kind);
      await refreshDisks();
      setAction(null);
      toast('ok', t('diskManager.deleted', { name: disk.name }));
      if (diskOnCanvas(disk)) {
        toast('warning', t('diskManager.canvasRecreate', { name: disk.name }));
      }
    } catch (e) {
      toast('error', technicalText(t('diskManager.deleteError', { name: disk.name, detail: errText(e) })));
    } finally {
      setBusy(false);
    }
  }

  async function confirmResize(disk: ManagedDisk) {
    if (busy || invalidSize) return;
    setBusy(true);
    try {
      await ipc.diskResize(disk.name, newMb);
      await refreshDisks();
      setAction(null);
      toast('ok', t('diskManager.resized', { name: disk.name, size: diskSizeLabel(newMb) }));
    } catch (e) {
      toast('error', technicalText(t('diskManager.resizeError', { name: disk.name, detail: errText(e) })));
    } finally {
      setBusy(false);
    }
  }

  const kindLabel = (kind: ManagedDiskKind) =>
    t(kind === 'vdi' ? 'diskManager.kindVdi' : 'diskManager.kindVolume');

  return (
    <>
      <div
        className="modal-backdrop"
        onMouseDown={(event) => {
          if (event.target === event.currentTarget) requestClose();
        }}
      >
        <div className="modal wide disk-manager" role="dialog" aria-modal="true" aria-labelledby="disk-manager-title">
          <div className="side-title">
            <span id="disk-manager-title">{t('diskManager.title')}</span>
            <button
              className="icon-btn sm"
              type="button"
              onClick={() => void refreshDisks()}
              disabled={noTools || busy}
              title={t('diskManager.refreshHint')}
              aria-label={t('diskManager.refreshHint')}
            >
              <RefreshIcon />
            </button>
          </div>

          <p className="hint">{t('diskManager.hint')}</p>

          {noTools ? (
            <p className="disk-manager-empty">{t('diskManager.noTools')}</p>
          ) : disks.length === 0 ? (
            <p className="disk-manager-empty">{t('diskManager.empty')}</p>
          ) : (
            <div className="disk-manager-scroll">
              <table className="disk-manager-table">
                <thead>
                  <tr>
                    <th>{t('diskManager.name')}</th>
                    <th>{t('diskManager.kind')}</th>
                    <th>{t('diskManager.capacity')}</th>
                    <th>{t('diskManager.onDisk')}</th>
                    <th>{t('diskManager.inUseByHeader')}</th>
                    <th>{t('diskManager.actions')}</th>
                  </tr>
                </thead>
                <tbody>
                  {disks.map((disk) => {
                    const reason = deleteReason(disk, t);
                    return (
                      <tr key={`${disk.kind}:${disk.name}`}>
                        <td>
                          <span className="disk-manager-name">{disk.name}</span>
                          {disk.note && !reason && <small className="disk-manager-note">{disk.note}</small>}
                        </td>
                        <td><span className={`disk-manager-kind ${disk.kind}`}>{kindLabel(disk.kind)}</span></td>
                        <td>{disk.capacityMb === null ? '—' : diskSizeLabel(disk.capacityMb)}</td>
                        <td>{disk.sizeOnDiskMb === null ? '—' : diskSizeLabel(disk.sizeOnDiskMb)}</td>
                        <td>
                          {disk.inUseBy.length
                            ? disk.inUseBy.map((user) => user.name).join(', ')
                            : '—'}
                        </td>
                        <td>
                          <div className="disk-manager-actions">
                            {disk.kind === 'vdi' && disk.resizable && (
                              <button
                                type="button"
                                className="btn"
                                onClick={() => openResize(disk)}
                                disabled={busy}
                                title={t('diskManager.growHint')}
                              >
                                <PlusIcon /> {t('diskManager.grow')}
                              </button>
                            )}
                            <span className="disk-manager-action" title={reason ?? t('diskManager.deleteHint', { name: disk.name })}>
                              <button
                                type="button"
                                className="btn danger"
                                onClick={() => setAction({ type: 'delete', disk })}
                                disabled={!!reason || busy}
                                aria-label={t('diskManager.deleteHint', { name: disk.name })}
                              >
                                <TrashIcon /> {t('common.delete')}
                              </button>
                            </span>
                            {reason && <small className="disk-manager-reason">{reason}</small>}
                          </div>
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          )}

          <div className="modal-actions">
            <button ref={closeRef} type="button" className="btn" onClick={onClose} disabled={busy}>
              {t('common.close')}
            </button>
          </div>
        </div>
      </div>

      {action?.type === 'delete' && (
        <div
          className="modal-backdrop confirm"
          onMouseDown={(event) => {
            if (event.target === event.currentTarget && !busy) setAction(null);
          }}
        >
          <div className="modal disk-manager-confirm" role="alertdialog" aria-modal="true" aria-labelledby="disk-delete-title">
            <h3 id="disk-delete-title">{t('diskManager.deleteTitle', { name: action.disk.name })}</h3>
            <p className="hint">{t('diskManager.deleteSize', { size: sizeText(action.disk) })}</p>
            <p className="disk-manager-warning">{t('diskManager.deleteWarning')}</p>
            {action.disk.kind === 'volume' && (
              <p className="hint">{t('diskManager.deleteVolumeNote')}</p>
            )}
            {diskOnCanvas(action.disk) && (
              <p className="hint">{t('diskManager.canvasRecreate', { name: action.disk.name })}</p>
            )}
            {busy && <p className="delete-vm-busy" role="status">{t('diskManager.working')}</p>}
            <div className="modal-actions">
              <button
                ref={confirmRef}
                type="button"
                className="btn"
                onClick={() => setAction(null)}
                disabled={busy}
              >
                {t('common.cancel')}
              </button>
              <button
                type="button"
                className="btn danger"
                onClick={() => void confirmDelete(action.disk)}
                disabled={busy}
              >
                {t('common.delete')}
              </button>
            </div>
          </div>
        </div>
      )}

      {action?.type === 'resize' && (
        <div
          className="modal-backdrop confirm"
          onMouseDown={(event) => {
            if (event.target === event.currentTarget && !busy) setAction(null);
          }}
        >
          <div className="modal disk-manager-confirm" role="dialog" aria-modal="true" aria-labelledby="disk-resize-title">
            <h3 id="disk-resize-title">{t('diskManager.resizeTitle', { name: action.disk.name })}</h3>
            <p className="hint">
              {t('diskManager.currentSize', { size: sizeText(action.disk) })}
            </p>
            <label className="field">
              {t('diskManager.newSize')}
              <input
                type="number"
                min={currentMb / GB_MB}
                step="1"
                value={sizeGb}
                onChange={(e) => setSizeGb(e.target.value)}
                spellCheck={false}
              />
            </label>
            {invalidSize && (
              <p className="hint err">
                {t('diskManager.sizeInvalid', { min: diskSizeLabel(currentMb) })}
              </p>
            )}
            <p className="hint">{t('diskManager.guestHint')}</p>
            {busy && <p className="delete-vm-busy" role="status">{t('diskManager.working')}</p>}
            <div className="modal-actions">
              <button
                ref={confirmRef}
                type="button"
                className="btn"
                onClick={() => setAction(null)}
                disabled={busy}
              >
                {t('common.cancel')}
              </button>
              <button
                type="button"
                className="btn primary"
                onClick={() => void confirmResize(action.disk)}
                disabled={busy || invalidSize}
              >
                {t('diskManager.grow')}
              </button>
            </div>
          </div>
        </div>
      )}
    </>
  );
}
