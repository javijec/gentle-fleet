# Investigación técnica — Gentle Shell / Pi

**Plataforma:** Linux x86_64. **Estado:** investigación P0, no contrato Fleet.
**Convenciones:** VERIFIED / OBSERVED / UNTESTED / UNSUPPORTED definidas en
[`PROVIDER_COMPARISON.md`](PROVIDER_COMPARISON.md). Todas las afirmaciones
aquí van acompañadas de una referencia de evidencia `[PI-E#]`.

## Versiones y evidencia

| Dato | Valor | Evidencia |
|---|---|---|
| `gentle-shell --version` | `gentle-shell 4.8.0`, `pi 1.5.2`, `home isolated <HOME>/.gentle-shell/agent` | `spikes/p0/fixtures/command-outputs.txt` |
| `pi --version` | `1.5.2` | ídem |
| Paquete global | `gentle-pi 4.8.0-main.652552becbb6` (`package.json`) | ídem (anotación del paquete) |
| Modelo por defecto observado | `openai-codex / gpt-6-luna`, api `openai-codex-responses` | `gentle-shell-pi.jsonl` `chat.response` (`get_state`) [PI-E1] |

Índice de evidencia:

| ID | Script / captura | Qué cubre |
|---|---|---|
| PI-E1 | `spikes/p0/pi/probe_pi.py::scenario_chat`; `fixtures/gentle-shell-pi.jsonl` (`chat.*`) | arranque, `get_state`, prompt, delta, herramienta, `agent_settled` |
| PI-E2 | `probe_pi.py::scenario_permission`; `gentle-shell-pi.jsonl` (`permission.ui_request`, `permission.tool`) | prompt de permiso y denegación |
| PI-E3 | `probe_pi.py::scenario_resume`; `gentle-shell-pi.jsonl` (`resume.*`, `resume-reopen.*`) | `sessionFile`, `switch_session`, `get_messages` |
| PI-E4 | `probe_pi.py::scenario_disconnect`; `gentle-shell-pi.jsonl` (`disconnect.result`) | cierre de stdin y trabajo abandonado |
| PI-E5 | `spikes/p0/fixtures/command-outputs.txt` | versiones y flags de `pi --help` |

## 1. Comandos reales de ejecución y conexión

**VERIFIED (1.5.2, Linux):** el modo RPC se activa con:

```bash
GENTLE_SHELL_INTERACTIVE_HOST=1 gentle-shell --mode rpc --session-dir <dir>
```

- `GENTLE_SHELL_INTERACTIVE_HOST=1` activa el extension host interactivo de
  Gentle Pi; sin él el canal de escritorio no está disponible [PI-E1].
- `--mode rpc` es un flag de Pi (también `--mode text|json`), no un subcomando
  [PI-E5].
- `--session-dir <dir>` dirige los archivos de sesión a un directorio propio;
  `--no-session` crea una sesión efímera [PI-E5]. El probe usa `--session-dir`
  para no tocar el historial del usuario [PI-E3].
- No hay socket ni servidor: la conexión **es** el proceso, por stdin/stdout
  [PI-E1].

## 2. Protocolo nativo y ejemplos

**VERIFIED:** cada línea de stdout es un JSON. Los requests salen por stdin con
`{"id","type",...}` y las respuestas vuelven con
`{"id","type":"response","command","success","data"}` [PI-E1].

Comandos usados por el probe (de la superficie documentada en
`pi docs/rpc-commands.md`, comprobados los que aparecen en la captura):

```json
{"id":"r1","type":"get_state"}
{"id":"r2","type":"prompt","message":"/gentle:mode normal"}
{"id":"r3","type":"prompt","message":"Use the bash tool to run exactly: echo gf-pi > p0-pi.txt. Then stop."}
{"id":"r4","type":"switch_session","sessionPath":"/tmp/.../sessions/<timestamp>_<id>.jsonl"}
{"id":"r5","type":"get_messages"}
```

Respuestas reales [PI-E1] [PI-E3]:

```json
{"id":"r2","type":"response","command":"prompt","success":true,"data":{"disposition":"handled"}}
{"id":"r3","type":"response","command":"prompt","success":true,"data":{"disposition":"started"}}
{"id":"r4","type":"response","command":"switch_session","success":true,"data":{"cancelled":false}}
```

`disposition` observado: `handled` (comando de control) y `started` (prompt de
agente). Pi documenta también `queued`. **No significa finalización** [PI-E1].

