# P4 — Matriz de proveedores y capacidades por versión

**Estado:** investigación P4.0 completada (22/11/2026). **Rama:** `feature/p4-multi-provider`.
**Plataforma de referencia:** Linux x86_64 (P4-F16). Windows/macOS quedan para P7.
**Referencia funcional:** `docs/P4_SPEC.md` (18 decisiones ratificadas, no modificadas).

## Convenciones de evidencia

| Marca | Significado |
|---|---|
| **VERIFIED** | Comprobado con prueba reproducible en esta sesión o en fixture P0 con comando/script identificable. |
| **OBSERVED** | Observado en captura real, sin prueba completa o sin repetir en la versión instalada. |
| **UNTESTED** | No probado en esta sesión; el protocolo puede existir, pero falta evidencia. |
| **UNSUPPORTED** | No disponible en esa versión/configuración, o el productor lo documenta como inexistente. |

Una celda vacía de capacidad **no** se rellena con "sí" por analogía entre proveedores.

## 1. Versiones realmente instaladas

| Proveedor | Ejecutable resuelto | Versión detectada | Evidencia |
|---|---|---|---|
| OpenCode (referencia) | `~/.local/share/mise/installs/opencode/2.3.2/node_modules/.bin/opencode` | `opencode v2.3.2` | `opencode --version`; `docs/research/OPENCODE.md` |
| Codex | `~/.local/share/mise/installs/codex/0.171.0/bin/codex` (wrapper mise) | `codex-cli 0.171.0` | `codex --version`; App Server `[experimental]`; esquema generado |
| Pi estándar | `~/.local/share/mise/installs/pi/1.6.0/pi/pi` | **`pi 1.6.0`** | `pi --version`; `readlink -f` de `command -v pi` |
| Gentle Shell | `~/.local/bin/gentle-shell` → paquete global `gentle-pi 4.8.0-main.652552becbb6` | `gentle-shell 4.8.0-main.652552becbb6`, **envuelve Pi 1.5.2** | `gentle-shell --version`; `package.json` del paquete pnpm |

**Deriva detectada (importante para P4.2/P4.3):** conviven dos binarios Pi distintos. La documentación P0 fija `pi 1.5.2` porque lo que se probó fue el Pi embebido en Gentle Shell. El `pi` estándar del PATH hoy es **1.6.0**. P4.2 apunta a 1.6.0; P4.3 apunta al Pi 1.5.2 que trae Gentle Shell. `docs/research/PI.md`, `docs/research/PROVIDER_COMPARISON.md` y `docs/research/KNOWN_LIMITATIONS.md` describen 1.5.2; proponemos añadir una nota de versión por corte sin borrar la evidencia P0 (§5).

Codex tiene además 0.169.0 y 0.170.0 instalados; el activo es 0.171.0. `~/.codex/version.json` está desactualizado (`latest_version 0.170.0`): **no usar ese fichero como detección de versión**, usar `codex --version`.

## 2. Transporte y forma de protocolo

| Proveedor | Transporte | Formato | Descubrimiento | Evidencia |
|---|---|---|---|---|
| OpenCode 2.3.2 | Servicio HTTP local del usuario (attach) o `serve` propio; SSE `/api/event` | HTTP JSON + SSE | `/api/info`, `/api/event`, `service.json` | VERIFIED P1/P2, DRIVERS.md |
| Codex 0.171.0 | App Server sobre stdio por defecto; opcional `unix://`, `ws://IP:PORT`, `off`; `--listen`, `--ws-auth capability-token\|signed-bearer-token` | JSON-RPC 2.0 por línea, requests iniciados por servidor con `id` numérico | `codex app-server generate-json-schema` → 714 definiciones v2, 110 `ClientRequest`, 10 `ServerRequest`, 87 `ServerNotification` (sin `--experimental`); 883/189 con él | VERIFIED (esquema + handshake con `CODEX_HOME` temporal) |
| Pi 1.6.0 | Proceso propio, stdin/stdout; la conexión **es** el proceso | JSONL estricto; correlación por `id` en `{"type":"response",...}` | `get_commands`; docs empaquetadas `docs/rpc-commands.md`, `docs/json.md`, `docs/rpc-extension-ui.md`, `docs/session-format.md` | VERIFIED |
| Gentle Shell 4.8.0 | Proceso propio Pi RPC con `GENTLE_SHELL_INTERACTIVE_HOST=1`; Desktop RPC **dentro** de Extension UI | `prompt` con `/gentle:desktop <JSON>` → respuesta `disposition:"handled"`; respuesta de dominio en `extension_ui_request` `setWidget`, `widgetKey:"gentle-desktop-rpc"`, `widgetLines[0]` = JSON con `schema:"gentle-shell.desktop-rpc/v1"` | `capabilities` negocia métodos/eventos por sesión | VERIFIED (23 métodos y 8 eventos anunciados en sesión efímera; noveno evento `session_snapshot` condicional) |

