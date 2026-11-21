# P4 — Diseño de contratos y arquitectura propuesta (P4.0)

**Estado:** propuesta técnica para revisión. **Rama:** `feature/p4-multi-provider`.
**Referencia funcional:** `docs/P4_SPEC.md` (18 decisiones, no modificadas).
**Regla:** este documento **no ratifica** nombres definitivos de traits, crates, tablas, migraciones ni operaciones wire (P4_SPEC §5, §98). Presenta alternativas con evidencia de código y recomienda una; la ratificación se hace en el corte que implemente el caso de uso probado.

No se modificó código de producción en P4.0.

## 1. Principios de diseño aplicados

1. `SessionManager` sigue siendo autoridad de estado, aprobaciones e idempotencia **sin ramas por proveedor** (P4_SPEC §3, D02).
2. El conocimiento del protocolo nativo queda en el driver dentro de `fleet-runner`; `fleetd` no importa Codex/Pi/Gentle Shell (P3-D1/D3).
3. IDs nativos opacos y confinados al driver; las decisiones y opciones nativas se traducen a tipos Fleet antes de publicar (D04, P0-D6, P4-F08).
4. Una capacidad ausente responde con error tipado `UnsupportedCapability`, nunca con éxito fingido (P4_SPEC §4).
5. Reutilizar primero; ampliar solo con caso de uso probado; sin crates, tablas ni traits vacíos por anticipación (P4_SPEC §3, §98).
6. Mantener la compatibilidad con runners P3: spool durable, ACK, dedup por `(runner_id, runner_seq)` y reattachment autenticado.

## 2. Inventario real de contratos (verificado en código)

| Capa | Contrato actual | Ubicación | ¿Reutilizable para P4? |
|---|---|---|---|
| Driver | `AgentDriver`: `descriptor`, `open(ctx)`, `send(AgentInput)`, `next_event(timeout)`, `close`, `inspect_recovery`; defaults `answer`, `interrupt`, `session_state` | `crates/fleet-agent/src/lib.rs:112-137` | **Sí**, núcleo estable; insuficiente para controles/adopción |
| Driver | `LaunchContext{workspace,mode}`, `AgentInput{text,turn_id}`, `SendReceipt{accepted}`, `RecoveryEvidence{session_alive,detail}` | `fleet-agent/src/lib.rs:86-106` | Parcial; `RecoveryEvidence` demasiado pobre (B8) |
| Registro | `DriverFactory{id,compatibility,create,metadata,probe,supports}`, `ProviderRegistry` | `fleet-agent/src/lib.rs:141-215` | **Sí**; el registro en sí no necesita cambios |
| Dominio | `ProviderCapabilities`: 8 bools (`streaming_text`, `tool_events`, `session_history`, `native_turn_ids`, `cancel_turn`, `approvals`, `can_own_process`, `can_attach_service`); `ProviderDescriptor` | `fleet-domain/src/lib.rs:73-91` | Parcial; faltan capacidades de control/adopción/import (B2) |
| Core | `RunnerCommand{Prompt,Answer,Interrupt,Stop}`, `RunnerHandle`, `RunnerLauncher` | `fleet-core/src/runner.rs:24-75` | **Sí** para prompt/aprobación/parada; `Interrupt` es no-op |
| Wire runner | `RunnerCommand`, `RunnerRequest{Hello,Status,Submit,Ack,Pending,QueryApprovals,Shutdown}`, `RunnerResponse` | `fleet-protocol/src/runner.rs:34-127` | **Sí**, versionado; sin variantes de control/adopción |
| Proceso runner | `DriverCommand{Prompt,Answer,Interrupt(no-op),Stop}`; bridge `native_key()` | `fleet-runner/src/main.rs:343-359,456-523`; `fleetd/src/process_runner.rs:136-185,272-285` | Igual que core |
| Store | `SessionStore` (sin métodos de sesión nativa), `OperationKind{SendPrompt,AnswerApproval}`, `events` JSON payload, `approvals` con `allowed_decisions` JSON | `fleet-domain/src/store.rs:60-64,161-257`; `fleet-store-sqlite/src/lib.rs:18-123,1328-1333` | **Sí** para lo existente; sin vínculo de adopción (B3) |
| API pública | `Request`/`Response` con 14 variantes; `Frame{Request,Response,Event,Notice}`; `AgentSummary{id,version,availability,capabilities,mode}` | `fleet-protocol/src/lib.rs:17-172` | **Sí**; sin operaciones de control/adopción/historial (B3) |
| CLI/daemon | `fleetctl` 12 comandos; dispatch `match` en `fleetd/main.rs:154-353`; registros en `fleetd/main.rs:55-58` y `fleet-runner/main.rs:80-84` | `fleetctl/src/main.rs:85-123` | **Sí**; alta redundancia de registro (B5) |

