# P4 — Plan de pruebas y criterios de aceptación (P4.0)

**Estado:** plan propuesto para P4.1–P4.4. **Rama:** `feature/p4-multi-provider`.
**Referencias:** `docs/P4_SPEC.md` (decisiones funcionales), `docs/P4_PROVIDER_MATRIX.md`, `docs/P4_RPC_RESEARCH.md`, `docs/P4_CONTRACT_DESIGN.md`, `docs/QUALITY.md`.

## 1. Reglas de ejecución y evidencia

1. Toda prueba se etiqueta con **proveedor + versión exacta + plataforma** (`Linux x86_64`). La versión se detecta en runtime (`codex --version`, `pi --version`, `gentle-shell --version`, `opencode --version`), nunca se asume del nombre del binario.
2. Clasificación obligatoria del resultado: **VERIFIED** (comando reproducible en el informe), **OBSERVED** (captura real sin prueba completa), **UNTESTED**, **UNSUPPORTED**. Un test simulado no se presenta como prueba real.
3. Una capacidad ausente se reporta con error tipado `UnsupportedCapability`; el test debe verificar el error, no marcar PASS ficticio.
4. Ninguna prueba modifica credenciales, perfiles ni historiales personales: `CODEX_HOME`, cwd y `--session-dir` temporales; para Gentle Shell, solo métodos de lectura sobre el perfil real, nunca `set_config`/`apply_profile`/`set_permission_mode`/`switch_session` sobre la sesión del usuario.
5. Los smokes reales son **opt-in** (`GF_E2E_REAL=1`), con coste y binario documentados. Los fixtures deterministas usan FakeDriver/servidor local y no sustituyen al smoke real.
6. Secretos: no copiar `auth.json`, `service.json` ni tokens; solo comprobar presencia y permisos. Los logs y capturas pasan por `redact_provider_error` y por un escáner de patrones (`ses_`, `msg_`, `per_`, `call_`, `exec_`, `evt_`, `sh_`, URLs, rutas de HOME) antes de versionarse.
7. Las pruebas de concurrencia se serializan cuando compartan proceso/servicio nativo, según `docs/QUALITY.md`.
8. Los controles mutables solo se declaran VERIFIED tras probarlos en un **perfil realmente aislado**, comprobando el estado antes y después y sin alterar datos personales. Sin aislamiento confiable, marcar UNTESTED; no afirmar capacidad completa.
9. Para nuevos campos API/runner y persistencia, probar lectura de fixtures anteriores y errores tipados de incompatibilidad; no confundir `serde(default)` con compatibilidad demostrada.

## 2. Niveles de prueba

### 2.1 Unitarias (cargo test, deterministas)

| Área | Casos mínimos | Cumple |
|---|---|---|
| Traducción de protocolo → dominio | decoding/encoding de requests/events, `sessionID`/`threadId`/`turnId`/`requestId` descartados, `native_request_id` con `#[serde(skip)]`, sin JSON nativo en `Event` público | P0-D6, F08 |
| `ProviderCapabilities` | descriptores de control/enumeración/adopción/import; una capacidad ausente produce `UnsupportedCapability`; desactivable por versión | P0-D2, F04 |
| Aprobaciones tipadas | opciones nativas convertidas a `ApprovalOption`; `decline` ≠ `cancel`; `option_id` público Fleet distinto de IDs nativos y **estable por aprobación/replay**; parámetros `execpolicy_amendment`/`network_policy_amendment`/`answers`/`scope` tipados; opción no anunciada, argumentos inválidos o expirados → rechazo; idempotencia por `operation_key` y sin autoaprobación | D09, F07/F08 |
| Control de estado | `get_control`/`set_control` con perfil aislado; ámbito `turn`/`session`/`profile`; rechazo o `Deferred` durante turno si corresponde; confirmar estado efectivo y ausencia de escrituras sobre perfil personal | F09/F14 |
| Recuperación | cinco `RecoveryKind` distintos; `LiveTurnRecoverable` solo con evidencia; `Uncertain` y `Lost` no reenvían prompts | F11/F13, D09 |
| Store y adopción | vínculo `UNIQUE(provider_id, native_session_id)`; descubrimiento global de solo lectura a nivel factory/catálogo, **sin crear runner de conversación**; actividad externa, evidencia ambigua o carrera sin protección de exclusividad → fallo cerrado; procedencia `observed_live`/`imported_from_provider`/`reconstructed_partial`; importación idempotente; sin duplicados en replay | F10–F13 |
| Redacción | `redact_provider_error` sobre `AgentError` y `AgentNotice` en rutas de stop; ningún ID nativo o URL en eventos públicos | IDs opacos |
| Interrupt | `RunnerCommand::Interrupt` llega a `AgentDriver::interrupt` en ambos bucles; `cancel_turn` deja de ser inutilizable | B6 |
| Registro | composición del catálogo **fuera de `fleet-agent`** o duplicación temporal controlada; misma lista de IDs, versiones, capacidades y modos en ambos binarios, sin dependencias circulares | B5 |