Ningún proveedor nuevo expone un servidor HTTP público. Codex ofrece WS/unix como transportes opcionales del App Server; no son necesarios para P4.1.

## 3. Matriz de capacidades

Leyenda: ✅ VERIFIED · ◐ OBSERVED · ○ UNTESTED · ✗ UNSUPPORTED · — no aplica.

| Capacidad Fleet / proveedor | OpenCode 2.3.2 | Codex 0.171.0 | Pi 1.6.0 | Gentle Shell 4.8.0 (Pi 1.5.2) |
|---|---|---|---|---|
| Chat estructurado y prompt | ✅ | ✅ (`turn/start`, handshake verificado; turno con modelo real OBSERVED en P0) | ✅ (`prompt`, sin llamada real en 1.6.0: ○ para respuesta de modelo) | ◐ (P0 con modelo real; 1.6.0 sin modelo configurado) |
| Streaming de texto | ✅ SSE | ◐ `item/agentMessage/delta` (esquema + P0) | ◐ `message_update` con `text_delta` (P0 1.5.2) | ◐ (P0 1.5.2) |
| Mensaje final canónico | ✅ | ◐ `item/completed`, `turn/completed` | ◐ `message_end` (documentado; P0 `message_update`) | ◐ |
| Eventos de herramienta | ✅ | ◐ `item/started`/`item/completed` + P0 `exec` | ◐ `tool_execution_start/update/end` (P0) | ◐ (P0 `bash`) |
| Fin real de turno | ✅ | ◐ `turn/completed` | ◐ **`agent_settled`**; `agent_end` NO basta | ◐ `agent_settled` (P0) |
| IDs nativos de turno | ✅ `sessionID` opaco | ✅ `threadId`/`turnId` (devueltos por `thread/start`, `turn/start`) | ✅ `sessionId` UUID (existe incluso con `--no-session`) | ✅ `sessionId` + `requestId` Desktop RPC |
| Cancelación de turno | ✗ declarado `cancel_turn:false` en el driver actual | ✅ `turn/interrupt{threadId,turnId}` (esquema VERIFIED; ejecución ○) | ◐ `abort` (documentado, ○ en 1.6.0) | ◐ `abort` (○) |
| Interrupción/steer en caliente | — | ◐ `turn/steer{threadId,expectedTurnId,input}` (esquema VERIFIED; ejecución ○) | ◐ `steer` (○) | ◐ `steer` (○) |
| Aprobaciones interactivas | ✅ `permission.asked` → `once\|always\|reject` | ✅ requests JSON-RPC del servidor, payload de decisión tipado (P0 `accept` VERIFIED; variantes ○) | ✗ en Pi base (sin extensiones); ◐ Extension UI genérico `select/confirm/input/editor` | ✅ `select` de permiso + 23 métodos Desktop RPC (solo lectura ejecutada) |
| Aprobaciones parametrizadas | ✗ tres opciones fijas | ✅ `acceptWithExecpolicyAmendment`, `applyNetworkPolicyAmendment`, `requestUserInput.answers`, `scope:turn\|session` (esquema VERIFIED) | ◐ `input`/`editor` devuelven valor libre (documentado) | ◐ opciones nativas `Allow once` / `Allow for this session` / `Deny` (P0) |
| Confirmación de entrega de decisión | ✅ `permission.replied` | ✅ `serverRequest/resolved{threadId,requestId}` | ◐ `extension_ui_response` (P0) | ◐ `extension_ui_response` (P0) |
| Listado de modelos | — | ✅ `model/list` → 11 modelos; default `gpt-6.2-sol` (VERIFIED) | ◐ `get_available_models` → `{"models":[]}` sin credenciales (VERIFIED respuesta, sin modelos reales) | ◐ `get_config{store:"models"}` → mapa agent→modelo (VERIFIED, sin secretos) |
| Esfuerzo de razonamiento | — | ✅ `effort` string libre en `turn/start`/`thread/start`; effort por modelo `low..ultra`; `ReasoningEffort` **no es enum** (VERIFIED esquema) | ◐ `get_available_thinking_levels` → `["off"]`; `set_thinking_level` documentado, efecto ○ | ◐ niveles `off/minimal/low/medium/high/xhigh/max` documentados, efecto ○ |
| Cambio de modelo/esfuerzo **durante** sesión | — | ◐ `thread/settings/update`, `turn/settings/update` existen pero **`requires experimentalApi capability`** (VERIFIED rechazo con gate false); per-turn en `turn/start` sí disponible | ◐ `set_model`/`cycle_model`/`set_thinking_level` documentados, efecto ○ | ◐ `set_config`/`apply_profile` (no ejecutados: modificarían perfil real) |
| Modos de permiso nativos | — | ◐ `approvalPolicy` / `sandboxPolicy` por turno y thread; `collaborationMode/list` experimental | ✗ | ✅ `normal/auto/yolo/super-yolo`; `get_permission_mode` → `{mode:"super-yolo",default_mode:"super-yolo"}` (VERIFIED) |
| Perfiles | — | ✗ | ✗ | ✅ `gentle-pi.session-profile/v1`; `get_config{store:"profiles"}`; `apply_profile` (no ejecutado) |
| Enumeración de sesiones nativas | — | ✅ `thread/list` con cursor y filtros; `thread/loaded/list` indica carga en memoria (VERIFIED) | ◐ sin comando de lista global; sesiones = ficheros en `--session-dir` | ◐ `list_sessions` anunciado; en `--no-session` responde `sessions_unavailable` (VERIFIED) |
| Historial estructurado | ✅ `/api` | ✅ `thread/read`, `thread/turns/list` paginado, `thread/items/list` (en esquema; runtime `not supported yet` en hilo no materializado) | ◐ `get_messages`, `get_entries` (cursor opaco `since`), `get_tree`, `get_session_stats`, `session-format.md` v3 | ◐ `get_messages` + `get_entries`; `session_snapshot` condicional |
| Reanudar historial entre procesos | ✅ | ✅ `thread/resume{threadId,excludeTurns:true}`; P0 verificado tras turno real | ◐ `switch_session` con fichero; en sesión efímera 1.6.0 inconcluso (no reclamar carga) | ◐ `switch_session` (P0 con fichero real) |
| Recuperar turno en vuelo tras reinicio | ✅ vía runner P3 | ✗ **no existe**: `turn/steer` exige turno activo en el mismo proceso; nuevo proceso solo resume historial | ✗ sin servidor; depends del runner poseedor del proceso | ✗ sin servidor; depende del runner poseedor del proceso |
| Evidencia de actividad externa | — | ✅ `thread/status/changed` con `activeFlags: waitingOnApproval\|waitingOnUserInput`; `ThreadStatus.active` | ○ | ○ (feed `agents_update`, `session_snapshot` condicional) |
| Runner persistente P3 (daemon caído) | ✅ | ◐ compatible en principio: el runner posee el proceso App Server y el spool; sin prueba real aún | ◐ compatible en principio: proceso propio; sin prueba | ◐ compatible en principio; sin prueba |
| Configuración/credenciales nativas preservadas | ✅ | ✅ `CODEX_HOME` real sin tocar; la prueba usó directorio temporal vacío | ✅ `--session-dir` temporal; no se copiaron credenciales | ✅ no se llamaron `set_config`/`apply_profile` sobre el perfil real |
| Subagentes / delegación | — | — | ✗ (solo extension de ejemplo) | ◐ `list_subagents`, `cancel_subagent`, `get_subagent_thread` + tools `subagent_*` — **P4-F05: documentar, NO implementar** |