Esquema SQLite actual: **versión 3** (`fleet-store-sqlite/src/lib.rs:310-390`); V1 tablas base, V2 FKs de aprobaciones, V3 `runners` + `runner_events`. `sessions` guarda solo `provider_id`, sin identidad nativa ni procedencia de adopción. Próxima migración, si se justifica, sería **V4**.

## 3. Brechas que bloquean P4.1–P4.3

| ID | Brecha (verificada en código) | Severidad | Bloquea |
|---|---|---|---|
| B1 | No hay métodos get/set de modelo, esfuerzo, modo ni perfil en `AgentDriver` ni en ninguna otra capa | **P0** | F09, F14 |
| B2 | `ProviderCapabilities` no puede anunciar controles, enumeración, adopción ni importación; `AgentSummary` no los expone | **P0** | F04, F09-F14 |
| B3 | No existe enumeración/adopción/importación de sesiones nativas en store, wire ni runner | **P0** | F10-F13 |
| B4 | Aprobaciones = `Vec<String>` + un `decision: String` (`core/lib.rs:1273-1278,1361-1366`); sin opciones ni argumentos nativos tipados | **P0** | F08 |
| B5 | Registro de proveedores duplicado en dos binarios + `Cargo.toml`; derive de registros | **P1** | P4.1/2/3 |
| B6 | `interrupt` no soportado por defecto y no-op en ambos bucles (`core/runner.rs:235`, `runner/main.rs:270`); `cancel_turn` declarado pero inutilizable | **P1** | P4.2 (Pi `abort`) |
| B7 | `finish_stop` publica `error` crudo y `mark_stop_unconfirmed` publica `reason` crudo como eventos (`core/lib.rs:1725-1732,1769-1776`): posible fuga de IDs/URLs nativos | **P1** | Regla "IDs opacos" |
| B8 | `RecoveryEvidence` = un bool + string libre; sin distinción historial/reattach/turno vivo | **P2** | F11, F13 |
| B9 | No hay tipos `ModelId`/`Effort`/modo/perfil en `fleet-domain` (`RunnerMode` es propiedad del proceso, no modo de agente) | **P2** | F09, F14 |
| B10 | No existen crates de Codex/Pi/Gentle Shell; OpenCode es la única referencia real | **P1** | P4.1/2/3 (se resuelve en esos cortes, no aquí) |

**Contención actual de IDs nativos (a preservar):** `#[serde(skip)] native_request_id` (`domain/store.rs:151-153`) con test que prueba que no sale al wire (`protocol/src/lib.rs:197-220`); `map_stream_event` descarta `sessionID` (`driver-opencode/src/lib.rs:390-512`); `redact_provider_error` (`agent/src/lib.rs:35-77`) se aplica en fallos de turno, eventos y aprobación incierta. Las dos excepciones son B7.

## 4. Decisiones de diseño propuestas (con alternativas)

### 4.A — Controles dinámicos (modelo, esfuerzo, modo, perfil) · B1/B2/B9 · F09/F14

**Caso de uso probado:** Codex `model/list` + `effort` en `turn/start`; Pi `set_model`/`set_thinking_level`; Gentle `get_permission_mode`/`apply_profile` y `get_config{store:"models"}`. Conmutar en caliente en Codex está detrás de `experimentalApi`; en Pi/Gentle está documentado pero sin ejecutar.

