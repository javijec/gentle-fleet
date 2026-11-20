# Investigación técnica — Codex app-server

**Plataforma:** Linux x86_64. **Estado:** investigación P0, no contrato Fleet.
**Convenciones:** VERIFIED / OBSERVED / UNTESTED / UNSUPPORTED definidas en
[`PROVIDER_COMPARISON.md`](PROVIDER_COMPARISON.md).

## Versiones y evidencia

| Dato | Valor | Evidencia |
|---|---|---|
| `codex --version` | `codex-cli 0.171.0` | `spikes/p0/fixtures/command-outputs.txt` |
| `initialize.result.userAgent` | `gentle-fleet-p0/0.171.0 (Linux Unknown; x86_64) ...` | `codex.jsonl` `response.initialize` [CX-E1] |
| `thread.cliVersion` | `0.171.0` | `codex.jsonl` `response.thread/start` [CX-E1] |
| Esquema generado | `codex app-server generate-json-schema --out ...` → 714 definiciones v2 | `codex-schema-summary.json` [CX-E4] |

Índice de evidencia:

| ID | Script / captura | Qué cubre |
|---|---|---|
| CX-E1 | `spikes/p0/codex/probe_codex.py`; `fixtures/codex.jsonl` | handshake, thread, turn, aprobación, read, resume |
| CX-E2 | `fixtures/codex-missing-auth.jsonl` | `CODEX_HOME` sin `auth.json` → 401 y turno `failed` |
| CX-E3 | `fixtures/codex-manual-success.jsonl` | captura manual anterior: `waitingOnApproval`, estados MCP, rate limits |
| CX-E4 | `fixtures/command-outputs.txt`, `fixtures/codex-schema-summary.json` | `codex app-server --help`, subcomandos, versión y recuento del esquema |

## 1. Comandos reales de ejecución y conexión

**VERIFIED (0.171.0, Linux):**

```bash
codex app-server                                      # JSON-RPC por stdio
codex app-server generate-json-schema --out <dir>     # esquema de la versión
codex app-server daemon | proxy | generate-ts         # subcomandos de la ayuda
```

`app-server` está marcado como `[experimental]` en su propia ayuda [CX-E4]. No
hay API HTTP: la conexión es el stdio del proceso [CX-E1].

## 2. Protocolo nativo y ejemplos

**VERIFIED:** cada mensaje es un frame JSON-RPC 2.0 por línea. El cliente envía
`{"jsonrpc":"2.0","id","method","params"}`; el servidor responde con `result` o
`error`, y además emite notificaciones y **requests hacia el cliente** con `id`
[CX-E1].

Handshake real [CX-E1]:

```json
{"jsonrpc":"2.0","id":"1","method":"initialize","params":{
  "clientInfo":{"name":"gentle-fleet-p0","version":"0.1.0"},
  "capabilities":{"experimentalApi":false}}}
→ {"id":"1","result":{"userAgent":"...","codexHome":"...","platformFamily":"unix","platformOs":"linux"}}
{"jsonrpc":"2.0","method":"initialized"}
```

Inicio de thread y turno [CX-E1]:

```json
{"jsonrpc":"2.0","id":"2","method":"thread/start","params":{
  "cwd":"<workspace>","approvalPolicy":"on-request","sandbox":"read-only"}}
{"jsonrpc":"2.0","id":"3","method":"turn/start","params":{
  "threadId":"<uuid>","input":[{"type":"text","text":"Run exactly this shell command: ..."}]}}
```

La respuesta de `turn/start` contiene `turn.status:"inProgress"`,
`itemsView:"notLoaded"` e `items:[]`: **es una aceptación, no una finalización**
[CX-E1].

Ids nativos observados: `threadId` con forma de UUID (`01a1f9...`), `turnId`,
`itemId` (`msg_*`, `exec-<uuid>`) y `requestId` numérico en los requests del
servidor [CX-E1].

## 3. Inicio, continuación, recuperación y cierre de sesión

