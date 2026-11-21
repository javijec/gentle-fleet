# P3 — runner persistente y recuperación de sesiones

Rama: `feature/p3-persistent-runner`. Base: `fe33197` (P2.2 integrado en `main`).
Plataforma validada: **Linux x86_64**. Windows sigue sin validar.

## 1. Arquitectura final

```text
fleetctl / (futuro SDK)
        | envelopes Fleet (UDS 0600)
        v
     fleetd  --- SQLite autoritativa (fleet.sqlite)
        | RunnerRegistry (identidad persistida)
        | RunnerFrame JSONL (UDS 0600 por runner)
        v
  fleet-runner (proceso independiente)
        | AgentDriver (FakeDriver / OpenCodeDriver)
        | spool SQLite propio (sólo eventos no ACK)
        v
  OpenCode / proveedor nativo
```

- `fleetd`: API de clientes, workspaces, sesiones, turnos, operaciones,
  aprobaciones, timeline y cursores. No posee hilos de driver ni escribe el
  spool del runner.
- `fleet-runner`: posee el `AgentDriver`, el proceso/servicio nativo, la cola de
  operaciones, el spool y la conexión con el daemon. No escribe `fleet.sqlite`.
- `SessionManager` sólo conoce `AgentDriver` y `SessionStore`; en P3 additionally
  conoce el trait `RunnerLauncher`. No importa OpenCode, Codex, Pi ni SQLite.
- `InProcessRunnerLauncher` existe únicamente para tests de contrato; el daemon
  real usa `ProcessRunnerLauncher`.

