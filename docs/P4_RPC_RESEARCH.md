# P4 — Investigación de protocolos RPC (P4.0)

**Estado:** investigación ejecutada 22/11/2026 sobre Linux x86_64. **Rama:** `feature/p4-multi-provider`.
**Alcance:** documentar protocolos, métodos, eventos, estructuras, particularidades y límites de los proveedores objetivo. **No se implementa ningún driver en este corte.**

Las etiquetas VERIFIED / OBSERVED / UNTESTED / UNSUPPORTED se definen en [`P4_PROVIDER_MATRIX.md`](P4_PROVIDER_MATRIX.md). Todos los sondeos usaron directorios temporales; no se leyeron ni copiaron credenciales ni historiales personales.

---

## 1. Codex 0.171.0 — App Server

### 1.1 Detección y esquema

- `codex --version` → `codex-cli 0.171.0`. Instalaciones disponibles 0.169.0, 0.170.0, 0.171.0 (activa). `~/.codex/version.json` dice `latest_version 0.170.0`: **fuente no fiable**; usar el binario.
- `codex app-server --help` → comando `[experimental]`; subcomandos `daemon | proxy | generate-ts | generate-json-schema`; transports `stdio://` (por defecto), `unix://`, `ws://IP:PORT`, `off`; opciones `--listen`, `--ws-auth capability-token|signed-bearer-token`; error de sobrecarga de cola `-32001`.
- `codex app-server generate-json-schema --out <tmp>`: **714 definiciones v2**, `ClientRequest.json` con **110 métodos**, `ServerRequest.json` 10, `ServerNotification.json` 87. Coincide exactamente con el recuento P0 → `docs/research/CODEX.md` sigue vigente en versión.
- Variante `--experimental`: 883 definiciones, 189 métodos de cliente (+79). Los métodos experimentales **no** están disponibles sin `capabilities.experimentalApi:true`.
- Esquemas por mensaje generados: `CommandExecutionRequestApprovalParams/Response`, `ApplyPatchApproval*`, `ExecCommandApproval*`, `FileChangeRequestApproval*`, `PermissionsRequestApproval*`, `ToolRequestUserInput*`, etc.

### 1.2 Handshake y modelo de mensajes

`initialize` → respuesta con `userAgent`, `platformFamily:"unix"`, `platformOs:"linux"`; luego notificación `initialized`. A partir de ahí el cliente puede enviar requests y **debe seguir leyendo** mientras espera respuestas: el servidor inicia sus propios requests (aprobaciones) con `id` numérico. Un bucle que solo espera la respuesta al propio `id` bloquea las aprobaciones.

Sondeo VERIFIED con `CODEX_HOME` temporal vacío (sin `auth.json`, sin llamadas de modelo): handshake, `thread/start`, `thread/list`, `thread/read`, `model/list`. `thread/start` devuelve `cliVersion:"0.171.0"`, `historyMode:"paginated"`, `status:{type:"idle"}` y una `path` persistida.

### 1.3 Métodos relevantes (nombres exactos del esquema)

| Grupo | Métodos |
|---|---|
| Config | `initialize`, `initialized`, `config/read`, `config/value/write`, `config/batchWrite`, `configRequirements/read`, `model/list`, `modelProvider/list`, `modelProvider/default/set` |
| Threads | `thread/start`, `thread/resume`, `thread/list`, `thread/read`, `thread/turns/list`, `thread/items/list`, `thread/loaded/list`, `thread/archive`, `thread/unarchive`, `thread/delete`, `thread/fork`, `thread/revert`, `thread/name/set`, `thread/metadata/update`, `thread/compact/start`, `thread/unsubscribe`, `thread/inject_items`, `thread/approveGuardianDeniedAction` |
| Turnos | `turn/start`, `turn/steer`, `turn/interrupt` |
| Cuenta | `account/rateLimits/read`, `account/read`, `account/usage/read`, `account/login/start`, `account/logout` |
| Notificaciones | `account/rateLimits/updated`, `account/updated`, `thread/status/changed`, `thread/tokenUsage/updated`, `turn/completed`, `turn/started`, `item/started`, `item/completed`, `item/agentMessage/delta`, `deprecationNotice`, `serverRequest/resolved`, `thread/started`, `thread/archived`, `mcpServer/startupStatus/updated` |
| Solo experimental | `thread/settings/update`, `turn/settings/update`, `thread/items/read`, `thread/search`, `thread/queue/*`, `process/spawn|writeStdin|resizePty|kill`, `thread/backgroundTerminals/*`, `environment/info`, `collaborationMode/list` |