## 4. Implicaciones directas para los cortes

**Criterio de evidencia:** el inventario anterior se refiere al ejecutable local 4.8.0; la documentación actual del productor puede incluir métodos posteriores. No trasladar un inventario nuevo a una versión anterior sin ejecutar `capabilities` en ella. Pi 1.6.0 sin modelo configurado impide demostrar un chat real hasta que se habilite un modelo de prueba, sin alterar credenciales personales.

- **P4.1 Codex:** es el proveedor con mejor cobertura estructurada. Riesgo principal: confundir `thread/resume` (historial) con recuperación de turno en vuelo (inexistente). Segundo riesgo: `availableDecisions` aparece en runtime pero no en el esquema generado (`additionalProperties` no restringido). Tercero: `thread/items/list` está en esquema pero el runtime lo rechaza en hilo no materializado; hay que esperar al primer mensaje del usuario.
- **P4.2 Pi estándar:** el driver debe poseer el proceso y tratar `agent_settled` como fin de trabajo. `get_available_models` devuelve lista vacía sin login: la matriz debe publicar "consulta VERIFIED, modelos UNSUPPORTED en este entorno", no fingir modelos. `--no-session` todavía asigna `sessionId`: no usar esa presencia como prueba de sesión persistida.
- **P4.3 Gentle Shell:** proveedor distinto de Pi (P4-F02). `session_snapshot` **no** aparece en la negociación de una sesión efímera; Fleet debe negociar por sesión y no hardcodear el evento. Los 23 métodos se documentan pero solo se ejecutaron los de lectura. El productor documenta nueve eventos en total, de los cuales esta sesión efímera anunció ocho; `session_snapshot` requiere feed activo. Ningún método modificador debe ejecutarse sobre el perfil real del usuario.
- **P4.4:** la suite debe ser matrix-driven por capacidades y ejecutar las mismas aserciones donde la capacidad exista; una celda ✗ es un resultado explícito, no un fallo de test.

