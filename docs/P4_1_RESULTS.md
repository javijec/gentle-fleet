# P4.1 — Codex: resultados de implementación (Linux x86_64)

**Rama:** `feature/p4-multi-provider`. **Fecha:** 22/11/2026.
**Versión validada:** `codex-cli 0.171.0` (`codex --version`), App Server `[experimental]` con `capabilities.experimentalApi:false`.
**Estado:** **P4.1 — CERRADA EN LINUX** tras la revalidación final P4.1-H sobre el commit `d1eab2a` (Codex `0.171.0`, 8 smokes reales PASS, regresiones y CI en verde). La adopción con escritura continúa bloqueada por falta de garantía de exclusividad; Windows y macOS siguen pendientes de P7.

## 1. Commits

| Commit | Contenido |
|---|---|
| `155a6d7` | P4.1-A: redacción de errores en stop, interrupt extremo a extremo, composición de proveedores, compatibilidad de wire |
| `7451eae` | P4.1-B…G: driver Codex, aprobaciones tipadas, controles, descubrimiento/historial y E2E reales |
| `3c88703` | Propagación de argumentos tipados de aprobación desde la API al driver |
| `a5cc4d1` | Documentación inicial de resultados |
| `ce9595b`…`dcb77b1` | Auditoría correctiva: RPC malformado, turnos inciertos, V4, historial tipado, aprobaciones sin autoaprobación |
| `d1eab2a` | P4.1-H: evidencia de control efectivo, herramientas importadas, migración V3→V4 y supervivencia de aprobaciones |

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
| Persistencia de controles deseados en el daemon | Resuelta en P4.1-H | La preferencia se guarda por sesión en SQLite V4 y sobrevive al reinicio de `fleetd`; su estado efectivo se contrasta por separado con el proveedor |
| Puerto de transporte/IDs nativos en errores de stop | Media | Corregido en P4.1-A con redacción y test de regresión |

## 7. Auditoría correctiva previa (contexto)

Se revisó el driver y se corrigieron comportamientos no cubiertos por el informe inicial:

- El lector JSON-RPC aplica backpressure en lugar de descartar notificaciones; la cola se drena mientras se esperan respuestas propias. Frames sobredimensionados o inválidos ahora fallan de manera explícita, y el código incluye tests de saturación y tamaño.
- Se respeta la configuración nativa de sandbox y aprobación. Las variables `FLEET_CODEX_*` solo anulan valores cuando están declaradas explícitamente.
- La huella de aprobación incluye argumentos tipados y evita reutilizar la misma clave con otros parámetros. Antes de reclamar una aprobación se comprueban opciones y campos anunciados en su evento persistido; el driver valida además preguntas y permisos nativos.
- El historial se pagina con detección de ciclo, conserva contenido original, agrega campos tipados opcionales para herramientas e identificadores de origen estables, y deduplica elementos incluso entre claves de operación distintas. La adopción con escritura permanece prohibida.
- Los errores de importación dejan el efecto como incierto en vez de informar completado; el destino debe ser de igual proveedor y no estar en turno activo. Una importación interrumpida puede contener un prefijo durable: no se prometen transacciones de lote.
- Las preferencias de controles se guardan por sesión en SQLite V4 para persistir tras reinicio de `fleetd`.
- Se mejoró la asociación de deltas con mensajes individuales; errores ambiguos al iniciar un turno dejan la operación `Uncertain`, sin reenvío automático.
- Los tests reales de chat, controles, aprobaciones y descubrimiento ya no admiten varios fallos silenciosos; CI valida sintaxis de scripts Bash. El CI no ejecuta llamadas pagas al modelo.

## 8. P4.1-H — Resultados finales de auditoría y validación real

**Versión instalada:** `codex-cli 0.171.0` (`codex --version`; binario `~/.local/share/mise/installs/codex/0.171.0/bin/codex`). `~/.codex/version.json` está obsoleto y no se usa para detección.
**Commit probado:** `d1eab2a` sobre `feature/p4-multi-provider` (HEAD final). No se modificó código después de ejecutar los smokes finales salvo documentación.

