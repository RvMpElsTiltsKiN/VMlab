import { memo } from 'react';
import { Handle, Position, type NodeProps } from '@xyflow/react';
import { resolveState, STATE_LABEL } from '../lib/status';
import { useLab } from '../store/useLab';
import type { MachineNode } from '../types';
import { ContainerIcon, CubeIcon, PlayIcon, RestartIcon, StopIcon, TerminalIcon } from './icons';

const HANDLES: [string, Position][] = [
  ['t', Position.Top],
  ['r', Position.Right],
  ['b', Position.Bottom],
  ['l', Position.Left],
];

/**
 * Nodo del lienzo. Todos los asideros son de tipo `source` y el lienzo usa `ConnectionMode.Loose`:
 * una red local no tiene dirección, así que se puede conectar desde/hacia cualquier lado.
 */
function MachineNodeViewInner({ id, data, selected }: NodeProps<MachineNode>) {
  const state = useLab((s) => resolveState(data, s.snapshot));
  const busy = useLab((s) => !!s.busy[id]);
  const labsnapBusy = useLab((s) => s.labsnapBusy);
  const act = useLab((s) => s.act);
  const openTerminal = useLab((s) => s.openTerminal);
  const slot = data.kind === 'vbox' && data.slot === true;
  const running = state === 'running' || state === 'paused';
  const sub = slot ? 'sin asignar' : data.kind === 'docker' ? data.image || 'sin imagen' : data.target.slice(0, 13);

  return (
    <div
      className={`node ${data.kind} ${selected ? 'selected' : ''}`}
      data-state={busy ? 'busy' : state}
      data-slot={slot ? 'true' : undefined}
    >
      {HANDLES.map(([hid, pos]) => (
        <Handle key={hid} id={hid} type="source" position={pos} />
      ))}
      <div className="node-head">
        <span className="node-icon">{data.kind === 'vbox' ? <CubeIcon /> : <ContainerIcon />}</span>
        <div className="node-titles">
          <div className="node-title" title={data.label}>{data.label}</div>
          <div className="node-sub" title={sub}>{data.kind === 'vbox' ? 'VirtualBox' : 'Docker'} · {sub}</div>
        </div>
      </div>
      <div className="node-foot">
        <span className="status"><i className="dot" />{slot ? 'Sin asignar' : busy ? 'Trabajando…' : STATE_LABEL[state]}</span>
        <span className="node-actions nodrag">
          <button
            disabled={slot || busy || labsnapBusy || state === 'unknown'}
            onClick={() => act(id, running ? 'stop' : 'start')}
            title={slot ? 'Asigna una VM en el panel derecho' : running ? 'Detener' : 'Iniciar'}
          >
            {running ? <StopIcon /> : <PlayIcon />}
          </button>
          <button
            disabled={slot || busy || labsnapBusy || !running}
            onClick={() => act(id, 'restart')}
            title={slot ? 'Asigna una VM en el panel derecho' : 'Reiniciar'}
          >
            <RestartIcon />
          </button>
          {data.kind === 'docker' && state === 'running' && (
            <button
              className="nodrag"
              disabled={busy || labsnapBusy}
              onClick={() => openTerminal(id)}
              title="Abrir terminal"
            >
              <TerminalIcon />
            </button>
          )}
        </span>
      </div>
    </div>
  );
}

export const MachineNodeView = memo(MachineNodeViewInner);
