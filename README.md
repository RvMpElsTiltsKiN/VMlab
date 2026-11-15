# VMLab

[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)
![Platform: Windows](https://img.shields.io/badge/platform-Windows%2010%2F11-blue)

**English** · [Español](README.es.md)

A visual canvas to build local labs: drag **VirtualBox** VMs and **Docker** containers onto the canvas, wire them together to define networks, and control them with one click. Then go further: simulate bad links, see every IP, capture packets, open terminals, snapshot the whole lab and share it as a single file.

<!-- Replace with your own capture / GIF (see docs/screenshots/README.md) -->
<!-- ![VMLab main window](docs/screenshots/main.png) -->

## Features

- **Visual canvas.** Drag VMs and containers in, draw cables between them. Each group of connected nodes is one local network. Cables turn green when both ends really have an IP on the same network.
- **Shareable labs (`.vmlab`) and templates.** Export/import a whole lab as one file, or start from a template (web + database, two VMs on an internal network, containers, attacker VM + target).
- **Terminal per node.** One click opens a shell: `docker exec` for containers, SSH for VMs (using the detected IP).
- **Lab snapshots.** Save the state of every machine on the canvas under one name and restore it later (VirtualBox snapshots; Docker via `docker commit`).
- **Network conditions per node.** Latency, jitter, packet loss and bandwidth with presets (LAN, WAN, 4G, satellite, bad connection). Docker uses `tc netem`; VirtualBox supports bandwidth only.
- **Visibility.** IP, segment and DNS name for each node, copy with a click, `/etc/hosts` written in Linux VMs, and **packet capture** to `.pcap` you open in Wireshark.
- **docker-compose import** and per-container run options (environment variables, ports published only on `127.0.0.1`).
- **Per-VM network mode.** Internal network (managed by VMLab, built-in DHCP), NAT, bridged, host-only, NAT network or disconnected, on any of the 4 adapters.
- **In-guest network setup (Linux).** Through Guest Additions: detects Netplan, NetworkManager, systemd-networkd, ifupdown, wicked, dhcpcd, ConnMan, network-scripts, and writes DHCP, static IP or an ISC DHCP server config.
- **Silent Docker Desktop start** from the app. If VMLab started Docker Desktop, it stops it when you close VMLab (and, only if no other WSL distribution is running, shuts WSL down to free memory). A Docker Desktop that was already running is never touched.
- **Bilingual UI (Spanish / English)**, light/dark theme, optional update check, small installer (uses the system WebView2, no embedded Chromium).

<!-- ## Screenshots (activar cuando existan las capturas en docs/screenshots/)
| Canvas | Network conditions | Packet capture |
|---|---|---|
| ![](docs/screenshots/canvas.png) | ![](docs/screenshots/conditions.png) | ![](docs/screenshots/capture.png) |
-->

## Install

Download the latest installer from the [Releases](../../releases) page:

- `VMLab_x.y.z_x64-setup.exe` — graphical installer (recommended)
- `VMLab_x.y.z_x64_*.msi` — for managed deployments

> **Windows SmartScreen:** the installer is not code-signed, so Windows may show "Unknown publisher". Choose **More info → Run anyway** only if you downloaded it from this repository's Releases page.

Requirements: Windows 10/11, **VirtualBox** (for VMs) and/or **Docker Desktop** (for containers). VMLab does not bundle or redistribute them. Optional: **Wireshark** to open captures.

## Quick start

1. Open VMLab; the top-left badges show whether VirtualBox and Docker were found.
2. Drag two machines from the left panel to the canvas.
3. Drag from a node's edge to the other node to draw a cable.
4. Power off the VMs and press **Apply network**.
5. Press **Start all**. When both have an IP on the same network the cable turns green.

Full walkthrough: [USER_GUIDE.txt](USER_GUIDE.txt) (Spanish: [GUIA_DE_USUARIO.txt](GUIA_DE_USUARIO.txt)).

## Network model

Each **group of connected nodes** is one shared network segment. Conditions and captures therefore apply **per node**, not per cable.

| Technology | What VMLab does on **Apply network** |
|---|---|
| VirtualBox | Attaches the chosen adapter (2 by default) to the internal network `vmlab-xxxxxxxx` and provides built-in DHCP. Adapter 1 (NAT) is only touched if you select it. The VM must be powered off. |
| Docker | Creates a bridge network `vmlab-xxxxxxxx`, connects the containers and disconnects them from other `vmlab-*` / legacy `lablab-*` networks. Empty ones are removed. |
| Unconnected node | VirtualBox: own internal network or disabled adapter. Docker: no `vmlab-*` networks. |

Mixed VirtualBox + Docker segments are flagged but not bridged.

## Limitations

- Windows only (for now). VirtualBox and Docker only; **Hyper-V and WSL2 are not supported**.
- In-guest configuration and `/etc/hosts` writing target **Linux** guests with Guest Additions running and a root/passwordless-sudo user.
- Docker network conditions are lost when the container restarts; re-apply them.
- VirtualBox network conditions: bandwidth only (no latency/loss).
- Snapshots: running VMs are not restored (power them off first). Docker restore recreates the container and keeps the old one renamed `<name>-prerestore-<timestamp>`; **volumes and published ports are not preserved**. The canvas layout is not part of a snapshot; use **Export lab**.
- Docker features that need a helper (conditions, capture) pull the `nicolaka/netshoot` image (~100 MB) the first time.
- Paused VMs are shown as "running" (VirtualBox does not distinguish them in `list runningvms`).
- Messages coming from the Rust engine are still in Spanish; the English UI marks them as technical messages.
- Environment variables are stored in the exported `.vmlab` file: don't put secrets there.
- Only import `docker-compose.yml` files you trust: `docker compose config` expands `${VAR}` from your Windows environment and reads `env_file` paths, and the result becomes node variables.

## Privacy and updates

Everything stays on your machine. The canvas is stored in `%APPDATA%\dev.vmlab.app\workspace.json`; VM passwords are never stored. On start VMLab can check GitHub Releases for a newer version (verified with a signature); disable it in **Lab menu → Check for updates on start**. No telemetry.

## Security

See [SECURITY.md](SECURITY.md) for the security model and how to report vulnerabilities.

## Architecture

```
vmlab/
├─ crates/vmlab-core/     ← UI-less logic, testable with `cargo test`
│  └─ src/  validate · runner · tools · vbox · docker · topology · engine ·
│           connectivity · guest · labsnap · terminal · netem · capture · compose · model
├─ src-tauri/             ← Tauri shell: IPC commands, monitor, migration, updater
├─ src/                   ← React UI (components, store, i18n es/en, lib)
├─ scripts/               ← icon generation
└─ assets/icon-source.svg
```

```
React (webview)                         Rust (native process)
invoke('machine_action', {…}) ───────►  commands.rs (async)
                                           └► vmlab-core::engine
                                                ├─ validate (allow-list)
                                                └─ runner   (program + argv[], NO shell, timeout)
listen('vmlab:status')       ◄───────  monitor.rs (every 2 s, emits on change)
```

## Build from source

Requirements: Node.js 20+, Rust (*stable-msvc* toolchain), Visual Studio Build Tools with "Desktop development with C++", WebView2.

```powershell
npm install
npm run test:core      # Rust tests
npm run tauri:dev      # dev mode with hot reload
npm run dist:win       # installers (NSIS .exe and MSI)
```

Outputs: `src-tauri\target\release\bundle\nsis\` and `...\bundle\msi\`. `npm run dev` runs the UI in a browser with a backend simulator (no VirtualBox/Docker needed).

Custom icon: `npm run icons -- C:\path\logo.png` (square PNG/SVG, ≥ 1024 px).

### Releases (maintainers)

Pushing a `v*` tag runs `.github/workflows/build-windows.yml`, which builds the installers and creates a **draft** release with `latest.json` for the updater. It needs the secrets `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. The matching public key goes in `src-tauri/tauri.conf.json` → `plugins.updater.pubkey`. **Never commit the private key.**

## Migrating from LabLab

VMLab is the new name of LabLab. On first launch it copies your workspace and preferences from the old data folder without deleting anything. **Apply network** moves machines from `lablab-*` to `vmlab-*` networks and removes the old empty ones.

## License

[MIT](LICENSE) © 2026 RvMpElsTiltsKiN. VirtualBox, Docker and Wireshark are separate programs with their own licenses.
