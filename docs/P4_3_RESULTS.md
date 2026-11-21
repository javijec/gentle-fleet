# P4.3 — Gentle Shell: resultados (Linux x86_64)

**Rama:** `feature/p4-multi-provider`. **Fecha:** 22/11/2026.
**Estado:** **P4.3-H — EN REVALIDACIÓN** tras la auditoría del driver. El CI y los smokes reales anteriores pertenecen a los commits previos al endurecimiento; las pruebas reales sobre el HEAD correctivo están pendientes.
**Proveedor:** `gentle-shell`. **No se implementó P4.4 ni se hizo merge.**

## 1. Versiones verificadas

| Componente | Versión real | Evidencia |
|---|---|---|
| Gentle Shell | **4.8.0** | `gentle-shell --version` (primera línea) |
| Pi embebido | **1.5.2** | `gentle-shell --version` (segunda línea) |
| Pi estándar (otro proveedor) | 1.6.0 | `pi --version`; no interactúa con este driver |

Las versiones se detectan en runtime; no se hardcodean como constantes de compatibilidad (rango `>=4.8,<5`).

## 2. Commits

| Commit | Contenido |
|---|---|
| `c3522e3` | Cierre documental P4.2-H (ocho smokes Pi confirmados por el usuario) |
| `85e13ff` | Extracción del transporte JSONL neutral a `fleet-rpc-jsonl` |
| `fa17ae8` | Driver Gentle Shell, Desktop RPC, eventos, controles y seis E2E |
| `f179bf5` | Descubrimiento nativo de solo lectura desde `list_sessions` |
| `617a94d` | Corrección CI: test de sondeo independiente del binario instalado |

## 3. Protocolo real negociado

Activación verificada: `GENTLE_SHELL_INTERACTIVE_HOST=1 gentle-shell --mode rpc --no-session`.

Un request Desktop RPC se envía como prompt Pi `/gentle:desktop <JSON>`. La respuesta del comando (`disposition:"handled"`) **no** contiene la respuesta de dominio: esta llega como `extension_ui_request{method:"setWidget", widgetKey:"gentle-desktop-rpc", widgetLines[0]}` y se correlaciona por `requestId` del envelope `gentle-shell.desktop-rpc/v1`.

**24 métodos negociados en la instalación real:** `capabilities`, `get_snapshot`, `get_config`, `set_config`, `get_permission_mode`, `set_permission_mode`, `get_subscription_usage`, `refresh_subscription_usage`, `get_design_canvas`, `list_jobs`, `cancel_job`, `get_job_output`, `apply_profile`, `list_subagents`, `cancel_subagent`, `get_subagent_thread`, `list_sessions`, `switch_session`, `get_changes`, `get_change_diff`, `get_review_mode`, `set_review_mode`, `get_usage_stats`, `get_update_status`.

**9 eventos negociados:** `config_changed`, `permission_mode_changed`, `rate_limit`, `design_canvas`, `jobs_update`, `profile_applied`, `agents_update`, `review_mode_changed`, `session_snapshot`.

Estos números son observaciones de esta versión, no constantes de compatibilidad. `list_subagents`, `cancel_subagent` y `get_subagent_thread` solo se inventarían; **no se implementan ni exponen** (P4-F05).

## 4. Arquitectura

- **`fleet-driver-gentle-shell`** independiente de Pi, Codex, OpenCode, GPUI y Herdr. Usa el transporte JSONL compartido (`fleet-rpc-jsonl`) con captura explícita de `setWidget` solo para este proveedor; Pi mantiene su comportamiento anterior.
- El runner es dueño del proceso Gentle Shell y del spool P3. `fleetd` no conoce el protocolo.
- `desktop.rs`: validación de límites (64 KiB/request, 128 caracteres de `requestId`, 64 de método), serialización, correlación por `requestId`, errores tipados (`UnsupportedMethod`, `NativeError`, `DesktopTimeout`, `Transport`), seguimiento de `seq` con detección de duplicados/huecos/resets y `get_snapshot` para reconciliar. Los frames Desktop nunca se publican como texto del asistente.
- `events.rs`: streaming, mensaje final, herramientas y `agent_settled` como terminal; `agent_end` no cierra el turno. Deltas y mensajes finales se deduplican.
- Aprobaciones Extension UI: opciones `select`/`confirm`/`input`/`editor` → `ApprovalRequested` con `option_id` público, argumentos tipados, id nativo privado y entrega vía `extension_ui_response`; notificaciones fire-and-forget no reciben respuestas ficticias.
- Controles: modelos/esfuerzos descubiertos en runtime; `permission_mode` leído con `get_permission_mode`; `set_permission_mode` solo con `FLEET_GS_ALLOW_MODE_WRITE=1` y `persist:false`, nunca sobre el modo global del usuario.
- Registro único en `fleet-providers`; `fleetctl agents` muestra cinco entradas (cuatro reales + FakeDriver).