## 5. Contradicciones con documentación anterior y actualización propuesta

1. **Deriva Pi (P0 vs P4.0):** `docs/research/PI.md:12-13`, `PROVIDER_COMPARISON.md:24` y `KNOWN_LIMITATIONS.md` fijan `pi 1.5.2`. Sigue siendo correcto para el Pi embebido en Gentle Shell; es incompleto para el `pi` del PATH, que es 1.6.0. **Propuesta:** añadir a esos documentos una nota `P4.0: Pi estándar 1.6.0 (distinto del Pi 1.5.2 embebido en Gentle Shell)` sin borrar las líneas P0.
2. **`PI.md:84`** ("capabilities invocada pero no se conservó la lista") — resuelto en P4.0: inventario de 23 métodos y 8 eventos anunciados en sesión efímera capturado; el noveno, `session_snapshot`, está documentado como condicional. Proponer nota de resolución.
3. **`PI.md:94-95` / `KNOWN_LIMITATIONS.md`** (`new_session` UNTESTED, `list_sessions` "anunciado pero sin probar") — `new_session` VERIFIED en 1.6.0; `list_sessions` probado con `sessions_unavailable` en sesión efímera. `abort`, `steer`, `follow_up` siguen UNTESTED.
4. **Extension UI:** `PI.md:80-84` la describe como no nativa de Pi. Correcto para el diálogo *Gentle*, incorrecto si se lee como ausencia de Extension UI: Pi 1.6.0 documenta `rpc-extension-ui.md` con `select/confirm/input/editor`.
5. **Comandos nuevos en 1.6.0:** `get_entries`, `get_tree`, `get_last_assistant_text`, `bash`, `export_html`, `set_*_mode`, compact/retry. La investigación debe citarse por versión, no globalmente.
6. **Sin cambio en P4_SPEC:** ninguna de estas contradicciones toca las 18 decisiones funcionales. No se modifica `docs/P4_SPEC.md`.


## 6. Actualización P4.1 — celdas verificadas con driver real

