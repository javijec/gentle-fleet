# P5 — Transporte remoto SSH: resultados

**Rama:** `feature/p5-ssh-transport`. **Fecha de cierre:** 22/11/2026. **Plataformas comprobadas:** servidor Linux x86_64; clientes Linux y Windows 11 (PowerShell 7 + OpenSSH_for_Windows_9.5p2).
**Estado:** **P5 — CERRADA CON ALCANCE EXPLÍCITO**. Transporte SSH y CLI remotos validados contra un servidor Linux en dos equipos reales (Windows → Linux). Se sustituye el criterio anterior «dos Linux independientes» por un recorrido entre **dos máquinas físicas de sistemas operativos diferentes**, que es más representativo para el cliente Windows de P5; no implica soporte de `fleetd`/runners nativos en Windows ni validación macOS.

## 1. Commits

| Commit | Contenido |
|---|---|
| `b7c0e52` | Cierre documental P4.4-H sobre `4c6b0e3` |
| `5636219` | Bridge SSH, transporte unificado, `--host`, gestión de hosts y E2E localhost |
| `46491b0` | ADR/diseño, plan de pruebas y CI |
| `c371330` | Corrección de falsos fallos en aislamiento P4.4 (pgrep por workspace y espera de estado) |

## 2. Arquitectura implementada

`fleet-ssh` expone `FleetReader`/`FleetWriter` sobre socket local o SSH; `fleetctl` usa el mismo `Frame` en ambos. El bridge `fleet-ssh-bridge` corre en el host remoto, reescribe `Hello` con el token local de ese host y bombea stdin/stdout con buffers de 16 KiB en dos hilos, con half-close y sin detener `fleetd`/runners. Ver `P5_DESIGN.md`.

Comandos nuevos: `fleetctl --host <alias> <comando>` para todos los comandos existentes y `fleetctl host add|list|show|remove|check`.

## 3. Versiones detectadas

Los smokes iniciales Linux utilizaron OpenCode 2.3.2, Codex 0.171.0, Pi 1.6.0 y Gentle Shell 4.8.0 (Pi embebido 1.5.2). En la prueba manual Windows → Linux del cierre se detectaron OpenCode 2.3.2, Codex 0.171.0, Pi 1.6.0 y Gentle Shell `4.9.0-main.023987cb1c91`. Los drivers no se modificaron en P5.

## 4. Resultados reales

| Caso | Resultado | Evidencia |
|---|---|---|
| SSH autenticado y paridad `agents` | PASS | `tests/e2e_p51_ssh.sh` con sshd/known_hosts efímeros |
| Workspace y ruta del host remoto | PASS | mismo script (la ruta se interpreta en el host destino) |
| Sesión, prompt y streaming remotos | PASS | `turn_completed Completed` y deltas sobre SSH |
| Dos clientes remotos | PASS | timelines idénticas |
| Caída SSH durante turno | PASS | runner vivo, turno termina, timeline durable |
| Reconexión y replay por cursor | PASS | timeline local y remota idénticas, un solo `UserMessage` |
| ACK/reintento con misma `operation_key` | PASS | no duplica prompt |
| Host no confiable | PASS | error `HostKeyRejected` |
| Bridge ausente | PASS | error explícito con stderr del bridge |
| Dos hosts aislados | PASS | `tests/e2e_p52_two_hosts.sh`: dos sshd/fleetd, workspaces, sesiones, claves de operación y timelines separadas |
| Chat real por SSH | PASS | OpenCode, Codex, Pi y Gentle Shell completaron `P5 <proveedor> OK` con `GF_E2E_REAL=1` |
| Dos máquinas Linux físicas distintas | **UNTESTED, no exigidas para este cierre** | No hay segundo host Linux disponible; reemplazadas en el criterio de cierre por Windows 11 → Linux real |
| Dos máquinas físicas, Windows 11 → Linux | **PASS manual** | `ssh -o BatchMode=yes`, `host check linux` → `connected linux fleet-v1-p2`; agentes, workspace, sesión, prompt y timeline |
| Streaming y replay desde Windows | **PASS manual** | FakeDriver `ses_00000002` (11 eventos), OpenCode `ses_0000000c` (turno completo), Gentle Shell `ses_00000025` (turno completo), más Codex y Pi confirmados por el usuario |
| Recuperación de turno remoto tras pérdida del observador | **PASS para replay; corte manual no confirmado inequívocamente** | OpenCode `ses_00000038`: generación de 150 líneas terminada en evento 82; segundo turno con herramienta `sleep 30`, replay `--since 280` de eventos 281–285, termina `Completed`; no se reenvió el prompt |

## 5. Unitarias, regresiones y CI

- **Suite de tests unitarios**, `cargo fmt/check/clippy -D warnings/build` y `git diff --check`: PASS.
- 14 E2E deterministas P1–P3 y 5 P4.4: PASS tras la corrección de `e2e_p44_concurrency.sh`.
- `e2e_p51_ssh.sh` y `e2e_p52_two_hosts.sh`: PASS.
- CI incorpora ambos scripts P5 con guard `command -v sshd` (SKIP explícito si no hay sshd). Sin modelos de pago.

