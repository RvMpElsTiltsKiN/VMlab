import { useEffect, useState } from 'react';
import type { Update } from '@tauri-apps/plugin-updater';
import { useT } from '../i18n';
import { isTauri } from '../lib/ipc';
import { checkForUpdate, checkUpdatesOnStartup } from '../lib/updates';

/** Espera antes de consultar GitHub para no retrasar el arranque visible. */
const CHECK_DELAY_MS = 5000;

/**
 * Banner discreto con la actualización disponible. No descarga ni instala nada
 * hasta que el usuario pulsa «Actualizar».
 */
export function UpdateBanner() {
  const t = useT();
  const [update, setUpdate] = useState<Update | null>(null);
  const [hidden, setHidden] = useState(false);
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState<number | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    if (!isTauri) return;
    const timer = window.setTimeout(() => {
      if (!checkUpdatesOnStartup()) return;
      void checkForUpdate().then((next) => {
        if (next) setUpdate(next);
      });
    }, CHECK_DELAY_MS);
    return () => window.clearTimeout(timer);
  }, []);

  async function install() {
    if (!update || busy) return;
    setBusy(true);
    setFailed(false);
    try {
      let total = 0;
      let received = 0;
      await update.downloadAndInstall((event) => {
        if (event.event === 'Started') {
          total = event.data.contentLength ?? 0;
          setProgress(total > 0 ? 0 : null);
        } else if (event.event === 'Progress') {
          received += event.data.chunkLength;
          setProgress(total > 0 ? Math.min(1, received / total) : null);
        } else {
          setProgress(1);
        }
      });
      const { relaunch } = await import('@tauri-apps/plugin-process');
      await relaunch();
    } catch {
      setBusy(false);
      setProgress(null);
      setFailed(true);
    }
  }

  if (!update || hidden) return null;

  const percent = progress === null ? null : Math.round(progress * 100);

  return (
    <aside className="update-banner" role="status" aria-live="polite">
      <h3>{t('update.available', { version: update.version })}</h3>
      {update.body && <p className="update-notes">{update.body}</p>}
      {busy && (
        <>
          {percent !== null && (
            <div className="update-progress" aria-hidden="true">
              <i style={{ width: `${percent}%` }} />
            </div>
          )}
          <p className="update-status">
            {percent !== null ? t('update.progress', { percent }) : t('update.installing')}
          </p>
        </>
      )}
      {failed && <p className="update-status error">{t('update.installError')}</p>}
      <div className="update-actions">
        <button className="btn" disabled={busy} onClick={() => setHidden(true)}>
          {t('update.later')}
        </button>
        <button className="btn primary" disabled={busy} onClick={() => void install()}>
          {busy ? t('update.installing') : t('update.install')}
        </button>
      </div>
    </aside>
  );
}