### 2.2 Contract tests (fixtures offline, matrix-driven)

Un único conjunto de aserciones parametrizado por capacidades, con tres capas de fixture por proveedor:

- **OpenCode 2.3.2**: regresión P1–P3. `permission.asked` → `once|always|reject`, `permission.replied`, SSE, replay y kill de daemon/runners. Es la línea base: cualquier cambio de contrato debe mantener estas pruebas en verde.
- **Codex 0.171.0**: esquema regenerado (714 v2 / 110 ClientRequest), handshake, `thread/start/list/read/resume`, `turn/start/interrupt/steer`, requests de aprobación con `id` numérico y `availableDecisions` en runtime, `serverRequest/resolved`, `thread/status.changed.activeFlags`, gate `requires experimentalApi`.
- **Pi 1.6.0**: `get_state`, `get_available_models` (lista vacía → UNTESTED de modelo real), `get_available_thinking_levels`, `new_session`, `get_messages`, `get_entries`, `get_commands`, `agent_settled` vs `agent_end`, `message_end`, `tool_execution_*`, framing solo por LF (incluir U+2028/U+2029).
- **Gentle Shell 4.8.0**: `capabilities` con 23 métodos y 8 eventos anunciados en sesión efímera, más `session_snapshot` como noveno evento condicional, envelope `kind:"response"|"event"`, correlation por `requestId`, límites 64 KiB/128/64, códigos de error, `select` de permiso con opciones nativas, distinción Extension UI vs Desktop RPC.

Fixtures a versionar: los `/tmp/p4_*` de la investigación se convertirán en **sondas ejecutables reproducibles y fixtures anonimizados versionados** bajo `spikes/p4/` o `tests/fixtures/` en los cortes correspondientes, para permitir validación independiente. **No se versionan capturas sin sanitizar.**

### 2.3 Pruebas de proceso (E2E deterministas)

Convención de nombres: `tests/e2e_p4_<proveedor>_<caso>.sh`, con `timeout` acotado y logs en `/tmp`. Deben reutilizar el patrón de `tests/e2e_p3_*.sh` (SIGKILL de `fleetd`, reattach, replay, dos clientes).

| Escenario | Codex P4.1 | Pi P4.2 | Gentle P4.3 | Conformidad P4.4 |
|---|---|---|---|---|
| Arrancar proveedor, un prompt, turno completo y texto | ✔ | ✔ | ✔ | matriz 4 proveedores |
| Streaming + mensaje final canónico | `item/agentMessage/delta` + `turn/completed` | `message_update` + `message_end` | igual Pi + `agent_settled` | aserción por capability |
| Herramienta | `item/started/completed` | `tool_execution_*` | `tool_execution_*` | si `tool_events` |
| Aprobación tipada y entrega | `availableDecisions` parametrizadas | solo si Extension UI presente | `select` nativo | nunca autoaprobar; fallo cerrado |
| Cancelación / steer | `turn/interrupt`, `turn/steer` | `abort` | `abort` | si `cancel_turn` |
| Modelo / esfuerzo / modo / perfil | `model/list`, effort per-turn, settings experimental (rechazo documentado) | `set_model`, `set_thinking_level` | lectura y setters en perfil aislado cuando sean soportados | verificar cambio real y estado efectivo; si no puede aislarse, UNTESTED |
| Historial estructurado y procedencia | `thread/turns/list` paginado, `excludeTurns:true` | `get_entries`, `get_messages` | `get_snapshot`, `get_messages` | sin deltas inventados |
| Descubrimiento de sesiones | `thread/list`, `thread/loaded/list` | `--session-dir` | `list_sessions` (`sessions_unavailable` en efímera) | enumeración no intrusiva |
| Adopción segura / bloqueo de externa activa | con `activeFlags` | sin señal global → fallo cerrado | con feed/`session_snapshot` | exclusividad, vínculo durable |
| Muerte de `fleetd` + reattach + replay sin duplicados | ✔ | ✔ | ✔ | regresión P3 obligatoria |
| Kill del runner y fail-closed | ✔ | ✔ | ✔ | regresión P3 obligatoria |
| Dos clientes, concurrencia entre proveedores | ✔ | ✔ | ✔ | ✔ |
| Spool saturado y backpressure | ✔ | ✔ | ✔ | ✔ |