## 5. Matriz de estado

| Área | Estado | Evidencia |
|---|---|---|
| RPC base | **PASS** | `e2e_p4_gs_lifecycle.sh`: arranque, `get_state`, cierre confirmado |
| Desktop RPC | **PASS** | Negociación real de 24 métodos/9 eventos; correlación y límites con tests unitarios y sonda ignorada `real_capabilities_roundtrip` |
| Negociación de capacidades | **PASS** | `DesktopCapabilities::from_response` + sonda real sobre el ejecutable instalado |
| Chat real | **PASS** | `e2e_p4_gs_chat.sh`: respuesta `P4 GS OK` |
| Streaming | **PASS** | deltas observados en el mismo smoke |
| Herramientas | **PASS** | `e2e_p4_gs_tools.sh`: `tool_started`/`tool_completed` reales |
| Aprobaciones nativas | **UNTESTED** con permiso real | El modo activo `super-yolo` no emitió `select` durante los smokes. Hay captura nativa previa y pruebas deterministas de opciones y respuestas; falta ejecución nativa con modo seguro y home aislado |
| Controles de sesión | **EN REVALIDACIÓN** | Modelo y esfuerzo ahora se aplican antes del prompt mediante setters Pi nativos y se verifican con `get_state`; el smoke de controles sobre el HEAD correctivo falta |
| Modos de permiso | **OBSERVED / READ-ONLY** | `get_permission_mode` → `super-yolo`; se desactivó el setter hasta verificar un home aislado: una variable de entorno no demuestra aislamiento |
| Perfiles | **UNTESTED** | `get_config`/`apply_profile` no se ejecutan: no hay home aislada con credenciales sin copiar secretos |
| Descubrimiento | **PARCIAL / ERROR EXPLÍCITO** | La consulta ahora se hace desde la fábrica en `fleetd`, no desde un cache del runner; `sessions_unavailable` en `--no-session` se propaga en vez de inventar sesiones |
| Historial | **UNSUPPORTED por seguridad** | La importación efímera con `get_messages` no podía probar la identidad nativa; se deshabilitó hasta disponer de un RPC de lectura enlazado a la sesión solicitada, sin falsas importaciones |
| Snapshot y secuencias | **PARCIAL / CONSERVADOR** | Un `seq=1` ambiguo provoca reconciliación; solo un `session_snapshot` explícito permite reset. Snapshot validado por `sessionId` y `seq` antes de aplicarse. Falta validar un reset real |
| Interrupción | **PASS** | `e2e_p4_gs_interrupt.sh`: `agent_settled{aborted}` → un solo `turn_completed Interrupted` |
| Supervivencia P3 | **PASS** | `e2e_p4_gs_recovery.sh`: runner vivo tras SIGKILL de `fleetd`, reattach y timeline sin duplicados |
| Concurrencia | **PASS** | `e2e_p4_gs_concurrency.sh`: matar el runner Gentle Shell no detiene Codex |
| Regresiones anteriores | **PASS** | 14 E2E deterministas P1–P3, 8 smokes Codex y 8 smokes Pi |
| GitHub Actions | **PENDING** | La validación correspondiente al último commit P4.3-H debe consultarse antes del cierre. El CI anterior `44951201465` fue PASS sobre `47404f1` |

## 6. Pruebas ejecutadas

