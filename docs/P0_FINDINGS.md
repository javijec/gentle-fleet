# P0 — hallazgos verificados de los tres motores (20/11/2026)

Estado: **P0 ejecutado con motores reales**. Todo lo que sigue fue observado con
los binarios instalados y los programas de `spikes/p0/`; las capturas
anonimizadas están en `spikes/p0/fixtures/`. Lo que no se pudo comprobar queda
marcado como **desconocido**, no se infiere de la documentación.

## 1. Versiones y método

| Motor | Versión comprobada | Superficie usada |
|---|---|---|
| Gentle Shell | `4.8.0-main.652552becbb6` (Pi `1.5.2`) | `gentle-shell --mode rpc`, JSONL por stdio |
| Codex | `codex-cli 0.171.0` | `codex app-server`, JSON-RPC 2.0 por stdio |
| OpenCode | `2.3.2` | `opencode serve`, HTTP + SSE en loopback |

Se generaron los esquemas JSON del app-server instalado
(`codex app-server generate-json-schema --out ...`): 292 tipos v2. La forma del
protocolo de Codex depende de la versión; no se debe congelar un contrato Fleet a
partir de esa forma sin fixtures por versión.

Los tres probes terminan en `PASS` y son opt-in: no se ejecutan en CI. Usan
credenciales reales de la máquina pero no las leen, imprimen ni guardan. El
modelo de OpenCode por defecto (`tidepool-alpha-free`) permitió probar chat sin
credencial; Gentle Shell y Codex sí usaron sus sesiones autenticadas.

## 2. Gentle Shell / Pi

### Activación y transporte

- `GENTLE_SHELL_INTERACTIVE_HOST=1 gentle-shell --mode rpc` habilita el canal de
  escritorio de Gentle Pi. Sin esa variable el extension host no se activa.
- Mensajes JSONL por stdin/stdout. stderr se captura por separado; en las
  ejecuciones correctas no aportó eventos de dominio.
- El prompt de control es un mensaje más: `/gentle:desktop {JSON}` y los
  comandos `/gentle:*` (por ejemplo `/gentle:mode normal`). La respuesta del
  canal de escritorio vuelve como `extension_ui_request` con
  `method: "setWidget"`, `widgetKey: "gentle-desktop-rpc"` y un JSON en
  `widgetLines`, correlacionado por `requestId` propio, distinto del `id` RPC.
- El canal de escritorio anuncia `list_sessions`, `switch_session`,
  `get_permission_mode`, `set_permission_mode`, `get_snapshot` y otros. Se probó
  `capabilities`; el resto está documentado pero **no verificado** en este corte.

### Prompt, streaming y fin de turno

- `{"id","type":"prompt","message":...}` responde
  `{"type":"response","command":"prompt","success":true,"data":{"disposition":"started"}}`.
  `disposition` es `started | handled | queued` y **no** significa que la
  respuesta terminó.
- Streaming: `message_update` con `assistantMessageEvent.type = "text_delta"`.
- Herramientas: `tool_execution_start` / `tool_execution_update` /
  `tool_execution_end`, con `toolName`, `args` y `result`.
- Ciclo de vida: `agent_start`, `turn_start`, `message_start/end`, `turn_end`,
  `agent_end` y `agent_settled`. **La señal fiable de fin de trabajo es
  `agent_settled`**; un `turn_end` puede ir seguido de otro `turn_start` porque
  el agente continuó por su cuenta.
- `steer`, `follow_up`, `abort` y `clear_queue` existen como comandos RPC; el
  único no probado en este corte es `abort`.

### Permisos

- El modo inicial observado era `SUPER-YOLO` (configuración guardada del
  usuario), no el modo normal. El probe fuerza `/gentle:mode normal` para ser
  determinista. Un driver no debe depender del modo ambiental del usuario.
- La política interactiva **no es nativa de Pi**: la implementa la extensión
  Gentle Pi. `rm victim.txt` (sin `-r`) se ejecutó sin preguntar; `rm -r
  victimdir` produjo:

  ```json
  {"type":"extension_ui_request","method":"select",
   "title":"Allow recognized data-loss command?\nrm -r victimdir",
   "options":["Allow once","Allow for this session","Deny"]}
  ```

- La respuesta correcta es `{"type":"extension_ui_response","id":<id del request>,
  "value":"Deny"}` (o `confirmed`/`cancelled` según el método). Al denegar, la
  herramienta devuelve un texto de política y el directorio queda intacto.
- Otros métodos posibles: `confirm`, `input`, `editor`, además de `notify`,
  `setStatus` y `setWidget` (presentación, sin respuesta).