### 2.4 Smokes reales (opt-in, con versión)

| Script | Proveedor | Evidencia exigida |
|---|---|---|
| `tests/e2e_opencode_{,approval,survival}.sh` | OpenCode 2.3.2 | Ya ejecutados en P3; se reejecutan en P4.4 como regresión |
| `tests/e2e_p4_codex_chat.sh` | Codex 0.171.0 | versión, prompt real, herramienta/historial si la cuenta lo permite, aprobación sin auto-responder |
| `tests/e2e_p4_pi_chat.sh` | Pi 1.6.0 | versión, `agent_settled`, herramienta, modelo configurado o UNTESTED explícito |
| `tests/e2e_p4_gentle_shell_chat.sh` | Gentle Shell 4.8.0 (Pi 1.5.2) | versión, negociación `capabilities`, chat, permiso `select` si aparece, sin métodos modificadores sobre el perfil real |

Si una cuenta no tiene modelo disponible (caso Pi 1.6.0 en este entorno), el smoke se marca **UNTESTED** con el stderr exacto; no se reporta PASS.

## 3. Criterios de aceptación por corte

### P4.1 — Codex

- App Server estructurado 0.171.0 con handshake y bucle que atiende requests del servidor.
- Thread/turno/streaming/herramientas reales; `turn/interrupt` y `turn/steer` verificados o marcados UNTESTED con motivo.
- Aprobaciones parametrizadas sin autoaprobación; `serverRequest/resolved` como confirmación; entrega incierta → `Uncertain`.
- `thread/list`, `thread/loaded/list`, `thread/resume{excludeTurns:true}` y paginación; **diferencia explícita** entre resume de historial y turno en vuelo (inexistente).
- `thread/items/list` no disponible en hilo no materializado → error tipado, no fallo oculto.
- Validar que las opciones públicas de aprobación sobreviven a replay sin filtrar IDs nativos; probar catálogo de proveedores sin ciclos de dependencias.
- Regresiones P1–P3 en verde; sin cambios de contrato que rompan OpenCode.
- `cargo fmt/check/clippy -D warnings/test/build` y `git diff --check` en verde; CI verde.
- Documentación: matriz y RPC research actualizados con evidencia de versión; contradicción de `~/.codex/version.json` registrada.

### P4.2 — Pi estándar

- Driver de proceso propio con stdin/stdout, framing LF, correlación por `id`.
- Fin de trabajo por `agent_settled`; `agent_end` no usado como terminal.
- `abort`/`clear_queue` conectados a `Interrupt`/`Stop`; `cancel_turn` declarado solo si se probó.
- Historial con `get_entries`/`get_messages` y procedencia; `switch_session` con evidencia, no por presencia de `sessionId` en `--no-session`.
- Modelos/esfuerzo: consulta verificada; si no hay modelo configurado, reportar UNTESTED, nunca PASS.
- Sin extensiones Gentle: no `/gentle:*`, no Desktop RPC, no modos de permiso (Extension UI genérica documentada pero no confundida con capacidades Gentle).
- Misma validación Rust y CI que P4.1.

### P4.3 — Gentle Shell

- Proveedor independiente (P4-F02), no alias de Pi; transporte Pi RPC compartido donde corresponda.
- Negociación `capabilities` por sesión: 23 métodos/8 eventos anunciados en la prueba efímera, con `session_snapshot` como noveno evento condicional documentado, `get_snapshot` al detectar hueco de `seq`.
- Chat y eventos de Extension UI/Desktop RPC correctamente separados; permisos nativos `select` tipados, sin autoaprobación.
- Controles de lectura verificados; setters para modelo, esfuerzo, modos y perfiles anunciados como modificables requieren **prueba de escritura con perfil/entorno aislado, confirmación nativa y verificación de no afectar datos personales**. No ejecutar `set_config`/`apply_profile`/`set_permission_mode` sobre el perfil real. Si no hay aislamiento fiable, marcar UNTESTED, no PASS.
- Subagentes documentados pero no implementados ni expuestos (P4-F05); sin referencia a GPUI (P4-F17).
- Misma validación Rust y CI que P4.1.

