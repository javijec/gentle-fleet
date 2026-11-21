# P4.3 — Gentle Shell: resultados (Linux x86_64)

**Rama:** `feature/p4-multi-provider`. **Fecha:** 22/11/2026.
**Estado:** **P4.3 — CERRADA EN LINUX** con limitaciones explícitas (permisos nativos y perfiles no verificables en el entorno actual).
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
| Aprobaciones nativas | **UNSUPPORTED** en la configuración actual | El modo nativo es `super-yolo` y no hay extensión que emita `select`; la traducción Extension UI existe y está cubierta por tests unitarios, sin simulación |
| Controles de sesión | **PASS** | `fleetctl controls` lista modelo, esfuerzo, modo y Pi embebido (verificación real de negociación) |
| Modos de permiso | **OBSERVED** | `get_permission_mode` → `super-yolo`; escritura protegida por flag explícito y sin persistencia |
| Perfiles | **UNTESTED** | `get_config`/`apply_profile` no se ejecutan: no hay home aislada con credenciales sin copiar secretos |
| Descubrimiento | **PARCIAL** | `list_sessions` responde `sessions_unavailable` con `--no-session`; el driver expone la limitación y no inventa sesiones |
| Historial | **PARCIAL / UNTESTED** | Ruta de importación implementada con `source_ref` estable; sin sesión nativa enlazada disponible no se ejecutó un smoke real |
| Snapshot y secuencias | **OBSERVED / PARCIAL** | `session_snapshot` aparece en la negociación real; duplicados, huecos y resync probados en tests unitarios |
| Interrupción | **PASS** | `e2e_p4_gs_interrupt.sh`: `agent_settled{aborted}` → un solo `turn_completed Interrupted` |
| Supervivencia P3 | **PASS** | `e2e_p4_gs_recovery.sh`: runner vivo tras SIGKILL de `fleetd`, reattach y timeline sin duplicados |
| Concurrencia | **PASS** | `e2e_p4_gs_concurrency.sh`: matar el runner Gentle Shell no detiene Codex |
| Regresiones anteriores | **PASS** | 14 E2E deterministas P1–P3, 8 smokes Codex y 8 smokes Pi |
| GitHub Actions | **PASS** | CI `44950955792` sobre `617a94d` → success (los commits anteriores fallaron por un test dependiente del entorno, corregido en `617a94d`) |

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
| Sin permisos interactivos reales (modo `super-yolo`) | Media | UNSUPPORTED documentado; no se cambió el modo global ni se simuló una aprobación |
| Perfiles sin entorno aislado con credenciales | Media | UNTESTED; no se copiaron credenciales |
| `list_sessions` con `--no-session` → `sessions_unavailable` | Media | Descubrimiento parcial; sin adopción con escritura |
| Historial de sesión enlazada no ejecutado | Media | Ruta implementada, sin evidencia real; no se marca PASS |
| Mensajes `session_snapshot` condicionales por sesión | Media | El driver no los asume; se negocian y se reconcilian con `get_snapshot` |
| Subagentes anunciados por el proveedor | Baja | Inventario solamente; fuera de alcance hasta después de P6 |
| Windows/macOS | Media (P7) | Sin validar funcionalmente; no se declara compatibilidad |

## 8. Seguridad y exclusiones

No se autoaprobó ninguna solicitud, no se modificó el modo global del usuario, no se copiaron credenciales, no se controlaron procesos externos activos, no se usaron GPUI/Herdr y no se implementaron subagentes, SSH, SDK ni nada de P4.4.

## 9. Veredicto y recomendación

**P4.3 — CERRADA EN LINUX.** Gentle Shell funciona como cuarto proveedor real con chat estructurado, Desktop RPC negociado, controles comprobados, eventos/herramientas, interrupción y recuperación P3, sin romper OpenCode, Codex ni Pi. Las capacidades no verificables por el entorno (permisos interactivos, perfiles, historial de sesión enlazada) permanecen UNSUPPORTED/UNTESTED con evidencia, no como éxitos simulados. Se recomienda revisión antes de autorizar P4.4.