**OBSERVED:** el canal de escritorio de Gentle Pi usa un prompt de control:

```json
{"id":"d1","type":"prompt","message":"/gentle:desktop {\"schema\":\"gentle-shell.desktop-rpc/v1\",\"requestId\":\"d1\",\"method\":\"capabilities\",\"params\":{}}"}
```

La respuesta vuelve como `extension_ui_request` con
`method:"setWidget"`, `widgetKey:"gentle-desktop-rpc"` y un JSON en
`widgetLines`, correlacionado por `requestId` propio [PI-E1]. La ayuda anuncia
métodos como `list_sessions`, `switch_session`, `get_permission_mode` y
`set_permission_mode`; **solo `capabilities` fue invocado** y su lista de métodos
no se conservó completa. El resto es **UNTESTED** [PI-E1].

## 3. Inicio, continuación, recuperación y cierre de sesión

| Operación | Estado | Resultado |
|---|---|---|
| Inicio | VERIFIED | `get_state` devuelve `sessionId` y `sessionFile`; con `--session-dir`, el archivo está bajo ese directorio [PI-E3] |
| Sesión efímera | OBSERVED | `--no-session` existe como flag; usado en probes exploratorios [PI-E5] |
| Continuación | VERIFIED | `switch_session` con la ruta devuelve `cancelled:false`; `get_messages` devuelve `messages` con system/user/assistant [PI-E3] |
| Otra forma de abrir | UNTESTED | `pi --session <path|id>`, `--continue`, `--resume` y `--session-id` aparecen en `pi --help` [PI-E5] |
| Nueva sesión en RPC | UNTESTED | `new_session` documentado, no ejecutado |
| Cierre explícito de sesión | UNTESTED | No hay comando `close`/`stop` probado; `abort` y `clear_queue` tampoco |
| Cierre del proceso | VERIFIED | Cerrar stdin termina el proceso con `exit_code:0` [PI-E4] |

La recuperación es de **historial**, no de un turno en vuelo: al cerrar el
proceso durante un `sleep 20 && echo ...`, el archivo de sesión quedó escrito
pero la orden tardía no se ejecutó (`late_file_created:false`) [PI-E4].

## 4. Envío de prompts y recepción de respuestas

**VERIFIED:** un prompt produce, en este orden [PI-E1]:

1. `response` con `disposition:"started"`.
2. `agent_start`, `turn_start`, `message_start/end`.
3. Uno o más `message_update` con `assistantMessageEvent.type:"text_delta"` y
   campo `delta`, por ejemplo `persist`, `ed`, `-p`, `0` (fragmentos reales del
   caso de resume).
4. `tool_execution_start/update/end` si el agente usa herramientas.
5. `turn_end`; puede aparecer otro `turn_start` porque el agente continuó.
6. `agent_settled` como última señal.

Ejemplo real de streaming [PI-E1]:

```json
{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"persist"}}
```

El texto completo también está disponible en `message_end` / `get_messages`;
el driver no debe reconstruirlo solo por deltas si puede leer el mensaje final.

## 5. Eventos de streaming y detección de fin de turno

**VERIFIED:** `agent_settled` es la señal de «no hay más trabajo automático». Un
`turn_end` **no** basta: en el chat P0 hubo `turn_end → turn_start → turn_end →
agent_settled` [PI-E1]. El probe espera `agent_settled` con timeout de 240 s.

Eventos de presentación ignorados por el probe: `setStatus`, `setWidget`,
`notify`. No son eventos de dominio [PI-E1].

## 6. Herramientas, permisos interactivos y cancelación

**VERIFIED:** herramienta `bash` observada con [PI-E1]:

```json
{"type":"tool_execution_start","toolName":"bash","args":{"command":"echo gf-pi > p0-pi.txt","timeout":30}}
{"type":"tool_execution_end","toolName":"bash","result":{"content":[{"type":"text","text":"(no output)"}],"structuredContent":{"exit_code":0}},"isError":false}
```

**OBSERVED en exploración manual antes de la automatización:** `rm victim.txt`
(sin `-r`) se ejecutó sin diálogo; `rm -r victimdir` sí disparó el prompt. El
probe automatizado solo cubre el segundo caso [PI-E2].

**VERIFIED:** el permiso no es nativo de Pi sino de la extensión Gentle Pi, y
llega como UI request [PI-E2]:

