import { useEffect, useState } from 'react';
import { useT } from '../i18n';
import { isTauri, windowControls } from '../lib/ipc';
import { useLab } from '../store/useLab';
import { CloseIcon, LockIcon, Logo, MaxIcon, MinIcon, MoonIcon, NetworkIcon, PlayIcon, RestoreIcon, StopIcon, SunIcon, UnlockIcon } from './icons';
import { LabMenu } from './LabMenu';

/**
 * Barra de título propia (la ventana se crea con `decorations: false`).
 * `data-tauri-drag-region` solo actúa sobre el elemento que lo lleva, no sobre sus hijos,
 * por eso los botones siguen siendo clicables y el resto de la barra arrastra la ventana.
 */
export function TitleBar() {
  const t = useT();
  const theme = useLab((s) => s.theme);
  const toggleTheme = useLab((s) => s.toggleTheme);
  const applying = useLab((s) => s.applying);
  const labsnapBusy = useLab((s) => s.labsnapBusy);
  const applyNetwork = useLab((s) => s.applyNetwork);
  const actAll = useLab((s) => s.actAll);
  const canvasLocked = useLab((s) => s.canvasLocked);
  const toggleCanvasLock = useLab((s) => s.toggleCanvasLock);
  const hasNodes = useLab((s) => s.nodes.length > 0);
  const [maximized, setMaximized] = useState(false);

  useEffect(() => {
    const sync = () => windowControls.isMaximized().then(setMaximized);
    sync();
    const off = windowControls.onResized(sync);
    return () => { off.then((f) => f()); };
  }, []);

  return (
    <header className="titlebar" data-tauri-drag-region>
      <div className="brand" data-tauri-drag-region>
        <Logo />
        <span className="brand-name" data-tauri-drag-region>VMLab</span>
      </div>

      <div className="titlebar-actions">
        <LabMenu />
        <button className="btn primary" onClick={applyNetwork} disabled={!hasNodes || applying || labsnapBusy} title={t('titlebar.applyHint')}>
          <NetworkIcon /> {applying ? t('titlebar.applying') : t('titlebar.apply')}
        </button>
        <button className="btn" onClick={() => actAll('start')} disabled={!hasNodes || labsnapBusy} title={t('titlebar.startAllHint')}>
          <PlayIcon /> {t('titlebar.startAll')}
        </button>
        <button className="btn" onClick={() => actAll('stop')} disabled={!hasNodes || labsnapBusy} title={t('titlebar.stopAllHint')}>
          <StopIcon /> {t('titlebar.stopAll')}
        </button>
        <button
          className={`btn lock-toggle ${canvasLocked ? 'on' : ''}`}
          onClick={toggleCanvasLock}
          aria-pressed={canvasLocked}
          title={canvasLocked ? t('titlebar.lockedHint') : t('titlebar.lockHint')}
        >
          {canvasLocked ? <LockIcon /> : <UnlockIcon />} {canvasLocked ? t('titlebar.locked') : t('titlebar.lock')}
        </button>
      </div>

      <div className="titlebar-spacer" data-tauri-drag-region />

      <button
        className="icon-btn"
        onClick={toggleTheme}
        title={theme === 'dark' ? t('titlebar.lightMode') : t('titlebar.darkMode')}
        aria-label={t('titlebar.changeTheme')}
      >
        {theme === 'dark' ? <SunIcon /> : <MoonIcon />}
      </button>

      {isTauri && (
        <div className="win-controls">
          <button onClick={() => windowControls.minimize()} aria-label={t('titlebar.minimize')}><MinIcon /></button>
          <button onClick={() => windowControls.toggleMaximize()} aria-label={maximized ? t('titlebar.restore') : t('titlebar.maximize')}>
            {maximized ? <RestoreIcon /> : <MaxIcon />}
          </button>
          <button className="close" onClick={() => windowControls.close()} aria-label={t('titlebar.close')}><CloseIcon /></button>
        </div>
      )}
    </header>
  );
}
