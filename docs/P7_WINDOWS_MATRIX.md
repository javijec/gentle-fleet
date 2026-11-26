# Matriz P7 Windows — W0

Estados: PASS = ejecutado y aprobado; FAIL = ejecutado y fallido;
BLOCKED = impedido por otro fallo/requisito; UNSUPPORTED = no soportado;
UNTESTED = no ejecutado o aún sin resultado. Compilar no prueba runtime.
En la columna Compila, PASS significa `cargo check` y `cargo build` completos.
Compilación y prueba de proceso corresponden a la base `427a5c1`; los tests
unitarios se repitieron tras la adaptación SSH en `e31efcb`.
El [CI definitivo de `64dfac0`](https://github.com/javijec/gentle-fleet/actions/runs/45234810431)
terminó PASS en Windows nativo, backend Linux y Desktop Linux. Windows ejecutó
fmt, check, clippy, tests Rust y build del workspace completo.
La matriz muestra solo componentes del motor activo. La fila histórica del cliente gráfico retirado permanece en la versión original del documento en Git; no se modifica ningún resultado del backend.

| Componente | Compila | Tests unitarios | E2E | Validación real | Bloqueadores / siguiente etapa |
|---|---|---|---|---|---|
| fleet-domain | PASS | PASS | UNTESTED | UNTESTED | Contratos de proceso pendientes; W1 |
| fleet-protocol | PASS | PASS | FAIL | UNTESTED | TCP/token frente a pipe/DACL y token sobrescrito; W1 |
| fleet-agent / FakeDriver | PASS | PASS | FAIL | UNTESTED | Fake descubierto y sesión creada; prompt bloqueado; W1 |
| fleet-core | PASS | PASS | UNTESTED | UNTESTED | Reconciliación/procesos Windows; W1 |
| fleet-store-sqlite | PASS | PASS | UNTESTED | UNTESTED | ACL, WAL y crash Windows; W1 |
| fleet-rpc-jsonl | PASS | PASS | UNTESTED | UNTESTED | Procesos nativos; W2 |
| fleet-providers | PASS | PASS | UNTESTED | UNTESTED | Detección real; W2 |
| fleet-client | PASS | PASS | UNTESTED | UNTESTED | HOME/estado durable; W1 |
| fleet-ssh | PASS | PASS | UNTESTED | UNTESTED | Fixture token adaptado; host Windows sin validar; W4 |
| fleetctl | PASS | UNTESTED | FAIL | UNTESTED | Descubrimiento/registro OK; prompt no autorizado; W1 |
| fleetd | PASS | UNTESTED | FAIL | UNTESTED | Token sobrescrito tras inicio del runner, ACL; W1 |
| fleet-runner | PASS | PASS | FAIL | UNTESTED | Iniciado por ruta explícita; token y handles; W1 |
| fleet-conformance | PASS | PASS | UNTESTED | UNTESTED | Proveedores reales; W2 |
| fleet-driver-opencode | PASS | PASS | UNTESTED | UNTESTED | Binario Windows no validado; W2 |
| fleet-driver-codex | PASS | PASS | UNTESTED | UNTESTED | Binario Windows no validado; W2 |
| fleet-driver-pi | PASS | PASS | UNTESTED | UNTESTED | HOME / `/tmp`; W2 |
| fleet-driver-gentle-shell | PASS | PASS | UNTESTED | UNTESTED | Probe real opt-in; W2 |
| SSH Windows→Linux | UNTESTED | UNTESTED | UNTESTED | UNTESTED | P5 anterior no repetido en W0; W4 |
| SSH Linux→Windows | UNTESTED | UNTESTED | UNTESTED | UNTESTED | Bridge y shell Windows; W4 |

Los tests de proceso Linux basados en Bash, SIGKILL y permisos Unix no están
adaptados para ejecución Windows; su éxito en Linux no modifica esta matriz.
`fleetd` y `fleetctl` no tienen pruebas unitarias en los binarios (0 ejecutadas).
Los PASS de tests excluyen los probes opt-in ignorados y código `cfg(unix)`.
W0 queda cerrado como auditoría; los FAIL y UNTESTED funcionales siguen siendo
pendientes, no declaraciones de compatibilidad.

**W1-A PASS:** la regresión de proceso Windows comprueba que dos
runners no sobrescriben el token del daemon y que sus tokens no autentican
clientes del daemon. Runners aislados también rechazaron credenciales cruzadas.
El IPC seguro y el recorrido funcional completo siguen sin validar; ver
`P7_W1_RESULTS.md` y el [CI Windows/Linux](https://github.com/javijec/gentle-fleet/actions/runs/45238966908).

**W1-B ABIERTO:** el backend candidato usa named pipes; W1-A y el fixture
W1-B con EOF del bridge pasaron localmente tras corregir I/O concurrente.
Las ACL heredadas de `fleetd.addr` y `fleetd.token` no están protegidas y
permiten modificación a identidades adicionales en el entorno probado:
**FAIL** de seguridad. Prueba real de otra cuenta: **BLOCKED**. Ver evidencia
y otras carreras pendientes en `P7_W1_RESULTS.md`.

**W1-B (implementación candidata):** se sustituyó el backend Windows TCP por
named pipes con DACL explícita y verificación SID, y se prepararon regresiones
W1-A/W1-B sobre ese transporte. Las pruebas de CI y de aislamiento entre
usuarios distintos están pendientes; la matriz anterior contiene los fallos
históricos de W0 y no debe interpretarse como estado funcional actual.
**Estado W1-B: UNTESTED/BLOCKED hasta resultados concluyentes.**

**Actualización ACL W1-B (código publicado, verificación pendiente):**
`fleetd.addr`, `fleetd.token`, las direcciones y tokens de runner se
crean ahora mediante un helper Windows que solicita DACL protegida del SID
propietario al crear el archivo. Hay aserciones de ACL en ambos fixtures.
Estado actual para esta corrección: **UNTESTED** en Windows tras el parche,
porque CI está bloqueado por facturación. El FAIL histórico anterior sigue
siendo evidencia de la vulnerabilidad de la implementación previa.
Directorio de runtime, otra cuenta, PID reuse y backpressure: pendientes.
W1-B permanece ABIERTA.

**Revisión posterior a 4342725:** timeout/completado tiene corrección candidata
y prueba unitaria Windows; PID reuse y aislamiento real de dos SID siguen
pendientes. El estado histórico de esta matriz no sustituye las corridas
multiplataforma del SHA candidato. Ver `P7_W1_RESULTS.md`.

## Estado vigente W1-B (24/11/2026)

| Criterio | Estado |
|---|---|
| Full Windows/Linux de 175fad2 | PASS histórico |
| fleet-core Windows 10/10 de 175fad2 | PASS histórico |
| Protocolo Windows 10/10 reportado sobre d7e4a28 | PASS histórico |
| Full Windows/Linux del candidato 69f7729 y HEAD documental | UNTESTED |
| Regresiones de identidad, ACL, cancelación y recuperación del candidato | UNTESTED |
| Aislamiento efectivo entre dos SID | UNTESTED — diferido por decisión del propietario hasta release |
| GitHub Actions | BLOCKED por facturación |

W1-B: cierre administrativo con alcance limitado, aceptación técnica
pendiente. W1-C: traspaso autorizado; implementar solo tras gate de entrada
automatizado PASS y resolución de defectos P0/P1 confirmados. No se modifica
el modelo de amenaza ni el gate de release. Ver P7_W1_RESULTS.md.

## P7-H (25/11/2026)

Workspace headless integrado con P6. La nueva validación full Windows/Linux y las regresiones W1-A/B permanecen UNTESTED; no se trasladan PASS históricos al candidato. Aislamiento entre SID sigue diferido al release, no PASS.

### Revalidación posterior

El operador confirmó PASS del gate automatizado Windows/Linux y protocolo 10/10 tras la corrección del coordinador en `c1b3d77`. Ver `P7_W1_RESULTS.md`. Esta evidencia sustituye el estado UNTESTED automatizado del candidato anterior; revisión independiente de logs no realizada. Dos SID siguen UNTESTED hasta release. No es aceptación de Windows como plataforma de producción.


## Actualización W1 (26/11/2026)

| Alcance | Estado actualizado |
|---|---|
| W1-A/B/C | Aceptación histórica de desarrollo con límites documentados |
| W1-D/E/F, FakeDriver y proceso nativo | Implementación completa; PASS integral reportado |
| Full Windows/Linux, protocolo 10/10, Lifecycle 20/20 | Gate w1f reportado PASS; informes originales pendientes de revisión |
| Regresión SHM de lifecycle-19 | Corrección publicada; revalidación reportada PASS |
| Pruebas manuales de seguridad diferidas | UNTESTED/BLOCKED según evidencia, no PASS |
| OpenCode, Codex, Pi, Gentle Shell Windows reales | W2 pendiente |
| SSH bidireccional a servidor Windows | W4 pendiente |

HEAD remoto observado `8662aea`; no se verificó el SHA ejecutado mediante JSON. Esta actualización no certifica release ni sustituye criterios de P7_W1_F_PLAN.md y QUALITY.md. La matriz histórica anterior se conserva como evidencia de versiones previas.