### 1.4 Formas de parámetros verifyadas

- `thread/start{cwd, approvalPolicy, sandbox, model, modelProvider, effort?, personality, serviceTier, config, baseInstructions, developerInstructions, ephemeral, threadSource}`.
- `thread/resume{threadId*, excludeTurns, model, sandbox, approvalPolicy, …}`.
- `thread/list{cursor, limit, archived, cwd, currentAccountOnly, searchTerm, sortKey, sortDirection, sectionId, included/excludedThreadIds, useStateDbOnly}` → `{data, nextCursor, backwardsCursor, readStates}`.
- `thread/turns/list{threadId*, cursor, limit, itemsView(notLoaded|summary|full), sortDirection}`.
- `thread/items/list{threadId*, turnId, cursor(string|{type:"item",itemId}), limit, sortDirection, deferOutput}`.
- `thread/read{threadId*, includeTurns}`; `includeTurns:true` está obsoleto: P0 registró dos variantes de aviso y la recomendación de usar `excludeTurns:true` + paginación.
- `turn/start{threadId*, input*(UserInput[]), model, effort, approvalPolicy, sandboxPolicy, summary, serviceTier, serviceTierForTurn, outputSchema, toolOutput, parentTurnId, rootTurnId, turnTrigger, personality, cwd}`.
- `turn/steer{threadId*, expectedTurnId*, input*, clientUserMessageId}`: requisito de turno activo **en el mismo proceso**.
- `turn/interrupt{threadId*, turnId*}` → `{}`; el turno termina con `status:"interrupted"`.

`ReasoningEffort` es un **string no vacío libre**, no un enum. `ThreadStatus = notLoaded|idle|systemError|active{activeFlags: waitingOnApproval|waitingOnUserInput}`. `TurnStatus = completed|interrupted|failed|inProgress`.

### 1.5 Aprobaciones (P4-F08)

Requests iniciados por el servidor; el cliente responde con `{"jsonrpc":"2.0","id":<numérico>,"result":...}`:

| Request | Decisiones admitidas |
|---|---|
| `item/commandExecution/requestApproval` | `accept`, `acceptForSession`, `decline`, `cancel`, `{acceptWithExecpolicyAmendment:{execpolicy_amendment:string[]}}`, `{applyNetworkPolicyAmendment:{network_policy_amendment:{action,host}}}` |
| `item/fileChange/requestApproval` | `accept`, `acceptForSession`, `decline`, `cancel` |
| `item/tool/requestUserInput` | `{answers:{[key]:{answers:string[]}}}` |
| `item/permissions/requestApproval` | `{permissions, scope?:"turn"\|"session", strictAutoReview?}` (solo subconjunto concedido) |
| `applyPatchApproval` / `execCommandApproval` (legacy) | `approved`, `approved_for_session`, `approved_mcp_policy_amendment`, `timed_out`, `abort`, `{approved_execpolicy_amendment:{proposed_execpolicy_amendment:string[]}}`, `{network_policy_amendment:{...}}`, `{denied:{rejection}}` |

`decline` = deniega y continúa el turno; `cancel` = deniega e interrumpe inmediatamente. El runtime envía `availableDecisions` (P0 fixture: `["accept", {"acceptWithExecpolicyAmendment":{…}}, "cancel"]`) aunque el esquema generado no lo declara (`additionalProperties` no restringido). Toda solicitud resuelta emite `serverRequest/resolved{threadId,requestId}`; sirve como confirmación de entrega. **Ninguna decisión se envía por defecto.**