| Alternativa | Descripción | Ventajas | Desventajas / riesgo | Impacto |
|---|---|---|---|---|
| **A1 (recomendada): capacidades + methods por defecto con `UnsupportedCapability`** | Ampliar `ProviderCapabilities` con descriptores de control (consultable/mutable, ámbito `turn`\|`session`\|`profile`) y añadir a `AgentDriver` métodos `list_controls()`, `get_control(name)`, `set_control(name,value,scope)` con implementación por defecto que devuelve unsupported. `ControlValue` tipado, sin JSON crudo. | Núcleo pequeño; ningún proveedor queda obligado; `SessionManager` no conoce nombres de proveedor; sirve para los tres. | Hay que definir vocabulario de nombres de control y validar valores por proveedor; el valor "aplicado" debe confirmarse leyendo el estado nativo. | `fleet-domain` (capacidades + `ControlValue`), `fleet-agent`, cuatro capas de runner/wire, `AgentSummary`, `fleetctl`. Sin `SessionManager`. |
| A2: trait separado `DriverControls` con downcasting desde `AgentDriver` | `SessionManager` pregunta si el driver implementa el trait extendido. | No ensucia `AgentDriver` con métodos que muchos drivers no usarán. | Downcasting y `Any` rompen el estilo estático del código; dos identificationes de capacidad (bool + trait) que pueden divergir; más difícil de propagar por runner. | Similar en wire, peor mantenimiento. |
| A3: comandos genéricos opacos `SetNativeSetting{key,json}` | Un solo comando con JSON. | Mínimo código inicial. | Viola P4-SPEC §4 (sin JSON privado), pierde validación y versionado, filtra configuración nativa. | **Rechazada.** |

**Recomendación:** A1. Las cadenas de control son un enum/identificador tipado con valor tipado; el namespace describe la capacidad (`model`, `effort`, `permission_mode`, `profile`), no el proveedor. Confirmación de estado efectivo obligatoria: `set_control` devuelve el valor observado tras la operación o un estado `Deferred`/`Rejected` cuando el proveedor no permite aplicarlo en un turno activo (P4_SPEC §4.1.3). No se modifica ningún fichero de configuración del usuario directamente.

### 4.B — Aprobaciones nativas tipadas · B4 · F07/F08

**Caso de uso probado:** Codex `availableDecisions` con decisiones parametrizadas (`acceptWithExecpolicyAmendment`, `applyNetworkPolicyAmendment`, `requestUserInput.answers`, `permissions.scope`); Gentle `select` con tres opciones; OpenCode `once|always|reject` como línea base de regresión.

| Alternativa | Descripción | Ventajas | Desventajas | Impacto |
|---|---|---|---|---|
| **B1 (recomendada): opciones y respuesta tipadas opacas en el evento Fleet** | `ApprovalRequested` incorpora un vector de `ApprovalOption` (identificador opaco + etiqueta + parámetros tipados opcionales) y `ApprovalAnswer` lleva `approval_id` Fleet + opción elegida + argumentos opcionales. El driver traduce al payload nativo. | Respeta F08; conserva `approval_id` como clave Fleet; idempotencia y durabilidad siguen en `SessionManager`; sin JSON privado en el wire cliente. | Amplía `DomainEvent`, `AnswerApproval`, `RunnerCommand::Answer`, `Approval` store y `AgentDriver::answer`. Requiere migrar el payload JSON de aprobaciones o añadir columna con caso de uso probado. | Dominio, protocolo, core, runner, store, CLI. Sin ramas por proveedor. |
| B2: mapa `decision: String` + `arguments: Vec<String>` | Mantener un string con dictamen genérico y argumentos posicionales. | Cambio pequeño; reutiliza `allowed_decisions` actual. | Pierde semántica de decisiones parametrizadas, facilita autoaprobación accidental y no distingue `decline` de `cancel`. | Menor, pero incompleto para Codex. |
| B3: reenviar `availableDecisions` tal cual desde el proveedor | Copiar el array nativo al cliente. | Cero traducción. | **Rechazada:** filtra RPC privado y acopla el wire a versiones del proveedor (P4-SPEC §3). |