Tras la implementación (`docs/P4_1_RESULTS.md`) las celdas de Codex 0.171.0 pasan a **VERIFIED** con evidencia reproducible: streaming y mensajes finales (`e2e_p4_codex_chat.sh`), herramientas (`item/started`/`item/completed`), aprobaciones parametrizadas con `option_id` y argumentos tipados (`e2e_p4_codex_approval.sh`), `turn/interrupt` (`e2e_p4_codex_interrupt.sh`), modelos/esfuerzos descubiertos (`e2e_p4_codex_controls.sh`), enumeración de sesiones e importación de historial (`e2e_p4_codex_discovery.sh`), supervivencia a SIGKILL de `fleetd` (`e2e_p4_codex_survival.sh`) y concurrencia con OpenCode (`e2e_p4_codex_concurrency.sh`).

Siguen **UNSUPPORTED/UNTESTED**: adopción con escritura (sin exclusividad verificable), recuperación de turno en vuelo entre procesos, `turn/steer` y cambios de modelo/esfuerzo dentro de un turno activo (requieren `experimentalApi`, no habilitado). No se ha probado Windows/macOS; siguen en P7.


## 7. Cierre P4.1-H

Revalidación final sobre `d1eab2a` con Codex `0.171.0`: 8 smokes reales PASS, incluidos control efectivo confirmado en el thread nativo (`effort=high`), importación de herramientas estructuradas, supervivencia de aprobaciones y aislamiento entre proveedores. Las celdas UNSUPPORTED (adopción con escritura, recuperación de turno en vuelo) se mantienen por limitación real del proveedor, no por falta de pruebas. Ver `docs/P4_1_RESULTS.md` §8.


## 8. Cierre P4.2

Pi estándar 1.6.0 pasa a VERIFIED para chat/streaming, herramientas, controles modelados y aplicados, interrupción real, descubrimiento/historial con dedup, reattach y aislamiento multiproveedor (`docs/P4_2_RESULTS.md`). Aprobaciones de permisos: UNSUPPORTED en esta instalación (sin extensión que las emita). `steer`/`follow_up`: UNTESTED. Adopción con escritura y recuperación de turno en vuelo: UNSUPPORTED.


## 9. Cierre P4.3

Gentle Shell 4.8.0 (Pi embebido 1.5.2): VERIFIED para RPC base, Desktop RPC negociado, chat/streaming, herramientas, controles, interrupción, reattach y aislamiento. OBSERVED: eventos `session_snapshot` y modo `super-yolo`. UNSUPPORTED/UNTESTED: permisos interactivos reales, perfiles, historial de sesión enlazada. Subagentes solo inventariados. Ver `docs/P4_3_RESULTS.md`.


## 10. P4.3-H — Límites de capacidad tras auditoría

Los smokes reales iniciales de Gentle Shell son evidencia de la integración previa a H. El último driver debe revalidarse antes de ratificar el cierre. Los controles modelo/esfuerzo requieren confirmación nativa; `permission_mode` se expone solo para lectura mientras no se pruebe aislamiento fiable de home y perfil. `list_sessions` se sondea desde la fábrica sin caché del runner y puede responder `sessions_unavailable`; importar historia ajena desde un proceso `--no-session` permanece **UNSUPPORTED** para impedir mezclas de identidad. La aprobación nativa de permisos es **UNTESTED** en el entorno `super-yolo`, aunque su traducción tipada tiene pruebas de protocolo.


## 11. Evidencia de cierre P4.3-H

El commit `5ba188a` pasó CI `44953487212`. El usuario confirmó PASS de los siete scripts P4.3-H en Linux: lifecycle, recovery, concurrency, chat, tools, interrupt y controls. Los resultados de `recovery` verificaron tanto supervivencia a SIGKILL de `fleetd` como reattach sin eventos duplicados. `model` y `effort` se aplicaron realmente en Gentle Shell; `permission_mode` permaneció de solo lectura. Mantener explícitos `UNTESTED` (permisos interactivos nativos y perfiles) y `UNSUPPORTED` (importación externa sin identidad de sesión verificable); no inferir cobertura de Windows/macOS ni iniciar P4.4 sin auditoría.


## 10. Matriz final P4.4

La matriz definitiva por proveedor y capacidad, con estado, versión, prueba y limitaciones, está en `docs/P4_4_RESULTS.md` §4. Las celdas no verificadas se mantienen como UNTESTED/UNSUPPORTED.