Decisiones ratificadas en [`DECISIONS.md`](DECISIONS.md#decisiones-p3--runner-persistente-21112026): P3-D1…D8.

## 2. Protocolo daemon↔runner

Módulo `fleet_protocol::runner`, wire `fleet-runner-v1`, JSONL con `RunnerFrame`:

- `Request`: `Hello`, `Status`, `Submit`, `Ack`, `Pending`, `QueryApprovals`,
  `Shutdown`.
- `Response`: `Hello`, `Status`, `Accepted`, `Pending`, `Approvals`,
  `Shutdown`; errores `RunnerError{code,message}`.
- `Notice`: `StopUnconfirmed`, `approval_delivery_failed`.
- `Event`: `runner_seq` + `AgentEvent` tipado. No contiene JSON nativo ni IDs
  del proveedor.

`Hello` valida `runner_id`, `boot_id`, `protocol_version` y token privado. Una
identidad o versión incompatibles se rechazan sin reattach (VERIFIED).

## 3. Estados y ciclo de vida

- Runner: `Starting → Active → Disconnected → Recovering → Stopping → Stopped`,
  con `Lost` y `Uncertain` como ramas de fallo.
- Sesión: `Running → (daemon muerto) NeedsReconciliation → Recovering → Running`
  sólo cuando el handshake y el estado real del runner lo confirman.
- Si el runner no responde o la identidad no coincide, la sesión permanece
  `NeedsReconciliation`. No se crea un runner sustituto ni se reenvía ningún prompt.
- El PID es informativo: la identidad de arranque (`boot_id`) y `generation`
  impiden confundir un proceso reutilizado.

## 4. Spool y ACK

- Base por runner: `<runtime>/fleet-runner-<runner_id>.spool.sqlite` (WAL).
- Tablas: `spool(runner_seq, event, bytes, acked)` y
  `operations(operation_key, operation_id, created_at)`.
- Límites: `MAX_SPOOL_EVENTS=1024`, `MAX_SPOOL_BYTES=4 MiB`.
- Reglas verificadas: el runner persiste antes de entregar; el daemon persiste
  el evento Fleet y registra `(runner_id, runner_seq)` antes de hacer ACK; un
  reenvío se deduplica y sólo confirma; los ACK se compactan sin borrar
  evidencia pendiente; al llenarse el spool se emite un `AgentError` de
  saturación y se suspenden nuevas operaciones, sin descartar eventos.
- `seq` público sigue siendo asignado por `SessionManager`/SQLite. `runner_seq`
  es privado del par daemon-runner.

## 5. Migraciones SQLite

`user_version=3` crea:

- `runners(runner_id, session_id UNIQUE FK, provider_id, host_id, boot_id,
  pid, protocol_version, generation, status, endpoint, last_ack_seq, timestamps)`.
- `runner_events(runner_id FK, runner_seq, fleet_event_id, created_at,
  PRIMARY KEY(runner_id, runner_seq))`.

Las migraciones 1 y 2 (P2) se conservan; la 3 es forward-only. Los datos P2
sobreviven (ver `e2e_persistence.sh` y `e2e_long_replay.sh`).

## 6. Garantías de operaciones y aprobaciones

- `operation_key` se reserva en el runner antes de despachar; el mismo key no
  provoca un segundo efecto.
- Si la entrega de una decisión falla, el runner responde
  `approval_delivery_failed`; Fleet marca la aprobación y la operación
  `uncertain` y exige reconciliación. No hay auto-reintento.
- Una aprobación pendiente durante la muerte del daemon sigue en la DB y en el
  runner; tras el reattach se puede responder una sola vez (VERIFIED).
- Dos clientes que responden simultáneamente producen un solo ganador; el
  perdedor recibe `Conflict`.

## 7. Matriz de fallos

| Escenario | Resultado verificado |
|---|---|
| Cierra `fleetctl` | Sesión y runner continúan |
| Muere `fleetd` (SIGKILL) | Runner vivo, spool crece, reattach automático al reiniciar |
| Muere `fleet-runner` | Sesión queda `NeedsReconciliation`/`Uncertain`; no se inventa proceso |
| Muere el proveedor | No se declara `Running`; queda incierto (no probado con kill real) |
| Reinicio del host | Procesos perdidos; sobreviven datos SQLite y spool en disco; no se simula que sigue vivo |
| Token/boot_id inválido | Reattachment rechazado |
| Versión de protocolo incompatible | Reattachment rechazado |
| ACK perdido | Reenvío deduplicado por `(runner_id, runner_seq)` |

## 8. Pruebas ejecutadas y resultados

| Prueba | Resultado |
|---|---|
| `cargo fmt --all -- --check` | PASS |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS |
| `cargo test --workspace --locked` | PASS — 40 tests unitarios |
| `cargo build --workspace --locked` | PASS |
| `e2e_fake.sh` / `e2e_concurrency.sh` / `e2e_backpressure.sh` | PASS |
| `e2e_unix_permissions.sh` | modos 0600/0700 VERIFIED; cruce UID **UNTESTED** aquí |
| `e2e_persistence.sh` / `e2e_approvals.sh` / `e2e_long_replay.sh` | PASS sin regresiones P2 |
| `e2e_p3_survival.sh` | PASS — runner vivo tras SIGKILL, 7 eventos spooled, replay sin duplicar prompt |
| `e2e_p3_approval_disconnect.sh` | PASS — aprobación pendiente decidida una vez tras reattach |
| `e2e_p3_identity.sh` | PASS — token y versión inválidos rechazados |
| `e2e_opencode.sh` | PASS |
| `e2e_opencode_approval.sh` (`GF_E2E_REAL=1`) | PASS — `once` y `reject` |
| `e2e_opencode_survival.sh` (`GF_E2E_REAL=1`) | PASS — comando `sleep 12` terminó con `fleetd` muerto; replay y archivo verificados |

## 9. Limitaciones

- Windows no validado; el aislamiento depende de ACL de fichero/socket no probado.
- No se probó matar el proceso nativo de OpenCode (`service`) ni el host.
- El límite del spool está implementado pero no se saturó en un test real.
- La reconciliación de aprobaciones `uncertain` requiere intervención; no hay
  API automática en P3.
- No se implementaron P4, P5, P6, worktrees, plugins ni delegación multiagente.

## 10. Pendiente para P4

Registrar los drivers Codex y Pi con el mismo `AgentDriver` y `RunnerLauncher`,
reutilizando este spool y este modelo de aprobaciones sin tocar `SessionManager`.
El driver Pi necesita propiedad estricta del proceso (stdio muere con el
Cliente); el driver Codex debe aislar `CODEX_HOME` sin perder autenticación.
