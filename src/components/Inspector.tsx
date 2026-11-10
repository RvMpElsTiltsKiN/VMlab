import { useEffect, useMemo, useState } from 'react';
import { groupLive, probeKey } from '../lib/connectivity';
import { ipc } from '../lib/ipc';
import { resolveState, STATE_LABEL } from '../lib/status';
import { computeGroups, NET_COLORS } from '../lib/topology';
import { useLab } from '../store/useLab';
import type {
  GuestConfigRequest,
  GuestIpMode,
  GuestProfile,
  GuestReport,
  GuestSystem,
  MachineData,
  MachineNode,
  NetMode,
  NodeNet,
} from '../types';
import { TrashIcon } from './icons';

const DEFAULT_NET: NodeNet = { mode: 'intnet', adapter: 2 };

const NET_MODES: { value: NetMode; label: string }[] = [
  { value: 'intnet', label: 'Red interna (VMLab)' },
  { value: 'nat', label: 'NAT' },
  { value: 'bridged', label: 'Adaptador puente' },
  { value: 'hostonly', label: 'Solo anfitrión' },
  { value: 'natnetwork', label: 'Red NAT' },
  { value: 'none', label: 'Sin conectar' },
];

const CLIENT_SYSTEMS: { value: GuestSystem; label: string }[] = [
  { value: 'netplan', label: 'Netplan' },
  { value: 'networkManager', label: 'NetworkManager' },
  { value: 'systemdNetworkd', label: 'systemd-networkd' },
  { value: 'ifupdown', label: 'ifupdown (/etc/network/interfaces)' },
  { value: 'wicked', label: 'wicked (SUSE)' },
  { value: 'dhcpcd', label: 'dhcpcd' },
  { value: 'connman', label: 'ConnMan' },
  { value: 'networkScripts', label: 'network-scripts (RHEL/CentOS)' },
];

const errText = (e: unknown) => (typeof e === 'string' ? e : e instanceof Error ? e.message : JSON.stringify(e));

function modeHint(mode: NetMode): string {
  switch (mode) {
    case 'intnet':
      return 'Red aislada entre las VMs del lienzo. VMLab crea la red y su DHCP integrado.';
    case 'nat':
      return 'Salida a internet y a la red del host mediante NAT. No comparte red con otras VMs.';
    case 'bridged':
      return 'La VM aparece como un equipo más en la red física del host. Indica la interfaz del host.';
    case 'hostonly':
      return 'Red privada entre la VM y el host. Indica el adaptador solo-anfitrión (p. ej. vboxnet0).';
    case 'natnetwork':
      return 'Varias VMs comparten una red NAT con salida a internet. Indica el nombre de la red NAT.';
    case 'none':
      return 'Adaptador desconectado. La VM no tendrá red por este adaptador.';
  }
}

function SlotPanel({ node }: { node: MachineNode }) {
  const inventory = useLab((s) => s.inventory);
  const nodes = useLab((s) => s.nodes);
  const updateNode = useLab((s) => s.updateNode);
  const assigned = new Set(
    nodes
      .filter((n) => n.id !== node.id && n.data.kind === 'vbox' && !!n.data.target)
      .map((n) => n.data.target),
  );
  const available = (inventory?.vbox ?? []).filter((vm) => !assigned.has(vm.target));

  return (
    <div className="slot-box">
      <div className="side-title"><span>Hueco de máquina virtual</span></div>
      <label className="field">Asignar una de tus VM
        <select
          value=""
          disabled={!available.length}
          onChange={(e) => {
            const vm = available.find((v) => v.target === e.target.value);
            if (vm) updateNode(node.id, { target: vm.target, label: vm.label, slot: false });
          }}
        >
          <option value="">{available.length ? 'Elige una VM…' : 'No hay VM disponibles'}</option>
          {available.map((vm) => <option key={vm.target} value={vm.target}>{vm.label}</option>)}
        </select>
      </label>
      {!available.length && (
        <p className="hint">
          {inventory
            ? 'Todas tus VM ya están en el lienzo. Crea una nueva en VirtualBox para asignarla.'
            : 'No se pudo leer el inventario de VirtualBox.'}
        </p>
      )}
      {available.length > 0 && <p className="hint">Al asignar una VM se rellena su UUID y el nombre del nodo.</p>}
    </div>
  );
}

