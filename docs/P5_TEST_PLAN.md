# P5 — Plan de pruebas

**Estado:** plan cerrado para **cliente Windows/Linux → servidor Linux**. Pruebas automáticas Linux PASS y recorrido físico Windows → Linux manual documentado en `docs/P5_RESULTS.md`. Las pruebas no ejecutadas permanecen identificadas como UNTESTED.

## 1. Niveles

| Nivel | Alcance |
|---|---|
| Unitarias | Validación de hosts, reescritura de `Hello`, rechazo de stdout contaminado, framing compartido |
| Deterministas | `tests/e2e_p51_ssh.sh` con sshd efímero en localhost; `tests/e2e_p52_two_hosts.sh` con dos sshd y dos `fleetd` aislados |
| Reales opt-in | El mismo script con `GF_E2E_REAL=1`: chat de los cuatro proveedores sobre SSH |
| Validación real entre plataformas | Windows 11 con `fleetctl.exe` + SSH cliente → `fleet-ssh-bridge`/`fleetd`/runners Linux, cuatro proveedores reales y replay por cursor; PASS manual |
| No ejecutado | Dos Linux físicos, corte SSH manual inequívocamente confirmado en Windows, daemon/runner Windows nativos, aprobaciones interactivas reales desde Windows y macOS |

## 2. Casos

| Caso | Esperado | Evidencia |
|---|---|---|
| SSH autenticado | `agents` local/remoto idénticos | `e2e_p51_ssh.sh` |
| Host no confiable | Error `HostKeyRejected` | mismo script |
| Bridge ausente | Error explícito | mismo script |
| Daemon remoto apagado | Error explícito | diagnóstico manual/CI |
| Streaming remoto | eventos y `turn_completed` | `e2e_p51_ssh.sh` |
| Dos clientes | misma timeline | mismo script |
| Caída SSH | runner y turno sobreviven | mismo script |
| Reconexión | replay por cursor sin duplicar prompt | mismo script |
| ACK perdido / reintento | misma `operation_key` no duplica | mismo script |
| Aprobación pendiente | sin autoaprobación | regresión P2/P4 + contrato común |
| Cliente lento | colas acotadas en bridge (16 KiB) y Fleet | inspección + E2E |
| Dos hosts | sesiones, workspaces y timelines aislados | `e2e_p52_two_hosts.sh` |
| Ruta remota inválida | no inicia runner | validación de `HostConfig` |
| stdout contaminado | error de protocolo | test unitario |
| Secretos en diagnósticos | redacción central | regresiones P4.4-H |
| Bridge muerto | Fleet remoto permanece vivo | `e2e_p51_ssh.sh` |

## 3. Regresiones

Se ejecutan P1–P3 y P4.1–P4.4 deterministas sobre el HEAD final, además de los scripts P5. CI usa únicamente FakeDriver/sshd efímero; no depende de modelos de pago.

## 4. Regresiones P5-H

Verificar en el CI del HEAD: rechazo de metacaracteres en la ruta remota, configuración con pin no implementado (fail-closed), límite de tamaño JSONL y rechazo de frames parciales, journal durable 0600 anterior al primer prompt remoto y recuperación de `operation_key`, comprobación de un único terminal del **segundo turno** tras cortar SSH, clasificación de errores estándar de OpenSSH y que la caída del bridge no afecta al runner. No dar PASS de tests omisos ni usar el primer `TurnCompleted` de una timeline como evidencia del turno siguiente.

Las dos instancias `sshd` sobre localhost **no equivalen a dos máquinas físicas**. La validación manual real cruzó dos computadoras independientes (Windows 11 cliente → Linux servidor), por lo que el cierre P5 cambió explícitamente el criterio físico anterior; dos Linux distintos siguen `UNTESTED`, como también la confirmación inequívoca del `Ctrl+C` en Windows. La E2E de corte SSH forzado sí pasó en CI Linux; no mezclar sus garantías con la prueba manual.
