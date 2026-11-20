# Comparación de proveedores y contratos comunes (P0)

Documento de investigación. **No fija el contrato Fleet ni ratifica decisiones.**
Plataforma de todas las pruebas: **Linux x86_64**. Windows no fue probado.

## Convenciones de evidencia

| Etiqueta | Significado |
|---|---|
| **VERIFIED** | Afirmación comprobada por una aserción automática de un probe que terminó en `PASS`, o por un comando ejecutado cuya salida está conservada. Incluye versión y plataforma. |
| **OBSERVED** | Vista en una ejecución real o en una captura, sin una aserción automática dedicada. No se generaliza. |
| **UNTESTED** | No hay prueba P0. Puede existir en la documentación del proveedor; no se afirma. |
| **UNSUPPORTED** | La superficie nativa de la versión probada lo rechazó o no lo expone. No significa «Fleet no lo implementa todavía». |

Índice de evidencia: [`spikes/p0/README.md`](../../spikes/p0/README.md) y
[`spikes/p0/fixtures/README.md`](../../spikes/p0/fixtures/README.md). Los detalles
y ejemplos por motor están en [PI.md](PI.md), [CODEX.md](CODEX.md) y
[OPENCODE.md](OPENCODE.md).

## Versiones fijadas

| Motor | Salida exacta | Fuente |
|---|---|---|
| Gentle Shell | `gentle-shell 4.8.0` + `pi 1.5.2`; paquete `gentle-pi 4.8.0-main.652552becbb6` | `spikes/p0/fixtures/command-outputs.txt`, `package.json` del paquete global |
| Codex | `codex-cli 0.171.0`; `cliVersion` en thread `0.171.0` | `command-outputs.txt`, `codex.jsonl` `response.thread/start` |
| OpenCode | `opencode v2.3.2`; `/api/info` → `"2.3.2"` | `command-outputs.txt`, `opencode.jsonl` `event.session.created` |

## Matriz de capacidades verificadas

Leyenda: **V** = VERIFIED, **O** = OBSERVED, **U** = UNTESTED, **–** = UNSUPPORTED en la versión probada.

| Capacidad | Gentle Shell / Pi 1.5.2 | Codex 0.171.0 | OpenCode 2.3.2 |
|---|---|---|---|
| Transporte estructurado | V: JSONL stdio | V: JSON-RPC 2.0 stdio | V: HTTP + SSE loopback |
| Descubrimiento / versión | V: `get_state` | V: `initialize` | V: `GET /api/info` |
| Prompt con cwd real | V: cwd del proceso | V: `thread/start.cwd` | V: `location.directory` |
| Aceptación de prompt ≠ fin | V: `disposition:started` | V: `turn.status:inProgress` | V: `POST /prompt` devuelve mensaje |
| Texto en streaming | V: `message_update`/`text_delta` | V: `item/agentMessage/delta` | V: `session.text.delta` |
| Texto completo al cerrar | V: `message_end` | V: `item/completed` | V: `session.text.ended` |
| Eventos de herramientas | V: `tool_execution_*` | V: `item/started`/`item/completed` | V: `session.tool.*` + `shell.*` |
| IDs nativos opacos | V: `sessionId`, `toolCallId` | V: `threadId`, `turnId`, `itemId` | V: `sessionID`, `messageID`, `permissionID` |
| Fin de turno explícito | V: `agent_settled` | V: `turn/completed` | V: `session.execution.succeeded/failed/interrupted` |
| Permisos interactivos | V: `extension_ui_request` `select` | V: request JSON-RPC `requestApproval` | V: `permission.asked` + `/reply` |
| Decisión deny/reject | V: `value:"Deny"` bloqueó | U: solo `accept` probado | V: `reject` → `tool.failed` |
| Decisión allow once | U: no se probó `Allow once` | V: `decision:"accept"` | V: `decision:"once"` |
| Cancelación en caliente | U: `abort` existe en ayuda | U: `turn/interrupt` en esquema | U: `/interrupt` en OpenAPI |
| Adjuntar a servidor de usuario | –: no aplica, proceso propio | –: no aplica | V: `service.json` + Basic auth |
| Persistencia nativa | V: `.jsonl` por sesión | V: `rollout-*.jsonl` | V: SQLite, sobrevive reinicio |
| Reanudar historia en otro proceso | V: `switch_session` | V: `thread/resume` | V: `GET /api/session/{id}` |
| Listar sesiones nativo | U: no en RPC; O: canal de escritorio | O: `thread/list` (manual); V: `thread/turns/list` | O: `GET /api/session` (global, manual) |
| Paginación de historial | U | V: `thread/turns/list`; O: `thread/items/list` en esquema | O: `message?limit&cursor` existe en OpenAPI, no ejecutado con cursor |
| Cursor/replay nativo | U | U | O: `durable.seq/version` en algunos eventos |
| Sesión sobrevive a cierre de cliente | –: el proceso muere con stdin [PI-E4] | O: el thread persiste al cerrar el app-server; U con turno en vuelo | V: el servidor sigue; O: aprobación pendiente [OC-E3] |
| Muerte de daemon / SIGKILL | U (no aplica aún) | U | U |
| Windows | U | U | U |
| Smoke sin credenciales | –: usa login codex | –: usa login ChatGPT | V: modelo `tidepool-alpha-free` |

