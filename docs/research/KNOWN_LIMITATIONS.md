# Limitaciones conocidas del research P0

Inventario único de **fallos reproducidos**, **funcionalidades no soportadas** y
**aspectos todavía no investigados**. Evita duplicar los detalles de los
documentos por motor: aquí se clasifican y se enlaza la evidencia.

Plataforma: **Linux x86_64**. Versiones: Gentle Shell `4.8.0` / Pi `1.5.2`,
Codex `0.171.0`, OpenCode `2.3.2`. Windows no fue probado en ningún caso.

Etiquetas: **VERIFIED** (aserción automática o salida de comando conservada),
**OBSERVED** (visto en ejecución real sin aserción), **UNTESTED** (sin prueba),
**UNSUPPORTED** (rechazado o ausente en la versión probada). Definición completa
en [`PROVIDER_COMPARISON.md`](PROVIDER_COMPARISON.md).

## A. Fallos reproducidos y comportamientos adversos

| # | Motor | Clase | Comportamiento | Impacto para P1/P2 | Evidencia |
|---|---|---|---|---|---|
| A1 | Pi | VERIFIED | Cerrar stdin mata el proceso con exit 0 y aborta la orden en vuelo; el archivo tardío no se creó | Sin Runner dueño del proceso no se cumple «desconectar cliente ≠ detener sesión» | [PI.md §7](PI.md#7-persistencia-reconexión-y-comportamiento-al-cerrar-procesos), `gentle-shell-pi.jsonl` `disconnect.result` |
| A2 | Pi | VERIFIED | `turn_end` puede ir seguido de otro `turn_start`; solo `agent_settled` cierra el trabajo | El driver debe esperar `agent_settled`, no el primer `turn_end` | [PI.md §5](PI.md#5-eventos-de-streaming-y-detección-de-fin-de-turno), `chat.lifecycle` |
| A3 | Pi | OBSERVED | El modo de permisos ambiental era `SUPER-YOLO`; el probe lo fuerza a `normal` | El driver debe fijar el modo por sesión, no heredar configuración | [PI.md §6](PI.md#6-herramientas-permisos-interactivos-y-cancelación), `chat.response`/`chat.ui_request` |
| A4 | Codex | VERIFIED | `CODEX_HOME` sin `auth.json`: `initialize` y `thread/start` pasan, pero el turno recibe 401, reintenta y termina `failed` | El Runner debe enlazar/heredar la autenticación sin copiar secretos | [CODEX.md §8](CODEX.md#8-autenticación-y-manejo-seguro-de-credenciales), `codex-missing-auth.jsonl` |
| A5 | Codex | VERIFIED | `thread/read includeTurns:true` emite `deprecationNotice` (dos redacciones distintas) | Usar `excludeTurns:true` + paginación desde el primer driver | [CODEX.md §3](CODEX.md#3-inicio-continuación-recuperación-y-cierre-de-sesión), `codex.jsonl` 50–51 |
| A6 | Codex | VERIFIED | Los requests del servidor llegan mientras se esperan respuestas propias | Un cliente JSON-RPC secuencial se bloquea; hace falta demultiplexado | [CODEX.md §6](CODEX.md#6-herramientas-permisos-interactivos-y-cancelación), `notification.item/commandExecution/requestApproval` |
| A7 | OpenCode | OBSERVED | Una `permission.asked` de una sesión abandonada apareció en el stream de otra conexión minutos después | P2 debe reconciliar aprobaciones pendientes al reconectar | [OPENCODE.md §6](OPENCODE.md#6-herramientas-permisos-interactivos-y-cancelación), `opencode-orphan-permission.jsonl` |
| A8 | OpenCode | OBSERVED | Tras `reject`: `session.tool.failed` y `session.execution.interrupted` con `reason:"shutdown"` | No interpretar `reason` sin prueba; el servidor siguió operando | [OPENCODE.md §5](OPENCODE.md#5-eventos-de-streaming-y-detección-de-fin-de-turno), `opencode.jsonl` 91 y 94 |
| A9 | OpenCode | VERIFIED | `GET /api/session` lista sesiones de todos los directorios del usuario | Filtrar por `location.directory` antes de publicar al cliente | [OPENCODE.md §3](OPENCODE.md#3-inicio-continuación-recuperación-y-cierre-de-sesión), probe `probe_auto` |
| A10 | OpenCode | OBSERVED | `shell.created` pone el `sessionID` en `data.info.metadata`, no en `data.sessionID` | El filtrado del stream debe mirar ambos sitios | [OPENCODE.md §4](OPENCODE.md#4-envío-de-prompts-y-recepción-de-respuestas), `opencode.jsonl` `event.shell.created` |

No se registraron pérdidas de eventos, duplicación de prompts ni decisiones de
permiso duplicadas en los escenarios probados. **No** es evidencia de que no
puedan ocurrir: esos casos pertenecen a las pruebas de falla de P2/P3.

## B. Funcionalidades no soportadas o ausentes en la versión probada

| Motor | Elemento | Clase | Observación | Evidencia |
|---|---|---|---|---|
| Pi | Servidor al que conectarse | UNSUPPORTED | La conexión es el proceso stdio; no hay attach | [PI.md §1](PI.md#1-comandos-reales-de-ejecución-y-conexión) |
| Pi | `list_sessions` por RPC | OBSERVED (ausencia) | No aparece en la superficie RPC usada; sí se anuncia en el canal de escritorio, sin probar | [PI.md §2](PI.md#2-protocolo-nativo-y-ejemplos) |
| Pi | Cierre limpio de sesión vía RPC | UNTESTED / no observado | El probe solo cerró stdin | [PI.md §3](PI.md#3-inicio-continuación-recuperación-y-cierre-de-sesión) |
| Codex | API HTTP pública | UNSUPPORTED en 0.171.0 | Solo `app-server` por stdio | [CODEX.md §1](CODEX.md#1-comandos-reales-de-ejecución-y-conexión) |
| Codex | Historial hidratado en `thread/read` | UNSUPPORTED (deprecado) | El servidor pide paginar | [CODEX.md §3](CODEX.md#3-inicio-continuación-recuperación-y-cierre-de-sesión) |
| OpenCode | `opencode serve --standalone` | UNSUPPORTED en 2.3.2 | *Unrecognized flag*, aunque la ayuda de `--print-logs` lo menciona | [OPENCODE.md §1](OPENCODE.md#1-comandos-reales-de-ejecución-y-conexión), `command-outputs.txt` |
| OpenCode | Chat por salida TUI / ANSI | UNSUPPORTED por decisión P0 | P0 usa solo la API HTTP estructurada | [OPENCODE.md §9](OPENCODE.md#9-limitaciones-y-comportamientos-no-comprobados) |
| Todos | Paridad de eventos, permisos y resume | UNSUPPORTED como supuesto | Cada motor tiene vocabulario propio; `AgentDriver` debe declarar `Unsupported` cuando falte | [PROVIDER_COMPARISON.md](PROVIDER_COMPARISON.md#matriz-de-capacidades-verificadas) |

## C. Aspectos todavía no investigados (UNTESTED)

| Ámbito | Pendiente | Por qué bloquea / dónde se prueba |
|---|---|---|
| Plataforma | Windows en los tres motores; supervivencia de runner detached (Job Objects/handles) | Bloquea la promesa multiplataforma; P3 |
| Procesos | SIGKILL de fleetd, muerte de runner, caída y reinicio de host | Bloquea cualquier garantía de recuperación; P3 |
| Pi | `abort`, `steer`, `follow_up`, `new_session`, `Allow once`, `confirm`, `input`, `editor` | Cancelación y permisos completos; P2/P4 |
| Pi | Métodos del canal `/gentle:desktop` distintos de `capabilities` | Posible att/portable listado de sesiones; P4 |
| Pi | `pi --session`, `--continue`, `--resume` como alternativas de resume | P4 |
| Codex | `decline`, `cancel`, `acceptForSession`, enmiendas de execpolicy/network, file-change y user-input approvals | Vocabulario completo de decisiones; P2/P4 |
| Codex | `turn/interrupt`, `thread/items/list`, reconexión con turno en vuelo y keepalive | Cancelación y recuperación; P2/P3 |
| OpenCode | `always`, `session.permission.create`, `/interrupt`, `/compact`, `/fork`, filtro `?types=` | Cancelación y permisos avanzados; P2/P4 |
| OpenCode | Replay desde `durable.seq/version`, backpressure y cliente SSE lento | Cursor y gap de timeline; P2 |
| Todos | Reenvío seguro tras timeout, aprobación duplicada, prompt duplicado, dos clientes en la misma sesión | Criterios de P1/P2; hoy solo hay análisis en `LIFECYCLE.md` |
| Seguridad | Rotación/expiración de credenciales, permisos de `service.json`, secretos en logs del proveedor | P7 |

## Regla de actualización

Toda transición de **UNTESTED** a **VERIFIED** debe añadir el probe o la captura
que la respalda y referenciarla desde el documento del motor correspondiente.
No se promueve un comportamiento a partir de documentación externa sin ejecutar
el binario fijado.