```json
{"type":"extension_ui_request","id":"<uuid>","method":"select",
 "title":"Allow recognized data-loss command?\nrm -r victimdir",
 "options":["Allow once","Allow for this session","Deny"]}
```

La respuesta correcta usa el mismo `id` [PI-E2]:

```json
{"type":"extension_ui_response","id":"<uuid>","value":"Deny"}
```

Tras denegar, `tool_execution_end` trae
`"Gentle AI safety policy blocked the command because it was not confirmed."`,
`isError:true`, y el directorio permanece intacto [PI-E2].

**OBSERVED:** el modo de permisos inicial de la máquina era `SUPER-YOLO`; el
probe fuerza `/gentle:mode normal` al inicio y la captura confirma
`setStatus statusKey:"gentle:mode", statusText:"Permissions: NORMAL"` [PI-E1].
Un driver no debe depender del modo ambiental.

**UNTESTED:** `Allow once`, `Allow for this session`, `confirm`, `input`,
`editor`, `abort`, `clear_queue`, `steer` y `follow_up` aparecen en ayuda o en el
código de la extensión pero no tienen aserción en P0 [PI-E1] [PI-E5].

## 7. Persistencia, reconexión y comportamiento al cerrar procesos

- **VERIFIED:** el archivo de sesión es JSONL bajo `--session-dir`, con nombre
  `<timestamp>_<uuid>.jsonl`. `get_state` expone la ruta [PI-E3].
- **VERIFIED:** un segundo proceso con `switch_session` recupera el historial
  completo (el mensaje asistido `persisted-p0` estaba presente) [PI-E3].
- **VERIFIED:** cerrar stdin → exit 0, sin terminar la orden en vuelo [PI-E4].
- **OBSERVED:** el archivo de sesión de un proceso abortado queda con historial
  parcial [PI-E4].
- **UNTESTED:** SIGKILL, caída del host, comportamiento con `--continue` y
  reconstrucción de un turno interrumpido [PI-E3] [PI-E4].

Consecuencia para Fleet: la desconexión del cliente no puede confundirse con el
fin de la sesión; hace falta un Runner que posea el proceso (P0-D3).

## 8. Autenticación y manejo seguro de credenciales

- **VERIFIED:** el perfil aislado es `<HOME>/.gentle-shell/agent` y contiene
  `auth.json` con credencial OAuth `openai-codex`; el probe **no abre** ese
  archivo [PI-E5].
- **OBSERVED:** `gentle-shell auth check --provider openai-codex --json` devuelve
  `{"status":"ready",...,"authType":"oauth"}` en la máquina de pruebas.
- **VERIFIED:** el probe usa el login nativo del perfil sin copiar secretos, y la
  anonimización reemplaza `<HOME>` antes de escribir fixtures [PI-E1].
- Ninguna credencial, token ni `accountId` aparece en las capturas.

## 9. Limitaciones y comportamientos no comprobados

| Clase | Detalle |
|---|---|
| VERIFIED | El proceso muere con stdin; no hay supervivencia al cliente [PI-E4] |
| VERIFIED | `turn_end` no equivale a fin de trabajo [PI-E1] |
| OBSERVED | El modo de permisos depende de configuración previa del usuario [PI-E1] |
| UNTESTED | `list_sessions` por RPC, `new_session`, `abort`, `steer`, `follow_up`, `Allow once`, `confirm`, `input`, `editor` |
| UNTESTED | `pi --session/--continue/--resume` como alternativa de resume |
| UNTESTED | Windows, SIGKILL, caída de host, turnos interrumpidos |
| UNSUPPORTED | No existe un servidor Gentle Shell/Pi al que conectarse en la versión probada; la conexión es el proceso |

## 10. Referencias

- Programa: [`spikes/p0/pi/probe_pi.py`](../../spikes/p0/pi/probe_pi.py)
- Captura: [`spikes/p0/fixtures/gentle-shell-pi.jsonl`](../../spikes/p0/fixtures/gentle-shell-pi.jsonl)
- Comandos: [`spikes/p0/fixtures/command-outputs.txt`](../../spikes/p0/fixtures/command-outputs.txt)
- Contrato de canal de escritorio leído durante P0:
  `~/.local/share/pnpm/global/.../gentle-pi/docs/desktop-rpc.md` (fuente local,
  no incluida en el repo; **solo sirve como pista**, no como evidencia).
- Comparación: [`PROVIDER_COMPARISON.md`](PROVIDER_COMPARISON.md)
