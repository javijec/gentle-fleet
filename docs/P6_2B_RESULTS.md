# P6.2-B — resultados

## Estado: EN PROGRESO; integración real y CI de esta etapa pendientes

### Implementación

| Área | Estado |
|---|---|
| Adapter | `apps/desktop/src/fleet/` usa `fleet-client` y sus tipos públicos reexportados; no implementa transporte ni protocolo |
| Catálogo | Presenta Local y aliases SSH de `fleetctl host`; leer el catálogo no prueba conexiones |
| Consultas | La ruta real de éxito consulta proveedores, workspaces y sesiones, y conserva un cliente SDK de la máquina seleccionada |
| Concurrencia | Operaciones fuera de Render, máximo 2 workers, cancelación cooperativa entre requests, respuesta filtrada por generación e identidad |
| Errores | Traducción a categorías/mensajes sanitizados; seguridad OpenSSH permanece en `StrictHostKeyChecking=yes` |
| UI | Panel Machines expandible, selección de máquina/sesión, vista central, acciones conectar/actualizar/desconectar/reintentar/cancelar |
| Ciclo de vida | No crea sesiones ni manda StopSession; desconectar cierra solo el transporte de cliente |
| P6.2-C | No implementada |

### Validaciones observadas

| Comando/evidencia | Resultado |
|---|---|
| `cargo fmt --all -- --check` | PASS |
| `cargo check --workspace --locked` | PASS |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS |
| `cargo test --workspace --locked` | PASS en el último pase completo; la suite desktop se volvió a ampliar después a 20 pruebas PASS. 1 diagnóstico real opt-in ignorado en Gentle Shell |
| `cargo build --workspace --locked` | PASS |
| `tests/e2e_p51_ssh.sh` | PASS: paridad de transporte, dos clientes, supervivencia al disconnect, replay y errores de seguridad |
| `tests/e2e_p52_two_hosts.sh` | PASS: dos hosts SSH de fixture con sesiones, operation keys y timelines aislados |
| `tests/e2e_p61_sdk.sh` | PASS: SDK local con FakeDriver, consulta de agentes, prompt/timeline y cancelación |
| Omarchy/Hyprland | Ventana GPUI visible/acepta input en `eDP-1` 1920×1080. Captura `/tmp/opencode/p62b-machines-initial.png` muestra el panel Machines real con Local y Desconectada; no hay alias SSH configurados. |
| Conexión local real | BLOQUEADA: `fleetctl agents` devolvió `Connection refused`; `fleetd.service` está inactivo. No se inició `fleetd` (no se debe ocultar el error del entorno). |
| Smoke remoto real | No ejecutado: `fleetctl host list` no reportó aliases configurados. El E2E P5.2 solo valida SSH con servidores de fixture, no otra máquina física. |
| Respuesta de error visible en UI | Pendiente: el dispatcher sintético de Hyprland no produjo activación verificable durante esta sesión; la prueba GPUI sí selecciona el row y descarta el resultado no conectado del alias fixture. |
| CI para commit B | Pendiente |
| Windows/macOS | No validados |

### Limitaciones y riesgos pendientes

- No se puede declarar satisfecho el criterio de conexión/consulta real del SDK
  desde Desktop hasta disponer de `fleetd` local activo bajo el entorno de
  usuario, sin que Desktop lo arranque automáticamente.
- No se verificaron UI local conectada ni máquina SSH física remota. Los E2E
  proporcionan cobertura de contrato/seguridad, no sustituyen esas integraciones.
- Una operación síncrona en curso solo se cancela entre requests; una lectura RPC
  bloqueada no se interrumpe a mitad de frame. La selección nueva descarta su
  resultado y el máximo de dos workers limita solicitudes simultáneas.
- La administración de altas/eliminaciones de hosts permanece en `fleetctl`;
  Desktop descubre, selecciona, conecta y refresca, pero no edita configuración.
- No hay chat, timeline, creación de sesión ni ejecución de agentes: corresponden
  a la etapa P6.2-C.