### Comandos ejecutados

```bash
cargo build --workspace --locked
for t in interrupt controls discovery survival concurrency chat approval approval_survival; do
  GF_E2E_REAL=1 bash tests/e2e_p4_codex_$t.sh
done
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
git diff --check
# 14 scripts deterministas P1-P3 definidos por CI y docs/QUALITY.md
```

### Resultado de los smokes reales

| Smoke real | Resultado | Evidencia |
|---|---|---|
| `e2e_p4_codex_chat.sh` | PASS | streaming, `turn_completed Completed`, respuesta `P4 CODEX OK` |
| `e2e_p4_codex_approval.sh` | PASS | sandbox `read-only`, `approval_requested`, opciones `accept,cancel,amend_execpolicy`, `approval_resolved`, archivo aislado `FLEET_APPROVAL_PROBE`, turno completado |
| `e2e_p4_codex_interrupt.sh` | PASS | `turn/interrupt` real, `agent_notice`, `turn_completed Interrupted`, sin terminal contradictoria |
| `e2e_p4_codex_controls.sh` | PASS | 10 modelos y 6 esfuerzos descubiertos, valor inválido rechazado, preferencia `effort=high` persistida tras reinicio de `fleetd`, `native_effort=high` comprobado en el thread nativo del siguiente turno |
| `e2e_p4_codex_discovery.sh` | PASS | refs opacos `cx-*`, workspace correcto, historial con 3 eventos `history`, herramienta estructurada importada, dedup con misma clave y con clave distinta |
| `e2e_p4_codex_survival.sh` | PASS | runner vivo tras SIGKILL de `fleetd`, eventos spoleados, reattach, replay sin duplicar prompt, operación `completed` |
| `e2e_p4_codex_concurrency.sh` | PASS | OpenCode y Codex terminales, timelines aisladas; al matar el runner Codex, OpenCode sigue completando turnos |
| `e2e_p4_codex_approval_survival.sh` | PASS | aprobación pendiente persistió tras SIGKILL de `fleetd`, sin resolución implícita, entregada explícitamente con `accept` y confirmada por el proveedor |

### Cantidad de pruebas

- **72 tests unitarios** (`cargo test --workspace --locked`, 19 bloques `test result: ok`).
- **14 E2E deterministas P1–P3** (incluidas OpenCode, replay, aprobaciones, backpressure, kill de runner y multi-sesión): PASS.
- **8 smokes reales opt-in** sobre Codex 0.171.0 y OpenCode 2.3.2: PASS.
- Verificaciones Rust y `git diff --check`: PASS. Cross-compilación Windows de crates portables: PASS como señal de portabilidad (no validación funcional; Windows/macOS → P7).

### Defectos y huecos corregidos en P4.1-H

| Hallazgo | Corrección | Commit |
|---|---|---|
| No se distinguía preferencia Fleet de control efectivamente aplicado | `NativeSessionInfo`reporta `model`/`effort` observados del proveedor; E2E verifica `native_effort=high` | `d1eab2a` |
| Sin prueba real de persistencia V4 tras reinicio de daemon | E2E ampliado con reinicio y comprobación `controls`; test V3→V4 con sesión previa | `d1eab2a` |
| Importación de historial sin comprobación de herramientas estructuradas | E2E verifica `history	tool` con procedencia y dedup | `d1eab2a` |
| No se probaba conservación de aprobaciones ante muerte del daemon | Nuevo smoke `approval_survival` | `d1eab2a` |
| Aislamiento ante fallo de un proveedor no cubierto | E2E mata el runner Codex y verifica OpenCode | `d1eab2a` |
| RPC malformado, turnos inciertos, aprobaciones huérfanas, historial no tipado | Correcciones de auditoría sucesivas | `ce9595b`…`dcb77b1` |

### Migraciones verificadas