## 6. Limitaciones aceptadas

- **Windows 11 comprobado exclusivamente como cliente `fleetctl.exe`**, consumiendo por SSH un `fleetd` y runners Linux. No se verificaron `fleetd`, runners ni proveedores como procesos nativos de Windows; eso permanece en P7. macOS sigue sin validar.
- No se probaron dos hosts Linux físicos ni una máquina remota Windows actuando como servidor; no son requisitos de aceptación de este corte acotado.
- El cliente observador `watch` regresó a PowerShell antes de observar todos los `delta` en una prueba manual; posteriormente `timeline` mostró el turno `Completed`. No se confirmó explícitamente si ese regreso fue causado por `Ctrl+C`. El corte SSH forzado sí está automatizado y pasó con `tests/e2e_p51_ssh.sh` sobre Linux localhost; no confundir ambas evidencias.
- En el segundo turno manual de OpenCode se comprobó el replay por cursor 281–285 y el terminal `Completed`, pero no se ejecutó una verificación externa de ausencia de duplicados para la timeline completa. La E2E determinista sí comprueba duplicados.
- Los comandos `approvals/answer` están disponibles en el mismo wire y tienen regresiones de capas previas, pero **no se hizo un flujo de aprobación interactiva real Windows → Linux**. Las limitaciones de permisos propias de cada proveedor siguen vigentes.
- Los avisos `ignored Desktop RPC snapshot without matching native session identity` de Gentle Shell se descartan; durante la prueba no impidieron completar el turno. Registrar su frecuencia como observabilidad P7, no como PASS de una capacidad de snapshot adicional.
- Se observaron procesos `fleetd` y runners de E2E previos coexistiendo en Linux. Su limpieza/inventario queda como mejora operacional P7; no se modificaron ni terminaron durante la validación.
- `expected_host_key` no implementa pinning adicional: Fleet rechaza una configuración con ese campo; la verificación efectiva se apoya en el `known_hosts` de OpenSSH.
- La reconexión es manual, mediante un nuevo `watch --since` o `timeline`; la reconexión automática/backoff pertenece a P6. Los efectos con ACK incierto se recuperan con `fleetctl pending` y `operation <key>`, sin reenvío automático.

## 7. Criterio de cierre aceptado

P5 queda **CERRADA** en la rama de función sin merge a `main`. Criterio original adaptado de dos Linux físicos a dos computadoras reales Windows 11 → Linux para validar la interoperabilidad de **cliente y transporte**. Existe evidencia de wire compartido, SSH/host-key, creación de workspace y sesiones, chat real con los cuatro proveedores, herramientas, eventos persistidos, replay por cursor y supervivencia del runner probada en CI frente a la caída del bridge/SSH. Las excepciones de §6 no se presentan como validaciones pasadas ni amplían el alcance a servidor Windows. P6 no se inicia con este commit.

## 8. P5-H — Auditoría y correcciones posteriores

Se identificaron riesgos en validación de rutas/comando SSH remoto, recuperabilidad de la `operation_key` cuando se pierde el ACK, falsos PASS del terminal de un turno anterior, framing JSONL sin límite y cierre/diagnóstico de procesos bridge/SSH. Se implementaron correcciones y regresiones en la rama `feature/p5-ssh-transport` sin alterar `main`.

Cambios: allowlist de argumentos del bridge seguro frente a shell remoto; campo `expected_host_key` no implementado rechazado en lugar de ignorado; journal cliente 0600 con `fleetctl pending` y reconciliación explícita con `operation <key>`; límite JSONL de 8 MiB; manejo de half-close/errores; recolección del proceso SSH; clasificación SSH basada en errores reales; test con cursor capturado después del primer terminal.

**CI de cierre técnico:** último HEAD anterior a este cierre documental `2b2c33722114a7c8ebf4337be4c316a1e9a78b41`, [GitHub Actions #45076748228](https://github.com/javijec/gentle-fleet/actions/runs/45076748228) **SUCCESS**, con `cargo fmt`, Clippy `-D warnings`, tests Rust, build, regresiones P1–P4.4 y ambos E2E SSH ejecutados como **PASS**. También `37397a4` / [#45076529611](https://github.com/javijec/gentle-fleet/actions/runs/45076529611) PASS para los cambios de código P5-H. Los proveedores reales están documentados por smokes opt-in y las pruebas manuales Windows → Linux; no se confunden con CI FakeDriver.

**Decisión de cierre:** P5 termina con alcance **servidor Linux + cliente Windows/Linux sobre SSH**. La validación de dos Linux físicos deja de ser condición bloqueante, pero sigue anotada como `UNTESTED`. Los límites de §6 quedan abiertos para sus fases correspondientes. No se hizo merge a `main` ni se inició P6.
