# P4.2 — Pi estándar: resultados (Linux x86_64)

**Rama:** `feature/p4-multi-provider`. **Fecha:** 22/11/2026.
**Versión verificada:** `pi 1.6.0` (`/home/javijec/.local/share/mise/installs/pi/1.6.0/pi/pi`).
**Estado:** **P4.2 — CERRADA EN LINUX** tras P4.2-H. El usuario ejecutó los ocho smokes reales sobre el HEAD final del endurecimiento y confirmó PASS; CI verde en `d20c5b0`.

## 1. Commits

| Commit | Contenido |
|---|---|
| `b2f0357` | P4.2-A…C: transporte Pi RPC, eventos, controles, aprobaciones Extension UI, descubrimiento e historial; registro en `fleet-providers` |
| `dca9d90` | P4.2-D…G: sondeo real de capacidades, aplicación efectiva de controles, interrupción verificada y smokes E2E |

## 2. Versión y entorno

- `pi --version` → **1.6.0** (sin cambios respecto a P4.0).
- Documentación oficial consultada en la instalación: `docs/rpc.md`, `docs/rpc-commands.md`, `docs/json.md`, `docs/rpc-extension-ui.md`, `docs/session-format.md`.
- **Modelo:** al inicio de la etapa `get_available_models` devolvió `[]` y `get_state.model.id` era `unknown`. El usuario configuró un modelo durante la ejecución; se volvió a verificar: **10 modelos** disponibles (`openai-codex:gpt-5.6-luna`, `gpt-6.1-terra`, …), modelo activo `gpt-6.1-terra`, 6 niveles de esfuerzo. Ningún smoke se marcó PASS antes de esa comprobación.
- No se copiaron credenciales ni se modificó la configuración personal de Pi; solo se usó el modelo ya configurado por el usuario en workspaces temporales.

## 3. Arquitectura implementada

- **`fleet-driver-pi`** (crate nuevo, `provider_id="pi"`): `PiFactory` + `PiDriver`. El runner posee el proceso `pi --mode rpc --session-dir <directorio temporal>` y su directorio de sesiones; el driver nunca escribe la base Fleet.
- **Transporte (`rpc.rs`)**: JSONL con framing estricto por LF (U+2028/U+2029 se tratan como contenido), límite de 1 MiB por frame con error explícito (nunca truncado), correlación por `id`, colas acotadas (256 eventos, 64 solicitudes UI) con backpressure que bloquea al lector en lugar de perder eventos, stderr en hilo separado, timeouts (5 s sondeo / 30 s comandos) y cierre acotado con kill de respaldo.
- **Eventos (`events.rs`)**: `message_update.text_delta` → `AssistantDelta`; `message_end` → `AssistantMessage` solo si no hubo deltas (sin duplicado); `tool_execution_*` → inicio/salida/fin de herramienta; `thinking_delta` → aviso de razonamiento; `agent_end` **no** cierra el turno; `agent_settled` → `TurnCompleted` (`Interrupted` si `aborted`). Los IDs nativos nunca se usan como IDs Fleet.
- **Controles**: modelos y esfuerzos descubiertos con `get_available_models`/`get_available_thinking_levels`/`get_state`; `ListControls`/`SetControl` con validación contra valores disponibles. Los controles deseados se aplican al proceso Pi antes del prompt y el estado efectivo confirmado se publica como aviso durable. Cambios durante un turno activo se rechazan con `ControlDuringTurn`.
- **Interrupción**: `InterruptTurn` → `abort` + `clear_queue`. `cancel_turn` solo se declaró `true` después de verificar una interrupción real con `agent_settled{aborted:true}`.
- **Aprobaciones**: traducción genérica de `extension_ui_request` (`select`/`confirm`/`input`/`editor`) a `ApprovalRequested` con `option_id` público, argumentos tipados y respuesta `extension_ui_response`; los diálogos no traducibles se cancelan explícitamente, nunca se autoaprueban.
- **Descubrimiento/historial**: enumeración de archivos `.jsonl` nativos con `native_ref` opaco (hash FNV), sin crear runner ni tocar sesiones externas; importación de mensajes con `source_ref` estable, herramientas tipadas y deduplicación incluso entre `operation_key` distintos.
- **Composición**: `fleet-providers` registra `fake`, `opencode`, `codex` y `pi` en una sola función; `fleetctl agents` distingue los cuatro. No se agregó Gentle Shell ni abstracción especulativa compartida.

