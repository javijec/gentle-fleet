# Investigación técnica — OpenCode v2 API

**Plataforma:** Linux x86_64. **Estado:** investigación P0, no contrato Fleet.
**Convenciones:** VERIFIED / OBSERVED / UNTESTED / UNSUPPORTED definidas en
[`PROVIDER_COMPARISON.md`](PROVIDER_COMPARISON.md).

## Versiones y evidencia

| Dato | Valor | Evidencia |
|---|---|---|
| `opencode --version` | `opencode v2.3.2` | `spikes/p0/fixtures/command-outputs.txt` |
| `/api/info` | `{"version":"2.3.2","pid":...,"capabilities":{"persistentPty":true}}` | `opencode.jsonl` (probe `server.info`) [OC-E1] |
| `session.created.data.version` | `"2.3.2"` | `opencode.jsonl` línea 2 [OC-E1] |
| Modelo por defecto usado | `opencode/tidepool-alpha-free` (coste 0) | captura de la ejecución; sin credencial [OC-E1] |

Índice de evidencia:

| ID | Script / captura | Qué cubre |
|---|---|---|
| OC-E1 | `spikes/p0/opencode/probe_opencode.py::probe_auto`; `fixtures/opencode.jsonl` | servidor aislado, SSE, chat, `once`, `reject`, persistencia |
| OC-E2 | `probe_opencode.py::probe_attach` | attach al servicio de usuario (`/api/info`) |
| OC-E3 | `fixtures/opencode-orphan-permission.jsonl` | aprobación pendiente de una sesión abandonada (captura manual) |
| OC-E4 | `fixtures/command-outputs.txt` | versiones y `opencode serve --help` |
| OC-E5 | `fixtures/opencode-openapi-subset.json` (subconjunto de `GET /openapi.json`) | esquema OpenAPI 3.1, rutas citadas y `Permission.Reply`/`Permission.Request`/`Session.Info` |

## 1. Comandos reales de ejecución y conexión

**VERIFIED (2.3.2, Linux):**

```bash
opencode serve --hostname 127.0.0.1 --port <p>
# stdout:
#   server listening on http://127.0.0.1:<p>
#   server password <43 caracteres>
opencode service status                    # http://127.0.0.1:49374 en la máquina probada
opencode auth list                         # proveedores configurados
```

Autenticación HTTP [OC-E1]:

```bash
curl -H "Authorization: Basic $(printf 'opencode:%s' "$PASSWORD" | base64 -w0)" \
     http://127.0.0.1:<p>/api/info
```

- Sin esa cabecera, la API responde 401 `{"_tag":"UnauthorizedError",...}` [OC-E1].
- `GET /openapi.json` devuelve el esquema (OpenAPI 3.1, título `opencode HttpApi`) [OC-E5].
- **UNSUPPORTED:** `--standalone` no existe en 2.3.2; la ayuda de `--print-logs`
  lo menciona pero `opencode serve --standalone` responde *Unrecognized flag* [OC-E4].
- **VERIFIED:** `--service` y `--stdio` sí aparecen en la ayuda [OC-E4].
- El password lo imprime el propio proceso en stdout; **no debe loguearse ni
  guardarse en fixtures** [OC-E1].

**VERIFIED:** attach al servicio de usuario [OC-E2]: el archivo
`$XDG_STATE_HOME/opencode/service.json` (por defecto
`<HOME>/.local/state/opencode/service.json`) contiene `url`, `pid` y `password`.
Con esos valores, `GET /api/info` funcionó contra el servicio ya corriendo. El
mismo store contiene las sesiones creadas por un `serve` independiente.

## 2. Protocolo nativo y ejemplos

**VERIFIED:** HTTP/JSON para comandos y SSE para eventos; no hay framing propio
[OC-E1].

Crear sesión [OC-E1]:

```json
POST /api/session
{"title":"gf-p0-chat","location":{"directory":"<workspace>"},
 "permissions":[{"action":"shell","resource":"*","effect":"ask"}]}
→ {"data":{"id":"ses_...","location":{"directory":"<workspace>"},"title":"gf-p0-chat"}}
```

Enviar prompt [OC-E1]:

```json
POST /api/session/{sessionID}/prompt
{"text":"Run exactly this shell command: echo gf-opencode > p0-oc.txt. Then stop."}
→ {"data":{"id":"msg_...","sessionID":"ses_...","type":"user","delivery":"steer"}}
```

Suscripción [OC-E1]:

```bash
GET /api/event
Accept: text/event-stream
# cada evento: data: {"id":...,"type":"session.tool.called","data":{...},"durable":{...}}
```