### 1.6 Controles y particularidades

- Modelo/esfuerzo por turno en `turn/start.model`/`.effort`; por thread en `thread/start|resume|fork.model`.
- `thread/settings/update` y `turn/settings/update` existen en el esquema pero con `experimentalApi:false` responden explícitamente `requires experimentalApi capability` (VERIFIED). No se puede depender de ellos para P4.1 sin opt-in documentado.
- `model/list` → 11 modelos, default `gpt-6.2-sol`; esfuerzos anunciados `low, medium, high, xhigh, max, ultra` (modelo `gpt-5.5` para en `xhigh`). Los IDs de modelo son opacos: obtenerlos de `model/list`, nunca hardcodearlos.

### 1.7 Historial, sesiones y recuperación

- `thread/list` permite enumeración no intrusiva con paginación y filtros. `thread/loaded/list` dice qué threads están en memoria: señal útil (no suficiente) de actividad externa. `thread/status.changed.activeFlags` expone `waitingOnApproval`/`waitingOnUserInput`.
- Un thread recién creado **no está materializado** hasta el primer mensaje del usuario: `thread/turns/list` responde *"not materialized yet; unavailable before first user message"*, `thread/items/list` responde *"not supported yet"* y `thread/resume{excludeTurns:true}` responde *"no rollout found"*. P4.1 debe esperar al primer turno antes de ofrecer importación de historial.
- `thread/resume{excludeTurns:true}` reabre un rollout persistido (`<CODEX_HOME>/sessions/YYYY/MM/DD/rollout-…jsonl`) para **nuevos** turnos. `thread/read` es lectura de metadatos sin suscripción.
- **No existe** reattachments a un turno en vuelo entre procesos: `turn/steer` exige turno activo en el mismo proceso; `turn/interrupt` lo cancela; un cliente desconectado no termina la sesión, pero un proceso nuevo solo recupera historial, no una aprobación pendiente. Esta distinción es obligatoria en P4.1 (P4_SPEC §4.3.6).

### 1.8 Clasificación Codex

- **VERIFIED:** versión, recuentos de esquema, handshake, `thread/start/list/read`, `model/list`, existencia y gate de `turn/steer`, `turn/interrupt`, `thread/settings/update`, `turn/settings/update`, formas de decisión, P0 resume entre procesos.
- **OBSERVED:** `thread/items/list` rechazado en hilo no materializado; `availableDecisions` en runtime; mensajes de deprecación.
- **UNTESTED:** `turn/start` con prompt real en esta sesión, `turn/completed`, ejecución de `steer`/`interrupt`, decisiones distintas de `accept`, `requestUserInput`, `permissions`, transportes WS/unix, SIGKILL.
- **UNSUPPORTED:** recuperación de turno en vuelo entre procesos; API HTTP pública.

---

## 2. Pi estándar 1.6.0 — Pi RPC

### 2.1 Activación y forma

`pi --mode rpc --no-session` con cwd y `--session-dir` temporales. JSONL estricto en stdin/stdout; diagnósticos en stderr. correlación por `id` opcional (string), repetido en `{"type":"response","command":...,"success":...,"data":...,"id":...}`.

**Framing:** dividir solo por LF. `readline` de Node divide erróneamente en U+2028/U+2029, que aparecen en contenido de modelos. un driver Rust debe usar `BufRead::read_line`/split por `\n`.

Apagado: cerrar stdin o SIGTERM/SIGHUP = ordenado; segundo signal = salida inmediata. La conexión es el proceso: no hay servidor ni socket.

Documentación empaquetada (autoritativa, instalada):
`~/.local/share/mise/installs/pi/1.6.0/pi/docs/{rpc.md,rpc-commands.md,json.md,rpc-extension-ui.md,session-format.md}`.

### 2.2 Comandos documentados

