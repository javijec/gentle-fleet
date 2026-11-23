# P6.2-A — resultados

## Estado: EN PROGRESO; CI del commit pendiente y smoke del panel lateral incompleto

Se creó el esqueleto de Gentle Fleet Desktop con GPUI 0.2.2, vista de bienvenida,
panel de Machines/sesiones vacío, navegación local, controles de tema y vista
informativa para «Abrir sesión». No se conecta a `fleetd` ni `fleet-client`, y no
inicia procesos de agentes.

### Validaciones observadas

| Validación | Resultado |
|---|---|
| Estado inicial del repositorio | Limpio antes del cambio |
| Rama base | `feature/p6-2-gpui-desktop`, en `4166c4a`, igual a `feature/p6-sdk-gpui` y su origin |
| `git fetch origin` | Correcto; no había avance de SDK para fast-forward |
| Investigación de GPUI | Documentación crates.io y ejemplo GPUI 0.2.2 consultados |
| `cargo fmt --all -- --check` | PASS |
| `cargo check --workspace --locked` | PASS |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS |
| `cargo test --workspace --locked` | PASS en la validación más reciente; 1 diagnóstico real opt-in ignorado |
| `cargo build --workspace --locked` | PASS |
| `cargo test -p gentle-fleet-desktop --locked` | PASS: 12 pruebas, incluidas pruebas GPUI de Tab, Enter y foco al navegar |
| Conversión de color | Corregida RGB-hex → `Hsla`; pruebas para rojo, escala de grises y token del canvas oscuro |
| Identidad de botones | IDs distintos y estables, cursor de mano y estilo de foco |
| Smoke Linux | Parcial en el binario actualizado: monitor `eDP-1` encendido (1920×1080); ventana GPUI visible y acepta input (`dev.gentle-fleet.desktop`). Welcome, Ajustes, «Abrir sesión» informativa y «Volver» se observaron en pantalla. Tema oscuro y claro se observaron; ventana redimensionada de 1180×640 a 1280×700 y layout inspeccionado. El panel se ocultó una vez con dispatchers sintéticos de Hyprland; no se verificó su restauración. Cierre normal de la última ventana: PASS |
| CI remota | No hay CI para los cambios pendientes. El histórico [run 45167387221](https://github.com/javijec/gentle-fleet/actions/runs/45167387221) reportó error de clasificación de host key; el E2E P5 volvió a pasar localmente con `StrictHostKeyChecking=yes` |
| E2E shell de backend | PASS en CI: suite FakeDriver, concurrencia, backpressure, permisos, persistencia, approvals, supervivencia runner, identidad, replay, P4.4, SSH y P6.1 |
| Windows/macOS | No validados |

La corrección de tema resuelve la conversión incorrecta que pasaba los canales
RGB directamente a `hsla`. GPUI restaura el foco en el control de la página nueva
al navegar, para que Tab/Enter continúen desde una acción válida. Las pruebas de
ventana simulan tanto KeyDown como KeyUp: `TestAppContext::simulate_keystrokes`
solo emite KeyDown, mientras que el click de teclado de GPUI se procesa en
KeyUp. La integración real se ejecutó con `wtype` en Hyprland y se inspeccionaron
capturas de los estados. La interacción de teclado/ratón fue inyectada por
Wayland/Hyprland (`wtype` y dispatcher), no por dispositivos físicos. El smoke
del panel lateral sigue incompleto; no se declara cerrado el corte.

El primer build expuso un problema en la dependencia transitiva
`gpui_http_client 0.2.2` (`zed-async-tar` → `xattr`); el parche local mínimo
descrito en el diseño permitió completar las validaciones Rust. No se modificó
la lógica del backend Fleet ni se relajaron reglas de Clippy. P6.2-A no se
considera cerrada hasta completar interacción gráfica real y obtener CI verde.
