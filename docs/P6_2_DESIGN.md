# P6.2 — Gentle Fleet Desktop: diseño

## Alcance

Aplicación nativa independiente en `apps/desktop`, escrita en Rust con GPUI.
Esta etapa solo implementa el shell visual; no conecta con `fleetd`, no importa
`fleet-client`, no arranca agentes y no contiene datos autoritativos de Fleet.
En P6.2-B, la integración deberá consumir la API pública exclusivamente a través
de `fleet-client`.

## Decisión de framework

- `gpui = 0.2.2`, fijado exactamente en `apps/desktop/Cargo.toml`.
- La documentación publicada y el ejemplo `hello_world` de GPUI 0.2.2 usan
  `Application::new`, `App::open_window`, entidades GPUI y vistas `Render`.
- GPUI ofrece un executor integrado, pero A no necesita tareas asíncronas ni
  crea hilos propios.
- GPUI 0.2.2 activa por defecto `font-kit`, `wayland` y `x11`. En Linux necesita
  toolchain Rust estable actual, bibliotecas de desarrollo X11/Wayland, xkbcommon,
  fontconfig/freetype y un renderer GPU compatible. El build en este equipo sigue
  pendiente mientras se resuelve una dependencia transitiva de GPUI 0.2.2:
  `zed-async-tar` selecciona `xattr 0.2.3`, cuyo código usa
  `libc::ENOATTR`, ausente en Linux con la versión de libc del workspace.
- La versión se elige por estar publicada y documentada, evitando una referencia
  Git flotante al monorepo completo de Zed. GPUI es pre-1.0 y conserva riesgo de
  cambios incompatibles entre versiones; futuras actualizaciones requieren
  revisión explícita del lockfile.
- Se conserva un parche local mínimo de `xattr 0.2.3` en
  `apps/desktop/vendor/xattr`: su dependencia `zed-async-tar` referencia
  `libc::ENOATTR` en Linux, mientras libc moderno expone ese errno como
  `ENODATA`. El parche solo adapta ese alias para Linux; revisar y retirar al
  actualizar GPUI/zed-async-tar. Incluye las licencias upstream.

Referencias consultadas (22-11-2026):

- <https://docs.rs/gpui/0.2.2/gpui/>
- <https://docs.rs/crate/gpui/0.2.2/source/examples/hello_world.rs>
- <https://github.com/zed-industries/zed/tree/main/crates/gpui>

El README `main` de Zed ya documenta `gpui_platform` como crate aparte, pero esa
crate no está publicada en crates.io. No se añadió una dependencia inexistente ni
se usó una versión no fijada; 0.2.2 incluye el entrypoint `Application` usado
por el ejemplo publicado.

## Límites y estado

- `app.rs`: arranque, ventana GPUI y vista raíz.
- `state.rs`: estado visual efímero de la ventana; no contiene sesiones Fleet.
- `navigation.rs`: destinos locales de navegación.
- El panel lateral inicial de `layout.rs` (solo placeholders de A) se sustituyó
  en B por Machines y datos dinámicos en `app.rs`, consumiendo el modelo del
  adapter `fleet/`.
- `components/`: botones visuales compartidos.
- `theme.rs`: paletas, colores, radios, espaciados y escala tipográfica.

GPUI es propietario de las vistas y llama a `Render` por frame. Cambios de
interacción mutan solo el estado pequeño de `DesktopView` y notifican a GPUI.
No se clona un estado de Fleet ni se necesita un global lock.

La barra propia ofrece marca y ajustes; los controles de ventana los proveen las
decoraciones nativas del sistema para respetar cada plataforma. La ventana es
redimensionable y define un mínimo. Se implementan modo oscuro/claro, bienvenida,
panel, configuración y una pantalla informativa al elegir «Abrir sesión».

## Ciclo de vida y futuro SDK

Abrir/cerrar la aplicación solo crea/destruye ventanas y estado visual. No envía
`CreateSession`, `StartSession` ni `StopSession`; cerrar Desktop no detiene
runners. Fleet seguirá siendo dueño de sesiones y timeline. P6.2-B añadirá un
límite `fleet` para coordinar el SDK con estado de vista, sin exponer drivers ni
transporte a componentes visuales.

## Portabilidad

El crate usa solo GPUI y código Rust portable; las features X11/Wayland activas
son las del target Linux. No se validó compilación Windows/macOS en esta etapa.
GPUI publica backends para ambos, pero son riesgos de integración aún no
verificados por Gentle Fleet.

## P6.2-B — Machines y adapter del Fleet SDK

P6.2-A quedó cerrada por separado; sus evidencias y procedencia están en
[`P6_2A_RESULTS.md`](P6_2A_RESULTS.md). B añade `apps/desktop/src/fleet/` como
frontera entre GPUI y `fleet-client`. Desktop consume el SDK y sus tipos públicos
de respuesta; no depende de Fleet core, runner, drivers, SQLite ni de la
implementación de SSH.

El alias de SSH es la etiqueta visible, pero el ID interno es la variante
`MachineId::Ssh(alias)`; local usa `MachineId::Local`. La API
`fleet_client::configured_hosts()` reutiliza `fleet-ssh::load_hosts()` y expone
solo alias ordenados. Leer el catálogo no prueba hosts remotos ni los declara
conectados. La conexión se inicia por selección/acción explícita y las consultas
reales son `list_agents`, `list_workspaces` y `list_sessions`.

GPUI agenda las operaciones síncronas con su executor de fondo, admite a lo sumo
dos operaciones simultáneas, cancela cooperativamente entre requests y asocia
cada respuesta a generación + MachineId. Solo se conserva un `FleetClient` en la
vista; cambiar de host cierra únicamente el transporte del cliente previo. Un
IO ya bloqueado no puede abortarse a mitad de request: SSH conserva el
`ConnectTimeout=10` existente y los resultados vencidos no alteran la vista.
Desconectar/cerrar Desktop no manda `StopSession` ni mata runners.

Los errores se traducen a mensajes seguros y categorías de transporte,
autenticación, host key, bridge, daemon y protocolo. No se presenta stderr crudo.
Se mantiene `StrictHostKeyChecking=yes`; verificar/aceptar una clave nueva ocurre
fuera de Desktop mediante el flujo seguro de OpenSSH/known_hosts. B no crea
sesiones ni implementa chat/timeline; «Abrir chat» se explica como trabajo de
P6.2-C, que no se inicia en esta etapa.
