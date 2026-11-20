# Pruebas, CI, aceptación y política de errores

## Jerarquía de validación

1. **Contract tests**: codificación/decodificación de requests/events; schema version negotiation; fixtures positivos y negativos; pruebas de compatibilidad entre CLI y daemon.
2. **Unit tests**: SessionManager state machine, operación/idempotencia, monotonía de secuencias, ACL, error mapping, políticas de permisos.
3. **Process tests**: arrancar fleetd, runner y un fake agent como procesos separados; matar clientes, daemon, runner y agente en puntos sincronizados.
4. **Provider fixtures**: mensajes reales anonimizados de Pi, Codex, OpenCode con versión del binario; contratos por capacidades.
5. **Smoke real**: opt-in para los tres agentes, bajo HOME o perfil aislado cuando posible. No guardar credenciales; documentar si se probó en modo autenticado.
6. **Platform matrix**: Linux y Windows para flujo local; remoto SSH en etapa P5.

## CI desde el primer commit de implementación

- Rust stable y versión mínima declarada; `cargo fmt --all -- --check`.
- `cargo check --workspace --all-targets --locked`.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`.
- `cargo test --workspace --locked` y test serial de procesos cuando afecte carreras.
- `git diff --check`; cargo deny/audit como puerta de release, no como excusa para refactorizaciones masivas.
- Matriz Linux + Windows en cambios de proceso/transporte. Un runner con una sola plataforma no constituye aceptación cross-platform.
- Sin dependencia Git a repositorios privados de Core/Protocol anteriores. En un monorepo hay un solo Cargo.lock que permite construir CI desde checkout.

## Pruebas negras mínimas, sin proveedor de pago

T01 Session creada en workspace existente y cwd efectivo comprobado por FakeDriver.
T02 CLI envía entrada y otro cliente recibe AssistantDelta y TurnCompleted en secuencia.
T03 Un cliente se desconecta y reconecta con cursor; sin pérdida ni duplicaciones lógicas.
T04 Dos clientes envían Start con la misma operación: exactamente una sesión/runner.
T05 Un prompt con ACK perdido nunca se reenvía automáticamente; se ve OutcomeUnknown.
T06 Una aprobación se resuelve una vez; competidor recibe Conflict; sin respuesta nunca se autoaprueba.
T07 El cliente lento produce Backpressure/Gap, sin consumir memoria sin límite.
T08 Daemon crash con runner vivo: reattach y replay del spool (P3).
T09 Muerte del runner se detecta y clasifica, sin crear worker sustituto a ciegas.
T10 Reinicio del host se diferencia claramente de reinicio de daemon; si el driver no puede recuperar, estado informado explícitamente.
T11 Ruta inválida o no autorizada no lanza un proceso.
T12 CLI remoto sobre SSH (P5) produce los mismos eventos/errores que local.

### P1.1 — ejecutadas y en verde

- `tests/e2e_fake.sh`: dos clientes, secuencia idéntica.
- `tests/e2e_concurrency.sh`: dos sesiones aisladas, segundo prompt rechazado con `TurnAlreadyRunning`, clave duplicada bajo concurrencia, stop con driver bloqueado en <2 s.
- `tests/e2e_backpressure.sh`: cola acotada, desconexión con `Backpressure`.
- `tests/e2e_unix_permissions.sh`: socket 0600, directorio 0700 y rechazo de otro usuario local (Linux).
- `tests/e2e_opencode.sh`: smoke real opt-in con dos clientes.
- 15 tests unitarios (`cargo test --workspace`), `cargo fmt --check` y `cargo clippy -D warnings`.

Detalle en [`P1_1_RESULTS.md`](P1_1_RESULTS.md). Windows sigue sin validar.

### P2 — ejecutadas y en verde

- 25 tests unitarios (store, migración, integridad, cursor, idempotencia, aprobaciones y recuperación).
- `tests/e2e_persistence.sh`: timeline antes/después de reiniciar `fleetd` y clave reutilizada sin reenvío.
- `tests/e2e_approvals.sh`: un solo ganador, rechazo y evento observable.
- `tests/e2e_opencode_approval.sh`: smoke real opt-in con `once` y `reject`.
- P1.1 y smoke real de OpenCode sin regresiones; detalle en [`P2_RESULTS.md`](P2_RESULTS.md).

## Criterios de aceptación por historia

- Cada historia vertical tiene una prueba que cruza **cliente SDK → Fleet API → SessionManager → Driver → respuesta/eventos → cliente**.
- Si solo pasa un test de almacén SQLite, la historia **no** está terminada.
- Si la función depende de capacidad de proveedor, registrarla y comprobarla en una versión real.
- Toda afirmación de reconexión especifica cuál proceso cayó: UI, conexión, daemon, runner, proveedor o host.
- Una prueba omitida se reporta como omitida, no aprobada.
- Los errores encontrados se clasifican como P0 (pérdida/duplicación grave), P1 (flujo principal imposible), P2 (limitaciones documentables), P3 (cosmético/mejora); solo P0/P1 bloquean avance inmediato.

## Política anti-parches

Antes de corregir un fallo: reproducirlo; identificar contrato o invariante afectado; demostrar test rojo; corregir en el módulo propietario; test verde; buscar regresiones. Si dos correcciones seguidas requieren tocar tres módulos no relacionados, detener el corte y revisar el límite arquitectónico. No perseguir una lista infinita de amenazas fuera del modelo local. Cada fase termina con un único documento breve de decisiones, no una auditoría integral de meses.

### P2.1 — casos regresivos adicionales

- `tests/e2e_long_replay.sh`: timeline de más de 256 eventos, comparación de 270 secuencias contiguas.
- `tests/e2e_concurrency.sh`: un prompt rechazado por turno ocupado no reserva `operation_key`.
- Unit tests: claves foráneas en cada nueva conexión, privacidad de ID nativo en el wire, estado de aprobación en `Dispatching` antes del ACK.
- Verificar los resultados de GitHub Actions sobre el último commit; los smokes de OpenCode real son opt-in.