| Grupo | Comandos |
|---|---|
| Prompt/cola | `prompt` (`streamingBehavior:"steer"\|"followUp"`; `disposition:"handled"\|"queued"\|"started"`), `steer`, `follow_up`, `abort`, `clear_queue`, `new_session` |
| Estado/historial | `get_state`, `get_messages`, `get_entries` (cursor opaco `since`), `get_tree`, `get_fork_messages`, `get_last_assistant_text`, `get_session_stats`, `switch_session`, `fork`, `clone`, `set_session_name`, `export_html` |
| Modelo/esfuerzo | `set_model`, `cycle_model`, `get_available_models`, `set_thinking_level`, `cycle_thinking_level`, `get_available_thinking_levels` |
| Modos de cola | `set_steering_mode`, `set_follow_up_mode` |
| Compactación/reintento | `compact`, `set_auto_compaction`, `set_auto_retry`, `abort_retry` |
| Bash | `bash`, `abort_bash` |
| Descubrimiento | `get_commands` |

### 2.3 Eventos documentados

- Ciclo: `agent_start`, `agent_end`, `turn_start`, `turn_end`, **`agent_settled{aborted}`**.
- Mensajes: `message_start`, `message_update` con `assistantMessageEvent.type ∈ {start,text_start,text_delta,text_end,thinking_start,thinking_delta,thinking_end,toolcall_start,toolcall_delta,toolcall_end,done,error}`, `message_end` (final canónico).
- Herramientas: `tool_execution_start`, `tool_execution_update`, `tool_execution_end`.
- Estado/cola: `queue_update`, `entry_appended`, `session_info_changed`, `thinking_level_changed`.
- Compactación/reintento: `compaction_start/end`, `auto_retry_start/end`, `summarization_retry_scheduled/attempt_start/finished`.
- Solo RPC: `bash_execution_update`, `extension_error`.

`agent_end` **no** significa fin de trabajo: hay que esperar `agent_settled`. `disposition:"handled"` significa que no se inició una ejecución.

### 2.4 Sondeo ejecutado (VERIFIED, sin llamada de modelo)

- `get_state` → `model` (objeto), `thinkingLevel:"off"`, `isStreaming:false`, `isCompacting:false`, `steeringMode`/`followUpMode:"one-at-a-time"`, `sessionId` UUID, `autoCompactionEnabled:true`, `messageCount:0`, `pendingMessageCount:0`. Con `--no-session` **no hay `sessionFile`**, pero `sessionId` existe: no usarlo como prueba de persistencia.
- `get_available_models` → `{"models":[]}`; stderr `Warning: No models available`. Modelo placeholder `unknown`. Sin login no se puede producir evidencia de respuesta real de modelo en 1.6.0.
- `get_available_thinking_levels` → `{"levels":["off"]}`.
- `new_session` → `{"success":true,"data":{"cancelled":false}}`.
- `get_messages` → `{"messages":[]}`; `get_entries` → entrada raíz `thinking_level_change` + `leafId`; `get_session_stats` → totales a cero y sin `contextUsage`.
- `get_commands` → comandos de extensión `llama`, `mcp` y `skill:*` del directorio de usuario; **no hay `/gentle:*`** en Pi estándar.
- `switch_session` con fichero artesanal bajo `--no-session`: `success:true`, pero `get_messages` sigue vacío y `sessionId` no cambia → **inconcluso**, no reclamar carga. La evidencia P0 con fichero real sigue siendo la autoritativa.
- Cerrar stdin → salida 0.

### 2.5 Extension UI en Pi base (corrección a documentación P0)

Pi estándar **sí** tiene un subprotocolo genérico de Extension UI documentado en `rpc-extension-ui.md`: `extension_ui_request`/`extension_ui_response` correlacionados por `id`, con diálogos `select` (`value`/`cancelled`), `confirm` (`confirmed`), `input`, `editor` (`value`) y fire-and-forget `notify`, `setStatus`, `setWidget`, `setTitle`, `set_editor_text`. El `timeout` opcional resuelve automáticamente del lado del agente; Fleet debe conservar el timeout nativo pero **nunca autoaprobar**. Lo que no existe en Pi base son los *widgets y comandos Gentle* (`gentle-desktop-rpc`, `/gentle:*`). `examples/extensions/subagent/` es un ejemplo, no capacidad integrada.

