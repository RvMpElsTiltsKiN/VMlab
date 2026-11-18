import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { technicalText, useT } from '../i18n';
import { ipc, isTauri } from '../lib/ipc';
import {
  OTHER_OS,
  buildCreateVmRequest,
  catalogChoice,
  effectiveAdvanced,
  entryFromChoice,
  genericVmDefaults,
  machineFromCreatedVm,
  ostypeFromChoice,
  recommendedAdvanced,
} from '../lib/vmRequest';
import { useLab } from '../store/useLab';
import type {
  VmDiskBus,
  VmFirmware,
  VmGraphics,
  VmOsDefaults,
  VmOsEntry,
  VmOsType,
} from '../types';

const UUID_RE = /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;
const MAX_DISK_GB = Math.floor(2_000_000 / 1024);
const SETTING_KEYS = [
  'memoryMb', 'cpus', 'diskGb', 'vramMb', 'firmware', 'tpm', 'graphics', 'diskBus',
] as const;
type SettingKey = typeof SETTING_KEYS[number];
type Touched = Partial<Record<SettingKey, boolean>>;

const errText = (e: unknown) => (typeof e === 'string' ? e : e instanceof Error ? e.message : JSON.stringify(e));
const numberValue = (text: string) => {
  const value = Number(text);
  return Number.isFinite(value) ? Math.trunc(value) : 0;
};
const isoFileName = (path: string) => path.split(/[\\/]/).pop() || path;

/** Misma regla que `validate::vm_name` del núcleo Rust. */
function validVmName(name: string): boolean {
  return name.length >= 1
    && name.length <= 64
    && !name.startsWith('-')
    && !name.startsWith(' ')
    && !name.startsWith('.')
    && !name.endsWith(' ')
    && !name.endsWith('.')
    && !name.includes('..')
    && !UUID_RE.test(name)
    && /^[A-Za-z0-9 ._()-]+$/.test(name);
}

function inRange(value: number, min: number, max: number): boolean {
  return Number.isInteger(value) && value >= min && value <= max;
}

