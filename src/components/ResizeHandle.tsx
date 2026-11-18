import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
} from 'react';

export const MIN_PANEL_WIDTH = 240;
export const MAX_PANEL_WIDTH = 560;

const clamp = (value: number) => Math.min(MAX_PANEL_WIDTH, Math.max(MIN_PANEL_WIDTH, Math.round(value)));

interface ResizeHandleProps {
  /** `left`: separador de la barra lateral; `right`: separador del inspector. */
  side: 'left' | 'right';
  width: number;
  defaultWidth: number;
  label: string;
  onChange: (width: number) => void;
  onActiveChange?: (active: boolean) => void;
}

/**
 * Tirador entre un panel y el lienzo. Mantiene el pointer capture mientras se
 * arrastra para que el cursor no se pierda al pasar por encima del lienzo.
 */
export function ResizeHandle({ side, width, defaultWidth, label, onChange, onActiveChange }: ResizeHandleProps) {
  const drag = useRef<{ pointerId: number; startX: number; startWidth: number } | null>(null);
  const [active, setActive] = useState(false);
  const direction = side === 'left' ? 1 : -1;

  const change = useCallback((next: number) => onChange(clamp(next)), [onChange]);

  useEffect(() => () => {
    if (drag.current) onActiveChange?.(false);
  }, [onActiveChange]);

  useEffect(() => {
    if (!active) return;
    const previousCursor = document.body.style.cursor;
    const previousSelect = document.body.style.userSelect;
    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';
    return () => {
      document.body.style.cursor = previousCursor;
      document.body.style.userSelect = previousSelect;
    };
  }, [active]);

  function onPointerDown(event: ReactPointerEvent<HTMLDivElement>) {
    if (event.button !== 0) return;
    event.preventDefault();
    drag.current = { pointerId: event.pointerId, startX: event.clientX, startWidth: width };
    event.currentTarget.setPointerCapture(event.pointerId);
    setActive(true);
    onActiveChange?.(true);
  }

  function onPointerMove(event: ReactPointerEvent<HTMLDivElement>) {
    const start = drag.current;
    if (!start || start.pointerId !== event.pointerId) return;
    change(start.startWidth + direction * (event.clientX - start.startX));
  }

  function endDrag(event: ReactPointerEvent<HTMLDivElement>) {
    if (!drag.current || drag.current.pointerId !== event.pointerId) return;
    try {
      event.currentTarget.releasePointerCapture(event.pointerId);
    } catch {
      // El navegador ya ha liberado el capture (p. ej. pointercancel).
    }
    drag.current = null;
    setActive(false);
    onActiveChange?.(false);
  }

  function onKeyDown(event: ReactKeyboardEvent<HTMLDivElement>) {
    let delta = 0;
    if (event.key === 'ArrowRight') delta = direction * (event.shiftKey ? 48 : 16);
    else if (event.key === 'ArrowLeft') delta = -direction * (event.shiftKey ? 48 : 16);
    else if (event.key === 'Home') {
      event.preventDefault();
      change(defaultWidth);
      return;
    } else return;
    event.preventDefault();
    change(width + delta);
  }

  return (
    <div
      className={`resize-handle ${side === 'left' ? 'resize-left' : 'resize-right'} ${active ? 'active' : ''}`}
      role="separator"
      aria-orientation="vertical"
      aria-label={label}
      aria-valuemin={MIN_PANEL_WIDTH}
      aria-valuemax={MAX_PANEL_WIDTH}
      aria-valuenow={Math.round(width)}
      tabIndex={0}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={endDrag}
      onPointerCancel={endDrag}
      onDoubleClick={() => change(defaultWidth)}
      onKeyDown={onKeyDown}
    />
  );
}
