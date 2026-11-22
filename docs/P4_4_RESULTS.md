# P4.4 — Conformidad, estabilización y entrega multiproveedor

**Rama:** `feature/p4-multi-provider`. **Fecha:** 22/11/2026. **Plataforma:** Linux x86_64.
**Estado:** **P4.4-H — EN REVALIDACIÓN** tras auditoría independiente. P4.3 continúa cerrada (`7446111`); los resultados P4.4 anteriores corresponden al framework previo al endurecimiento y no validan automáticamente el código correctivo.

## 1. Versiones verificadas

| Proveedor | Versión detectada en runtime |
|---|---|
| OpenCode | 2.3.2 |
| Codex | 0.171.0 |
| Pi estándar | 1.6.0 |
| Gentle Shell | 4.8.0 (Pi embebido 1.5.2) |

## 2. Commits

| Commit | Contenido |
|---|---|
| `bad8d54` | Framework de conformidad `fleet-conformance` y ejecutor común (`P4.4-A…D`) |
| `b8399cf` | Auditorías deterministas de aislamiento por pares y seguridad (`P4.4-E…F`) |

Commit final de documentación: ver sección 9.

## 3. Arquitectura del framework

`fleet-conformance` es un crate independiente que habla exclusivamente la Fleet API JSONL:

- `src/client.rs`: cliente mínimo con correlación por `request_id`, que ignora eventos/avisos intercalados hasta recibir su respuesta.
- `src/lib.rs`: `run_all(providers, options)` → ejecuta **el mismo conjunto** de comprobaciones para cada proveedor, leyendo en runtime la versión, capacidades y controles de `ListAgents`/`ListControls`. No hay copias por proveedor.
- `src/main.rs`: CLI `--providers`, `--real`, `--json`; estado explícito por celda: `PASS`, `FAIL`, `UNTESTED`, `UNSUPPORTED`, `SKIP` (con justificación). `SKIP` nunca cuenta como `PASS`. El binario termina con código 1 si cualquier celda es `FAIL`.

Comprobaciones comunes: `agent_registered`, `capability_contract`, `session_lifecycle`, `timeline_integrity`, `two_subscribers`, `control_discovery`, `control_invalid_rejected`, `approval_contract`, `native_discovery_readonly`, `no_autoapproval` y `real_chat` (opt-in). En modo real, `real_chat` adicionalmente comprueba idempotencia: repetir `operation_key` con el mismo payload devuelve el mismo `turn_id` y produce un solo `UserMessage`; cambiar el payload con la misma clave devuelve `Conflict`.

Ejecutores: `tests/e2e_p44_conformity.sh` (determinista y opt-in real), `tests/e2e_p44_concurrency.sh` (seis pares de proveedores) y `tests/e2e_p44_security.sh` (secretos, IDs nativos y permisos de token).

## 4. Matriz final de conformidad

Estado por celda con evidencia. `PASS` implica aserción ejecutada; `UNTESTED`/`UNSUPPORTED` se conservan explícitamente.

