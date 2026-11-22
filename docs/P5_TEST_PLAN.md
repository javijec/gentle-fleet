# P5 — Plan de pruebas

**Estado:** plan ejecutado; resultados en `docs/P5_RESULTS.md`.

## 1. Niveles

| Nivel | Alcance |
|---|---|
| Unitarias | Validación de hosts, reescritura de `Hello`, rechazo de stdout contaminado, framing compartido |
| Deterministas | `tests/e2e_p51_ssh.sh` con sshd efímero en localhost; `tests/e2e_p52_two_hosts.sh` con dos sshd y dos `fleetd` aislados |
| Reales opt-in | El mismo script con `GF_E2E_REAL=1`: chat de los cuatro proveedores sobre SSH |
| No ejecutado | Recorrido entre dos máquinas Linux físicas/varias (sin segundo host disponible) |

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