### Persistencia, resume y desconexión

- Sin `--no-session`, Pi escribe `<session-dir>/<timestamp>_<id>.jsonl`. Con
  `--session-dir` aislado, el probe no tocó el historial del usuario.
- `switch_session` con esa ruta carga la sesión en otro proceso y
  `get_messages` devuelve el historial (incluido el texto asistido esperado).
  No hay comando RPC `list_sessions` en la superficie probada; el driver debe
  guardar la ruta de sesión si quiere enumerarla.
- **Cerrar stdin mata al proceso Pi inmediatamente** (exit 0) y aborta el trabajo
  en curso: un `sleep 20 && echo ...` lanzado antes del cierre no llegó a crear
  el archivo. El archivo de sesión queda con historial parcial. Esto confirma
  que el requisito «la desconexión del cliente no detiene la sesión» solo se
  puede cumplir si un runnerdueño del proceso piensa por separado del cliente.

## 3. Codex app-server

### Handshake y turno

- `initialize` con `clientInfo` y `capabilities` devuelve `userAgent`,
  `codexHome`, `platformFamily` y `platformOs`; después se envía la notificación
  `initialized`.
- `thread/start` acepta `cwd`, `approvalPolicy` (`on-request`), `sandbox`
  (`read-only` / `workspace-write` / `danger-full-access`), `model`, etc.
- `turn/start` con `{"threadId","input":[{"type":"text","text":...}]}` responde
  con un `turn` en `inProgress`. El fin de turno es la notificación
  `turn/completed` con `turn.status` (`completed | failed | interrupted`).
- Notificaciones observadas en la ejecución correcta: `thread/started`,
  `thread/status/changed` (con `activeFlags` como `waitingOnApproval`),
  `turn/started`, `item/started`, `item/agentMessage/delta`, `item/completed`,
  `thread/tokenUsage/updated`, `account/rateLimits/updated`,
  `mcpServer/startupStatus/updated`, `deprecationNotice` y
  `serverRequest/resolved`. En la ejecución fallida por autenticación se vieron
  además `error` con reintentos (`willRetry`). `item/commandExecution/outputDelta`
  aparece en el esquema pero no en esta captura (la orden terminó sin salida).

### Aprobaciones

- El servidor envía un **request JSON-RPC con `id`**, por ejemplo
  `item/commandExecution/requestApproval`, con `threadId`, `turnId`, `itemId`,
  `command`, `reason`, `availableDecisions` y `commandActions`.
- Se responde `{"jsonrpc":"2.0","id":<id>,"result":{"decision":"accept"}}`.
  Otras decisiones del esquema: `acceptForSession`, `decline`, `cancel`,
  `acceptWithExecpolicyAmendment`, `applyNetworkPolicyAmendment`.
- Tras la respuesta aparece `serverRequest/resolved` y la ejecución continúa.
  Con `decline` el agente continúa el turno; con `cancel` se interrumpe (no
  probado en este corte). También existen `item/fileChange/requestApproval`,
  `item/tool/requestUserInput`, `item/permissions/requestApproval` y las viejas
  `applyPatchApproval` / `execCommandApproval` (no probadas).

### Persistencia, resume y versionado

- Los threads se guardan en `CODEX_HOME/sessions/.../rollout-*.jsonl`. Un nuevo
  proceso `app-server` con el mismo `CODEX_HOME` ejecuta `thread/resume` con el
  `threadId` y recupera turnos e ítems. `thread/list` y `thread/turns/list`
  funcionan; `thread/items/list` existe en el esquema pero no se probó.
- `thread/read` con `includeTurns: true` funciona pero emite
  `deprecationNotice`: usar `includeTurns: false` y paginar con
  `thread/turns/list` + `thread/items/list`.
- **Aislar `CODEX_HOME` sin `auth.json` rompe el turno**: se observaron 401 y
  reintentos hasta `turn.status = "failed"`. El probe crea un `CODEX_HOME`
  temporal y hace *symlink* del `auth.json` real (sin copiarlo ni leerlo).
- El cliente debe responder requests del servidor mientras espera respuestas de
  sus propios requests; un cliente JSON-RPC puramente secuencial se bloquea.

## 4. OpenCode v2 API

### Servidor y autenticación

- `opencode serve --hostname 127.0.0.1 --port <p>` imprime en stdout
  `server listening on ...` y `server password <43 chars>`. La API exige
  `Authorization: Basic base64("opencode:" + password)`; sin ella devuelve 401
  `UnauthorizedError`. El esquema OpenAPI está en `GET /openapi.json` (458 KB).