### 2.6 Clasificación Pi

- **VERIFIED (1.6.0):** versión, activación, forma de `response`, `get_state`, `get_available_models` (vacío), `get_available_thinking_levels`, `new_session`, `get_messages`, `get_entries`, `get_session_stats`, `get_commands`, cierre limpio.
- **OBSERVED (P0, 1.5.2 vía Gentle Shell):** orden de eventos real con `agent_settled`, `tool_execution_*` de `bash`, diálogo `select` de permiso.
- **UNTESTED:** `abort`, `clear_queue`, `steer`, `follow_up`, efectos de `set_model`/`set_thinking_level`, `compact`, `bash`, `fork`/`clone`, `--session/--continue/--resume`, SIGKILL y recuperación.
- **UNSUPPORTED:** Desktop RPC, modos de permiso, perfiles, subagentes integrados, servidor persistente.

---

## 3. Gentle Shell 4.8.0-main.652552becbb6 — Pi RPC + Desktop RPC

### 3.1 Productor y activación

Paquete global `gentle-pi 4.8.0-main.652552becbb6`, raíz en el store pnpm. `gentle-shell --version` imprime `gentle-shell 4.8.0`, `pi 1.5.2`, `home isolated ~/.gentle-shell/agent`. Activación del host interactivo: `GENTLE_SHELL_INTERACTIVE_HOST=1 gentle-shell --mode rpc`.

Documentación del productor (no GPUI; P4-F17): `docs/desktop-rpc.md` (1859 líneas), `docs/permission-modes.md` (145), `docs/session-profile-format.md` (80), `docs/gentle-shell.md` (566), `docs/gentle-agents-activity.md` (690), `docs/desktop-feed.md`, `docs/gentle-shell-desktop.md`, `docs/{auto-permission-answers,auto-permission-evidence,yolo-mode,readme-reference}.md`.

### 3.2 Transporte de Desktop RPC (dos capas, no confundir)

1. **Pi RPC**: `prompt` con mensaje `/gentle:desktop <un JSON request>`; la respuesta externa es `disposition:"handled"`.
2. **Extension UI**: la respuesta de dominio llega como `extension_ui_request`, `method:"setWidget"`, `widgetKey:"gentle-desktop-rpc"`, con un JSON string en `widgetLines`, correlacionado por `requestId` (no por el `id` del prompt).

Envelope: `{schema,kind:"response",requestId,ok,data}` o `{...,ok:false,error:{code,message}}`. Eventos: `{schema,kind:"event",seq,event,data}` con `seq` desde 1 y reset en apagado/recarga/switch; `get_snapshot` tras conectar o al detectar hueco. Límites: request ≤ 64 KiB, `requestId` ≤ 128 caracteres, `method` ≤ 64.

Códigos de error documentados: `invalid_request`, `unknown_store`, `read_only`, `invalid_data`, `revision_conflict`, `write_failed`, `unsupported_method`, `permission_unavailable`, `permission_denied`, `config_unavailable`, `usage_unavailable`, `design_unavailable`, `jobs_unavailable`, `agents_unavailable`, `profiles_unavailable`, `unknown_profile`, `profile_apply_busy`, `sessions_unavailable`, `session_switching`, `review_unavailable`, `review_mode_busy`, `stats_unavailable`, `not_found`, `internal_error`.

### 3.3 `capabilities` ejecutado (VERIFIED)

Sondeo con `--no-session`, cwd temporal, 5 prompts, todos `handled`. Sin secretos ni datos personales en la salida.

**23 métodos anunciados:** `capabilities`, `get_snapshot`, `get_config`, `set_config`, `get_permission_mode`, `set_permission_mode`, `get_subscription_usage`, `refresh_subscription_usage`, `get_design_canvas`, `list_jobs`, `cancel_job`, `get_job_output`, `apply_profile`, `list_subagents`, `cancel_subagent`, `get_subagent_thread`, `list_sessions`, `switch_session`, `get_changes`, `get_change_diff`, `get_review_mode`, `set_review_mode`, `get_usage_stats`.