| Capacidad | OpenCode 2.3.2 | Codex 0.171.0 | Pi 1.6.0 | Gentle Shell 4.8.0 |
|---|---|---|---|---|
| Chat | PASS — `real_chat` común | PASS — `real_chat` común | PASS — `real_chat` común | PASS — `real_chat` común |
| Streaming | PASS — deltas en `real_chat` | PASS — deltas en `real_chat` | PASS — deltas en `real_chat` | PASS — deltas en `real_chat` |
| Herramientas | PASS — regresión P1–P3 `e2e_opencode` | PASS — `e2e_p4_codex_tools` vía smoke de aprobación | PASS — `e2e_p4_pi_tools` | PASS — `e2e_p4_gs_tools` |
| Aprobaciones | PASS — regresión P1–P3 (`once/always/reject`) | PASS — `e2e_p4_codex_approval` real, opciones parametrizadas | UNSUPPORTED — sin extensión de permisos | UNTESTED real — traducción Extension UI probada en unit tests; modo nativo `super-yolo` |
| Interrupción | UNSUPPORTED — `cancel_turn=false` | PASS — `e2e_p4_codex_interrupt` | PASS — `e2e_p4_pi_interrupt` | PASS — `e2e_p4_gs_interrupt` |
| Modelo | UNSUPPORTED — sin controles | PASS — `e2e_p4_codex_controls` | PASS — `e2e_p4_pi_controls` | PASS — smoke H de modelo nativo |
| Esfuerzo | UNSUPPORTED | PASS — `e2e_p4_codex_controls` | PASS — `e2e_p4_pi_controls` | PASS — smoke H de esfuerzo nativo |
| Historial | PASS — P2/P3 | PASS — `e2e_p4_codex_discovery` | PASS — `e2e_p4_pi_discovery` | UNSUPPORTED: importación externa deshabilitada por falta de identidad nativa verificable |
| Descubrimiento | PARCIAL — sin API nativa expuesta | PASS — 68 sesiones listadas en la auditoría | PARCIAL — archivos aislados | UNSUPPORTED — `sessions_unavailable` en modo efímero |
| Recuperación P3 | PASS — `e2e_p3_*` | PASS — `e2e_p4_codex_survival` | PASS — `e2e_p4_pi_recovery` | PASS — `e2e_p4_gs_recovery` |
| Reattach | PASS | PASS | PASS | PASS |
| Replay | PASS — sin duplicados | PASS — sin duplicados | PASS — sin duplicados | PASS — sin duplicados |
| Concurrencia | PASS — seis pares deterministas | PASS — seis pares + smokes reales | PASS — seis pares + smokes reales | PASS — seis pares + smoke real con Codex |
| Aislamiento | PASS — matar su runner no afecta a los demás | PASS | PASS | PASS |
| Stop seguro | PASS — `session_lifecycle` común | PASS | PASS | PASS |

La salida completa con versión, check, estado y detalle está en el JSON producido por `fleet-conformance --json` (44 celdas en modo real).

## 5. Pruebas ejecutadas sobre el código final

- **113 tests unitarios** (`cargo test --workspace --locked`).
- `cargo fmt --all -- --check`, `cargo check --workspace --all-targets --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo build --workspace --locked`, `git diff --check`: **PASS**.
- **17 E2E deterministas**: 14 de P1–P3 + 3 nuevos de P4.4 → **PASS**.
- **8 smokes Codex**, **8 smokes Pi**, **6 smokes Gentle Shell** + `e2e_p44_conformity` real → **PASS**.
- Conformidad común: 40 celdas deterministas y 44 con `--real`, sin `FAIL`. Los estados no-PASS son `UNSUPPORTED`/`UNTESTED`/`SKIP` justificados.

## 6. Hallazgos y correcciones

| Hallazgo | Corrección |
|---|---|
| `sessions_unavailable` de Gentle Shell se marcaba `FAIL` | El framework clasifica el error nativo tipado como `UNSUPPORTED`, no como fallo |
| Un token de respuesta puede partirse entre deltas | `real_chat` concatena el texto observado antes de verificarlo |
| `control_discovery` vacío en OpenCode no implica ausencia de modelo | El smoke real no se gated por controles; los errores `no model`/`unsupported` se clasifican `UNTESTED` |
| E2E de seguridad confundía IDs Fleet (`ses_00000002`) con IDs nativos | El patrón exige suffixes hexadecimales largos, no IDs secuenciales de Fleet |
| La matriz inicial no cubría idempotencia de prompt | `real_chat` añade reintento con misma clave (mismo `turn_id`) y conflicto con clave distinta |

No se encontraron defectos P0/P1 abiertos dentro del alcance acordado.

## 7. Seguridad, persistencia y recuperación

`e2e_p44_security.sh` comprueba que la API pública no expone `sk-*`, `auth.json`, `service.json`, rutas de home, tokens Bearer, passwords ni IDs nativos con sufijo opaco; los archivos `.token` del runner tienen modo `0600`. La adopción con escritura sigue bloqueada para los cuatro proveedores cuando no hay exclusividad demostrable. La importación de historial conserva procedencia e identidad estable y no se marca completa cuando es parcial.

SQLite V4 sigue siendo el esquema actual; las migraciones V3→V4 y la compatibilidad de frames antiguos están cubiertas por tests unitarios y se mantienen sin cambios en esta etapa.

## 8. Riesgos pendientes

