import type { Translate } from '../i18n';
import type { MachineData, PowerState, Snapshot } from '../types';

export const EMPTY_SNAPSHOT: Snapshot = { states: {}, vboxOk: false, dockerOk: false };

/** Estado efectivo de un nodo a partir del último snapshot del backend. */
export function resolveState(data: Pick<MachineData, 'kind' | 'target'>, snap: Snapshot): PowerState {
  const known = snap.states[`${data.kind}:${data.target}`];
  if (known) return known;
  const toolOk = data.kind === 'vbox' ? snap.vboxOk : snap.dockerOk;
  return toolOk ? 'missing' : 'unknown';
}

/** Etiqueta traducible del estado. Recibe `t` para no fijar el idioma al cargar el módulo. */
export function stateLabel(state: PowerState, t: Translate): string {
  switch (state) {
    case 'running':
      return t('state.running');
    case 'stopped':
      return t('state.stopped');
    case 'paused':
      return t('state.paused');
    case 'missing':
      return t('state.missing');
    case 'unknown':
      return t('state.unknown');
  }
}
