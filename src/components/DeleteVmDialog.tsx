import { useEffect, useRef, useState } from 'react';
import { technicalText, useT } from '../i18n';
import { ipc } from '../lib/ipc';
import { useLab } from '../store/useLab';
import type { MachineInfo } from '../types';

const errText = (e: unknown) => (typeof e === 'string' ? e : e instanceof Error ? e.message : JSON.stringify(e));

/**
 * Confirmación de borrado de una VM, con las tres opciones de VirtualBox.
 * El backend vuelve a validar UUID, existencia y estado antes de tocar nada.
 */
export function DeleteVmDialog({ vm, onClose }: { vm: MachineInfo; onClose: () => void }) {
  const t = useT();
  const cancelRef = useRef<HTMLButtonElement>(null);
  const [busy, setBusy] = useState(false);

  const nodes = useLab((s) => s.nodes);
  const onCanvas = nodes.some((n) => n.data.kind === 'vbox' && n.data.target === vm.target);
  const removeNode = useLab((s) => s.removeNode);
  const refreshInventory = useLab((s) => s.refreshInventory);
  const toast = useLab((s) => s.toast);

  useEffect(() => {
    cancelRef.current?.focus();
  }, []);

  const requestClose = () => {
    if (!busy) onClose();
  };

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') requestClose();
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [busy, onClose]);

  async function remove(deleteFiles: boolean) {
    if (busy) return;
    setBusy(true);
    const detailText = deleteFiles
      ? t('deleteVm.deleteAll')
      : t('deleteVm.removeOnly');
    try {
      const result = await ipc.vmDelete({ uuid: vm.target, deleteFiles });
      const canvasNode = useLab.getState().nodes.find(
        (n) => n.data.kind === 'vbox' && n.data.target === vm.target,
      );
      if (canvasNode) removeNode(canvasNode.id);
      await refreshInventory();
      const message = result.warnings.length
        ? t('deleteVm.successWithWarnings', { name: vm.label, detail: detailText, warnings: result.warnings.join(' ') })
        : t('deleteVm.success', { name: vm.label, detail: detailText });
      toast('ok', message);
      onClose();
    } catch (e) {
      toast('error', technicalText(t('deleteVm.error', { name: vm.label, detail: errText(e) })));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div
      className="modal-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) requestClose();
      }}
    >
      <div className="modal delete-vm" role="dialog" aria-modal="true" aria-labelledby="deletevm-title">
        <h3 id="deletevm-title">{t('deleteVm.title', { name: vm.label })}</h3>
        <p className="delete-vm-question">{t('deleteVm.question')}</p>
        <p className="hint">{t('deleteVm.dataNote')}</p>
        {onCanvas && <p className="hint">{t('deleteVm.canvasNote')}</p>}

        {busy && <p className="delete-vm-busy" role="status">{t('deleteVm.busy')}</p>}

        <div className="modal-actions delete-vm-actions">
          <button ref={cancelRef} type="button" className="btn" onClick={requestClose} disabled={busy}>
            {t('common.cancel')}
          </button>
          <button type="button" className="btn" onClick={() => void remove(false)} disabled={busy}>
            {t('deleteVm.removeOnly')}
          </button>
          <div className="delete-vm-destructive">
            <button type="button" className="btn danger" onClick={() => void remove(true)} disabled={busy}>
              {t('deleteVm.deleteAll')}
            </button>
            <small>{t('deleteVm.irreversible')}</small>
          </div>
        </div>
      </div>
    </div>
  );
}