/** Formulario de creación de una VM vacía. El backend sigue siendo la autoridad. */
export function NewVmDialog({ onClose }: { onClose: () => void }) {
  const t = useT();
  const inventory = useLab((s) => s.inventory);
  const refreshInventory = useLab((s) => s.refreshInventory);
  const addMachine = useLab((s) => s.addMachine);
  const toast = useLab((s) => s.toast);

  const [name, setName] = useState('Nueva VM');
  const [catalog, setCatalog] = useState<VmOsEntry[]>([]);
  const [osChoice, setOsChoice] = useState('');
  // Solo para «Otro sistema…»: el id real de VirtualBox elegido en ese listado.
  const [otherOstype, setOtherOstype] = useState('');
  const [ostypes, setOstypes] = useState<VmOsType[]>([]);
  const [catalogLoading, setCatalogLoading] = useState(true);
  const [catalogError, setCatalogError] = useState<string | null>(null);
  const [ostypesLoading, setOstypesLoading] = useState(false);
  const [ostypesError, setOstypesError] = useState<string | null>(null);

  const [memoryMb, setMemoryMb] = useState(2048);
  const [cpus, setCpus] = useState(2);
  const [diskGb, setDiskGb] = useState(20);
  const [vramMb, setVramMb] = useState(128);
  const [firmware, setFirmware] = useState<VmFirmware>('bios');
  const [tpm, setTpm] = useState(false);
  const [graphics, setGraphics] = useState<VmGraphics>('vmsvga');
  const [diskBus, setDiskBus] = useState<VmDiskBus>('sata');

  const [isoPath, setIsoPath] = useState<string | null>(null);
  const [isoBusy, setIsoBusy] = useState(false);
  const [addToCanvas, setAddToCanvas] = useState(true);
  const [creating, setCreating] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);
  const [expertMode, setExpertMode] = useState(false);
  const [recommended, setRecommended] = useState<VmOsDefaults>(() => genericVmDefaults('other'));
  const [revertedNotice, setRevertedNotice] = useState(false);

  // Los campos que el usuario ha tocado se conservan al cambiar de sistema.
  const touchedRef = useRef<Touched>({});
  const catalogRequestedRef = useRef(false);
  const ostypesRequestedRef = useRef(false);

  // Única fuente de verdad: la selección visible. El catálogo se resuelve por
  // índice (hay ids repetidos: Ubuntu/Pop!_OS y Debian/Kali).
  const ostype = ostypeFromChoice(catalog, osChoice, otherOstype);

  const mark = (key: SettingKey) => {
    touchedRef.current[key] = true;
  };

  const applyAdvancedDefaults = useCallback((defaults: VmOsDefaults) => {
    const next = recommendedAdvanced(defaults);
    setVramMb(next.vramMb);
    setFirmware(next.firmware);
    setTpm(next.tpm);
    setGraphics(next.graphics);
    setDiskBus(next.diskBus);
  }, []);

  // Memoria/CPU/disco conservan lo editado; los cinco ajustes delicados siguen
  // siempre al sistema elegido mientras el modo experto esté desactivado.
  const applyDefaults = useCallback((defaults: VmOsDefaults) => {
    const touched = touchedRef.current;
    if (!touched.memoryMb) setMemoryMb(defaults.memoryMb);
    if (!touched.cpus) setCpus(defaults.cpus);
    if (!touched.diskGb) setDiskGb(defaults.diskGb);
    setRecommended({ ...defaults });
    if (!expertMode) applyAdvancedDefaults(defaults);
  }, [applyAdvancedDefaults, expertMode]);

  const loadOstypes = useCallback(async () => {
    if (ostypesRequestedRef.current) return;
    ostypesRequestedRef.current = true;
    setOstypesLoading(true);
    setOstypesError(null);
    try {
      setOstypes(await ipc.vmListOstypes());
    } catch (e) {
      const detail = errText(e);
      setOstypesError(detail);
      toast('error', technicalText(t('newvm.ostypesError', { detail })));
      ostypesRequestedRef.current = false;
    } finally {
      setOstypesLoading(false);
    }
  }, [t, toast]);

  const loadCatalog = useCallback(async () => {
    if (catalogRequestedRef.current) return;
    catalogRequestedRef.current = true;
    setCatalogLoading(true);
    setCatalogError(null);
    try {
      const list = await ipc.vmOsCatalog();
      setCatalog(list);
      if (list.length > 0) {
        setOsChoice(catalogChoice(0));
        applyDefaults(list[0].defaults);
      } else {
        setOsChoice(OTHER_OS);
        setOtherOstype('');
        applyDefaults(genericVmDefaults('other'));
        void loadOstypes();
      }
    } catch (e) {
      const detail = errText(e);
      setCatalogError(detail);
      toast('error', technicalText(t('newvm.catalogError', { detail })));
      catalogRequestedRef.current = false;
    } finally {
      setCatalogLoading(false);
    }
  }, [applyDefaults, loadOstypes, t, toast]);

  useEffect(() => {
    void loadCatalog();
  }, [loadCatalog]);

  const requestClose = () => {
    if (!creating) onClose();
  };

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') requestClose();
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [creating, onClose]);

  const chooseOs = (value: string) => {
    setOsChoice(value);
    setSubmitError(null);
    if (value === OTHER_OS) {
      setOtherOstype('');
      applyDefaults(genericVmDefaults('other'));
      void loadOstypes();
      return;
    }
    const entry = entryFromChoice(catalog, value);
    if (!entry) return;
    // El ostype se deriva de `osChoice`; aquí solo se actualizan los defaults
    // de los campos no editados a mano.
    applyDefaults(entry.defaults);
  };

  const chooseOstype = (value: string) => {
    setOtherOstype(value);
    setSubmitError(null);
    const info = ostypes.find((entry) => entry.id === value);
    if (info) applyDefaults(genericVmDefaults(info.family));
  };

  const changeFirmware = (value: VmFirmware) => {
    mark('firmware');
    setFirmware(value);
    // No se limpia la TPM: el aviso contextual avisa de que EFI es obligatoria
    // y la casilla queda deshabilitada. `effectiveAdvanced` no la enviará.
  };

  const restoreRecommended = () => {
    applyAdvancedDefaults(recommended);
    setRevertedNotice(true);
  };

  const toggleExpert = (checked: boolean) => {
    setExpertMode(checked);
    if (!checked) restoreRecommended();
    else setRevertedNotice(false);
  };

  useEffect(() => {
    if (!revertedNotice) return;
    const timer = setTimeout(() => setRevertedNotice(false), 3500);
    return () => clearTimeout(timer);
  }, [revertedNotice]);

  const chooseIso = async () => {
    if (isoBusy || creating) return;
    setIsoBusy(true);
    try {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const selected = await open({
        title: t('newvm.chooseIso'),
        multiple: false,
        filters: [{ name: 'ISO', extensions: ['iso'] }],
      });
      if (typeof selected === 'string') setIsoPath(selected);
    } catch (e) {
      const text = technicalText(t('newvm.isoError', { detail: errText(e) }));
      setSubmitError(text);
      toast('error', text);
    } finally {
      setIsoBusy(false);
    }
  };

  const knownNames = inventory?.vbox.map((vm) => vm.label) ?? [];
  const nameProblem = !validVmName(name)
    ? t('newvm.invalidName')
    : knownNames.some((known) => known.toLowerCase() === name.toLowerCase())
      ? t('newvm.duplicateName', { name })
      : null;
  const validationError = nameProblem
    ?? (!ostype ? t('newvm.selectOsError') : null)
    ?? (!inRange(memoryMb, 64, 131072) ? t('newvm.invalidMemory') : null)
    ?? (!inRange(cpus, 1, 64) ? t('newvm.invalidCpus') : null)
    ?? (!inRange(diskGb, 1, MAX_DISK_GB) ? t('newvm.invalidDisk') : null)
    ?? (!inRange(vramMb, 1, 256) ? t('newvm.invalidVram') : null);

  const notice = ostype === 'Windows11_64'
    ? t('newvm.win11Notice')
    : ostype === 'WindowsXP'
      ? t('newvm.xpNotice')
      : null;

  const advanced = useMemo(() => effectiveAdvanced(recommended, expertMode, {
    vramMb, firmware, tpm, graphics, diskBus,
  }), [recommended, expertMode, vramMb, firmware, tpm, graphics, diskBus]);

  const recommendedValues = recommendedAdvanced(recommended);
  const advancedDiffers = (
    advanced.vramMb !== recommendedValues.vramMb
    || advanced.firmware !== recommendedValues.firmware
    || advanced.graphics !== recommendedValues.graphics
    || advanced.diskBus !== recommendedValues.diskBus
  );
  const shownFirmware = expertMode ? firmware : advanced.firmware;
  const shownTpm = expertMode ? tpm : advanced.tpm;
  const showAdvancedWarnings = expertMode || advancedDiffers;

  const advancedWarnings: string[] = [];
  if (showAdvancedWarnings && shownFirmware === 'efi' && ostype !== 'Windows11_64') {
    advancedWarnings.push(t('newvm.warnEfiNonWin11'));
  }
  if (showAdvancedWarnings && ostype === 'Windows11_64'
    && (shownFirmware !== 'efi' || !shownTpm)) {
    advancedWarnings.push(t('newvm.warnWin11NeedsEfiTpm'));
  }
  if (showAdvancedWarnings && shownTpm && shownFirmware !== 'efi') {
    advancedWarnings.push(t('newvm.warnTpmNeedsEfi'));
  }

  const submit = async () => {
    if (creating) return;
    if (validationError) {
      setSubmitError(validationError);
      return;
    }
    setCreating(true);
    setSubmitError(null);
    try {
      const request = buildCreateVmRequest({
        name,
        choice: osChoice,
        otherOstype,
        memoryMb,
        cpus,
        diskGb,
        vramMb: advanced.vramMb,
        firmware: advanced.firmware,
        tpm: advanced.tpm,
        graphics: advanced.graphics,
        diskBus: advanced.diskBus,
        isoPath,
        expertMode,
        recommended,
      }, catalog);
      const created = await ipc.vmCreate(request);
      await refreshInventory();
      if (addToCanvas) {
        // `label` es el nombre real de la VM, nunca la etiqueta del SO elegido.
        addMachine(machineFromCreatedVm(created));
      }
      toast('ok', t('newvm.success', { name: created.name }));
      setCreating(false);
      onClose();
    } catch (e) {
      const text = technicalText(t('newvm.createError', { name, detail: errText(e) }));
      setSubmitError(text);
      toast('error', text);
    } finally {
      setCreating(false);
    }
  };

  const groups = (['windows', 'linux'] as const)
    .map((group) => ({ group, items: catalog.map((entry, index) => ({ entry, index })).filter(({ entry }) => entry.group === group) }))
    .filter(({ items }) => items.length > 0);

  return (
    <div
      className="modal-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) requestClose();
      }}
    >
      <div className="modal" role="dialog" aria-modal="true" aria-labelledby="newvm-title">
        <h3 id="newvm-title">{t('newvm.title')}</h3>

        <label className="field">
          {t('newvm.name')}
          <input
            value={name}
            maxLength={64}
            disabled={creating}
            spellCheck={false}
            className={nameProblem ? 'invalid' : undefined}
            placeholder={t('newvm.namePlaceholder')}
            onChange={(event) => setName(event.currentTarget.value)}
          />
        </label>

        <label className="field">
          {t('newvm.os')}
          <select
            value={osChoice}
            disabled={creating || catalogLoading || !!catalogError}
            onChange={(event) => chooseOs(event.currentTarget.value)}
          >
            {groups.map(({ group, items }) => (
              <optgroup key={group} label={group === 'windows' ? t('newvm.groupWindows') : t('newvm.groupLinux')}>
                {items.map(({ entry, index }) => (
                  <option key={`${index}-${entry.label}`} value={`cat-${index}`}>{entry.label}</option>
                ))}
              </optgroup>
            ))}
            <option value={OTHER_OS}>{t('newvm.otherOs')}</option>
          </select>
        </label>

        {catalogLoading && <p className="hint">{t('newvm.loadingCatalog')}</p>}
        {catalogError && (
          <div className="newvm-load-error">
            <span className="hint err">{t('newvm.loadError')}</span>
            <button className="btn" onClick={() => void loadCatalog()}>{t('newvm.retry')}</button>
          </div>
        )}
        {!catalogLoading && !catalogError && catalog.length === 0 && (
          <p className="hint">{t('newvm.noCatalog')}</p>
        )}

        {osChoice === OTHER_OS && (
          <label className="field">
            {t('newvm.selectOstype')}
            <select
              value={ostype}
              disabled={creating || ostypesLoading}
              onChange={(event) => chooseOstype(event.currentTarget.value)}
            >
              <option value="">{t('newvm.selectOstype')}</option>
              {ostypes.map((entry) => (
                <option key={entry.id} value={entry.id}>{entry.description} ({entry.id})</option>
              ))}
            </select>
            {ostypesLoading && <span className="hint">{t('newvm.loadingOstypes')}</span>}
            {ostypesError && (
              <div className="newvm-load-error">
                <span className="hint err">{t('newvm.loadError')}</span>
                <button className="btn" onClick={() => void loadOstypes()}>{t('newvm.retry')}</button>
              </div>
            )}
          </label>
        )}

        {ostype && <p className="hint newvm-ostype">{t('newvm.vboxType', { ostype })}</p>}
        {ostype && (
          <p className="hint newvm-summary">
            {t('newvm.summary', {
              ostype,
              firmware: advanced.firmware === 'efi' ? t('newvm.firmwareEfi') : t('newvm.firmwareBios'),
              tpm: advanced.tpm ? t('newvm.yes') : t('newvm.no'),
              graphics: advanced.graphics === 'vboxvga'
                ? 'VBoxVGA'
                : advanced.graphics === 'vboxsvga' ? 'VBoxSVGA' : 'VMSVGA',
              bus: advanced.diskBus.toUpperCase(),
            })}
          </p>
        )}

        {notice && <p className="hint warn">{notice}</p>}

        <div className="grid2">
          <label className="field">
            {t('newvm.memory')}
            <input type="number" min={64} max={131072} value={memoryMb} disabled={creating}
              onChange={(event) => { mark('memoryMb'); setMemoryMb(numberValue(event.currentTarget.value)); }} />
          </label>
          <label className="field">
            {t('newvm.cpus')}
            <input type="number" min={1} max={64} value={cpus} disabled={creating}
              onChange={(event) => { mark('cpus'); setCpus(numberValue(event.currentTarget.value)); }} />
          </label>
          <label className="field">
            {t('newvm.disk')}
            <input type="number" min={1} max={MAX_DISK_GB} value={diskGb} disabled={creating}
              onChange={(event) => { mark('diskGb'); setDiskGb(numberValue(event.currentTarget.value)); }} />
          </label>
        </div>

        <details className="newvm-advanced">
          <summary>{t('newvm.advanced')}</summary>
          <label className="field check newvm-expert">
            <input
              type="checkbox"
              checked={expertMode}
              disabled={creating}
              onChange={(event) => toggleExpert(event.currentTarget.checked)}
            />
            {t('newvm.expertMode')}
          </label>
          {!expertMode && <p className="hint">{t('newvm.recommendedHint')}</p>}
          {expertMode && (
            <>
              <p className="hint warn">{t('newvm.expertWarning')}</p>
              <button type="button" className="btn" disabled={creating} onClick={restoreRecommended}>
                {t('newvm.restoreRecommended')}
              </button>
            </>
          )}
          {revertedNotice && <p className="hint warn" role="status">{t('newvm.revertedNotice')}</p>}

          <div className="grid2">
            <label className="field">
              <span className="field-label">
                {t('newvm.vram')}
                {!expertMode && <em className="newvm-recommended">{t('newvm.recommended')}</em>}
              </span>
              <input type="number" min={1} max={256} value={vramMb} disabled={creating} readOnly={!expertMode}
                onChange={(event) => { mark('vramMb'); setVramMb(numberValue(event.currentTarget.value)); }} />
            </label>
            <label className="field">
              <span className="field-label">
                {t('newvm.firmware')}
                {!expertMode && <em className="newvm-recommended">{t('newvm.recommended')}</em>}
              </span>
              <select value={firmware} disabled={creating || !expertMode}
                onChange={(event) => changeFirmware(event.currentTarget.value as VmFirmware)}>
                <option value="bios">{t('newvm.firmwareBios')}</option>
                <option value="efi">{t('newvm.firmwareEfi')}</option>
              </select>
            </label>
            <label className="field check">
              <input
                type="checkbox"
                checked={tpm}
                disabled={creating || !expertMode || firmware !== 'efi'}
                onChange={(event) => { mark('tpm'); setTpm(event.currentTarget.checked); }}
              />
              {t('newvm.tpm')}
              {!expertMode && <em className="newvm-recommended">{t('newvm.recommended')}</em>}
            </label>
            <label className="field">
              <span className="field-label">
                {t('newvm.graphics')}
                {!expertMode && <em className="newvm-recommended">{t('newvm.recommended')}</em>}
              </span>
              <select value={graphics} disabled={creating || !expertMode}
                onChange={(event) => { mark('graphics'); setGraphics(event.currentTarget.value as VmGraphics); }}>
                <option value="vboxvga">VBoxVGA</option>
                <option value="vboxsvga">VBoxSVGA</option>
                <option value="vmsvga">VMSVGA</option>
              </select>
            </label>
            <label className="field">
              <span className="field-label">
                {t('newvm.diskBus')}
                {!expertMode && <em className="newvm-recommended">{t('newvm.recommended')}</em>}
              </span>
              <select value={diskBus} disabled={creating || !expertMode}
                onChange={(event) => { mark('diskBus'); setDiskBus(event.currentTarget.value as VmDiskBus); }}>
                <option value="ide">IDE</option>
                <option value="sata">SATA</option>
              </select>
            </label>
          </div>
          {advancedWarnings.map((warning) => <p key={warning} className="hint warn">{warning}</p>)}
          {expertMode && <p className="hint">{t('newvm.tpmHint')}</p>}
        </details>

        <div className="field newvm-iso">
          <span>{t('newvm.iso')}</span>
          <div className="newvm-iso-row">
            {isTauri ? (
              <button className="btn" onClick={() => void chooseIso()} disabled={creating || isoBusy}>
                {isoBusy ? t('newvm.choosingIso') : t('newvm.chooseIso')}
              </button>
            ) : (
              <input
                value={isoPath ?? ''}
                disabled={creating}
                spellCheck={false}
                placeholder={t('newvm.isoPlaceholder')}
                onChange={(event) => setIsoPath(event.currentTarget.value || null)}
              />
            )}
            {isoPath && <code className="newvm-iso-name" title={isoPath}>{isoFileName(isoPath)}</code>}
            {isoPath && (
              <button className="btn" onClick={() => setIsoPath(null)} disabled={creating}>
                {t('newvm.removeIso')}
              </button>
            )}
          </div>
          <span className="hint">{t('newvm.isoHint')}</span>
        </div>

        <label className="field check">
          <input
            type="checkbox"
            checked={addToCanvas}
            disabled={creating}
            onChange={(event) => setAddToCanvas(event.currentTarget.checked)}
          />
          {t('newvm.addToCanvas')}
        </label>

        {validationError && <p className="hint err">{validationError}</p>}
        {submitError && <p className="hint err">{submitError}</p>}

        <div className="modal-actions">
          <button className="btn" onClick={requestClose} disabled={creating}>{t('common.cancel')}</button>
          <button
            className="btn primary"
            onClick={() => void submit()}
            disabled={creating || !!validationError || catalogLoading || !!catalogError}
          >
            {creating ? t('newvm.creating') : t('newvm.create')}
          </button>
        </div>
      </div>
    </div>
  );
}