**Recomendación:** B1. `approval_id` sigue siendo la única clave pública; los IDs nativos (`requestId`, `threadId`) permanecen `#[serde(skip)]` y dentro del runner. El bucle de runner debe atender requests iniciados por el servidor mientras espera respuestas (§1.2 de `P4_RPC_RESEARCH.md`). Entrega incierta → `Uncertain` y reconciliación; **nunca hay reenvío automático** (D09, P0-D4).

### 4.C — Descubrimiento, adopción e importación de sesiones · B3 · F10–F13

**Caso de uso probado:** Codex `thread/list` + `thread/loaded/list` + `thread/resume{excludeTurns:true}` + `thread/turns/list`; Gentle `list_sessions` (con `sessions_unavailable` en sesión efímera) + `get_snapshot`; Pi sesiones en `--session-dir` con `get_entries`.

| Alternativa | Descripción | Ventajas | Desventajas | Impacto |
|---|---|---|---|---|
| **C1 (recomendada): capacidades de enumeración en el driver + operación de adopción con vínculo durable** | `AgentDriver` gana `list_native_sessions()` (solo lectura) y `inspect_native_session(id)` con evidencia de actividad. `SessionManager` ofrece una operación `AdoptNativeSession` que crea sesión Fleet + runner propio y registra vínculo `(provider_id, native_id)` con procedencia. La importación de historial es una operación separada e idempotente. | Cumple "descubrimiento amplio, adopción conservadora" (F10/F11); mantiene la autoridad en Fleet; el proveedor sigue siendo opaco; separa historial de turno vivo. | Requiere un vínculo persistente: **proponer** tabla V4 con `UNIQUE(provider_id, native_session_id)`, `fleet_session_id`, `adopted_at`, `evidence`, `provenance`, y un `OperationKind::AdoptSession`/`ImportHistory` para idempotencia. Migración solo cuando P4.1/P4.2/P4.3 ejecuten el caso real. | Dominio (store trait), store-sqlite (V4), core, wire, runner, CLI. `SessionManager` gana una operación genérica, **no una rama por proveedor**. |
| C2: el driver importa directamente a la base Fleet | El driver escribe sesiones/historial. | Menos capas. | **Rechazada:** rompe "un driver no escribe bases Fleet" (D03, reglas AGENTS) y la autoridad de `SessionManager`. | — |
| C3: solo importar historial, sin adopción ni vínculo | Leer eventos y crear sesión nueva. | Muy simple. | Incumple F11/F12 (exclusividad y adopción explícita) y permitiría dos administradores Fleet sobre la misma sesión externa. | Insuficiente. |

**Evidencia de exclusividad (F12):** la ausencia de PID o de `loaded` no acredita inactividad. La propuesta exige combinar señales del proveedor (`activeFlags`, `thread/loaded/list`, `session_snapshot`/feed, `sessions_unavailable`) y, si no se puede determinar con seguridad, **bloquear** la adopción. No se tomará posesión de procesos ajenos activos. pytest de adopción debe fallar cerrado cuando la evidencia sea ambigua.

**Procedencia (F13):** cada evento importado llevará una marca `observed_live | imported_from_provider | reconstructed_partial`. No se inventan deltas ni detalles ausentes. Historial recuperable **no** implica turno en vuelo reanudable.

### 4.D — Taxonomía de recuperación · B8 · F11/F13

**Caso de uso probado:** P3 demostró reattach de runner vivo; Codex solo ofrece `thread/resume` de historial; Pi/Gentle dependen de que el runner posea el proceso.

Proponer sustituir el string libre por un `RecoveryEvidence` con `kind: enum { RunnerAliveAttached, NativeHistoryResumable, NativeProcessRecovered, LiveTurnRecoverable, Uncertain, Lost }` manteniendo `session_alive: bool` y un `detail` **redactado** para compatibilidad. Cada kind debe mapearse a una evidencia verificable; `LiveTurnRecoverable` solo se emite si el proveedor y el runner pueden demostrarlo (hoy: ninguno de los tres nuevos). Alternativa rechazada: un único bool `can_resume`, porque mezcla cinco operaciones distintas y facilitaría reenviar prompts inciertos (D09).

### 4.E — Registro de proveedores sin divergencia · B5