## 4. Capacidades por clasificación

| Capacidad | Clasificación | Evidencia |
|---|---|---|
| Arranque/cierre Pi RPC y propiedad por runner | VERIFIED | `tests/e2e_p4_pi_lifecycle.sh` |
| Chat real, streaming y finalización por `agent_settled` | VERIFIED | `tests/e2e_p4_pi_chat.sh` (respuesta `P4 PI OK`) |
| Herramientas reales (`bash`) | VERIFIED | `tests/e2e_p4_pi_tools.sh` |
| Modelos/esfuerzos descubiertos y aplicados | VERIFIED | `tests/e2e_p4_pi_controls.sh` + avisos de estado efectivo en timeline |
| Interrupción real | VERIFIED | `tests/e2e_p4_pi_interrupt.sh`: un solo `turn_completed Interrupted` |
| Descubrimiento e historial con dedup | VERIFIED | `tests/e2e_p4_pi_discovery.sh` (archivo sintético aislado) |
| Reattach y timeline durable tras SIGKILL de `fleetd` | VERIFIED | `tests/e2e_p4_pi_recovery.sh` |
| Aislamiento Pi/Codex | VERIFIED | `tests/e2e_p4_pi_concurrency.sh` |
| Parser, correlación, frames inválidos, buffer lleno, EOF | VERIFIED | tests unitarios del crate (`cargo test`) |
| Aprobaciones/permisos nativos | UNSUPPORTED en esta instalación | Pi estándar no carga una extensión de permisos (`get_commands` → `llama`, `mcp`, `skill:*`); la traducción Extension UI existe y está cubierta por tests unitarios, pero no hay solicitudes reales que demostrar |
| `steer`/`follow_up` | UNTESTED / no conectados al API | El contrato Fleet expone `InterruptTurn`, no operaciones de encolado; `abort` y `clear_queue` sí están implementados |
| Adopción de sesiones externas con escritura | UNSUPPORTED | Sin garantía de exclusividad; solo descubrimiento/lectura |
| Recuperación de turno en vuelo si muere el proceso Pi | UNSUPPORTED | El proceso original es la única evidencia; no se reanuda un turno muerto |
| Desktop RPC / modos de permiso de Gentle Shell | Fuera de alcance | P4.3; no se usó como referencia |

## 5. Pruebas ejecutadas

- **85 tests unitarios** (`cargo test --workspace --locked`).
- **14 E2E deterministas P1–P3**: PASS.
- **8 smokes reales de Codex** (regresión P4.1): PASS.
- **8 scripts Pi**: 4 deterministas (`lifecycle`, `discovery`, `recovery`, `concurrency`) + 4 reales opt-in (`chat`, `tools`, `controls`, `interrupt`): PASS.
- `cargo fmt/check/clippy -D warnings/build` y `git diff --check`: PASS.
- Cross-compilación Windows de crates portables: se mantiene como señal de portabilidad; Windows/macOS siguen en P7.

## 6. Riesgos y limitaciones

| Riesgo | Severidad | Estado |
|---|---|---|
| Sin modelo configurable al inicio de la etapa | Alta (bloqueante temporal) | Resuelto por el usuario; se revalidó antes de declarar PASS |
| `agent_end` puede preceder a reintentos/compactación | Media | El driver ignora `agent_end`; solo `agent_settled` es terminal |
| Aprobaciones inexistentes en Pi estándar | Media | UNSUPPORTED documentado; no se simuló |
| Descubrimiento basado en archivos sin señal de actividad | Media | `externally_active=false`; adopción con escritura bloqueada |
| Importación de sesión muy grande | Media | Límites de 16 MiB y 10 000 mensajes; error tipado |
| Windows/macOS sin validar | Media (P7) | Sin declaración de compatibilidad |

