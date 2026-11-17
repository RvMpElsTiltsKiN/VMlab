import { useEffect, useState } from 'react';
import { useT, type TKey, type Translate } from '../i18n';
import { diagnoseProbes, endpointProbe } from '../lib/diagnostics';
import { useLab } from '../store/useLab';
import type { LinkDiagnostics, MachineNode, NodeIssue, NodeProbe } from '../types';

type Mark = 'ok' | 'fail' | 'unknown';
type StepKey = 'power' | 'guestAdditions' | 'adapter' | 'ipv4' | 'subnet';

interface Step {
  key: StepKey;
  mark: Mark;
  value?: string;
}

const ROW_KEYS: Record<StepKey, TKey> = {
  power: 'cable.row.power',
  guestAdditions: 'cable.row.guestAdditions',
  adapter: 'cable.row.adapter',
  ipv4: 'cable.row.ipv4',
  subnet: 'cable.row.subnet',
};

const ISSUE_KEYS: Record<NodeIssue['code'], TKey> = {
  notRunning: 'cable.issue.notRunning',
  noGuestAdditions: 'cable.issue.noGuestAdditions',
  adapterNotOnSegment: 'cable.issue.adapterNotOnSegment',
  noIpv4: 'cable.issue.noIpv4',
  probeFailed: 'cable.issue.probeFailed',
};

const MARK_TEXT: Record<Mark, string> = { ok: '✓', fail: '✗', unknown: '—' };

const MARK_KEYS: Record<Mark, TKey> = {
  ok: 'cable.state.ok',
  fail: 'cable.state.fail',
  unknown: 'cable.state.unknown',
};

/**
 * Filas de un extremo. Un fallo bloquea las comprobaciones siguientes: se
 * muestran como «sin comprobar» en vez de afirmar que también fallan.
 */
function endpointSteps(probe: NodeProbe): { steps: Step[]; detail?: string } {
  const has = (code: NodeIssue['code']) => probe.issues.some((issue) => issue.code === code);
  const steps: Step[] = [];
  let blocked = false;

  const push = (key: StepKey, fails: boolean, value?: string) => {
    if (blocked) {
      steps.push({ key, mark: 'unknown' });
      return;
    }
    if (fails) {
      steps.push({ key, mark: 'fail' });
      blocked = true;
      return;
    }
    steps.push({ key, mark: 'ok', value });
  };

  push('power', has('notRunning'));
  // Sin sondeo no se puede saber nada más del extremo.
  if (!blocked && has('probeFailed')) blocked = true;
  if (probe.kind === 'vbox') push('guestAdditions', has('noGuestAdditions'));
  // Un adaptador en puente puede seguir teniendo IPv4: se informa el aviso sin
  // ocultar las comprobaciones que sí se pudieron leer.
  if (has('adapterNotOnSegment')) {
    if (blocked) steps.push({ key: 'adapter', mark: 'unknown' });
    else {
      steps.push({ key: 'adapter', mark: 'fail' });
      if (!probe.ipv4) blocked = true;
    }
  } else {
    push('adapter', false);
  }
  push('ipv4', has('noIpv4'), probe.ipv4 ?? undefined);

  const ipv4 = steps[steps.length - 1];
  if (ipv4.key === 'ipv4' && ipv4.mark === 'ok' && !probe.ipv4) ipv4.mark = 'unknown';

  const failed = probe.issues.find((issue) => issue.code === 'probeFailed');
  return {
    steps,
    detail: failed?.code === 'probeFailed' ? failed.detail : undefined,
  };
}

/** La subred solo se comprueba si ambos extremos tienen IPv4 y máscara. */
function subnetStep(aSteps: Step[], bSteps: Step[], a: NodeProbe, b: NodeProbe, diag: LinkDiagnostics): Step {
  const hasIp = (steps: Step[]) => steps.some((step) => step.key === 'ipv4' && step.mark === 'ok');
  const known = hasIp(aSteps) && hasIp(bSteps) && !!a.ipv4 && !!a.netmask && !!b.ipv4 && !!b.netmask;
  if (!known) return { key: 'subnet', mark: 'unknown' };
  return { key: 'subnet', mark: diag.differentSubnet ? 'fail' : 'ok' };
}