function NetworkModePanel({ node, hasLinks }: { node: MachineNode; hasLinks: boolean }) {
  const updateNode = useLab((s) => s.updateNode);
  const net = node.data.net ?? DEFAULT_NET;
  const set = (patch: Partial<NodeNet>) => updateNode(node.id, { net: { ...net, ...patch } });

  return (
    <div className="net-mode">
      <div className="side-title"><span>Red · adaptador {net.adapter}</span></div>
      <div className="grid2">
        <label className="field">Modo
          <select value={net.mode} onChange={(e) => set({ mode: e.target.value as NetMode })}>
            {NET_MODES.map((m) => <option key={m.value} value={m.value}>{m.label}</option>)}
          </select>
        </label>
        <label className="field">Adaptador
          <select value={net.adapter} onChange={(e) => set({ adapter: Number(e.target.value) })}>
            {[1, 2, 3, 4].map((n) => <option key={n} value={n}>NIC {n}</option>)}
          </select>
        </label>
      </div>

      {net.adapter === 1 && (
        <p className="hint warn">El adaptador 1 suele ser el NAT de la VM. VMLab no lo toca salvo que lo elijas aquí.</p>
      )}

      {net.mode === 'bridged' && (
        <label className="field">Interfaz del host
          <input
            value={net.bridge ?? ''}
            onChange={(e) => set({ bridge: e.target.value })}
            placeholder="Wi-Fi, Ethernet o eth0"
            spellCheck={false}
            className="mono"
          />
        </label>
      )}
      {net.mode === 'intnet' && (
        <label className="field">Nombre de la red interna <span className="muted">(opcional)</span>
          <input
            value={net.name ?? ''}
            onChange={(e) => set({ name: e.target.value })}
            placeholder={hasLinks ? 'La red la define el lienzo' : 'vmlab-xxxxxxxx'}
            spellCheck={false}
            className="mono"
          />
        </label>
      )}
      {(net.mode === 'hostonly' || net.mode === 'natnetwork') && (
        <label className="field">{net.mode === 'hostonly' ? 'Adaptador solo-anfitrión' : 'Nombre de la red NAT'}
          <input
            value={net.name ?? ''}
            onChange={(e) => set({ name: e.target.value })}
            placeholder={net.mode === 'hostonly' ? 'vboxnet0' : 'NatNetwork'}
            spellCheck={false}
            className="mono"
          />
        </label>
      )}

      <p className="hint">{modeHint(net.mode)}</p>
      {net.mode !== 'intnet' && hasLinks && (
        <p className="hint warn">Este nodo tiene conexiones dibujadas, pero en modo «{NET_MODES.find((m) => m.value === net.mode)?.label}» no estará en la red interna del lienzo.</p>
      )}
      {net.mode === 'intnet' && hasLinks && net.name && (
        <p className="hint">Si el nodo está conectado manda la red del lienzo; este nombre solo se usa cuando está aislado.</p>
      )}
      <p className="hint">Se aplica con <b>Aplicar red</b>. La VM debe estar apagada.</p>
    </div>
  );
}

