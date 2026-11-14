# Security Policy / Política de seguridad

## Reporting a vulnerability / Cómo informar

Please **do not open a public issue** for security problems. Use GitHub's private reporting: **Security → Report a vulnerability** on this repository. Include steps to reproduce, affected version and impact. I'll acknowledge as soon as I can and credit you if you wish.

Por favor, **no abras un issue público** para problemas de seguridad. Usa el aviso privado de GitHub: **Security → Report a vulnerability**. Incluye pasos para reproducirlo, versión e impacto.

Supported version: the latest release. / Versión con soporte: la última publicada.

## Security model

VMLab is a local desktop app that drives `VBoxManage` and `docker` on your behalf.

- **No shell, ever.** Every command is `Command::new(program).args([...])`. No string interpolation.
- **Allow-list validation** (`crates/vmlab-core/src/validate.rs`) of names, images, networks, env variables, ports, adapters and capture/snapshot ids. Values starting with `-` are rejected (option injection).
- **Typed IPC.** The web layer can only call a small set of commands with closed types; it cannot ask the backend to "run this". Capture and snapshot files are referenced by id, never by path from the frontend.
- **Timeouts** on every process; Windows `.exe` only (never `.cmd`/`.bat`); no console windows.
- **Secrets.** VM passwords are used only for the operation, passed through a private temporary `--passwordfile` (never on the command line), and never stored in the workspace or in `.vmlab` files. Environment variables you define for containers *are* saved in the lab file — don't put secrets in them.
- **Network exposure.** Published container ports bind to `127.0.0.1` only.
- **Helper container.** Network conditions and captures use `nicolaka/netshoot` with `--cap-add NET_ADMIN` attached to the target container's network namespace; scripts receive their parameters as positional arguments, not interpolated text.
- **docker-compose import.** The file is validated with `docker compose config` in a fresh temporary folder (deleted afterwards, 1 MB input / 2 MB output limits). Compose expands `${VAR}` from your environment and reads `env_file` paths, so only import files you trust; the export warns when a lab contains environment variables.
- **Docker Desktop.** VMLab only stops Docker Desktop on exit if VMLab itself started it.
- **Updates.** The updater verifies a signature (Tauri updater key) before installing. The private signing key is never in the repository.
- **Telemetry:** none. The only network request made by VMLab itself is the optional update check (can be turned off).

## Known limits

- The installer is not code-signed (Windows SmartScreen warns).
- VMLab trusts the local VirtualBox and Docker installations; anyone who can control Docker on your machine can already control your containers.
- Captured `.pcap` files may contain sensitive traffic; they are stored in `%TEMP%\vmlab-captures`. Delete them when done.