/** Primer problema del cable, en una línea. */
function summaryText(diag: LinkDiagnostics, translate: Translate): string {
  const first = diag.issues[0];
  if (first) return translate(ISSUE_KEYS[first.code]);
  if (diag.differentSubnet) return translate('cable.issue.differentSubnet');
  return translate('cable.issue.noConnection');
}

function Row({ step, live }: { step: Step; live: boolean }) {
  const t = useT();
  // Con el cable verde, un aviso se muestra informativo (ámbar), nunca en rojo.
  const tone = step.mark === 'fail' && live ? 'info' : step.mark;
  return (
    <li className={`cable-row ${tone}`}>
      <span className="cable-mark" aria-hidden="true">{MARK_TEXT[step.mark]}</span>
      <span className="cable-row-label">
        {t(ROW_KEYS[step.key])}
        <span className="sr-only"> ({t(MARK_KEYS[step.mark])})</span>
      </span>
      {step.value && <span className="cable-row-value mono">{step.value}</span>}
    </li>
  );
}

function Endpoint({ node, steps, detail, live }: {
  node: MachineNode;
  steps: Step[];
  detail?: string;
  live: boolean;
}) {
  return (
    <div className="cable-end">
      <div className="cable-end-name">{node.data.label}</div>
      <ul className="cable-rows">
        {steps.map((step) => <Row key={step.key} step={step} live={live} />)}
      </ul>
      {detail && <p className="hint err cable-detail">{detail}</p>}
    </div>
  );
}

function useCableDiagnostics(a: MachineNode, b: MachineNode) {
  const probes = useLab((s) => s.probes);
  const snapshot = useLab((s) => s.snapshot);
  const probeA = endpointProbe(a, probes, snapshot);
  const probeB = endpointProbe(b, probes, snapshot);
  const key = probeA && probeB
    ? `${JSON.stringify(probeA)}|${JSON.stringify(probeB)}`
    : null;

  const [loaded, setLoaded] = useState<{ key: string; data: LinkDiagnostics } | null>(null);
  const [failed, setFailed] = useState<{ key: string; detail: string } | null>(null);

  useEffect(() => {
    if (!probeA || !probeB || !key) return;
    let alive = true;
    void diagnoseProbes(probeA, probeB)
      .then((data) => { if (alive) setLoaded({ key, data }); })
      .catch((error: unknown) => {
        if (!alive) return;
        const detail = typeof error === 'string' ? error : error instanceof Error ? error.message : String(error);
        setFailed({ key, detail });
      });
    return () => { alive = false; };
    // `key` resume el contenido de los dos sondeos; los objetos se recrean en cada render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);

  return {
    probes: probeA && probeB ? { a: probeA, b: probeB } : null,
    diag: loaded && loaded.key === key ? loaded.data : null,
    error: failed && failed.key === key ? failed.detail : null,
  };
}

/** Comprobaciones del cable: cada extremo por separado y la subred común. */
export function CableChecks({ a, b }: { a: MachineNode; b: MachineNode }) {
  const t = useT();
  const { probes, diag, error } = useCableDiagnostics(a, b);

  if (error) {
    return <p className="hint err">{t('cable.diagnoseError', { detail: error })}</p>;
  }
  if (!probes || !diag) {
    return <p className="hint cable-wait" role="status">{t('cable.checking')}</p>;
  }

  const live = diag.live;
  const aSteps = endpointSteps(probes.a);
  const bSteps = endpointSteps(probes.b);
  const shared = subnetStep(aSteps.steps, bSteps.steps, probes.a, probes.b, diag);

  return (
    <div className={`cable-checks ${live ? 'live' : 'off'}`}>
      <div className="cable-head">
        <span className={`net-live ${live ? 'ok' : 'bad'}`}>
          <i />
          {live ? t('cable.connected') : t('cable.disconnected')}
        </span>
      </div>
      {!live && <p className="hint cable-summary">{summaryText(diag, t)}</p>}
      <Endpoint node={a} steps={aSteps.steps} detail={aSteps.detail} live={live} />
      <Endpoint node={b} steps={bSteps.steps} detail={bSteps.detail} live={live} />
      <ul className="cable-rows cable-shared">
        <Row step={shared} live={live} />
      </ul>
    </div>
  );
}