function GuestPanel({ node, state }: { node: MachineNode; state: string }) {
  const updateNode = useLab((s) => s.updateNode);
  const toast = useLab((s) => s.toast);
  const labsnapBusy = useLab((s) => s.labsnapBusy);
  const [user, setUser] = useState(node.data.guestUser ?? '');
  const [password, setPassword] = useState('');
  const [profile, setProfile] = useState<GuestProfile | null>(null);
  const [detecting, setDetecting] = useState(false);
  const [applying, setApplying] = useState(false);
  const [report, setReport] = useState<GuestReport | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Solo se reinicia al cambiar de nodo: no se borra la contraseña al escribir el usuario.
  useEffect(() => {
    setUser(node.data.guestUser ?? '');
    setPassword('');
    setProfile(null);
    setReport(null);
    setError(null);
  }, [node.id]); // eslint-disable-line react-hooks/exhaustive-deps

  const mode: GuestIpMode = node.data.guestMode ?? 'dhcp';
  const system: GuestSystem = node.data.guestSystem ?? 'auto';
  const iface = node.data.guestIface ?? '';
  const canProbe = state === 'running' && !!user && !!password && !detecting && !applying && !labsnapBusy;
  const canApply = state === 'running' && !!user && !!password && !!iface && !detecting && !applying && !labsnapBusy;

  const setData = (patch: Partial<MachineData>) => updateNode(node.id, patch);

  const systems = profile
    ? profile.systems.filter((s) => (mode === 'server' ? s.kind === 'server' : s.kind === 'client'))
    : [];
  const serverAvailable = !profile || profile.systems.some((s) => s.kind === 'server');

  const resolveSystem = (): GuestSystem => {
    if (mode === 'server') return 'dhcpd';
    if (system !== 'auto' && system !== 'dhcpd') return system;
    const recommended = profile?.systems.find((s) => s.kind === 'client' && s.recommended)
      ?? profile?.systems.find((s) => s.kind === 'client');
    return recommended?.system ?? 'auto';
  };

  const detect = async () => {
    if (!user || !password) return toast('error', 'Escribe el usuario y la contraseña de la VM');
    setDetecting(true);
    setError(null);
    setReport(null);
    try {
      const p = await ipc.guestProbe(node.data.target, user, password);
      setProfile(p);
      const detectedIface = p.interfaces.some((i) => i.name === iface) ? iface : p.interfaces[0]?.name ?? '';
      const prefs = p.systems.filter((s) => s.kind === 'client');
      const detectedSystem = prefs.find((s) => s.recommended)?.system ?? prefs[0]?.system ?? 'auto';
      updateNode(node.id, { guestUser: user, guestIface: detectedIface, guestSystem: detectedSystem });
      if (!p.guestAdditions) setError('No se detectaron las Guest Additions. Instálalas dentro de la VM.');
    } catch (e) {
      setError(errText(e));
    } finally {
      setDetecting(false);
    }
  };

  const apply = async () => {
    if (!user || !password) return toast('error', 'Escribe el usuario y la contraseña de la VM');
    if (!iface) return toast('error', 'Indica la interfaz de red de la VM (p. ej. eth0)');
    const resolved = resolveSystem();
    if (mode === 'server' && resolved !== 'dhcpd') return toast('error', 'Elige el servidor DHCP ISC para el modo servidor');

    setApplying(true);
    setError(null);
    setReport(null);
    try {
      const dns = (node.data.guestDns ?? '').split(/[\s,;]+/).filter(Boolean);
      const req: GuestConfigRequest = {
        target: node.data.target,
        username: user,
        password,
        system: resolved,
        interface: iface,
        ipMode: mode,
        address: node.data.guestAddress || undefined,
        prefix: node.data.guestPrefix ?? undefined,
        gateway: node.data.guestGateway || undefined,
        dns,
        subnet: node.data.guestSubnet || undefined,
        rangeStart: node.data.guestRangeStart || undefined,
        rangeEnd: node.data.guestRangeEnd || undefined,
      };
      const r = await ipc.guestApply(req);
      setReport(r);
      toast('ok', `Configuración escrita con ${r.system}`);
    } catch (e) {
      setError(errText(e));
      toast('error', errText(e));
    } finally {
      setApplying(false);
    }
  };

  return (
    <div className="guest-box">
      <div className="side-title"><span>Dentro de la VM · Linux</span></div>
      <p className="hint">Escribe la configuración de red sin abrir la consola. Requiere <b>Guest Additions</b> y un usuario de la VM.</p>

      <div className="grid2">
        <label className="field">Usuario
          <input value={user} onChange={(e) => setUser(e.target.value)} spellCheck={false} autoComplete="off" className="mono" />
        </label>
        <label className="field">Contraseña
          <input
            type="password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            autoComplete="off"
            placeholder="no se guarda"
          />
        </label>
      </div>

      <button className="btn" disabled={!canProbe} onClick={detect}>
        {detecting ? 'Detectando…' : 'Detectar sistema de red'}
      </button>
      {state !== 'running' && <p className="hint warn">Inicia la VM para poder detectar y escribir su configuración.</p>}

      {profile && (
        <div className="guest-detected">
          <p className="hint"><b>{profile.os}</b> · {profile.kernel}</p>
          <p className="hint">
            {profile.guestAdditions ? 'Guest Additions OK' : 'Guest Additions no detectadas'}
            {' · '}{profile.isRoot ? 'usuario root' : profile.sudo ? 'sudo disponible' : 'sin sudo'}
          </p>
          <p className="hint mono">
            {profile.systems.filter((s) => s.kind === 'client').map((s) => `${s.label}${s.active ? ' ●' : ''} → ${s.configPath}`).join('  |  ')}
          </p>
        </div>
      )}

      <div className="grid2">
        <label className="field">Sistema
          <select
            value={mode === 'server' ? 'dhcpd' : system}
            onChange={(e) => setData({ guestSystem: e.target.value as GuestSystem })}
            disabled={mode === 'server'}
          >
            {mode === 'server' ? (
              <option value="dhcpd">Servidor DHCP ISC (dhcpd.conf)</option>
            ) : (
              <>
                <option value="auto">Automático (recomendado)</option>
                {(systems.length ? systems : CLIENT_SYSTEMS.map((s) => ({ system: s.value, label: s.label }))).map((s) => (
                  <option key={s.system} value={s.system}>{s.label}</option>
                ))}
              </>
            )}
          </select>
        </label>
        <label className="field">Interfaz
          <input
            list="vmlab-ifaces"
            value={iface}
            onChange={(e) => setData({ guestIface: e.target.value })}
            placeholder="eth0"
            spellCheck={false}
            className="mono"
          />
          <datalist id="vmlab-ifaces">
            {profile?.interfaces.map((i) => <option key={i.name} value={i.name}>{i.ipv4 ?? i.state ?? ''}</option>)}
          </datalist>
        </label>
      </div>

      <div className="seg" role="group" aria-label="Modo de IP">
        <button className={mode === 'dhcp' ? 'on' : ''} onClick={() => setData({ guestMode: 'dhcp', guestSystem: system === 'dhcpd' ? 'auto' : system })}>DHCP</button>
        <button className={mode === 'static' ? 'on' : ''} onClick={() => setData({ guestMode: 'static', guestSystem: system === 'dhcpd' ? 'auto' : system })}>Estática</button>
        <button
          className={mode === 'server' ? 'on' : ''}
          disabled={!serverAvailable}
          title={serverAvailable ? 'Escribe dhcpd.conf para que la VM sea el servidor DHCP' : 'No se detectó dhcpd en la VM'}
          onClick={() => setData({ guestMode: 'server', guestSystem: 'dhcpd' })}
        >
          Servidor DHCP
        </button>
      </div>

      {mode === 'static' && (
        <div className="grid3">
          <label className="field">IP
            <input value={node.data.guestAddress ?? ''} onChange={(e) => setData({ guestAddress: e.target.value })} placeholder="10.0.0.5" spellCheck={false} className="mono" />
          </label>
          <label className="field">Prefijo
            <input type="number" min={1} max={32} value={node.data.guestPrefix ?? 24} onChange={(e) => setData({ guestPrefix: Number(e.target.value) })} />
          </label>
          <label className="field">Puerta
            <input value={node.data.guestGateway ?? ''} onChange={(e) => setData({ guestGateway: e.target.value })} placeholder="10.0.0.1" spellCheck={false} className="mono" />
          </label>
        </div>
      )}

      {mode === 'server' && (
        <>
          <div className="grid2">
            <label className="field">Subred
              <input value={node.data.guestSubnet ?? ''} onChange={(e) => setData({ guestSubnet: e.target.value })} placeholder="10.77.5.0/24" spellCheck={false} className="mono" />
            </label>
            <label className="field">Puerta
              <input value={node.data.guestGateway ?? ''} onChange={(e) => setData({ guestGateway: e.target.value })} placeholder="10.77.5.1" spellCheck={false} className="mono" />
            </label>
          </div>
          <div className="grid2">
            <label className="field">Rango desde
              <input value={node.data.guestRangeStart ?? ''} onChange={(e) => setData({ guestRangeStart: e.target.value })} placeholder="10.77.5.100" spellCheck={false} className="mono" />
            </label>
            <label className="field">Rango hasta
              <input value={node.data.guestRangeEnd ?? ''} onChange={(e) => setData({ guestRangeEnd: e.target.value })} placeholder="10.77.5.200" spellCheck={false} className="mono" />
            </label>
          </div>
          <p className="hint warn">Se escribe <span className="mono">dhcpd.conf</span>. Desactiva el DHCP integrado de VirtualBox en esa red para no tener dos servidores.</p>
        </>
      )}

      {mode !== 'server' && (
        <label className="field">DNS <span className="muted">(separados por comas)</span>
          <input value={node.data.guestDns ?? ''} onChange={(e) => setData({ guestDns: e.target.value })} placeholder="1.1.1.1, 8.8.8.8" spellCheck={false} className="mono" />
        </label>
      )}

      <button className="btn primary" disabled={!canApply} onClick={apply}>
        {applying ? 'Escribiendo…' : 'Escribir configuración'}
      </button>

      {error && <p className="hint err">{error}</p>}
      {report && (
        <div className="report">
          <p className="hint"><b>{report.system}</b> · {report.files.length} fichero(s)</p>
          {report.files.map((f) => <p key={f} className="hint mono">✓ {f}</p>)}
          {report.messages.map((m, i) => <p key={`m${i}`} className="hint">{m}</p>)}
          {report.warnings.map((w, i) => <p key={`w${i}`} className="hint warn">{w}</p>)}
        </div>
      )}
    </div>
  );
}