**8 eventos anunciados en la sesión efímera:** `config_changed`, `permission_mode_changed`, `rate_limit`, `design_canvas`, `jobs_update`, `profile_applied`, `agents_update`, `review_mode_changed`. **Noveno evento condicional: `session_snapshot`**, documentado por el productor pero **no anunciado** en esta sesión efímera; se habilita al conectar el feed de sesión. Fleet debe negociar métodos/eventos por sesión, no asumir nueve siempre. La documentación upstream puede evolucionar independientemente del binario instalado; el recuento de 23 métodos se refiere solo a la versión local probada.

Llamadas de solo lectura ejecutadas:
- `get_permission_mode` → `{mode:"super-yolo", default_mode:"super-yolo"}`.
- `list_sessions` → `ok:false, error.code:"sessions_unavailable"` (sin contexto de sesión enlazada).
- `get_usage_stats` → `{range:"7d", scope:"project", totals:{…}, byModel:[], byDay:[], truncated:{…}}` a cero.
- `get_config {store:"models"}` → store de solo lectura, `status:"ok"`, digest de revisión, `value` = mapa agent→modelo (sin credenciales).

**No se ejecutó ningún método modificador** (`set_config`, `apply_profile`, `set_permission_mode`, `switch_session`, `set_review_mode`, `cancel_*`): no se alteró el perfil real del usuario.

### 3.4 Permisos, perfiles y modelos

- Modos (`permission-modes.md`): `normal`, `auto`, `yolo`, `super-yolo`. `/gentle:mode [normal|auto|yolo|super-yolo|status|default …]` y `/gentle:yolo`. El modo vivo es por sesión (SessionManager + session id + workspace Git canónico); el default global vive en `permission-mode.json` privado y versionado. Equivalentes RPC: `get_permission_mode`/`set_permission_mode` (`persist` escribe el default). Normal ofrece opciones con alcance, incluido `Always allow <family>`; Auto usa un revisor con el modelo actual (coste extra, evidencia acotada); YOLO/Super YOLO mantienen denegaciones duras y bloqueos de rutas sensibles. **Fleet nunca autoaprueba** (P4-F07).
- Perfiles: `session-profile-format.md` v1 `gentle-pi.session-profile/v1`, entradas custom con payload `bind` (orígenes `user|local|repo|global`, `startup` opcional) / `clear`; resultados de replay `absent|bound|cleared|invalid|unsupported`, con corroboración en disco. `get_config` expone stores `profiles`, `models`, `profile_fallbacks`, `subagents`, `pins`, `managed_assets` (destinos de escritura + revisiones); `apply_profile` es el método modificador, documentado pero no ejecutado.
- Modelos/esfuerzo: `set_model`, `get_available_models`, `set_thinking_level` (niveles `off|minimal|low|medium|high|xhigh|max`); `get_config{store:"models"}` da el mapa de enrutado. Efecto de cambio en caliente: UNTESTED.

### 3.5 Permisos en Pi RPC (capa 1)

P0 (`spikes/p0/fixtures/gentle-shell-pi.jsonl:69`) capturó `extension_ui_request method:"select"` con título `Allow recognized data-loss command?\nrm -r victimdir` y opciones `Allow once` / `Allow for this session` / `Deny`; la respuesta es `{"type":"extension_ui_response","id":…,"value":…}`. También aparecen `notify` y `setStatus` en el mismo canal. Esto confirma P4-F08: decisiones nativas tipadas con listas de opciones, no reducibles a once/always/reject.

### 3.6 Subagentes — documentar, NO implementar (P4-F05)

- Desktop RPC (anunciado, solo observación/cancelación): `list_subagents`, `cancel_subagent`, `get_subagent_thread`; evento `agents_update`; alcance por propiedad, `not_found` para IDs ajenos o expulsados, `agents_unavailable` sin owner.
- Tools invocables por el modelo (**no** Desktop RPC): `subagent_list_agents`, `subagent_run`, `subagent_status`, `subagent_result`, `subagent_list_tasks`, `subagent_reply`, `subagent_cancel`, `subagent_send_message`, `subagent_continue`, `subagent_parent_message` (`docs/gentle-shell.md:358-376`); además `orchestrator_session_id`, `orchestrator_list` y el fallback nativo `Agent`.

