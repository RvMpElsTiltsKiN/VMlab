import { useEffect, useState } from 'react';
import { Canvas } from './components/Canvas';
import { Inspector } from './components/Inspector';
import { Sidebar } from './components/Sidebar';
import { TitleBar } from './components/TitleBar';
import { UpdateBanner } from './components/UpdateBanner';
import { t } from './i18n';
import { useLab, type Toast } from './store/useLab';

/** Aviso de error: persistente, copiable y con contador si se repite. */
function ErrorToast({ toast, dismiss }: { toast: Toast; dismiss: (id: number) => void }) {
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 2000);
    return () => clearTimeout(timer);
  }, [copied]);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(toast.text);
      setCopied(true);
    } catch {
      // Sin permiso de portapapeles o fuera de un contexto seguro: no se hace nada.
    }
  };

  return (
    <div className="toast error" role="alert">
      <div className="toast-text">{toast.text}</div>
      <div className="toast-actions">
        {toast.count > 1 && <span className="toast-count" aria-label={`×${toast.count}`}>×{toast.count}</span>}
        <button
          type="button"
          className="toast-btn"
          onClick={() => void copy()}
          aria-label={copied ? t('toast.copied') : t('toast.copy')}
        >
          {copied ? t('toast.copied') : t('toast.copy')}
        </button>
        <button
          type="button"
          className="toast-btn"
          onClick={() => dismiss(toast.id)}
          aria-label={t('toast.dismiss')}
        >
          {t('toast.dismiss')}
        </button>
      </div>
    </div>
  );
}

function Toasts() {
  const toasts = useLab((s) => s.toasts);
  const dismiss = useLab((s) => s.dismissToast);
  return (
    <div className="toasts">
      {toasts.map((toast) =>
        toast.kind === 'error' ? (
          <ErrorToast key={toast.id} toast={toast} dismiss={dismiss} />
        ) : (
          <div
            key={toast.id}
            className={`toast ${toast.kind}`}
            role="status"
            aria-live="polite"
            onClick={() => dismiss(toast.id)}
          >
            {toast.text}
          </div>
        ),
      )}
    </div>
  );
}

export default function App() {
  const init = useLab((s) => s.init);
  useEffect(() => { void init(); }, [init]);

  return (
    <div className="app">
      <TitleBar />
      <main className="workspace">
        <Sidebar />
        <Canvas />
        <Inspector />
      </main>
      <Toasts />
      <UpdateBanner />
    </div>
  );
}