| Operación | Estado | Evidencia |
|---|---|---|
| Iniciar | VERIFIED | `thread/start`; devuelve `thread` con `cwd`, `path`, `status` [CX-E1] |
| Continuar | VERIFIED | `turn/start` con el mismo `threadId` [CX-E1] |
| Recuperar en otro proceso | VERIFIED | nuevo `app-server` + `initialize` + `thread/resume` devuelve el thread con sus turns [CX-E1] |
| Listar | VERIFIED / OBSERVED | `thread/turns/list` verificado en el probe; `thread/list` visto solo en ejecución manual [CX-E1] |
| Leer historial | VERIFIED | `thread/read`; emite `deprecationNotice` [CX-E1] |
| Cerrar | OBSERVED | el probe cierra stdin y el proceso termina; no hay un request `close` probado [CX-E1] |
| Interrumpir | UNTESTED | `turn/interrupt` aparece en el esquema, no se ejecutó [CX-E1] |

**VERIFIED:** `thread/read` con `includeTurns:true` funciona pero recibe dos
avisos distintos del propio servidor [CX-E1] [CX-E3]:

> omit `includeTurns` or set it to `false`, then page with `thread/turns/list` and `thread/items/list`

> use `excludeTurns: true`, then page with `thread/turns/list` and `thread/items/list`

La segunda forma es la más reciente observada. El driver debe paginar desde el
inicio.

## 4. Envío de prompts y recepción de respuestas

**VERIFIED:** el prompt entra como `input:[{"type":"text",...}]` en
`turn/start`. La respuesta llega como texto (`item/agentMessage/delta`) y como
`item/completed` con el mensaje completo, por ejemplo
`"Command completed successfully."` [CX-E1].

Los ítems de usuario también aparecen como `item/started` e
`item/completed` con `type:"userMessage"` [CX-E3]. El texto final del turno se
puede leer de `turn/completed.params.turn.items` [CX-E1].

## 5. Eventos de streaming y detección de fin de turno

Notificaciones VERIFIED en la ejecución correcta [CX-E1]: `thread/started`,
`thread/status/changed`, `turn/started`, `item/started`,
`item/agentMessage/delta`, `item/completed`, `thread/tokenUsage/updated`,
`account/rateLimits/updated`, `mcpServer/startupStatus/updated`,
`deprecationNotice` y `serverRequest/resolved`.

**VERIFIED:** el fin de turno es `turn/completed` con
`turn.status:"completed"`. El probe falla explícitamente si el estado es
`failed` o `interrupted` [CX-E1]. `thread/status/changed` con
`status.type:"idle"` es una señal auxiliar, no el cierre del turno [CX-E1].

**OBSERVED:** con aprobación pendiente, `thread/status/changed` trae
`activeFlags:["waitingOnApproval"]` [CX-E3].

**VERIFIED:** `turn.completed` incluye `itemsView:"summary"`; el detalle está en
`thread/turns/list` [CX-E1].

## 6. Herramientas, permisos interactivos y cancelación

**VERIFIED:** una orden del agente aparece como `item/started` con
`item.type:"commandExecution"`, `command`, `cwd`, `status:"inProgress"` y
`commandActions`; después `item/completed` con `exitCode` y `durationMs` [CX-E1].

**VERIFIED:** la aprobación es un request JSON-RPC del servidor, con `id`
numérico [CX-E1]:

```json
{"method":"item/commandExecution/requestApproval","id":0,"params":{
  "kind":"command","threadId":"<uuid>","turnId":"<uuid>","itemId":"exec-<uuid>",
  "reason":"Allow this command to write p0-codex.txt?",
  "command":"/usr/bin/bash -lc 'echo gf-codex > p0-codex.txt'",
  "availableDecisions":["accept",{"acceptWithExecpolicyAmendment":{...}},"cancel"]}}
```

Respuesta real [CX-E1]:

```json
{"jsonrpc":"2.0","id":0,"result":{"decision":"accept"}}
→ notificación serverRequest/resolved {"requestId":0}
```