Fleet P4 debe dejar puntos de extensión tipados si hace falta, pero no exponer ni conducir estas funciones.

### 3.7 Clasificación Gentle Shell

- **VERIFIED:** versión, transporte Desktop RPC, 23 métodos y **8 eventos efectivamente negociados** en sesión efímera; `session_snapshot` documentado como noveno condicional y no anunciado en la sonda. Cuatro llamadas de solo lectura, límites y códigos de error del productor.
- **OBSERVED (P0):** evento `session_snapshot`, diálogo `select` de permiso, `tool_execution_*`, cierre y resume con `switch_session` en sesión real.
- **UNTESTED:** `set_config`, `set_permission_mode`, `apply_profile`, `switch_session`, `get_changes`, `get_change_diff`, `get_review_mode`/`set_review_mode`, cancelaciones, reconexión con `seq` perdido, recuperación tras SIGKILL.
- **UNSUPPORTED en la sesión efímera probada:** `list_sessions` (`sessions_unavailable`) y `session_snapshot` no negociado.

---

## 4. Evidencia y reproducibilidad

| Fuente | Ubicación |
|---|---|
| Esquemas Codex 0.171.0 (stable/experimental) | `/tmp/p4-codex-schema`, `/tmp/p4-codex-schema-exp` (regenerables con `codex app-server generate-json-schema --out …`) |
| Sondeo seguro Codex | `/tmp/p4_codex_safe_probe.py` (CODEX_HOME temporal, sin auth real) |
| Sondeo Pi 1.6.0 | `/tmp/p4_pi_probe.py` (cwd y `--session-dir` temporales) |
| Sondeo capacidades Gentle Shell | `/tmp/gs_cap_probe.py` (solo lectura) |
| P0 real | `docs/research/{CODEX,PI,OPENCODE,PROVIDER_COMPARISON,KNOWN_LIMITATIONS}.md`, `spikes/p0/fixtures/*.jsonl` |

Los ficheros `/tmp/*` son **sondeos transitorios no reproducibles desde otro checkout**: no son fixtures versionados y pueden desaparecer. En P4.1–P4.3 deben reconstruirse como sondas ejecutables y fixtures mínimos, anonimizados y versionados, antes de fundamentar criterios de cierre en esos resultados (§7 del plan de pruebas).


## 5. Nota de implementación P4.1

El driver real confirma los hallazgos de esta investigación y añade tres precisiones observadas con la versión instalada: `model/list` expone `id`, `isDefault`, `supportedReasoningEfforts[].reasoningEffort` y `defaultReasoningEffort`; `thread/turns/list` con `itemsView:"full"` devuelve los ítems dentro de cada turno (los objetos de `thread/items/list` están envueltos en `item`); y `serverRequest/resolved` usa `params.requestId` numérico. `availableDecisions` es un campo de runtime no declarado en el esquema generado. Las sondas `/tmp` quedan sustituidas por los scripts versionados `tests/e2e_p4_codex_*.sh` con `GF_E2E_REAL=1`. Detalle en [`P4_1_RESULTS.md`](P4_1_RESULTS.md).


## 6. Nota de implementación P4.2

El driver real confirma el inventario de P4.0 para Pi 1.6.0: activación `pi --mode rpc --session-dir`, respuestas `{"type":"response","command":...,"success":...}`, eventos `message_update` delta-only con `assistantMessageEvent`, `agent_settled` como terminal y archivos de sesión JSONL v3. Tras configurar un modelo, `get_available_models` devuelve 10 entradas con `id`/`provider` y `get_state.model.id=gpt-6.1-terra`. Las sondas versionadas son `tests/e2e_p4_pi_*.sh`. No se usó Desktop RPC ni GPUI.
