# P4.1 — Codex: resultados de implementación (Linux x86_64)

**Rama:** `feature/p4-multi-provider`. **Fecha:** 22/11/2026.
**Versión validada:** `codex-cli 0.171.0` (`codex --version`), App Server `[experimental]` con `capabilities.experimentalApi:false`.
**Estado:** **CERRADA** en Linux para el alcance implementado; la adopción con escritura queda explícitamente bloqueada por falta de garantía de exclusividad.

## 1. Commits

| Commit | Contenido |
|---|---|
| `155a6d7` | P4.1-A: redacción de errores en stop, interrupt extremo a extremo, composición de proveedores, compatibilidad de wire |
| `7451eae` | P4.1-B…G: driver Codex, aprobaciones tipadas, controles, descubrimiento/historial y E2E reales |
| `3c88703` | Propagación de argumentos tipados de aprobación desde la API al driver |

## 2. Arquitectura implementada

- **`fleet-driver-codex`** (crate nuevo): `CodexFactory` + `CodexDriver`. El runner posee un proceso `codex app-server` por sesión; el driver nunca toca la base Fleet.
- **`rpc.rs`**: cliente JSON-RPC 2.0 sobre stdio con metros acotados (1 MiB/frame, 256 notificaciones, 64 requests del servidor), correlación por ID, timesouts (15 s init / 30 s llamadas) y un hilo lector que sigue entregando eventos mientras espera respuestas propias. Los requests iniciados por el servidor **nunca** se descartan: la cola acotada bloquea al lector, no pierde aprobaciones. Los errores/nativos se redactan antes de publicarse.
- **`events.rs`**: traducción pura de notificaciones a `DomainEvent`. `thread/status/changed` con estado `idle` **no** cierra turnos; solo `turn/completed`/`turn/failed`. Los deltas y el mensaje final se deduplican: si hubo deltas, no se emite un `AssistantMessage` duplicado. Los IDs `threadId`/`turnId`/`itemId`/ID JSON-RPC nunca se usan como IDs Fleet.
- **Composición**: `fleet-providers` registra `fake`, `opencode` y `codex` en una sola función compartida por `fleetd` y `fleet-runner`; fuera de `fleet-agent`, sin ciclos. Test de paridad incluido.
- **API**: nuevas operaciones `InterruptTurn`, `ListControls`, `SetControl`, `ListNativeSessions`, `ImportNativeHistory`; `AnswerApproval` acepta argumentos tipados opcionales. Todos los campos nuevos usan `#[serde(default)]`; se añadieron tests que deserializan frames y `AgentSummary` antiguos. `WIRE` sigue siendo `fleet-v1-p2` y `RUNNER_WIRE` `fleet-runner-v1`.

## 3. Contratos implementados frente a propuestos

| Contrato | Estado |
|---|---|
| `AgentDriver` núcleo (open/send/next_event/answer/close/inspect_recovery) | Reutilizado sin cambios de firma |
| `DriverFactory`/`ProviderRegistry` | Reutilizados; composición movida a `fleet-providers` |
| Interrupt hasta `AgentDriver::interrupt` | **Implementado** en ambos bucles y en la API pública; `cancel_turn` dejó de ser no-op |
| Aprobaciones: `approval_id` + `option_id` públicos, opciones anunciadas, argumentos tipados y validados | **Implementado**. El ID nativo y la acción nativa son privados del driver; `serverRequest/resolved` confirma la entrega; entrega incierta → `NeedsReconciliation`/`Uncertain`, sin reenvío |
| Controles modelo/esfuerzo | **Implementado** en modo estable: `model/list` descubre IDs y esfuerzos; `ListControls`/`SetControl`; selección por turno vía `AgentInput.controls`. `thread/settings/update` y `turn/settings/update` NO se habilitan (requieren `experimentalApi`); el estado devuelto es `deferred` durante un turno activo |
| Descubrimiento de sesiones nativas | **Implementado** a nivel factory, de solo lectura, con `native_ref` opaco (hash FNV) y señales de actividad (`thread/loaded/list`, `activeFlags`); no crea runner ni sesión |
| Importación de historial | **Implementada** con procedencia `imported_from_provider`, `itemsView:"full"` e idempotencia por `operation_key` (`OperationKind::ImportHistory`) |
| Adopción con escritura de sesiones externas | **NO implementada ni simulada**: Codex no ofrece lease/bloqueo demostrable frente a carreras; `loaded=false`/ausencia de PID no bastan. Queda `UNSUPPORTED` por decisión de seguridad (F11/F12) |
| Recuperación de turno en vuelo entre procesos | **UNSUPPORTED**: `thread/resume` recupera historial; `turn/steer` exige turno activo en el mismo proceso |
| `turn/steer`, `thread/archive`, settings experimentales | No implementados (fuera del alcance mínimo verificado) |

## 4. Capacidades reales verificadas (Codex 0.171.0)