- **108 tests unitarios** (`cargo test --workspace --locked`).
- **6 scripts Gentle Shell**: lifecycle, chat, tools, interrupt, recovery, concurrency → **PASS** (los reales con `GF_E2E_REAL=1`).
- **14 E2E deterministas P1–P3**: PASS.
- **8 smokes Codex** y **8 smokes Pi**: PASS (regresión multiproveedor).
- `cargo fmt/check/clippy -D warnings/build` y `git diff --check`: PASS.
- Sonda real de capacidades: 24 métodos y 9 eventos confirmados en el ejecutable instalado.

## 7. Limitaciones y riesgos

| Riesgo | Severidad | Estado |
|---|---|---|
| Sin permisos interactivos reales (modo `super-yolo`) | Media | UNTESTED: no se cambió el modo global; falta un permiso real en entorno con aislamiento demostrado |
| Perfiles sin entorno aislado con credenciales | Media | UNTESTED; no se copiaron credenciales |
| `list_sessions` con `--no-session` → `sessions_unavailable` | Media | Descubrimiento parcial; sin adopción con escritura |
| Historial de sesión enlazada no ejecutado | Alta | UNSUPPORTED: ruta anterior deshabilitada por no verificar identidad, sin posibilidad de importar silenciosamente otra sesión |
| Mensajes `session_snapshot` condicionales por sesión | Media | El driver no los asume; se negocian y se reconcilian con `get_snapshot` |
| Subagentes anunciados por el proveedor | Baja | Inventario solamente; fuera de alcance hasta después de P6 |
| Windows/macOS | Media (P7) | Sin validar funcionalmente; no se declara compatibilidad |

## 8. Seguridad y exclusiones

No se autoaprobó ninguna solicitud, no se modificó el modo global del usuario, no se copiaron credenciales, no se controlaron procesos externos activos, no se usaron GPUI/Herdr y no se implementaron subagentes, SSH, SDK ni nada de P4.4.

## 9. Veredicto y recomendación

**P4.3-H — EN REVALIDACIÓN.** Los smokes reales iniciales establecen la viabilidad del proveedor; los fixes P4.3-H requieren nuevo CI y comprobación sobre el commit final. La importación de historial y modificación de modos se mantienen bloqueadas por seguridad hasta contar con garantías nativas y aislamiento verificable. No comenzar P4.4.


## 10. P4.3-H — Correcciones de auditoría

- **Controles efectivos:** `model`/`effort` se configuran por Pi RPC antes del prompt y se verifican consultando el estado nativo. Si no aplican, el turno falla explícitamente en vez de continuar con otra configuración. `permission_mode` es de solo lectura hasta probar aislamiento real; no basta un flag.
- **Fallos RPC:** si se pierde el ACK después de enviar `prompt`, el driver emite `TurnUncertain` y prohíbe reenvío implícito. Frames corruptos y EOF sin `agent_settled` también producen un resultado incierto.
- **Desktop RPC saturado:** la espera de respuesta drena la cola ordinaria de eventos Pi en el buffer acotado del transporte, preservando el orden. El exceso de capacidad produce error explícito sin éxito ficticio.
- **Secuencias y snapshots:** un contador reiniciado sin señal nativa se trata como hueco; `session_snapshot` permite reconocer el reset. La reconciliación comprueba `seq` y `sessionId` antes de almacenar el snapshot o actualizar el modo de la sesión.
- **Descubrimiento:** se consulta en el proceso de la fábrica en `fleetd`, no mediante caché estática del runner. Si Desktop RPC responde `sessions_unavailable`, se informa el error.
- **Historial:** importación deshabilitada por seguridad hasta demostrar lectura de la sesión solicitada, identidad y fidelidad. Se conserva únicamente el descubrimiento de solo lectura cuando está disponible.
- **Permisos:** el parser y el constructor de respuestas continúan tipados. La ausencia de solicitudes reales bajo `super-yolo` implica UNTESTED, no UNSUPPORTED; se prohíbe modificar el perfil real para provocar un permiso.

**Revalidación pendiente:** ejecutar CI sobre el último commit y repetir los seis smokes `e2e_p4_gs_{lifecycle,chat,tools,interrupt,recovery,concurrency}.sh` en Linux. Las pruebas con modelos son opt-in y pueden consumir tokens. Antes de afirmar cierre, verificar el efecto real de `model`/`effort` y los límites de recuperación de sesión.