**Caso de uso probado:** hoy hay que editar `Cargo.toml` y dos `register(...)` en `fleetd/main.rs` y `fleet-runner/main.rs`.

| Alternativa | Descripción | Ventajas | Desventajas |
|---|---|---|---|
| **E1 (recomendada): función compartida `register_builtin_providers(&mut ProviderRegistry)` en `fleet-agent` (o crate de arranque común)** | Ambos binarios llaman la misma función; cada driver se registra una sola vez. | Elimina la divergencia; `SessionManager` sigue sin conocer proveedores; añadir un driver es un cambio local. | Pequeña dependencia de los binarios hacia el módulo de registro; requiere que cada `DriverFactory` sea `Send + Sync` (ya lo es en la práctica). |
| E2: descubrimiento por inventario/directorio en tiempo de ejecución | Plugins. | Extensibilidad dinámica. | **Rechazada:** fuera de alcance (P4_SPEC §3 prohíbe plugins en P4), más superficie de seguridad. |
| E3: dejar los dos registros y añadir un test de paridad | Test que compara IDs en ambos binarios. | Cero refactor. | Detecta la divergencia tarde y no evita el error; se acepta solo como red de seguridad temporal junto con E1. |

### 4.F — Corrección de fuga de IDs nativos · B7

Aplicar `redact_provider_error()` a los dos mensajes publicados sin redactar en `fleet-core/src/lib.rs:1725-1732` y `1769-1776`. No hay alternativa arquitectónica: es un arreglo local con prueba de regresión que falla si un `ses_`/URL aparece en un `EventKind::AgentError` o `AgentNotice` público. Debe hacerse **antes** de añadir drivers nuevos, porque Codex y Gentle producen más IDs y rutas.

### 4.G — `interrupt` de extremo a extremo · B6

El `RunnerCommand::Interrupt` ya existe en core y wire, pero es no-op en el bucle de core (`core/runner.rs:235`) y en el runner (`runner/main.rs:270`). Proponer conectarlo a `AgentDriver::interrupt` en ambos bucles, con traducción por driver: Codex `turn/interrupt`, Pi/Gentle `abort`. Es un cambio pequeño, habilita `cancel_turn` de forma real y evita declarar una capacidad inutilizable. Alternativa rechazada: quitar `cancel_turn` de las capacidades (perdería una función que los proveedores sí ofrecen).

## 5. Forma propuesta de contratos (orientativa, no ratificada)

~~~rust
// Solo intención; los nombres se fijan en el corte que implemente el caso de uso.
enum ControlName { Model, Effort, PermissionMode, Profile }
enum ControlScope { Turn, Session, Profile }
enum ControlValue { Id(String), Level(String), Mode(String), /* ... */ }

trait DriverControls {  // o métodos por defecto en AgentDriver (alternativa A1)
    fn list_controls(&self) -> Vec<ControlDescriptor>;   // consultable, mutable, ámbito
    fn get_control(&mut self, name: ControlName) -> Result<ControlValue, AgentError>;
    fn set_control(&mut self, name: ControlName, value: ControlValue,
                   scope: ControlScope) -> Result<ControlValue, AgentError>; // devuelve estado efectivo
}

struct ApprovalOption { key: OpaqueKey, label: String, params: Option<TypedParams> }
struct NativeSession { native_id: OpaqueKey, workspace_hint: Option<PathBuf>,
                       state_hint: ActivityHint, evidence: Vec<ActivityEvidence> }

enum RecoveryKind { RunnerAliveAttached, NativeHistoryResumable,
                    NativeProcessRecovered, LiveTurnRecoverable, Uncertain, Lost }
~~~

Reglas de oro: `OpaqueKey` nunca se serializa al cliente; `TypedParams` es un enum cerrado por versión de contrato; los nombres de control no llevan prefijo de proveedor; todo cambio devuelve estado efectivo o `Deferred`/`Rejected` observable.

## 6. Compatibilidad y extensiones futuras