- En 2.3.2 **no existe el flag `--standalone`** (la ayuda de `--print-logs` lo
  menciona, pero el comando lo rechaza). Sí existen `--service` y `--stdio`.
- Existe un servicio de usuario ya corriendo: `opencode service status` y
  `$XDG_STATE_HOME/opencode/service.json` (url + password). Se comprobó que
  `GET /api/info` funciona contra él y que **comparte el mismo store**: las
  sesiones creadas por un `serve` propio aparecen en el servicio de usuario.
- Implicación: el driver debe hacer *attach* al servicio de usuario cuando esté
  disponible, en vez de iniciar un segundo servidor. El password es secreto
  local: nunca debe entrar en logs ni en fixtures.

### Sesiones, eventos y prompts

- `POST /api/session` con `title`, `location.directory` y opcionalmente
  `permissions` (ruleset `{action,resource,effect}`) crea la sesión.
- `POST /api/session/{id}/prompt` con `text` escribe el prompt y devuelve el
  mensaje de usuario. No espera a la respuesta.
- `GET /api/event` es un SSE con envelope `{id,type,data,created,...}`; admite
  filtro `?types=...`. Algunos eventos de herramienta no llevan `sessionID` en
  `data` (por ejemplo `shell.created` lo pone en `data.info.metadata.sessionID`),
  así que el driver no puede filtrar solo por `data.sessionID`.
- Orden observado en un turno con herramienta: `session.execution.started` →
  `session.inbox.delivered` → `session.step.started` → `session.reasoning.*` →
  `session.tool.input.started/delta/ended` → `session.tool.called` →
  `session.tool.progress` → `session.tool.success` → `session.step.ended` →
  (nuevo `session.step.started` para la respuesta final) → `session.text.*` →
  `session.execution.succeeded`.
- `session.text.ended` trae el texto completo; `session.text.delta` permite
  streaming. `session.step.ended` trae tokens/coste. `GET
  /api/session/{id}/message` devuelve el historial persistido con paginación.

### Permisos

- Con `permissions: [{"action":"shell","resource":"*","effect":"ask"}]`, la
  herramienta `shell` produjo `permission.asked` con `id`, `sessionID`, `action`,
  `resources`, `save` y `source`. El nombre de la acción es el de la herramienta
  (`shell`), no `bash`.
- `POST /api/session/{id}/permission/{requestID}/reply` con
  `{"decision":"once"}` (o `always` / `reject`) responde 204. Se comprobaron
  ambos caminos: `once` ejecutó el comando y creó el archivo; `reject` produjo
  `session.tool.failed` (`aborted`, «The user declined this tool call») y
  `session.execution.interrupted`. El evento `permission.replied` confirma la
  decisión.
- Las aprobaciones quedan pendientes en el servidor si el cliente se desconecta:
  en una ejecución, un request de una sesión abandonada apareció en el stream de
  otra conexión después de varios minutos. Un driver debe reconciliar
  aprobaciones pendientes al reconectar (`GET /api/permission/request` o
  `GET /api/session/{id}/permission`).

### Persistencia y alcance

- Tras detener y reiniciar `opencode serve` en el mismo puerto, `GET
  /api/session/{id}` y `GET /api/session/{id}/message` devolvieron la sesión y
  sus mensajes. La persistencia es real (SQLite en
  `~/.local/share/opencode/opencode.db`); Fleet no debe escribir esa base.
- `GET /api/session` lista **todas** las sesiones del store de usuario, de todos
  los directorios/proyectos. Un driver debe filtrar por `location.directory`.
- El probe elimina las sesiones `gf-p0-*` que crea. Quedaron sesiones de probes
  manuales anteriores a la automatización; no contienen secretos.

## 5. Diferencias que afectan al contrato Fleet

| Dimensión | Gentle Shell / Pi | Codex app-server | OpenCode v2 |
|---|---|---|---|
| Transporte | stdio JSONL, un proceso por sesión | stdio JSON-RPC, un proceso por cliente | HTTP + SSE, servicio compartido |
| Dueño del proceso | el propio Pi; muere con stdin | el app-server; sigue tras desconectar | el servidor OpenCode |
| Reanudar | `switch_session(ruta)` | `thread/resume(id)` | abrir sesión existente por ID |
| Listar sesiones | no en RPC (sí en canal de escritorio) | `thread/list` | `GET /api/session` (global) |
| Unidad de fin de turno | `agent_settled` | `turn/completed` | `session.execution.{succeeded,failed,interrupted}` |
| Texto completo al final | `message_end` | `item/completed` | `session.text.ended` |
| Permiso | UI request dentro del stream | request JSON-RPC del servidor | evento + endpoint REST |
| Decisión | `extension_ui_response` | `result.decision` | `POST .../reply` |
| señal de progreso para Fleet | eventos de herramienta | `item/started/completed` + status | `session.step.*` + `session.tool.*` |
| Estabilidad de wire | media (extensiones del usuario) | baja (experimental, por versión) | media-alta (OpenAPI publicado) |
| Credenciales para smoke | sí | sí | no (modelo gratuito por defecto) |