| Capacidad | Clasificación | Evidencia |
|---|---|---|
| Handshake `initialize`/`initialized`, `thread/start`, `model/list` | VERIFIED | `e2e_p4_codex_controls.sh`, sondeo con esquema regenerado |
| Chat, streaming y terminal real (`turn/completed`) | VERIFIED | `e2e_p4_codex_chat.sh`: 6 deltas + `turn_completed Completed` |
| Herramientas (`item/started`/`item/completed`, `commandExecution`) | VERIFIED | `e2e_p4_codex_approval.sh` y `chat`: `tool_started`, `tool_output`, `tool_completed` |
| Aprobación nativa parametrizada y opciones `accept,cancel,amend_execpolicy` | VERIFIED | `e2e_p4_codex_approval.sh` con `FLEET_CODEX_SANDBOX=read-only`: `approval_requested` → `answer accept` → `approval_resolved` → herramienta → turno completado |
| Argumentos tipados (`amend_execpolicy`) | VERIFIED | prueba real con `fleetctl answer ... amend_execpolicy execpolicy_amendment=...`: herramienta completó y turno terminó |
| `turn/interrupt` de extremo a extremo | VERIFIED | `e2e_p4_codex_interrupt.sh`: `agent_notice` + `turn_completed Interrupted` |
| Modelos/esfuerzos descubiertos y cambio por turno | VERIFIED (lectura y selección `deferred`) | `e2e_p4_codex_controls.sh`: 10 modelos, 6 esfuerzos, valor inválido rechazado |
| Descubrimiento global de sesiones | VERIFIED | `e2e_p4_codex_discovery.sh`: refs opacos `cx-*`, workspaces, estado `notLoaded`, `externally_active=false` |
| Importación de historial idempotente | VERIFIED | mismo script: 2 mensajes con `provenance=imported_from_provider`; segunda llamada con la misma clave devolvió 2 sin duplicar eventos |
| Supervivencia a SIGKILL de `fleetd`, spool y replay sin duplicados | VERIFIED | `e2e_p4_codex_survival.sh`: 18 eventos spoleados, reattach, un solo `user` en replay, operación `completed` |
| Concurrencia OpenCode + Codex | VERIFIED | `e2e_p4_codex_concurrency.sh`: ambos turnos terminales, sin contaminación entre timelines |
| Adopción de sesión externa con escritura | UNSUPPORTED | sin garantía de exclusividad del proveedor |
| Recuperación de turno en vuelo | UNSUPPORTED | limitación del protocolo Codex |
| Cambio de modelo/esfuerzo **dentro** de un turno activo | UNTESTED / no habilitado | requiere `experimentalApi`; Fleet no lo activa |
| `turn/steer`, `thread/search`, `process/*` | UNTESTED | no implementados |

## 5. Regresiones y validación

- `cargo fmt --all -- --check`, `cargo check --workspace --all-targets --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, `cargo build --workspace --locked`, `git diff --check`: **PASS**.
- 14 scripts E2E deterministas P1–P3 (incluidos `e2e_opencode`, `e2e_p3_survival`, `e2e_p3_approval_disconnect`, `e2e_p3_runner_kill`, `e2e_p3_multi_session`): **PASS**.
- 7 scripts reales nuevos con `GF_E2E_REAL=1`: chat, aprobación, interrupción, controles, descubrimiento/historial, supervivencia, concurrencia: **PASS**.
- Cross-compilación `x86_64-pc-windows-gnu` de crates portables (`fleet-domain`, `fleet-protocol`, `fleet-agent`, `fleet-core`, `fleet-driver-codex`, `fleet-providers`): **PASS** como señal de portabilidad, no como validación funcional (Windows sigue en P7).
- No se copió `auth.json`, no se modificó `CODEX_HOME` personal ni se registraron secretos. Los smokes usaron la autenticación nativa heredada y workspaces temporales.

## 6. Riesgos y limitaciones

| Riesgo | Severidad | Estado |
|---|---|---|
| Adopción de sesión externa activa sin exclusividad demostrable | Alta | Bloqueada: solo descubrimiento/lectura; no se introdujo adopción insegura |
| `availableDecisions` no está en el esquema generado | Media | El driver lo trata como campo opcional en runtime y ofrece defaults por método |
| `thread/items/list` devuelve wrappers y `thread/turns/list` devuelve items completos | Media | La importación usa `itemsView:"full"`; los wrappers no se reinterpretan |
| `session_snapshot`/feed de Gentle no aplica aquí | — | Fuera de P4.1 |
| Controles deseados en memoria del daemon | Media | No sobreviven a un reinicio de `fleetd`; se documentan como preferencia de turno, no autoridad durable |
| Puerto de transporte/IDs nativos en errores de stop | Media | Corregido en P4.1-A con redacción y test de regresión |

## 7. Veredicto

**P4.1 se considera CERRADA en Linux** para el alcance implementado y verificado: Codex opera como proveedor de Gentle Fleet con la misma API, SessionManager, runner persistente, aprobaciones, persistencia y timeline que OpenCode, sin regresiones P1–P3 y con capacidades declaradas honestamente. Quedan fuera, con evidencia y sin simulación, la adopción con escritura, la recuperación de turno en vuelo y los cambios de configuración dentro de un turno activo. Windows/macOS permanecen pendientes de P7.