Esquema SQLite **V4** (`session_controls`) con `PRAGMA user_version = 4`. El test `v3_database_migrates_to_v4_without_losing_persisted_sessions` crea una base V3 con workspace y sesión persistidos, la migra y comprueba que la sesión sobrevive y que se pueden guardar/listar preferencias. Los frames antiguos de `NativeSessionInfo` siguen deserializando (campos nuevos con `serde(default)`).

### Capacidades soportadas, parciales y no soportadas

- **Soportadas y verificadas:** chat/streaming, herramientas, terminal real por `turn/completed`, aprobaciones parametrizadas con `option_id` y argumentos tipados, `turn/interrupt`, modelos/esfuerzos descubiertos y aplicados por turno, descubrimiento de sesiones, historial con herramientas y procedencia, supervivencia P3, concurrencia.
- **Parciales:** control dentro de un turno activo (solo `deferred`, se aplica al siguiente); importación interrumpida (prefijo durable, sin atomicidad de lote); paginación de historial (el código recorre cursores y detecta ciclos; el smoke real comprobó una página, **no** una sesión de más de 100 turnos).
- **No soportadas:** adopción de sesiones externas con escritura (sin exclusividad verificable); recuperación de turno en vuelo entre procesos; `turn/steer`, `thread/search` y APIs experimentales (`experimentalApi` no habilitado).

### Riesgos pendientes

| Riesgo | Severidad | Mitigación |
|---|---|---|
| Adopción con escritura sin garantía de exclusividad | Alta | Bloqueada; solo descubrimiento/lectura |
| Importación parcial con prefijo durable | Media | Operación `Uncertain`, sin reintento automático; documentado |
| `availableDecisions` no declarado en el esquema generado | Media | Campo runtime opcional con defaults por método |
| Preferencias de control en memoria del daemon antes de V4 | Resuelta | Persistidas en V4 y verificadas tras reinicio |
| Windows/macOS sin validar funcionalmente | Media (P7) | Cross-compile verde; no se declara compatibilidad |

### Estado final

**P4.1 — CERRADA EN LINUX.** Todo lo obligatorio fue ejecutado y pasó sobre el commit final. No se simuló ninguna capacidad no soportada por el proveedor. No se comenzó P4.2/P4.3/P4.4 ni se hizo merge a `main`.

## 9. P4.1-H2 — Corrección de concurrencia de importación

**Motivo:** una consulta nativa de historial puede tardar mientras otro cliente inicia un turno, y dos importaciones con la misma clave pueden competir entre la lectura previa y la reserva durable.

**Cambios aplicados en `fleet-core`:**

- Una reserva `Created` es la **única** autorización para consultar el proveedor y publicar historial. Ante `Existing`, se devuelve el resultado persistido si está `Completed`; `Reserved` o `Accepted` se informa como `OperationPending`, `Uncertain` como `NeedsReconciliation`, y `Failed` como `OperationFailed`. No se ejecuta una segunda importación.
- Se vuelve a validar el destino con el `ManagerState` bloqueado **después** de recuperar los datos nativos y **antes** de emitir eventos. Si la sesión cambió de proveedor/estado, tiene un turno activo o presenta estado incierto, se rechaza sin publicar el historial y se marca fallida la operación que no produjo efectos.
- Dos pruebas deterministas con una importación nativa bloqueada controladamente: una inicia un turno durante la consulta y comprueba `SessionBusy` sin nuevos eventos históricos; la otra enfrenta dos solicitudes con la misma clave y verifica propietario único, respuesta pendiente, replay del resultado completado y una sola ejecución nativa.

**Alcance de evidencia:** estas pruebas reproducen las carreras en forma determinista sin llamar a Codex; los ocho smokes reales P4.1-H anteriores validaron los flujos de proveedor pero no constituyen una ejecución nueva de estas dos carreras contra un App Server real. La importación parcial todavía puede dejar un prefijo durable marcado `Uncertain`, sin prometer atomicidad de lote.