## 6. Problemas encontrados

1. **P1 – Pi no sobrevive a la desconexión del cliente.** Confirmado con prueba
   de proceso. Bloquea cualquier promesa de «sesión durable» si el driver Pi se
   implementa antes del runner (P3).
2. **P1 – Aislamiento de `CODEX_HOME` sin credenciales** produce 401 y turnos
   fallidos. El runner debe heredar o enlazar la autenticación de forma
   explícita, sin copiar secretos.
3. **P2 – Configuración ambiental de Pi**: el modo de permisos guardado puede ser
   `SUPER-YOLO`. El driver debe fijar el modo por sesión.
4. **P2 – Codex deprecó `includeTurns: true`** en `thread/read`; conviene paginar
   desde el principio para no reescribir el driver.
5. **P2 – OpenCode no documenta bien el flag `--standalone`** en 2.3.2 y expone
   un store global; attach y filtrado por directorio son obligatorios.
6. **P2 – Aprobaciones huérfanas**: en OpenCode, una aprobación de una sesión
   abandonada siguió pendiente y apareció en otra conexión; en Pi, cualquier
   petición pendiente desaparece con el proceso. Necesita reconciliación en P2.
7. **P3 – Sin señal de «replay desde cursor» en Pi/Codex**: OpenCode expone
   `durable.seq/version` en algunos eventos; los otros dos solo ofrecen
   historial completo. El cursor Fleet debe ser propio (P2).
8. **Desconocido**: `abort`/cancelación real en Pi, `search`/items paginados de
   Codex, `session.permission.create` de OpenCode, y comportamiento en Windows.

## 7. Decisiones propuestas para P1 (ratificar, no automáticas)

| ID | Propuesta | Evidencia |
|---|---|---|
| P0-D1 | **Primer driver real: OpenCode**. El runner hace attach al servicio de usuario; si no existe, levanta `opencode serve` en loopback. | Único con API tipada, servicio compartido, persistencia verificada tras reinicio y smoke sin credenciales; DRIVERS pide elegir por evidencia. |
| P0-D2 | D04 se mantiene: `AgentDriver` con capacidades y native IDs; no asumir paridad. Las tablas anteriores son la entrada del registro de capacidades por versión. | Diferencias de fin de turno, permisos y resume. |
| P0-D3 | D05 se matiza: el Runner **posee o se adjunta** al proceso del agente; para Pi debe poseerlo (muere con stdin), para OpenCode puede adjuntarse. | Prueba de desconexión Pi + attach OpenCode. |
| P0-D4 | D09 se mantiene: nunca reenviar un prompt tras timeout sin reconciliar primero aprobaciones pendientes y cursor de eventos. | Aprobaciones huérfanas observadas en OpenCode; `disposition:started` de Pi. |
| P0-D5 | Fijar versión por driver en configuración y repetir el spike al cambiarla. Generar fixtures por versión. | Esquema Codex de 292 tipos y OpenAPI 2.3.2. |
| P0-D6 | El wire Fleet define su propio `event_id` secuencial y `seq` por sesión; no reutilizar IDs de proveedor como cursor. | Solo OpenCode expone algo parecido a `durable.seq`. |

Estas propuestas **no cambian D01–D12**; solo las concretan. Cualquier cambio
de decisión requiere ADR breve según `docs/DECISIONS.md`.

## 8. Criterio de cierre de P0

| Criterio de `PLAN.md` | Resultado |
|---|---|
| Spikes de los tres motores en versiones fijadas | ✅ tres programas ejecutables, `PASS` |
| Fixtures de chat, herramientas, permisos y recuperación | ✅ tres capturas anonimizadas |
| Documentación comprobada | ✅ este documento + README de spikes |
| Sin credenciales en fixtures | ✅ verificadas por inspección y anonimización |
| Sin servicio persistente ni esquema definitivo | ✅ no se creó ninguno |

Observaciones ajenas al objetivo quedaron registradas en la sección 6 con su
prioridad; no se rediseñó la arquitectura por ellas. **P0 puede cerrarse** si se
ratifican P0-D1 a P0-D6 antes de empezar P1. Queda explícitamente fuera de P0:
Windows, SIGKILL del daemon, cancelación real y paginación avanzada.
