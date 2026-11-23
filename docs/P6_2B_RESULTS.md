# P6.2-B — resultados

## Estado: CERRADA EN LINUX (23/11/2026)

Las consultas Fleet locales y SSH se demostraron desde la vista GPUI real,
contra un `fleetd` aislado iniciado explícitamente por el E2E y un host OpenSSH
de fixture. No se confunde este resultado con el E2E CLI de P5: la prueba
selecciona máquinas y sesiones, conecta, refresca, desconecta y reintenta a
través de los handlers y el render de Desktop.

### Implementación

| Área | Estado |
|---|---|
| Adapter | `apps/desktop/src/fleet/` usa `fleet-client` y sus tipos públicos reexportados; no implementa transporte ni protocolo |
| Catálogo | Presenta Local y aliases SSH de `fleetctl host`; leer el catálogo no prueba conexiones |
| Consultas | La ruta real de éxito consulta proveedores, workspaces y sesiones, y conserva un cliente SDK de la máquina seleccionada |
| Concurrencia y espera | Operaciones fuera de Render, máximo 2 transportes SDK reales en ejecución, cancelación cooperativa, deadline de respuesta de 20 s y sondeo cada 50 ms. El timeout libera la espera de GPUI, pero el transporte conserva su permiso hasta finalizar |
| Snapshots | `mark_disconnected`/`mark_failed` eliminan el snapshot anterior; el detalle y el panel solo muestran datos con la fila en `Connected` |
| Respuestas obsoletas | Generación + `MachineId` descartan resultados de una selección anterior |
| Navegación | «Volver al inicio» limpia `fleet.selected`/`selected_session`, de modo que Welcome se renderiza aunque hubiera una máquina seleccionada |
| Identidades UI | Elementos interactivos usan claves estables derivadas de `MachineId`/`SessionIdentity`, no índices de lista |
| Modularización | `app.rs` (vista, handlers, `run`) se separó de `machines.rs` (panel/detalle), `pages.rs` (bienvenida/configuración/información) y `app/tests.rs` |
| Errores | Categorías/mensajes sanitizados; `StrictHostKeyChecking=yes` intacto en `fleet-ssh` |
| Ciclo de vida | No crea sesiones ni manda StopSession; desconectar cierra solo el transporte de cliente |
| P6.2-C | No implementada |

### Validaciones observadas

| Comando/evidencia | Resultado |
|---|---|
| `cargo fmt --all -- --check` | PASS |
| `cargo check --workspace --locked` | PASS |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS |
| `cargo test --workspace --locked` | PASS |
| `cargo test -p gentle-fleet-desktop --locked` | PASS: 25 pruebas, 1 integración real opt-in ignorada |
| `cargo build --workspace --locked` | PASS |
| `tests/e2e_p51_ssh.sh` | PASS: paridad de transporte, dos clientes, supervivencia al disconnect, replay y errores de seguridad |
| `tests/e2e_p52_two_hosts.sh` | PASS: dos hosts SSH de fixture con sesiones, operation keys y timelines aislados |
| `tests/e2e_p61_sdk.sh` | PASS: SDK local con FakeDriver, consulta de agentes, prompt/timeline y cancelación |
| `tests/e2e_p62b_desktop_gpui.sh` | PASS local y en CI: `fleetd` local aislado + `sshd`/`fleetd` remotos de fixture; GPUI conecta Local, consulta proveedores/workspaces/sesiones, refresca, desconecta, reconecta, repite sobre SSH y verifica que los runners preexistentes sobreviven al disconnect del cliente |
| Snapshots tras desconexión/fallo | PASS: pruebas unitarias del adapter y aserciones GPUI verifican que el snapshot desaparece |
| Deadline y concurrencia real | Pruebas nuevas: una operación bloqueada devuelve `Timeout` sin liberar su permiso; 100 reintentos no admiten nuevos workers hasta que finalizan ambos hilos reales. Pendiente validar estos cambios en CI |
| Navegación a Welcome con máquina seleccionada | PASS: prueba GPUI |
| Claves estables host/sesión | PASS: prueba unitaria de `machine_key`/`session_key` |
| CI de la rama | PASS: [run 45208226815](https://github.com/javijec/gentle-fleet/actions/runs/45208226815) sobre `7ec7440` (el job `check` requirió un rerun por un fallo intermitente de `e2e_p3_survival.sh`; el rerun y el job GPUI pasaron) |
| Omarchy/Hyprland (smoke manual de esta etapa) | UNTESTED: no se repitió la interacción manual con `fleetd` real de usuario; el E2E GPUI cubre la lógica de vista y los transportes reales, no una sesión de escritorio interactiva |
| Segunda máquina física SSH | UNTESTED: el host remoto de la prueba es un `sshd` local efímero con runtime/DB aislados, no hardware distinto |
| Windows/macOS | No validados |

### Limitaciones y riesgos pendientes

- Una lectura RPC bloqueada no se interrumpe a mitad de frame: a los 20 s se
  abandona la espera de GPUI, pero el hilo real retiene su permiso de concurrencia
  hasta terminar. Así se impide la acumulación ilimitada de hilos por reintentos.
  Si dos llamadas quedan bloqueadas indefinidamente, la UI sigue respondiendo
  pero nuevas consultas Fleet quedan rechazadas hasta que se libere capacidad.
  Los timeouts efectivos y la cancelación del transporte subyacente siguen
  siendo una mejora posterior; nunca se afirma que el hilo terminó al vencer
  el deadline. La respuesta tardía se descarta por generación.
- La prueba SSH demuestra el camino OpenSSH real (host key, identidad, bridge,
  daemon remoto aislado), no una máquina física ni un proveedor de pago.
- La administración de altas/eliminaciones de hosts permanece en `fleetctl`;
  Desktop descubre, selecciona, conecta y refresca, pero no edita configuración.
- No hay chat, timeline, creación de sesión ni ejecución de agentes: corresponden
  a la etapa P6.2-C.

### Hotfix de concurrencia posterior al cierre

El guard de `FleetWorkers` ahora pertenece al hilo OS que ejecuta la llamada SDK,
no al worker GPUI que espera hasta el deadline. La prueba de estrés verifica
que ningún timeout/cancelación devuelve el permiso mientras la llamada sigue
bloqueada y que el cupo se recupera al terminar el transporte. No cambia el
contrato SSH ni inicia/detiene agentes. Validación CI del hotfix: pendiente.