- **P3:** ninguna propuesta toca el spool, el ACK ni la dedup. Las nuevas operaciones viajan por el mismo `RunnerCommand`/`RunnerRequest` tipados y quedan cubiertas por el spool si el daemon cae. `Interrupt` y `set_control` deben ser idempotentes por `operation_key`.
- **P5 (remoto):** mantener el wire tipado y versionado permite tunelizar el mismo protocolo sin exponer RPC nativo. No se añade SSH ni TCP público en P4.
- **P6 (UI):** `AgentSummary` y `ListAgents` exponen capacidades y descriptores de control; la UI podrá preguntar sin conocer al proveedor. No se toma GPUI como referencia (P4-F17).
- **Subagentes:** solo se documentan (§3.6 de `P4_RPC_RESEARCH.md`). No se crea trait, comando ni tabla para ellos en P4 (P4-F05).

## 7. Lo que explícitamente NO se propone en P4.0

- Nuevos crates de driver (se crean en P4.1/2/3 con su prueba vertical).
- Migración V4 ni tabla de vínculos hasta que un corte ejecute adopción real (regla "no nuevas tablas sin caso de uso probado").
- Cambios en `docs/P4_SPEC.md` ni en las 18 decisiones.
- `SessionManager` con `match provider_id`.
- ACP genérico, plugins, worktrees, editor de configuración, Herdr, GPUI ni subagentes.
- Declarar Windows/macOS como soportados (P4-F16).

## 8. Decisiones técnicas pendientes (mapa de P4_SPEC §8)

| # | Tema abierto en P4.0 | Estado tras la investigación | Requiere |
|---|---|---|---|
| 1 | Métodos/eventos reales de Gentle Shell | Resuelto para 4.8.0: 23 métodos, 9 eventos, `session_snapshot` condicional | Validar en sesión real enlazada en P4.3 |
| 2 | Componentes RPC compartibles Pi/Gentle | Parcial: transporte Pi y Extension UI comunes; Desktop RPC es específico | Decidir forma de código compartido en P4.3 |
| 3 | Extensiones concretas del código Fleet | Propuestas A1/B1/C1/D/E/F/G con alternativas | Ratificación en el corte que las implemente |
| 4 | Evidencia de exclusividad por proveedor | Codex `activeFlags`/`loaded/list`; Gentle `sessions_unavailable`/feed; Pi sin señal global | Prueba de adopción fallo-cerrado en P4.1–P4.3 |
| 5 | Historial y recuperación por proveedor | Diferenciados en §4.D y matriz; Pi 1.6.0 sin modelo real aquí | Smoke real con credenciales del usuario en cada corte |
| 6 | Cambios seguros durante turno activo | Codex per-turn sí, settings experimental; Pi/Gentle documentados sin ejecutar | Definir política `Deferred` y confirmación de estado efectivo |
| 7 | Subconjunto de RPC ampliado a tipar | Ninguno se implementa en P4; se documentan todos | Decisión posterior a P6 para subagentes |

## 9. Riesgos

| Riesgo | Severidad | Mitigación propuesta |
|---|---|---|
| `availableDecisions` de Codex no está en el esquema generado | Media | No validar contra esquema estricto; contrato propio con `additionalProperties` permitido y fixture de runtime |
| `thread/items/list` rechazado en hilo no materializado | Media | No ofrecer importación hasta el primer mensaje; error tipado `HistoryNotReady` |
| `session_snapshot` no negociado en sesiones efímeras de Gentle | Media | Negociar por sesión y usar `get_snapshot` al detectar hueco de `seq` |
| Pi 1.6.0 sin modelos configurados en este entorno | Media | Separar "consulta de modelos verificada" de "modelo usable": UNTESTED, no PASS |
| Cambio de versiones entre P0 y P4.0 (Pi 1.5.2 → 1.6.0) | Media | Evidencia por versión; `codex --version`/`pi --version` en cada smoke; range de compatibilidad (P0-D5) |
| Fuga de IDs nativos en rutas de stop | Alta (P1) | Corrección 4.F con test de regresión antes de P4.1 |
| Dos registros de proveedores divergentes | Media (P1) | 4.E1 + test de paridad |
| Adopción de sesión externa activa | Alta | 4.C con fallo cerrado y evidencia; nunca autoaprobar ni tomar posesión |
