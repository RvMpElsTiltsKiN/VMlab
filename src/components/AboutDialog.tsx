import { useEffect, useState } from 'react';
import { getVersion } from '@tauri-apps/api/app';
import packageJson from '../../package.json';
import { useT } from '../i18n';
import { useLab } from '../store/useLab';
import { Logo } from './icons';

const REPO_URL = 'https://github.com/RvMpElsTiltsKiN/vmlab';

/** Ventana «Acerca de»: versión, licencia, autor, repositorio y privacidad. */
export function AboutDialog({ onClose }: { onClose: () => void }) {
  const t = useT();
  const toast = useLab((s) => s.toast);
  const [version, setVersion] = useState(packageJson.version);

  useEffect(() => {
    let active = true;
    void getVersion()
      .then((value) => {
        if (active) setVersion(value);
      })
      .catch(() => {
        /* en el navegador de desarrollo no hay backend Tauri */
      });
    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [onClose]);

  async function copyRepo() {
    try {
      await navigator.clipboard.writeText(REPO_URL);
      toast('ok', t('about.copied'));
    } catch {
      toast('error', t('about.copyError'));
    }
  }

  return (
    <div
      className="modal-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div className="modal about" role="dialog" aria-modal="true" aria-labelledby="about-title">
        <h3 id="about-title">{t('about.title')}</h3>

        <div className="about-head">
          <Logo />
          <div>
            <div className="about-name">VMLab</div>
            {version && <div className="about-version">{t('about.version', { version })}</div>}
          </div>
        </div>

        <p className="about-line">
          {t('about.license')} · {t('about.author')}: RvMpElsTiltsKiN
        </p>

        <div className="about-repo-label">{t('about.repository')}</div>
        <div className="about-repo">
          <code>{REPO_URL}</code>
          <button className="btn" onClick={() => void copyRepo()}>{t('common.copy')}</button>
        </div>

        <p className="hint about-privacy">{t('about.privacy')}</p>

        <div className="modal-actions">
          <button className="btn" onClick={onClose}>{t('common.close')}</button>
        </div>
      </div>
    </div>
  );
}
