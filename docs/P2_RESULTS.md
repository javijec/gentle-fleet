# P2 — persistencia durable y aprobaciones interactivas

Rama: `feature/p2-persistence-approvals`. Plataforma validada: **Linux x86_64**.
Windows sigue sin validar (transporte con token, ver P1.1).

`SessionManager` sigue siendo independiente de OpenCode: `fleet-core` no importa
el driver, `reqwest`, `serde_json` ni SQLite concreto. Persiste a través del trait
`fleet_domain::store::SessionStore`; la única implementación real es
`fleet-store-sqlite`. Codex y Gentle Shell/Pi podrán usar el mismo store y modelo
de aprobación en P4 sin tocar el Core.

## 1. Esquema SQLite implementado

Base por host/usuario en `$XDG_DATA_HOME/gentle-fleet/fleet.sqlite` (o
`FLEET_DB_PATH` en pruebas), directorio 0700, WAL, `foreign_keys=ON`,
`busy_timeout=5000`.

| Tabla | Contenido |
|---|---|
| `workspaces` | `id`, `path` único |
| `sessions` | `session_id`, `workspace_id`, `provider_id`, `state`, `turn_state`, timestamps |
| `turns` | `turn_id`, `session_id`, `operation_id`, `state`, timestamps |
| `operations` | `operation_id`, `operation_key` único, `kind`, `session_id`, `target_id`, `fingerprint`, `status`, `message`, timestamps |
| `events` | `session_id`, `seq`, `event_id`, `kind`, `payload` JSON, `created_at`; PK `(session_id, seq)` |
| `approvals` | `approval_id`, `session_id`, `turn_id`, `kind`, `message`, `resources`, `allowed_decisions`, `status`, `native_request_id`, `decision`, timestamps |

No hay tablas `hosts`, `runner_bindings` ni spool: pertenecen a P3 y no hay caso
de uso en P2. `native_request_id` se guarda como dato opaco y **no se publica**
en el wire.

Índice `idx_events_session_seq` para paginación por cursor. Un reintento con el
mismo `(session_id, seq, event_id)` es idempotente; un `seq` distinto con otro
`event_id` produce `sequence_conflict`.

## 2. Migraciones y transacciones

- `PRAGMA user_version`: 0 → 1 aplica `MIGRATION_V1` dentro de una transacción.
- `init()` ejecuta `PRAGMA integrity_check`; una DB corrupta devuelve
  `corrupt_database` y no se usa.
- `reserve_operation` y `append_event` usan transacciones explícitas. Un evento
  se persiste **antes** de publicarse en el EventBus; si el store falla, el
  evento no sale y `SendPrompt` devuelve error.
- El `seq` lo sigue asignando Fleet dentro del lock de sesión; al reabrir se
  continúa desde `MAX(seq)+1`. Los IDs generados tras un reinicio usan una base
  temporal para no colisionar con IDs persistidos.
- `resolve_approval` es un `UPDATE ... WHERE status IN ('pending','delivered')`:
  solo la primera decisión ganadora afecta una fila.

## 3. Timeline durable y replay

- `SubscribeTimeline` suscribe el bus bajo el mismo lock con el que lee la
  primera página durable: sin hueco snapshot/stream.
- `ReadTimeline { session_id, since_seq, limit }` devuelve páginas de hasta 256
  eventos; no carga la timeline completa en memoria.
- Dos clientes reciben la misma secuencia `seq`; deduplican por `seq`. Un cliente
  que cae puede reconectarse con `since_seq` y reconstruir el historial.
- Los cursores inválidos o fuera de retención se reportan como error tipado;
  P2 no introduce poda ni `Gap` durable (no hay retención configurada aún).
- Backpressure de P1.1 se conserva: cola acotada y `Notice{Backpressure}`.

## 4. Ciclo de vida de operaciones

`reserved → accepted → completed | failed | uncertain`. La reserva (con
fingerprint `{session_id}\0{text}` para prompt, o sesión+aprobación+decisión para
`answer_approval`) ocurre antes de despachar al driver. Misma clave y mismos
argumentos devuelven la misma operación; argumentos distintos devuelven
`Conflict`. `GetOperation` consulta el estado durable.

Un timeout o pérdida del canal deja la operación en `uncertain`/`failed`, nunca
dispara un reenvío. Tras reiniciar `fleetd`, reutilizar la clave devuelve el
recibo original sin volver a enviar el prompt (VERIFIED en `e2e_persistence.sh`).