function TerminalPanel({ node, state }: { node: MachineNode; state: string }) {
  const openTerminal = useLab((s) => s.openTerminal);
  const updateNode = useLab((s) => s.updateNode);
  const probes = useLab((s) => s.probes);
  const labsnapBusy = useLab((s) => s.labsnapBusy);
  const isDocker = node.data.kind === 'docker';
  const ipv4 = isDocker ? null : probes[probeKey('vbox', node.data.target)]?.ipv4 ?? null;
  const running = state === 'running';

  return (
    <div className="terminal-box">
      <div className="side-title"><span>Terminal</span></div>
      {isDocker ? (
        <>
          <p className="hint">Abre una consola dentro del contenedor con <span className="mono">docker exec -it</span>.</p>
          <button className="btn" disabled={!running || labsnapBusy} onClick={() => openTerminal(node.id)}>Abrir terminal</button>
          {!running && <p className="hint warn">Inicia el contenedor para abrir su terminal.</p>}
        </>
      ) : (
        <>
          <label className="field">Usuario
            <input
              value={node.data.guestUser ?? ''}
              onChange={(e) => updateNode(node.id, { guestUser: e.target.value })}
              placeholder="ubuntu"
              spellCheck={false}
              autoComplete="off"
              className="mono"
            />
          </label>
          <p className="hint">IP detectada: {ipv4 ? <span className="mono">{ipv4}</span> : <b>no disponible</b>}</p>
          {!ipv4 && <p className="hint warn">Aplica la red y comprueba que la VM tiene Guest Additions para conocer su IP.</p>}
          <button className="btn" disabled={!running || labsnapBusy} onClick={() => openTerminal(node.id)}>Abrir terminal (SSH)</button>
          {!running && <p className="hint warn">Inicia la VM para abrir su terminal.</p>}
          <p className="hint">Hace falta un servidor SSH dentro de la VM. La contraseña se escribe en la ventana del terminal; VMLab no la guarda.</p>
        </>
      )}
    </div>
  );
}

