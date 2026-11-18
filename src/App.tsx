import { useCallback, useEffect, useState, type CSSProperties } from 'react';
import { Canvas } from './components/Canvas';
import { Inspector } from './components/Inspector';
import { ResizeHandle, MAX_PANEL_WIDTH, MIN_PANEL_WIDTH } from './components/ResizeHandle';
import { Sidebar } from './components/Sidebar';
import { TitleBar } from './components/TitleBar';
import { UpdateBanner } from './components/UpdateBanner';
import { useT } from './i18n';
import { useLab, type Toast } from './store/useLab';

const PANEL_DEFAULTS = { sidebar: 264, inspector: 326 } as const;
const PANEL_STORAGE = {
  sidebar: 'vmlab.layout.sidebarWidth',
  inspector: 'vmlab.layout.inspectorWidth',
} as const;
type Panel = keyof typeof PANEL_DEFAULTS;

const clampWidth = (value: number) =>
  Math.min(MAX_PANEL_WIDTH, Math.max(MIN_PANEL_WIDTH, Math.round(value)));

/** Ancho persistido por panel. Si localStorage falla, se usa el ancho actual. */
function usePanelWidth(panel: Panel) {
  const [width, setWidth] = useState(() => {
    try {
      const stored = localStorage.getItem(PANEL_STORAGE[panel]);
      if (stored === null) return PANEL_DEFAULTS[panel];
      const parsed = Number(stored);
      return Number.isFinite(parsed) ? clampWidth(parsed) : PANEL_DEFAULTS[panel];
    } catch {
      return PANEL_DEFAULTS[panel];
    }
  });

  const update = useCallback((next: number) => {
    const value = clampWidth(next);
    setWidth(value);
    try {
      localStorage.setItem(PANEL_STORAGE[panel], String(value));
    } catch {
      // Modo privado o almacenamiento lleno: el ancho sigue funcionando en memoria.
    }
  }, [panel]);

  return [width, update] as const;
}

/** Aviso persistente (error o aviso de Apply): copiable y con contador si se repite. */
function StickyToast({ toast, dismiss }: { toast: Toast; dismiss: (id: number) => void }) {
  const t = useT();
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
    <div className={`toast ${toast.kind}`} role="alert">
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
        toast.kind === 'error' || toast.kind === 'warning' ? (
          <StickyToast key={toast.id} toast={toast} dismiss={dismiss} />
        ) : (
          <div
            key={toast.id}
            className={`toast ${toast.kind}`}
            role="status"
            aria-live="polite"
            onClick={() => dismiss(toast.id)}
          >
            <div className="toast-text">{toast.text}</div>
            {toast.action && (
              <div className="toast-actions">
                <button
                  type="button"
                  className="toast-btn"
                  onClick={(event) => {
                    event.stopPropagation();
                    toast.action?.onClick();
                    dismiss(toast.id);
                  }}
                >
                  {toast.action.label}
                </button>
              </div>
            )}
          </div>
        ),
      )}
    </div>
  );
}

export default function App() {
  const t = useT();
  const init = useLab((s) => s.init);
  const [sidebarWidth, setSidebarWidth] = usePanelWidth('sidebar');
  const [inspectorWidth, setInspectorWidth] = usePanelWidth('inspector');
  const [resizing, setResizing] = useState(false);
  useEffect(() => { void init(); }, [init]);

  const workspaceStyle = {
    '--sidebar-w': `${sidebarWidth}px`,
    '--inspector-w': `${inspectorWidth}px`,
  } as CSSProperties;

  return (
    <div className="app">
      <TitleBar />
      <main className={`workspace ${resizing ? 'is-resizing' : ''}`} style={workspaceStyle}>
        <Sidebar />
        <ResizeHandle
          side="left"
          width={sidebarWidth}
          defaultWidth={PANEL_DEFAULTS.sidebar}
          label={t('resize.sidebar')}
          onChange={setSidebarWidth}
          onActiveChange={setResizing}
        />
        <Canvas />
        <ResizeHandle
          side="right"
          width={inspectorWidth}
          defaultWidth={PANEL_DEFAULTS.inspector}
          label={t('resize.inspector')}
          onChange={setInspectorWidth}
          onActiveChange={setResizing}
        />
        <Inspector />
      </main>
      <Toasts />
      <UpdateBanner />
    </div>
  );
}
