import { memo } from 'react';
import { Handle, Position, type NodeProps } from '@xyflow/react';
import { useT } from '../i18n';
import { probeKey } from '../lib/connectivity';
import { linkSummary } from '../lib/link';
import { resolveState, stateLabel } from '../lib/status';
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
  const t = useT();
  const state = useLab((s) => resolveState(data, s.snapshot));
  const busy = useLab((s) => !!s.busy[id]);
  const labsnapBusy = useLab((s) => s.labsnapBusy);
  const act = useLab((s) => s.act);
  const openTerminal = useLab((s) => s.openTerminal);
  const probes = useLab((s) => s.probes);
  const toast = useLab((s) => s.toast);
  const capturing = useLab((s) =>
    s.captures.some((c) => c.running && c.kind === data.kind && c.target === data.target),
  );
  const slot = data.kind === 'vbox' && data.slot === true;
  const running = state === 'running' || state === 'paused';
  const sub = slot ? t('node.unassignedShort') : data.kind === 'docker' ? data.image || t('node.noImage') : data.target.slice(0, 13);
  const link = slot ? '' : linkSummary(data.link);
  const ipv4 = slot ? null : probes[probeKey(data.kind, data.target)]?.ipv4 ?? null;

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
          {ipv4 && (
            <button
              className="node-ip mono nodrag"
              title={t('node.copyIp')}
              onClick={async (e) => {
                e.stopPropagation();
                try {
                  await navigator.clipboard.writeText(ipv4);
                  toast('ok', t('node.ipCopied'));
                } catch {
                  toast('error', t('node.ipCopyError'));
                }
              }}
            >
              {ipv4}
            </button>
          )}
          <div className="node-sub" title={sub}>{data.kind === 'vbox' ? 'VirtualBox' : 'Docker'} · {sub}</div>
        </div>
        {(capturing || link) && (
          <span className="node-badges">
            {capturing && <span className="node-rec" title={t('node.captureRunning')}>REC</span>}
            {link && <span className="node-link" title={t('node.linkConditions', { link })}>{link}</span>}
          </span>
        )}
      </div>
      <div className="node-foot">
        <span className="status"><i className="dot" />{slot ? t('node.unassigned') : busy ? t('node.working') : stateLabel(state, t)}</span>
        <span className="node-actions nodrag">
          <button
            disabled={slot || busy || labsnapBusy || state === 'unknown'}
            onClick={() => act(id, running ? 'stop' : 'start')}
            title={slot ? t('node.assignHint') : running ? t('node.stop') : t('node.start')}
          >
            {running ? <StopIcon /> : <PlayIcon />}
          </button>
          <button
            disabled={slot || busy || labsnapBusy || !running}
            onClick={() => act(id, 'restart')}
            title={slot ? t('node.assignHint') : t('node.restart')}
          >
            <RestartIcon />
          </button>
          {data.kind === 'docker' && state === 'running' && (
            <button
              className="nodrag"
              disabled={busy || labsnapBusy}
              onClick={() => openTerminal(id)}
              title={t('node.terminal')}
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
