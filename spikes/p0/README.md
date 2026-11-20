# Spikes P0 — compatibilidad real de los tres motores

Programas ejecutables que comprueban, contra los binarios instalados, lo que
Gentle Fleet necesita saber antes de fijar el contrato de P1. No forman parte del
producto: no hay daemon, base de datos ni crates aquí.

## Versiones probadas

| Motor | Comando | Versión |
|---|---|---|
| Gentle Shell | `gentle-shell --version` | `gentle-shell 4.8.0` + `pi 1.5.2` (paquete `gentle-pi 4.8.0-main.652552becbb6`) |
| Codex | `codex --version` | codex-cli 0.171.0 |
| OpenCode | `opencode --version` | 2.3.2 |

## Ejecución

Los tres necesitan sesión real iniciada en la máquina (Gentle Shell usa
`openai-codex`, Codex usa ChatGPT y OpenCode usa el modelo por defecto
`opencode/tidepool-alpha-free`, que no requiere credencial). **Son opt-in**:
nunca se ejecutan automáticamente en CI. Ningún script lee, imprime o guarda
credenciales.

```bash
# Gentle Shell / Pi: chat, permiso denegado, resume y desconexión
python3 spikes/p0/pi/probe_pi.py --capture /tmp/pi.jsonl

# Codex: initialize, turn, aprobación de comando y resume entre procesos
python3 spikes/p0/codex/probe_codex.py --capture /tmp/codex.jsonl

# OpenCode: servidor aislado, SSE, permisos once/reject y persistencia al reiniciar
python3 spikes/p0/opencode/probe_opencode.py --capture /tmp/opencode.jsonl

# OpenCode: comprobar el servicio de usuario ya en ejecución (solo lectura)
python3 spikes/p0/opencode/probe_opencode.py --attach
```

Cada probe termina con `PASS` (salida 0) o falla con un mensaje explícito. Los
tres escriben en directorios temporales y limpian las sesiones efímeras que
crean:

- Pi usa `--session-dir` aislado.
- Codex usa un `CODEX_HOME` temporal con un *symlink* a `auth.json` (no una copia).
- OpenCode borra las sesiones `gf-p0-*` que crea. Su base de datos de usuario es
  global; el probe no la modifica directamente.

Variables opcionales: `GENTLE_SHELL_BIN`, `CODEX_BIN`, `OPENCODE_BIN`,
`GF_CODEX_HOME` (usa ese `CODEX_HOME` sin symlink), `GF_KEEP_WORKDIR` (conserva
el workspace temporal), `XDG_STATE_HOME` (para `--attach`).

## Qué se comprobó

Ver [`docs/P0_FINDINGS.md`](../../docs/P0_FINDINGS.md) para el resumen y
[`docs/research/`](../../docs/research/) para el detalle por motor con etiquetas
de evidencia. Resumen:

| Capacidad | Gentle Shell / Pi | Codex app-server | OpenCode v2 API |
|---|---|---|---|
| Transporte | JSONL por stdin/stdout de un proceso | JSON-RPC 2.0 por stdin/stdout | HTTP + SSE sobre loopback |
| IDs nativos | `sessionFile`, `sessionId` | `threadId`, `turnId`, `itemId` | `sessionID`, `messageID`, `permissionID` |
| Streaming | `message_update` → `text_delta` | `item/agentMessage/delta` | `session.text.delta` (+ `reasoning`, `tool.input`) |
| Herramientas | `tool_execution_start/update/end` | `item/started`, `item/completed` con `commandExecution` | `session.tool.input.*`, `tool.called`, `tool.success/failed` |
| Permiso interactivo | `extension_ui_request` (`select`/`confirm`/`input`) + `extension_ui_response` | request JSON-RPC del servidor `item/*/requestApproval` + `result.decision` | `permission.asked` + `POST .../permission/{id}/reply` |
| Fin de turno | `agent_settled` | `turn/completed` | `session.execution.succeeded/failed/interrupted` |
| Persistencia | archivo `.jsonl` por sesión | `rollout-*.jsonl` en `CODEX_HOME/sessions` | base de datos SQLite del usuario |
| Resume | `switch_session` con ruta | `thread/resume` con ID | `GET /api/session/{id}` (sin reconstruir) |
| Cliente desconectado | **mata al proceso** (stdio) | el proceso sigue vivo; el request de aprobación queda pendiente | la sesión sigue; la aprobación queda pendiente |
| Servidor compartido | no | no | sí: `opencode serve` / servicio de usuario con password local |

## Fixtures

[`fixtures/`](fixtures/) contiene las capturas anonimizadas de las ejecuciones
reales: tres capturas de probe, dos capturas manuales complementarias, un
subconjunto de esquemas y la salida anonimizada de los comandos de versión. Se
regeneran con `--capture` o con el comando indicado en cada documento de
`docs/research/`. No sustituyen a una prueba contra el motor real; al cambiar de
versión hay que volver a ejecutar los probes.
