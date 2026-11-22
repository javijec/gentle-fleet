# P5 — Transporte remoto SSH: resultados

**Rama:** `feature/p5-ssh-transport`. **Fecha:** 22/11/2026. **Plataforma:** Linux x86_64.
**Estado:** **P5 — EN REVALIDACIÓN**: implementación y evidencia localhost con SSH real y dos hosts aislados; falta el recorrido entre dos máquinas Linux independientes para el cierre definitivo.

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

OpenCode 2.3.2, Codex 0.171.0, Pi 1.6.0, Gentle Shell 4.8.0 (Pi embebido 1.5.2) en este entorno; los proveedores no se modificaron en P5.

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
| Dos máquinas Linux físicas distintas | **UNTESTED** | No hay segundo host disponible; se probaron dos instancias aisladas en localhost |

## 5. Unitarias, regresiones y CI

- **125 tests unitarios**, `cargo fmt/check/clippy -D warnings/build` y `git diff --check`: PASS.
- 14 E2E deterministas P1–P3 y 5 P4.4: PASS tras la corrección de `e2e_p44_concurrency.sh`.
- `e2e_p51_ssh.sh` y `e2e_p52_two_hosts.sh`: PASS.
- CI incorpora ambos scripts P5 con guard `command -v sshd` (SKIP explícito si no hay sshd). Sin modelos de pago.

## 6. Limitaciones

- No se declara compatibilidad funcional con Windows/macOS (P7).
- La identidad de host se apoya en `known_hosts` de OpenSSH; el campo opcional de huella en la configuración Fleet no se valida automáticamente todavía.
- No se probó un host remoto con otro sistema operativo ni un segundo equipo físico.
- La reconexión es controlada por el usuario: se recomienda repetir la operación con la misma `operation_key`; no se reenvía automáticamente nada.

## 7. Criterio de cierre

Faltan las dos máquinas Linux independientes para cumplir el criterio 14. Mientras tanto P5 permanece **EN REVALIDACIÓN**; la implementación es funcional y la experiencia local/remota es equivalente en el entorno probado. P6 no se inicia.

## 8. P5-H — Auditoría y correcciones posteriores

Se identificaron riesgos en validación de rutas/comando SSH remoto, recuperabilidad de la `operation_key` cuando se pierde el ACK, falsos PASS del terminal de un turno anterior, framing JSONL sin límite y cierre/diagnóstico de procesos bridge/SSH. Se implementaron correcciones y regresiones en la rama `feature/p5-ssh-transport` sin alterar `main`.

Cambios: allowlist de argumentos del bridge seguro frente a shell remoto; campo `expected_host_key` no implementado rechazado en lugar de ignorado; journal cliente 0600 con `fleetctl pending` y reconciliación explícita con `operation <key>`; límite JSONL de 8 MiB; manejo de half-close/errores; recolección del proceso SSH; clasificación SSH basada en errores reales; test con cursor capturado después del primer terminal.

**Estado:** EN REVALIDACIÓN. La ejecución CI anterior `45074936294` corresponde al código anterior a P5-H y no certifica estos cambios. Registrar aquí la URL y SHA del CI final una vez verificado. La matriz opt-in de proveedores reales y el recorrido entre dos máquinas Linux independientes siguen siendo evidencias distintas de los E2E deterministas con FakeDriver.