El parámetro `types` permite filtrar tipos en la query; el probe no lo usa para
no perder eventos de herramientas [OC-E5].

Ids nativos observados: `ses_*`, `msg_*`, `call_*`, `per_*`, `sh_*`, `evt_*`,
`projectID`, `assistantMessageID` [OC-E1]. Son opacos y no deben convertirse en
IDs Fleet.

## 3. Inicio, continuación, recuperación y cierre de sesión

| Operación | Estado | Evidencia |
|---|---|---|
| Iniciar | VERIFIED | `POST /api/session` con `location.directory` [OC-E1] |
| Continuar | VERIFIED | `POST /api/session/{id}/prompt` en la misma sesión [OC-E1] |
| Recuperar tras reinicio | VERIFIED | tras matar y relanzar `serve` en el mismo puerto, `GET /api/session/{id}` y `GET .../message` devolvieron sesión y mensajes [OC-E1] |
| Adjuntar a servicio existente | VERIFIED | `service.json` + Basic auth; comparte store [OC-E2] |
| Listar | OBSERVED | `GET /api/session` devolvió **todas** las sesiones del usuario en ejecución manual; el probe no lo afirma [OC-E1] |
| Borrar | VERIFIED | `DELETE /api/session/{id}` → 204; el probe limpia las sesiones `gf-p0-*` que crea [OC-E1] |
| Cerrar agente | OBSERVED | el probe termina el proceso `serve`; no se probó un equivalente a StopSession con turno en vuelo [OC-E1] |
| Interrumpir turno | UNTESTED | `POST /api/session/{id}/interrupt` existe en OpenAPI [OC-E5] |

El store es global: el probe debe filtrar por `location.directory` para no
mostrar sesiones de otros proyectos del mismo usuario [OC-E1].

## 4. Envío de prompts y recepción de respuestas

**VERIFIED:** el orden observado en un turno con herramienta [OC-E1]:

1. `session.inbox.enqueued` → `session.execution.started` →
   `session.inbox.delivered`.
2. `session.step.started` → `session.step.first-token` →
   `session.reasoning.started/delta/ended`.
3. `session.tool.input.started/delta/ended` → `session.tool.called` →
   `session.tool.progress` → `session.tool.success` o `session.tool.failed`.
4. `session.step.ended` con `finish:"tool-calls"` y tokens/coste.
5. Otro `session.step.started` para la respuesta final →
   `session.text.started/delta/ended` → `session.step.ended` con `finish:"stop"`.
6. `session.execution.succeeded`.

Ejemplo real de texto final [OC-E1]:

```json
{"type":"session.text.ended","data":{"sessionID":"ses_...","ordinal":0,
 "text":"Done — the command `echo gf-opencode > p0-oc.txt` ran and wrote the file `p0-oc.txt` in the working directory."}}
```

**VERIFIED:** `POST /prompt` devuelve inmediatamente el mensaje de usuario; la
finalización llega por eventos, nunca en esa respuesta [OC-E1].

**OBSERVED:** `shell.created` y `shell.exited` son eventos auxiliares del
ejecutor; `shell.created` trae el `sessionID` en
`data.info.metadata.sessionID`, no en `data.sessionID` [OC-E1]. Un driver no
puede filtrar el stream solo por `data.sessionID`.

## 5. Eventos de streaming y detección de fin de turno

**VERIFIED:** `session.execution.started` / `succeeded` / `failed` /
`interrupted` son la señal agregada de turno. `session.step.*` es por paso del
modelo; un turno normal tuvo dos pasos [OC-E1].

**VERIFIED:** `session.text.ended` incluye el texto completo, por lo que no es
obligatorio reconstruirlo desde `session.text.delta` [OC-E1].

**OBSERVED:** algunos eventos incluyen `durable:{"aggregateID", "seq", "version"}`
(por ejemplo `session.created` `seq:0`, `session.execution.succeeded` `seq:23`)
[OC-E1]. El significado de `seq` para replay y su ámbito son **UNTESTED**; no se
debe usar como cursor Fleet sin prueba (P0-D6).

**OBSERVED:** con un permiso rechazado se emitió
`session.execution.interrupted` con `reason:"shutdown"` [OC-E1]. La interpretación
de ese motivo es **UNTESTED**; el servidor siguió operando después.

## 6. Herramientas, permisos interactivos y cancelación

**VERIFIED:** herramienta `shell` [OC-E1]:

```json
{"type":"session.tool.called","data":{"name":"shell","input":{"command":"echo gf-opencode > p0-oc.txt"}}}
{"type":"session.tool.success","data":{"content":[{"type":"text","text":"(no output)"}],
 "metadata":{"status":"completed","exit":0}}}
```

