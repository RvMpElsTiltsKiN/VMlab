# VMLab

[![Licencia: MIT](https://img.shields.io/badge/licencia-MIT-green.svg)](LICENSE)
![Plataforma: Windows](https://img.shields.io/badge/plataforma-Windows%2010%2F11-blue)

[English](README.md) · **Español**

Lienzo visual para montar laboratorios locales: arrastra máquinas de **VirtualBox** y contenedores **Docker**, conéctalas para definir redes y contrólalas con un clic. Y además: simula enlaces malos, ve todas las IP, captura paquetes, abre terminales, guarda instantáneas de todo el lab y compártelo en un solo archivo.

<!-- Sustituye por tu captura / GIF (ver docs/screenshots/README.md) -->
![Ventana principal de VMLab](docs/screenshots/main.png)

## Funciones

- **Lienzo visual.** Arrastra VMs y contenedores y une con cables. Cada grupo de nodos conectados es una red local. El cable se pone verde cuando los dos extremos tienen IP real en la misma red.
- **Laboratorios compartibles (`.vmlab`) y plantillas.** Exporta/importa un lab completo en un archivo o empieza desde una plantilla (web + base de datos, dos VMs en red interna, contenedores, VM atacante + objetivo).
- **Terminal por nodo.** Un clic abre una consola: `docker exec` en contenedores, SSH en VMs (con la IP detectada).
- **Instantáneas del lab.** Guarda el estado de todas las máquinas del lienzo con un nombre y vuelve a él (VirtualBox: snapshots; Docker: `docker commit`).
- **Condiciones de red por nodo.** Latencia, variación, pérdida y ancho de banda con preajustes (LAN, WAN, 4G, satélite, conexión mala). Docker usa `tc netem`; VirtualBox solo admite ancho de banda.
- **Visibilidad.** IP, segmento y nombre DNS de cada nodo, copiables; escritura de `/etc/hosts` en VMs Linux y **captura de paquetes** a `.pcap` para abrir en Wireshark.
- **Importar docker-compose** y opciones de ejecución por contenedor (variables de entorno, puertos publicados solo en `127.0.0.1`).
- **Modo de red por VM.** Red interna (gestionada por VMLab, con DHCP integrado), NAT, puente, solo anfitrión, red NAT o sin conectar, en cualquiera de los 4 adaptadores.
- **Configuración de red dentro de la VM (Linux)** vía Guest Additions: detecta Netplan, NetworkManager, systemd-networkd, ifupdown, wicked, dhcpcd, ConnMan, network-scripts y escribe DHCP, IP estática o un servidor DHCP ISC.
- **Arranque silencioso de Docker Desktop** desde la app. Si lo arrancó VMLab, lo detiene al cerrar VMLab (y, solo si no hay otra distribución de WSL en marcha, apaga WSL para liberar memoria). Un Docker Desktop que ya estaba abierto nunca se toca.
- **Interfaz bilingüe (español / inglés)**, tema claro/oscuro, comprobación de actualizaciones opcional e instalador pequeño (usa el WebView2 del sistema, sin Chromium embebido).

## Capturas

| Lienzo | Condiciones de red | Captura de paquetes |
|---|---|---|
| ![](docs/screenshots/canvas.png) | ![](docs/screenshots/conditions.png) | ![](docs/screenshots/capture.png) |

## Instalación

Descarga el instalador más reciente desde [Releases](../../releases):

- `VMLab_x.y.z_x64-setup.exe` — asistente gráfico (recomendado)
- `VMLab_x.y.z_x64_*.msi` — para despliegue empresarial

> **Windows SmartScreen:** el instalador no está firmado digitalmente, así que Windows puede mostrar «Editor desconocido». Pulsa **Más información → Ejecutar de todas formas** solo si lo has descargado de la página Releases de este repositorio.

Requisitos: Windows 10/11, **VirtualBox** (para VMs) y/o **Docker Desktop** (para contenedores). VMLab no los incluye ni los redistribuye. Opcional: **Wireshark** para abrir capturas.

## Primeros pasos

1. Abre VMLab; las etiquetas de arriba a la izquierda indican si encontró VirtualBox y Docker.
2. Arrastra dos máquinas del panel izquierdo al lienzo.
3. Arrastra desde el borde de un nodo hasta el otro para dibujar un cable.
4. Apaga las VMs y pulsa **Aplicar red**.
5. Pulsa **Iniciar todo**. Cuando ambas tengan IP en la misma red, el cable se pone verde.

Guía completa: [GUIA_DE_USUARIO.txt](GUIA_DE_USUARIO.txt) (inglés: [USER_GUIDE.txt](USER_GUIDE.txt)).

## Modelo de red

Cada **grupo de nodos conectados** es un segmento de red compartido. Por eso las condiciones y las capturas se aplican **por nodo**, no por cable.

| Tecnología | Qué hace VMLab al **Aplicar red** |
|---|---|
| VirtualBox | Conecta el adaptador elegido (2 por defecto) a la red interna `vmlab-xxxxxxxx` con DHCP integrado. El adaptador 1 (NAT) solo se toca si lo seleccionas. La VM debe estar apagada. |
| Docker | Crea una red *bridge* `vmlab-xxxxxxxx`, conecta los contenedores y los desconecta de otras redes `vmlab-*` / antiguas `lablab-*`. Borra las vacías. |
| Nodo sin conexiones | VirtualBox: red interna propia o adaptador desactivado. Docker: sin redes `vmlab-*`. |

Una red que mezcla VirtualBox y Docker se avisa, pero no se puentea.

## Limitaciones

- Solo Windows (por ahora). Solo VirtualBox y Docker; **no hay soporte para Hyper-V ni WSL2**.
- La configuración dentro de la VM y `/etc/hosts` son para invitados **Linux** con Guest Additions en marcha y un usuario root o con sudo sin contraseña.
- En Docker, las condiciones de red se pierden al reiniciar el contenedor; hay que volver a aplicarlas.
- En VirtualBox, las condiciones solo cubren ancho de banda (sin latencia ni pérdida).
- Instantáneas: las VMs encendidas no se restauran (apágalas antes). En Docker, restaurar recrea el contenedor y conserva el anterior renombrado `<nombre>-prerestore-<fecha>`; **no se conservan volúmenes ni puertos publicados**. La disposición del lienzo no entra en la instantánea: usa **Exportar lab**.
- Las funciones de Docker que usan un contenedor auxiliar (condiciones, captura) descargan la imagen `nicolaka/netshoot` (~100 MB) la primera vez.
- Las VMs en pausa se muestran como «en ejecución» (VirtualBox no las distingue en `list runningvms`).
- Los mensajes del motor Rust siguen en español; la interfaz en inglés los marca como mensajes técnicos.
- Las variables de entorno se guardan en el archivo `.vmlab` exportado: no pongas secretos.
- Importa solo ficheros `docker-compose.yml` de confianza: `docker compose config` expande `${VAR}` con tus variables de Windows y lee las rutas `env_file`, y el resultado pasa a ser variables del nodo.

## Privacidad y actualizaciones

Todo se queda en tu equipo. El lienzo se guarda en `%APPDATA%\dev.vmlab.app\workspace.json`; las contraseñas de las VMs nunca se guardan. Al iniciar, VMLab puede consultar GitHub Releases por si hay versión nueva (verificada con firma); se desactiva en **menú Laboratorio → Buscar actualizaciones al iniciar**. Sin telemetría.

## Seguridad

Consulta [SECURITY.md](SECURITY.md) para el modelo de seguridad y cómo informar de vulnerabilidades.

## Arquitectura

```
vmlab/
├─ crates/vmlab-core/     ← lógica sin UI, testeable con `cargo test`
│  └─ src/  validate · runner · tools · vbox · docker · topology · engine ·
│           connectivity · guest · labsnap · terminal · netem · capture · compose · model
├─ src-tauri/             ← envoltorio Tauri: comandos IPC, monitor, migración, actualizador
├─ src/                   ← interfaz React (componentes, store, i18n es/en, lib)
├─ scripts/               ← generación de iconos
└─ assets/icon-source.svg
```

```
React (webview)                         Rust (proceso nativo)
invoke('machine_action', {…}) ───────►  commands.rs (async)
                                           └► vmlab-core::engine
                                                ├─ validate (lista blanca)
                                                └─ runner   (programa + argv[], SIN shell, timeout)
listen('vmlab:status')       ◄───────  monitor.rs (cada 2 s, emite si hay cambios)
```

## Compilar desde el código

Requisitos: Node.js 20+, Rust (toolchain *stable-msvc*), Visual Studio Build Tools con «Desarrollo para el escritorio con C++», WebView2.

```powershell
npm install
npm run test:core      # tests de Rust
npm run tauri:dev      # modo desarrollo con recarga en caliente
npm run dist:win       # instaladores (NSIS .exe y MSI)
```

Salida: `src-tauri\target\release\bundle\nsis\` y `...\bundle\msi\`. `npm run dev` ejecuta la interfaz en el navegador con un simulador (sin VirtualBox/Docker).

Icono propio: `npm run icons -- C:\ruta\logo.png` (PNG/SVG cuadrado, ≥ 1024 px).

### Publicar versiones (mantenedores)

Subir una etiqueta `v*` ejecuta `.github/workflows/build-windows.yml`, que construye los instaladores y crea un *release* **borrador** con `latest.json` para el actualizador. Necesita los secretos `TAURI_SIGNING_PRIVATE_KEY` y `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. La clave pública va en `src-tauri/tauri.conf.json` → `plugins.updater.pubkey`. **Nunca subas la clave privada.**

## Migración desde LabLab

VMLab es el nuevo nombre de LabLab. Al abrirlo por primera vez copia tu espacio de trabajo y preferencias de la carpeta antigua sin borrar nada. **Aplicar red** pasa las máquinas de las redes `lablab-*` a `vmlab-*` y elimina las antiguas vacías.

## Licencia

[MIT](LICENSE) © 2026 RvMpElsTiltsKiN. VirtualBox, Docker y Wireshark son programas independientes con sus propias licencias.