## 5. Modelo de aprobaciones

`ApprovalRequest` es un recurso de dominio con `approval_id`, sesión, turno,
tipo, mensaje, recursos, decisiones permitidas, estado y `native_request_id`
opaco. El driver traduce la solicitud nativa; el SessionManager crea el recurso,
lo persiste y emite `ApprovalRequested`. La interfaz del driver ya tenía
`answer(request_id, decision)`; P2 la implementa en OpenCode.

Decisiones: `once`, `always`, `reject` según OpenAPI 2.3.2. `always` no se probó
en el smoke real. Ninguna aprobación se acepta por ausencia de respuesta,
desconexión o timeout. Si entregar la decisión al proveedor falla, el estado
pasa a `uncertain` y no se reintenta automáticamente.

## 6. Integración real con OpenCode

- `permission.asked` → `ApprovalRequested` con `id`, `action`, `resources` y
  decisiones `[once, always, reject]`.
- `POST /api/session/{id}/permission/{requestID}/reply {"decision":...}` → 204.
- `permission.replied` → `ApprovalResolved`; también se confirma localmente tras
  el 204 para no perder la confirmación si el SSE se corta.
- Las sesiones-de prueba se crean con
  `permissions:[{"action":"shell","resource":"*","effect":"ask"}]` cuando se
  activa `FLEET_OPENCODE_ASK_SHELL=1`.
- Smokes reales: aprobación `once` creó el archivo; `reject` no lo creó. Las
  sesiones nativas se borraron al terminar.

## 7. Sesiones y reinicios

Tras reiniciar `fleetd`, las sesiones persistentes se cargan con
`state=NeedsReconciliation` y `turn_state=Uncertain` si no hay runner verificado.
No se recrea ningún proceso ni se reenvía un prompt pendiente. Un turno en
ejecución cuando muere el daemon **no** se declara vivo: su resultado queda
incierto. Distinguimos cliente desconectado (daemon y sesión siguen), cliente
reiniciado (relee timeline), daemon reiniciado (recupera datos, no runner) y
runner desaparecido (P3).

## 8. Pruebas ejecutadas y resultados

| Prueba | Resultado |
|---|---|
| `cargo fmt --all -- --check` | PASS |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS |
| `cargo test --workspace --locked` | PASS — 25 tests unitarios |
| `cargo build --workspace --locked` | PASS |
| `tests/e2e_fake.sh` | PASS |
| `tests/e2e_concurrency.sh` | PASS |
| `tests/e2e_backpressure.sh` | PASS |
| `tests/e2e_unix_permissions.sh` | MODOS VERIFIED (0600/0700); cruce con otro usuario **UNTESTED** en este entorno |
| `tests/e2e_persistence.sh` | PASS — timeline idéntica antes/después de reiniciar `fleetd`, operación reutilizada sin reenvío |
| `tests/e2e_approvals.sh` | PASS — un solo ganador, rechazo y evento observable |
| `tests/e2e_opencode.sh` | PASS — smoke real con dos clientes |
| `tests/e2e_opencode_approval.sh` (`GF_E2E_REAL=1`) | PASS — `once` y `reject` reales, sin sesiones nativas residuales |

Tests unitarios nuevos: migración/integridad, paginación por cursor,
idempotencia tras reapertura, aprobación con un solo ganador, recuperación de
sesión tras reinicio y flujo de aprobación con FakeDriver.

## 9. Limitaciones y pendientes

- Windows no validado; la garantía del token local depende de la ACL del fichero.
- No hay runner independiente: morir `fleetd` pierde el proceso agente; P3.
- No hay retención/poda ni `Gap` durable: los cursores antiguos no se podan en P2.
- `always` y `session.permission.create` de OpenCode siguen sin probarse.
- La reconciliación de aprobaciones huérfanas tras reinicio del daemon no está
  implementada: el mapeo nativo vive en el driver y no sobrevive al proceso.
- No se persisten secretos del proveedor.

## 10. Requisitos para P3 y P4

- P3: runner con identidad persistente, spool acotado y reattach; las tablas de
  runner/binding se añadirán con su propio caso de uso, no en P2.
- P4: los drivers Codex y Pi implementarán el mismo `AgentDriver` y traducirán
  sus decisiones nativas al modelo de aprobación común, reutilizando el store.