**VERIFIED:** los permisos se configuran por sesión con reglas
`{action,resource,effect}`; el nombre de acción observado es `shell` (el de la
herramienta), no `bash` [OC-E1].

```json
{"type":"permission.asked","data":{"id":"per_...","sessionID":"ses_...",
 "action":"shell","resources":["echo gf-accepted > p0-accept.txt"],
 "save":["echo *"],"source":{"type":"tool","messageID":"msg_...","id":"call_..."}}}
```

Respuesta [OC-E1]:

```json
POST /api/session/{id}/permission/{requestID}/reply
{"decision":"once"}     → 204
{"decision":"reject"}   → 204
```

`Permission.Reply` admite `once`, `always`, `reject` según OpenAPI [OC-E5]; se
probaron `once` y `reject` [OC-E1].

- `once` → `permission.replied`, `session.tool.success`, archivo creado [OC-E1].
- `reject` → `permission.replied`, `session.tool.failed` con
  `error.type:"aborted"` y mensaje *The user declined this tool call*, archivo no
  creado [OC-E1].
- `always` y `session.permission.create` son **UNTESTED** [OC-E5].
- **OBSERVED:** una aprobación de una sesión abandonada reapareció en el stream
  de otra conexión minutos después [OC-E3]; no hay prueba de reconciliación
  automática.

## 7. Persistencia, reconexión y comportamiento al cerrar procesos

- **VERIFIED:** la sesión y sus mensajes sobreviven a detener y reiniciar `serve`
  [OC-E1].
- **VERIFIED:** el servicio de usuario ya corriendo, con distinto PID, ve las
  mismas sesiones [OC-E2].
- **VERIFIED:** cerrar el cliente SSE no detiene la sesión; el turno sigue en el
  servidor [OC-E1] [OC-E3].
- **OBSERVED:** una aprobación pendiente no se resuelve sola al desconectar [OC-E3].
- **UNTESTED:** SIGKILL del servidor, caída de host, migración entre hosts,
  Windows.

## 8. Autenticación y manejo seguro de credenciales

- **VERIFIED:** dos capas distintas: (1) password local del servidor para la API,
  leído de stdout (propio) o de `service.json` (attach) [OC-E1] [OC-E2]; (2)
  credenciales de proveedor de modelo gestionadas por OpenCode
  (`opencode auth list` mostró `OpenAI stored`) [OC-E4].
- **VERIFIED:** el modelo `tidepool-alpha-free` permitió el smoke sin credencial
  de usuario [OC-E1].
- **VERIFIED:** el probe no imprime ni guarda el password: solo lo usa en la
  cabecera `Authorization`; las capturas no lo contienen [OC-E1].
- Ningún archivo del repo contiene tokens, passwords ni `service.json`.

## 9. Limitaciones y comportamientos no comprobados

| Clase | Detalle |
|---|---|
| VERIFIED | Store global; requiere filtrar por directorio [OC-E1] |
| VERIFIED | `--standalone` rechazado en 2.3.2 [OC-E4] |
| OBSERVED | `durable.seq/version` solo en algunos eventos; semántica de replay desconocida [OC-E1] |
| OBSERVED | `reason:"shutdown"` tras `reject`; sin interpretar [OC-E1] |
| OBSERVED | Aprobación huérfana tras abandonar la sesión [OC-E3] |
| UNTESTED | `always`, `session.permission.create`, `/interrupt`, `/compact`, `/fork`, filtro `?types=`, backpressure y lag de SSE |
| UNTESTED | Replay desde `durable.seq`, Windows, SIGKILL, caída de host |
| UNSUPPORTED | Analizar salida TUI como chat: P0 usa exclusivamente la API HTTP [OC-E1] |

## 10. Referencias

- Programa: [`spikes/p0/opencode/probe_opencode.py`](../../spikes/p0/opencode/probe_opencode.py)
- Capturas: [`opencode.jsonl`](../../spikes/p0/fixtures/opencode.jsonl),
  [`opencode-orphan-permission.jsonl`](../../spikes/p0/fixtures/opencode-orphan-permission.jsonl),
  [`command-outputs.txt`](../../spikes/p0/fixtures/command-outputs.txt)
- Esquema OpenAPI: subconjunto conservado en
  [`opencode-openapi-subset.json`](../../spikes/p0/fixtures/opencode-openapi-subset.json),
  extraído de `GET /openapi.json` del servidor 2.3.2 (458 KB; regenerable con
  `opencode serve`).
- Documentación externa: `https://opencode.ai/docs/cli`, `https://opencode.ai/docs/api`
- Comparación: [`PROVIDER_COMPARISON.md`](PROVIDER_COMPARISON.md)