Detalles de las celdas negativas o dudosas:

- `--standalone` en `opencode serve` 2.3.2: **UNSUPPORTED**; la ayuda de
  `--print-logs` lo menciona pero el flag no existe (`command-outputs.txt`).
- `thread/read includeTurns:true`: funciona pero emite `deprecationNotice`;
  la forma preferida por el propio servidor es `excludeTurns:true` más paginación
  (`codex.jsonl` líneas 50–51 y `codex-manual-success.jsonl` línea 46).
- `session.execution.interrupted` con `reason:"shutdown"` tras un `reject` de
  OpenCode: **OBSERVED** (`opencode.jsonl` línea 94). La interpretación de `reason`
  es **UNTESTED**.
- Aprobación huérfana de OpenCode: **OBSERVED** en una ejecución manual
  (`opencode-orphan-permission.jsonl`); no hay aserción automática de reconciliación.

## Contratos comunes que necesitará `AgentDriver`

Derivados de [`docs/DRIVERS.md`](../DRIVERS.md) y de la evidencia P0. Son
**necesidades del contrato**, no firmas Rust definitivas ni implementación.

| Contrato común | Qué debe garantizar | Mapa nativo por proveedor |
|---|---|---|
| Identidad y diagnóstico | `provider_id`, versión exacta, capacidades efectivas por versión | `get_state` / `initialize` / `GET /api/info` |
| Abrir sesión en cwd real | `open(LaunchContext, resume?)`, devolver `NativeSessionId` opaco | spawn + cwd / `thread/start.cwd` / `location.directory` |
| Enviar entrada | `send(turn_id, input)` → `SendReceipt` con estado **aceptado**, nunca «terminado» | `prompt` → `disposition` / `turn/start` → `inProgress` / `POST /prompt` |
| Leer eventos normalizados | `next_event()` → eventos tipados Fleet; el driver no filtra JSON privado al cliente | `message_update`/`tool_execution_*` / `item/*` / `session.*` |
| Confirmar fin de turno | Un único evento de dominio por turno, con `status` | `agent_settled` / `turn/completed` / `session.execution.*` |
| Responder aprobación | `answer(NativeRequestId, Decision)` → `Ack`; una sola decisión ganadora | `extension_ui_response` / `result.decision` / `POST .../reply` |
| Interrumpir | `interrupt(turn)`; debe poder devolver `Unsupported` | `abort` (U) / `turn/interrupt` (U) / `/interrupt` (U) |
| Cerrar / detener | `close()` observable; distinto de «cliente se desconectó» | stdin (mata Pi) / cerrar stdio / borrar sesión vía API |
| Inspeccionar recuperación | `inspect_recovery()`: qué se pudo recuperar y qué quedó incierto | `get_messages` / `thread/read`+paginación / `GET /api/session` |
| Capacidades declaradas | `supports_resume`, `streaming_text`, `tool_events`, `approvals`, `cancel_turn`, `session_history`, `native_turn_ids`, `native_request_ids`, `cwd_support`, etc., conmutables por versión | tabla anterior |
| Propiedad del proceso | El Runner posee el proceso (Pi) o se adjunta al servidor (OpenCode); el driver no decide quién es dueño | ver P0-D3 |
| Higiene de secretos | Ningún evento público incluye password, token ni ruta de credenciales | ver cada `*.md` |

**Capacidades específicas que NO pertenecen al contrato común:** el canal
`/gentle:desktop` y los modos `SUPER-YOLO` de Gentle Pi; los `activeFlags`,
`execpolicy_amendment` y `serviceTier` de Codex; el ruleset de permisos,
`durable.seq`, los shells persistentes y el `projectID` de OpenCode. Se traducen
dentro del driver o se reportan como `Unsupported`.

## Propuestas P0-D1…D6 — RATIFICADAS el 20/11/2026