| Riesgo | Severidad | Estado |
|---|---|---|
| Permisos interactivos de Gentle Shell | Media | UNTESTED (modo `super-yolo`); no se cambió configuración personal |
| Perfiles nativos | Media | UNTESTED; sin home aislada con credenciales |
| Historial enlazado de Gentle Shell | Media | PARCIAL; sin sesión externa segura para importar |
| Descubrimiento de OpenCode/Gentle Shell | Media | PARCIAL/UNSUPPORTED documentado |
| Cancelación en OpenCode | Baja | UNSUPPORTED (`cancel_turn=false`) |
| Windows/macOS | Media (P7) | Sin validar; no se declara compatibilidad |

## 9. Estado final y siguientes etapas

**P4.4-H — EN REVALIDACIÓN.** Los resultados reales anteriores no sustituyen repetir la matriz de cuatro proveedores sobre los cambios de P4.4-H. El CI ejecuta fixtures FakeDriver para las pruebas P4.4 nuevas, y los cuatro proveedores requieren smokes locales explícitos. No se modifica `main` ni se comienza P5.

Para P5 queda el transporte remoto/SSH conservando el mismo wire; para P6, la interfaz gráfica; para P7, la validación funcional de Windows/macOS y la hardened final del producto.


## 10. P4.4-H — Correcciones de auditoría

Se cerraron ventanas de falso PASS del framework:

- **Aislamiento:** los seis pares de proveedores son obligatorios, sin `continue` sobre errores de sesión ni éxito con cero casos. Se comprueba supervivencia del runner contrario y se exige turno real completado en modo opt-in o en el fixture FakeDriver.
- **Clientes independientes:** el test anterior enviaba dos `SubscribeTimeline` sobre la misma conexión. El nuevo `two_client_replay` abre dos conexiones autenticadas separadas, compara frames y verifica orden, identidad y `seq` de los eventos entregados.
- **Identidad del turno:** `real_chat` exige `TurnStarted`, exactamente un `UserMessage`, un solo `TurnCompleted{status:Completed}` del `turn_id` correcto, texto comprobable y operaciones idempotentes. Ya no convierte errores por coincidencia de palabras `model` o `unsupported` en UNTESTED.
- **Workspace seguro:** `fleet-conformance` exige `--workspace` con ID registrado explícitamente. No selecciona el primer directorio personal del daemon. El script de conformidad crea uno temporal propio.
- **Un solo reporte:** el wrapper invoca el framework una vez en modo JSON, valida todas las celdas requeridas y genera resumen desde esa ejecución. No duplica prompts ni consume tokens dos veces.
- **RPC acotado:** lectura con timeout y timeline por cursor, con verificación de secuencias e identidad de sesión. El lector ya no reconsulta siempre solo los 256 primeros eventos.
- **Seguridad:** regresión Rust con credenciales ficticias que inyecta claves, Bearer, rutas de autenticación e IDs nativos en diagnósticos, comprobando la redacción. La inspección E2E de API mantiene un alcance separado; no equivale a prueba exhaustiva de todos los secretos posibles.
- **CI:** se agregan `e2e_p44_conformity.sh`, `e2e_p44_concurrency.sh` y `e2e_p44_security.sh` con `GF_E2E_FAKE_ONLY=1`, etiquetados explícitamente como fixtures. No se usan como evidencia de ejecución real de los cuatro proveedores.

**CI correctivo completado:** commit `b5bab0` — [GitHub Actions `44964910718`](https://github.com/javijec/gentle-fleet/actions/runs/44964910718) PASS. Incluye formato, Clippy, tests Rust (redacción de credenciales ficticias), compilación, regresiones P1–P3 y los tres nuevos E2E P4.4 con `GF_E2E_FAKE_ONLY=1`. La prueba de seguridad comprueba todos los tokens de runner mientras cada runner está activo y falla si no encuentra el token esperado.

**Pendiente para cerrar P4.4-H:** repetir `tests/e2e_p44_{conformity,concurrency,security}.sh` sin `GF_E2E_FAKE_ONLY` en Linux con las cuatro herramientas instaladas, activar `GF_E2E_REAL=1` para verificar chat y continuidad tras fallos, conservar el reporte JSON por versión y repetir los smokes específicos afectados. Estas pruebas reales no se ejecutaron en GitHub Actions; no deben confundirse con las fixtures FakeDriver.