function NetworkSummary() {
  const nodes = useLab((s) => s.nodes);
  const edges = useLab((s) => s.edges);
  const report = useLab((s) => s.report);
  const probes = useLab((s) => s.probes);
  const snapshot = useLab((s) => s.snapshot);
  const { groups } = useMemo(() => computeGroups(nodes, edges), [nodes, edges]);
  const byId = new Map(nodes.map((n) => [n.id, n]));

  return (
    <>
      <div className="side-title"><span>Redes</span></div>
      {groups.length === 0 && <p className="hint">Conecta dos máquinas (arrastra desde el borde de un nodo hasta otro) para crear una red local.</p>}
      <ul className="net-list">
        {groups.map((g) => {
          const members = g.nodeIds.map((id) => byId.get(id)).filter((n): n is MachineNode => !!n);
          const off = members.filter((n) => n.data.kind === 'vbox' && (n.data.net?.mode ?? 'intnet') !== 'intnet');
          const live = groupLive(members, probes, snapshot);
          const notes = [
            ...new Set(
              members.flatMap((n) => {
                const note = probes[probeKey(n.data.kind, n.data.target)]?.note;
                return note ? [note] : [];
              }),
            ),
          ];
          const allRunning = members.every((n) => resolveState(n.data, snapshot) === 'running');
          const segments = new Set(
            members.map((n) => probes[probeKey(n.data.kind, n.data.target)]?.segment).filter(Boolean),
          );
          const reason = !allRunning
            ? 'Alguna máquina del segmento está apagada.'
            : segments.size > 1
              ? 'Los extremos están en redes distintas.'
              : 'Sin IP en la red de VMLab o red sin aplicar.';
          return (
            <li key={g.index}>
              <div className="net-name"><i style={{ background: NET_COLORS[g.index % NET_COLORS.length] }} />Red {g.index + 1}<small>{g.nodeIds.length} nodos</small></div>
              <div className="net-members">{members.map((n) => n.data.label).join(' · ')}</div>
              <span className={`net-live ${live ? 'ok' : 'bad'}`}><i />{live ? 'Comunicación detectada' : 'Sin comunicación'}</span>
              {!live && <div className="hint">{notes[0] ?? reason}</div>}
              {g.mixed && <div className="hint warn">Mezcla VirtualBox y Docker: no habrá enlace automático entre ambos.</div>}
              {off.length > 0 && <div className="hint warn">{off.map((n) => n.data.label).join(', ')}: adaptador fuera de la red interna.</div>}
            </li>
          );
        })}
      </ul>
      {report && (
        <div className="report">
          <div className="side-title"><span>Última aplicación</span></div>
          <p className="hint">{report.applied} nodo(s) configurados · {report.networks.length} red(es)</p>
          {report.warnings.map((w, i) => <p key={`w${i}`} className="hint warn">{w}</p>)}
          {report.errors.map((e, i) => <p key={`e${i}`} className="hint err">{e}</p>)}
        </div>
      )}
      <p className="hint foot">Cable verde animado: las dos máquinas se ven en la misma red. Rojo: apagadas, sin IP o red sin aplicar.</p>
      <p className="hint foot">Selecciona una VM para elegir su modo de red (NAT, puente, interna…). VirtualBox se aplica sobre el adaptador indicado; la VM debe estar apagada.</p>
    </>
  );
}