Ratificadas con condiciones en
[`docs/DECISIONS.md`](../DECISIONS.md#ratificación-p0-d1d620112026). No cambian
D01–D12; las concretan. El texto inferior conserva el análisis original.

### P0-D1 — Primer driver real de P1: OpenCode

- **Propuesta:** implementar primero el driver OpenCode; el Runner se adjunta al
  servicio de usuario si existe y, si no, levanta `opencode serve` en loopback.
- **Justificación:** es el único con API tipada publicada, servicio compartido,
  persistencia verificada tras reinicio y smoke sin credenciales. Requiere menos
  traducción que Pi y menos vulnerabilidad de versión que Codex.
- **Alternativa rechazada por ahora:** Gentle Shell (candidato inicial de
  `DRIVERS.md`). Su proceso muere al cerrar stdin, por lo que P1 tendría que
  resolver propiedad de runner antes de poder demostrar el chat.
- **Consecuencias:** P1 no prueba posesión de proceso hijo; esa prueba se traslada
  a P3/P4 con Pi. Se necesita leer el password local del servicio sin loguearlo.
- **Estado:** RATIFICADA el 20/11/2026; condiciones en `docs/DECISIONS.md`.

### P0-D2 — `AgentDriver` con capacidades y native IDs, sin paridad falsa

- **Propuesta:** mantener D04; la tabla de capacidades de este documento es la
  entrada del registro por versión.
- **Justificación:** el fin de turno, el vocabulario de permisos y el resume
  difieren en los tres motores.
- **Alternativa:** una interfaz única con `approve: bool` y un solo evento de
  finalización; perdería semántica (por ejemplo `Allow for this session` o
  `always`).
- **Consecuencias:** más trabajo de traducción y pruebas matrix-driven por
  capacidad; el cliente ve `Unsupported` donde corresponde.
- **Estado:** RATIFICADA el 20/11/2026; condiciones en `docs/DECISIONS.md`.

### P0-D3 — El Runner posee o se adjunta; no asume un hijo único

- **Propuesta:** matizar D05. Con Pi el Runner debe poseer el proceso; con
  OpenCode puede adjuntarse al servidor de usuario.
- **Justificación:** cerrar stdin de Pi termina el proceso con exit 0 y aborta el
  trabajo en curso; el servicio OpenCode sobrevive al cliente.
- **Alternativa:** crear siempre un servidor propio por sesión; duplicaría
  instancias y rompería la detección de servicio compartido.
- **Consecuencias:** el patrón de identidad del Runner debe admitir `owned` y
  `attached`; la reconciliación de P3 no puede basarse solo en PID.
- **Estado:** RATIFICADA el 20/11/2026; condiciones en `docs/DECISIONS.md`.

### P0-D4 — Reconciliación obligatoria antes de reenviar

- **Propuesta:** reforzar D09. Antes de cualquier reenvío tras timeout, consultar
  aprobaciones pendientes y cursor de eventos; el estado incierto sigue siendo
  `OutcomeUnknown`/`NeedsReconciliation`.
- **Justificación:** Pi responde `disposition:started` sin promesa de finalización;
  en OpenCode una aprobación de una sesión abandonada apareció después en otra
  conexión.
- **Alternativa:** reenviar tras timeout con idempotencia por texto; no cubre
  efectos externos ya ejecutados.
- **Consecuencias:** P2 debe implementar consulta de aprobaciones y snapshot con
  cursor antes de considerar completo el reenvío.
- **Estado:** RATIFICADA el 20/11/2026; condiciones en `docs/DECISIONS.md`.

### P0-D5 — Versión fijada por driver y fixtures por versión

- **Propuesta:** registrar la versión exacta por motor y repetir el spike y las
  fixtures al cambiarla.
- **Justificación:** el esquema de Codex tiene 714 definiciones v2 en 0.171.0 y
  el OpenAPI de OpenCode 2.3.2 tiene 458 KB; son superficies versionadas.
- **Alternativa:** detectar capacidades en tiempo de ejecución sin fixtures;
  no detectaría cambios semánticos de eventos.
- **Consecuencias:** CI de contratos con matriz de versión; un cambio de versión
  bloquea el driver hasta regenerar evidencia.
- **Estado:** RATIFICADA el 20/11/2026; condiciones en `docs/DECISIONS.md`.

### P0-D6 — Cursor Fleet propio

- **Propuesta:** `event_id`/`seq` Fleet por sesión; no exponer IDs nativos como
  secuencia pública.
- **Justificación:** solo OpenCode ofrece algo parecido (`durable.seq/version`) y
  solo en algunos eventos; Pi y Codex no ofrecen cursor de replay.
- **Alternativa:** usar `created` (timestamp) como orden; el propio contrato
  `API.md` lo prohíbe explícitamente.
- **Consecuencias:** P2 debe persistir secuencia y detectar huecos; el conector
  de cada driver normaliza eventos hacia esa secuencia.
- **Estado:** RATIFICADA el 20/11/2026; condiciones en `docs/DECISIONS.md`.

## Cierre de P0

Ver [`P0_FINDINGS.md`](../P0_FINDINGS.md) para los criterios y
[`KNOWN_LIMITATIONS.md`](KNOWN_LIMITATIONS.md) para fallos, carencias y huecos.