### P4.4 — Conformidad, estabilización y entrega

- Suite matrix-driven para cuatro proveedores; una celda ✗ produce error tipado o estado no soportado explícito, nunca PASS fingido.
- Casos reales: conversación, herramienta, permiso si existe, modelo/esfuerzo si existe, stop, historial, adopción segura, reinicio de `fleetd`, reattach, replay, dos clientes, concurrencia entre proveedores, idempotencia y errores.
- Regresiones completas P1–P3 con OpenCode como línea base.
- Linux x86_64: CI y smokes reales con versiones identificadas. Windows/macOS siguen pendientes de P7.
- Matriz final publicada con soportado/parcial/no soportado/no probado por proveedor y versión.
- Cierre solo si se cumplen estos criterios; hallazgos ajenos se registran con prioridad sin rediseñar la arquitectura.

### Reglas adicionales para cerrar P4.1–P4.4

- `Interrupt` no puede seguir siendo no-op. Verificar extremo a extremo los proveedores que lo anuncian, los errores y carreras frente a stop/turno ya finalizado; sin E2E, no declarar `cancel_turn`.
- Aplicar redacción de URLs/IDs nativos en `finish_stop` y `mark_stop_unconfirmed`, con test de regresión en eventos públicos.
- La adopción exige exclusividad verificable y protección contra TOCTOU; `loaded=false`/PID ausente no son suficientes. Si no existe garantía nativa fiable, habilitar solo descubrimiento/lectura y declarar adopción UNSUPPORTED.
- Las migraciones y cambios de wire deben tener pruebas de compatibilidad con persistencia y clientes anteriores.
- Métodos RPC documentados pero no ejercitados no cumplen criterios de cierre de escritura.

## 4. Validación de P4.0 ejecutada

Comandos ejecutados sobre el HEAD de la rama, **antes de cualquier cambio de código**:

| Comando | Resultado |
|---|---|
| `cargo fmt --all -- --check` | PASS |
| `cargo check --workspace --all-targets --locked` | PASS |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS |
| `cargo test --workspace --locked` | PASS (15 bloques `test result: ok`) |
| `cargo build --workspace --locked` | PASS |
| `git diff --check` | PASS |

P4.0 no añade ni modifica código de producción: el diff debe contener solo documentación. No se ejecutaron smokes reales de pago en este corte; los sondeos fueron de solo lectura y sin credenciales, y sus resultados están clasificados en `P4_RPC_RESEARCH.md`.

## 5. Riesgo abierto para el plan

- El entorno no tiene modelo usable para Pi 1.6.0; P4.2 dependerá de la configuración del usuario para smokes reales. El plan contempla UNTESTED explícito.
- Codex 0.171.0 expone `availableDecisions` fuera del esquema; los contract tests no deben rechazarlo por estrictez de schema.
- Gentle Shell `session_snapshot` depende de sesión enlazada: P4.3 debe probar con sesión real, no solo `--no-session`.
- Los hallazgos de B7 (fuga de IDs en stop) y B5 (registro duplicado) se recomiendan **antes** de P4.1 para no propagarlos a tres drivers nuevos.


## 5. Ejecución real en P4.1

Todos los casos aplicables a Codex se ejecutaron con `GF_E2E_REAL=1` sobre Codex `0.171.0`: `tests/e2e_p4_codex_{chat,approval,interrupt,controls,discovery,concurrency,survival}.sh` → **PASS**. Las 14 E2E deterministas P1–P3, incluidas OpenCode y las de supervivencia P3, permanecen en verde. Las verificaciones Rust y `git diff --check` pasan. Los comandos y su evidencia están en [`P4_1_RESULTS.md`](P4_1_RESULTS.md).

No se ejecutaron pruebas reales de Pi ni Gentle Shell en este corte (P4.2/P4.3). Windows/macOS siguen pendientes de P7.