## 7. Reutilización prevista para P4.3 (no implementada)

El transporte JSONL de `fleet-driver-pi` (framing LF, colas acotadas, correlación por id, stderr separado) es candidato a compartirse con Gentle Shell en P4.3. No se creó una abstracción común en esta etapa para no acoplar Pi estándar con Desktop RPC. La capa de eventos debe permanecer separada: Gentle Shell añade `extension_ui_request` de su propio protocolo y negociación `capabilities`, que Pi estándar no posee.

## 8. Estado final

**P4.2 — CERRADA EN LINUX.** El driver Pi dispone de las correcciones de integridad de P4.2-H y el usuario confirmó los ocho smokes reales sobre el HEAD final (`d20c5b0`, CI success `44946545342`). Las limitaciones documentadas se conservan: sin adopción externa con escritura, sin reanudación de turno cuyo proceso murió y sin permisos nativos reales en Pi estándar cuando no hay extensión que los solicite. No se hizo merge a `main`.

## 9. P4.2-H — Endurecimiento posterior a auditoría

Se corrigieron los hallazgos P4.2-H de transporte, eventos, historial, interacciones y controles:

- **RPC y backpressure:** `PiRpc::call` drena y retiene temporalmente los eventos y solicitudes UI mientras espera el ACK. El buffering está acotado: si se supera, el resultado se declara incierto explícitamente, sin ocultar ni confirmar un evento perdido. Una prueba con proceso JSONL sintético satura la cola del lector antes de devolver un ACK y comprueba que el comando y los eventos sobreviven.
- **Frames inválidos / EOF:** un frame corrupto o mayor que 1 MiB pasa a ser fallo fatal del lector. Si un turno está activo, Fleet emite `TurnUncertain` en lugar de mantenerlo `Running` o asumir `Failed`; las llamadas `prompt` cuyo ACK es ambiguo también dejan la operación incierta, sin reenviarla automáticamente.
- **Deltas y mensajes:** la deduplicación se reinicia en cada límite de mensaje, de modo que los deltas del primer mensaje no ocultan un mensaje posterior. Regresión determinista agregada.
- **Historial:** todo JSONL inválido en el archivo se informa con error; el límite de 10 000 mensajes devuelve `HistoryResultLimit` en vez de truncar el lote. Se preserva el contenido textual original y se serializan los bloques no textuales para no perderlos. `source_ref` incluye sesión y entrada; se verifica la cabecera al importar y el descubrimiento lee solo la cabecera, evitando cargar historiales completos. La adopción con escritura permanece bloqueada.
- **Extension UI:** `notify`, `setStatus` y `setWidget` no reciben una respuesta `cancelled` ficticia.
- **Modelos:** el modelo activo se determina por combinación de proveedor e ID, no solo ID.

**Evidencia disponible:** pruebas unitarias/deterministas nuevas, regresiones del CI sobre el HEAD correctivo y los ocho smokes reales ejecutados por el usuario sobre `d20c5b0`:

| Smoke Pi (P4.2-H) | Resultado confirmado |
|---|---|
| Lifecycle | PASS |
| Discovery e historial | PASS |
| Recovery | PASS |
| Concurrency Pi + Codex | PASS |
| Chat real | PASS |
| Tools reales | PASS |
| Controls reales | PASS |
| Interrupt real | PASS |

Los smokes son opt-in y requieren Pi 1.6.0 con modelo disponible; se ejecutaron en workspaces temporales sin modificar credenciales personales.

**Limitaciones conservadas:** sin adopción externa con escritura, sin reanudación de un turno cuyo proceso Pi murió y sin permisos nativos reales de Pi estándar cuando no hay extensión que los solicite.
