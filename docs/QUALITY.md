# Pruebas, CI, aceptación y política de errores

## Jerarquía de validación

1. **Contract tests**: codificación/decodificación de requests/events; schema version negotiation; fixtures positivos y negativos; pruebas de compatibilidad entre CLI y daemon.
2. **Unit tests**: SessionManager state machine, operación/idempotencia, monotonía de secuencias, ACL, error mapping, políticas de permisos.
3. **Process tests**: arrancar fleetd, runner y un fake agent como procesos separados; matar clientes, daemon, runner y agente en puntos sincronizados.
4. **Provider fixtures**: mensajes reales anonimizados de Pi, Codex, OpenCode con versión del binario; contratos por capacidades.
5. **Smoke real**: opt-in para los tres agentes, bajo HOME o perfil aislado cuando posible. No guardar credenciales; documentar si se probó en modo autenticado.
6. **Platform matrix**: Linux x86_64 es la plataforma de referencia hasta P6. Windows y macOS se validan funcionalmente en P7; hasta entonces no se declara compatibilidad. Se conservan las comprobaciones de compilación cruzada disponibles.

## CI desde el primer commit de implementación

- Rust stable y versión mínima declarada; `cargo fmt --all -- --check`.
- `cargo check --workspace --all-targets --locked`.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`.
- `cargo test --workspace --locked` y test serial de procesos cuando afecte carreras.
- `git diff --check`; cargo deny/audit como puerta de release, no como excusa para refactorizaciones masivas.
- CI en Linux x86_64 (plataforma de referencia). Las comprobaciones de compilación cruzada disponibles se conservan como señal de portabilidad, pero una sola plataforma no constituye aceptación cross-platform. La validación funcional en Windows/macOS se programa en P7.
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

Detalle en [`P1_1_RESULTS.md`](P1_1_RESULTS.md). Windows/macOS pendientes de validación en P7.

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

### P2.2 — ejecutadas y en verde

- 38 tests unitarios, incluyendo redelivery de `commit_event`, FKs de aprobaciones, reserva concurrente, fail-closed ante fallo de diagnóstico, confirmación nativa por sesión y redacción de errores del proveedor.
- `tests/e2e_approvals.sh`: exige `dispatching` antes de la confirmación, cubre fallo de entrega con `FLEET_FAKE_FAIL_ANSWER` y verifica que el reintento devuelve `NeedsReconciliation`.
- `tests/e2e_long_replay.sh`: páginas de 256 eventos más la cola viva posterior al cutoff.
- Smoke real opt-in `tests/e2e_opencode_approval.sh` repetido con `once` y `reject`; pendiente de automatizar en CI con fixture local.


### P3 — ejecutadas y en verde

- 48 tests unitarios, incluyendo identidad/ACK de runner, migración v3 y frame del protocolo interno.
- `tests/e2e_p3_survival.sh`: SIGKILL real de `fleetd`, runner vivo, spool generado durante la caída y replay sin duplicar prompt.
- `tests/e2e_p3_approval_disconnect.sh`: aprobación pendiente durante la caída del daemon, decidida una sola vez tras reattach.
- `tests/e2e_p3_identity.sh`: token manipulado y versión incompatible rechazados.
- `tests/e2e_opencode_survival.sh` (opt-in): comando `sleep 12` real con daemon muerto, archivo y timeline verificados.
- Smoke real de aprobaciones `once`/`reject` y suite P1/P2 sin regresiones.

### P3.1 — regresiones tras la revisión

- Unit test `runner_event_and_dedup_are_one_atomic_commit`: commit
  único de timeline/deduplicación/ACK, replay idempotente y rollback en
  secuencia conflictiva.
- E2E `e2e_persistence.sh`: la sesión finalizada se recupera y permite
  un segundo prompt con nueva clave, sin duplicar el anterior.
- E2E `e2e_p3_survival.sh`: respeta el corte del watcher en
  `TurnCompleted` y comprueba por separado `SessionStateChanged(Running)`.
- El CI comprueba los E2E de supervivencia, aprobaciones durante
  desconexión e identidad de runner. Los smokes OpenCode reales siguen opt-in.
- Considerar PASS solamente la ejecución correspondiente al HEAD integrado,
  nunca la salida de un commit anterior.

### P3.2 — revisión independiente verificada

- 48 tests unitarios (añadidos: persistencia de estado de runner al parar, guard de recovery duplicado, normalización de sesión `recovering` huérfana).
- `tests/e2e_p3_provider_health.sh`: fail-closed ante pérdida de salud del proveedor con runner IPC vivo; ejecutado y en verde, añadido a CI.
- Correcciones de revisión: estados de runner al parar, `recover_runners` por estado de sesión, guard de ingress duplicado, fail-closed en writes de estado, avisos de spool duraderos, compilación Windows del runner, permisos del token y propagación de `operation_key`.
- `e2e_opencode_survival.sh` espera ahora al evento terminal durable antes de aseverar replay (la transición a `Recovering` se publica antes de que el ingress consuma el spool).

### P3.3 — auditoría final y regresiones

- 48 tests unitarios (incluye 3 tests del spool que saturan 1024 eventos, límite de bytes e idempotencia de reserva; y regresiones de ACK fallido y stop sin runner).
- Nuevos E2E: `e2e_p3_runner_kill.sh`, `e2e_p3_multi_session.sh`, `e2e_p3_provider_health.sh`. Todos en verde y añadidos a CI.
- Correcciones: firma Windows de `expected_token`, fail-closed en ACK, predicado de salud unificado, colas acotadas, stop sin runner, `AgentNotice` de saturación, limpieza de runners huérfanos y resistencia temporal de `e2e_backpressure.sh`.
- Windows: cross-check parcial PASS (dominio/protocolo/agente/core/CLI); workspace completo NO verificado por falta de compilador MinGW. Aislamiento entre usuarios en Windows NO implementado.


### P3 — criterios de cierre en Linux (estrategia de plataforma de referencia)

| Criterio P3 | Evidencia ejecutada sobre el HEAD de la rama |
|---|---|
| Supervivencia del runner ante caída de `fleetd` | `tests/e2e_p3_survival.sh`: SIGKILL de `fleetd`, runner vivo, turno continuado |
| Recuperación autenticada | `tests/e2e_p3_identity.sh`: token manipulado y versión de protocolo incompatible rechazados; `tests/e2e_p3_survival.sh`: reattach tras reinicio |
| Replay durable sin duplicados | `tests/e2e_persistence.sh`, `tests/e2e_long_replay.sh`, `tests/e2e_p3_multi_session.sh`, `tests/e2e_p3_survival.sh` |
| Aprobaciones consistentes | `tests/e2e_approvals.sh`, `tests/e2e_p3_approval_disconnect.sh`, `tests/e2e_opencode_approval.sh` (real, `once`/`reject`) |
| Spool acotado y backpressure | tests unitarios del spool (1024 eventos, límite de bytes, avisos duraderos) y `tests/e2e_backpressure.sh` |
| CI y pruebas reales de OpenCode | GitHub Actions `cargo` success en el commit final; `e2e_opencode.sh`, `e2e_opencode_approval.sh`, `e2e_opencode_survival.sh` ejecutados localmente con `GF_E2E_REAL=1` |

Plataformas pendientes de validación funcional: **Windows y macOS (P7)**. No se afirma que funcionen. Se mantienen las comprobaciones de compilación cruzada disponibles (parciales, sin SQLite embebido en Windows).


### P4.1-H — validación final de Codex (22/11/2026)

| Validación | Resultado |
|---|---|
| `cargo fmt/check/clippy -D warnings/test/build`, `git diff --check` | PASS |
| 14 E2E deterministas P1–P3, incluidas OpenCode y P3 survival | PASS |
| 8 smokes reales `GF_E2E_REAL=1` sobre `d1eab2a`: chat, aprobación con archivo aislado, interrupción, controles con persistencia V4 y efecto nativo, descubrimiento/historial con herramientas, supervivencia, concurrencia y aprobación superviviente a SIGKILL de `fleetd` | PASS |
| 72 tests unitarios y 14 E2E deterministas P1–P3 | PASS |
| Migración V3→V4 con sesión previa persistida | PASS |
| Cross-compilación Windows de crates portables (dominio, protocolo, agente, core, driver Codex, composición) | PASS como señal de portabilidad |
| Adopción de sesiones externas con escritura | UNSUPPORTED deliberado: sin garantía de exclusividad; no se simuló |
| Cambios de modelo/esfuerzo durante un turno activo | No habilitados: requieren `experimentalApi`; se ofrecen solo por turno |

Windows y macOS continúan pendientes de validación funcional en P7. Detalle en [`P4_1_RESULTS.md`](P4_1_RESULTS.md).