**OBSERVED en la ejecución fallida por autenticación:** el mismo flujo de
aprobación ocurrió antes de los 401 [CX-E2]. El probe responde requests del
servidor *mientras* espera sus propias respuestas; un cliente secuencial se
bloquea [CX-E1].

**UNTESTED:** `decline`, `cancel`, `acceptForSession`,
`acceptWithExecpolicyAmendment`, `item/fileChange/requestApproval`,
`item/tool/requestUserInput`, `item/permissions/requestApproval`,
`applyPatchApproval`, `execCommandApproval` y `turn/interrupt` [CX-E1].

## 7. Persistencia, reconexión y comportamiento al cerrar procesos

- **VERIFIED:** `thread.path` apunta a
  `<CODEX_HOME>/sessions/2026/11/20/rollout-...jsonl` [CX-E1].
- **VERIFIED:** un proceso nuevo con el mismo `CODEX_HOME` hace
  `thread/resume` y `thread/turns/list` y recupera el turno completo [CX-E1].
- **VERIFIED:** los threads sobreviven al cierre del `app-server`; el probe
  usa dos procesos distintos [CX-E1].
- **UNTESTED:** desconexión del cliente con un turno en vuelo, aprobación
  pendiente reconnectada, SIGKILL, Windows y caída de host [CX-E1].

## 8. Autenticación y manejo seguro de credenciales

- **OBSERVED:** `codex login status` → `Logged in using ChatGPT`.
- **VERIFIED:** si se fija un `CODEX_HOME` vacío, `initialize` y
  `thread/start` funcionan pero el turno recibe HTTP 401, reintenta 5 veces y
  termina con `turn.status:"failed"` [CX-E2]. `error.additionalDetails` incluye
  el mensaje *Missing bearer or basic authentication in header*.
- **VERIFIED:** el probe evita esto creando un `CODEX_HOME` temporal con un
  **symlink** a `auth.json` real, sin copiarlo ni leerlo [CX-E1].
- **VERIFIED:** la anonimización sustituye UUIDs, `<HOME>` y hostname antes de
  escribir fixtures; no hay tokens ni `accountId` en las capturas [CX-E1] [CX-E2].

Nota de seguridad: `codexErrorInfo` y `additionalDetails` pueden incluir URLs y
request IDs del proveedor; se conservan porque no son secretos, pero el driver
Fleet no debe reenviarlos al cliente sin traducirlos.

## 9. Limitaciones y comportamientos no comprobados

| Clase | Detalle |
|---|---|
| VERIFIED | Esquema experimental y dependiente de versión (714 definiciones v2) [CX-E4] |
| VERIFIED | `includeTurns` deprecado en dos variantes de mensaje [CX-E1] [CX-E3] |
| VERIFIED | `CODEX_HOME` sin credenciales produce 401 y turno fallido [CX-E2] |
| OBSERVED | `availableDecisions` ofrece opciones que no se probaron [CX-E1] |
| UNTESTED | `decline`/`cancel`, `turn/interrupt`, `thread/items/list`, keepalive y reconexión con turno en vuelo |
| UNTESTED | Windows, SIGKILL, caída de host |
| UNSUPPORTED | Ninguna API HTTP pública en esta versión; solo app-server stdio |

## 10. Referencias

- Programa: [`spikes/p0/codex/probe_codex.py`](../../spikes/p0/codex/probe_codex.py)
- Capturas: [`codex.jsonl`](../../spikes/p0/fixtures/codex.jsonl),
  [`codex-missing-auth.jsonl`](../../spikes/p0/fixtures/codex-missing-auth.jsonl),
  [`codex-manual-success.jsonl`](../../spikes/p0/fixtures/codex-manual-success.jsonl)
- Comandos: [`command-outputs.txt`](../../spikes/p0/fixtures/command-outputs.txt)
- Documentación externa de referencia (no incluida en el repo):
  `https://learn.chatgpt.com/docs/app-server`
- Comparación: [`PROVIDER_COMPARISON.md`](PROVIDER_COMPARISON.md)