export function Inspector() {
  const node = useLab((s) => s.nodes.find((n) => n.selected));
  const state = useLab((s) => (node ? resolveState(node.data, s.snapshot) : 'unknown'));
  const busy = useLab((s) => (node ? !!s.busy[node.id] : false));
  const labsnapBusy = useLab((s) => s.labsnapBusy);
  const hasLinks = useLab((s) => (node ? s.edges.some((e) => e.source === node.id || e.target === node.id) : false));
  const { updateNode, removeNode, act } = useLab.getState();
  const isSlot = node?.data.kind === 'vbox' && node.data.slot === true;

  return (
    <aside className="inspector">
      {!node ? (
        <NetworkSummary />
      ) : (
        <>
          <div className="side-title"><span>{node.data.kind === 'vbox' ? 'Máquina virtual' : 'Contenedor'}</span></div>
          {isSlot && <SlotPanel node={node} />}
          <label className="field">Nombre en el lienzo
            <input value={node.data.label} onChange={(e) => updateNode(node.id, { label: e.target.value })} />
          </label>
          {!isSlot && (
            <label className="field">{node.data.kind === 'vbox' ? 'UUID de la VM' : 'Nombre del contenedor'}
              <input value={node.data.target} readOnly className="mono" />
            </label>
          )}
          {node.data.kind === 'docker' && (
            <label className="field">Imagen
              <input value={node.data.image ?? ''} onChange={(e) => updateNode(node.id, { image: e.target.value })} spellCheck={false} className="mono" />
            </label>
          )}
          <div className="status big" data-state={isSlot ? 'missing' : busy ? 'busy' : state}>
            <i className="dot" />{isSlot ? 'Sin asignar' : busy ? 'Trabajando…' : STATE_LABEL[state]}
          </div>
          <div className="btn-grid">
            <button className="btn" disabled={busy || labsnapBusy || isSlot} onClick={() => act(node.id, 'start')}>Iniciar</button>
            {node.data.kind === 'vbox' && <button className="btn" disabled={busy || labsnapBusy || isSlot} onClick={() => act(node.id, 'startGui')}>Iniciar con ventana</button>}
            <button className="btn" disabled={busy || labsnapBusy || isSlot} onClick={() => act(node.id, 'stop')}>Detener</button>
            <button className="btn" disabled={busy || labsnapBusy || isSlot} onClick={() => act(node.id, 'restart')}>Reiniciar</button>
            <button className="btn" disabled={busy || labsnapBusy || isSlot} onClick={() => act(node.id, state === 'paused' ? 'resume' : 'pause')}>{state === 'paused' ? 'Reanudar' : 'Pausar'}</button>
            <button className="btn danger" disabled={busy || labsnapBusy || isSlot} onClick={() => act(node.id, 'forceStop')}>Forzar apagado</button>
          </div>

          {node.data.kind === 'vbox' && (
            <>
              <NetworkModePanel node={node} hasLinks={hasLinks} />
              {!isSlot && <GuestPanel node={node} state={state} />}
            </>
          )}

          {!isSlot && <TerminalPanel node={node} state={state} />}

          <button className="btn ghost danger-text" disabled={labsnapBusy} onClick={() => removeNode(node.id)}><TrashIcon /> Quitar del lienzo</button>
          <p className="hint">Quitar un nodo del lienzo no borra la máquina ni el contenedor.</p>
        </>
      )}
    </aside>
  );
}
