# Matriz P7 Windows — W0

Estados: PASS = ejecutado y aprobado; FAIL = ejecutado y fallido;
BLOCKED = impedido por otro fallo/requisito; UNSUPPORTED = no soportado;
UNTESTED = no ejecutado o aún sin resultado. Compilar no prueba runtime.
En la columna Compila, PASS significa `cargo check` y `cargo build` completos.
Compilación y prueba de proceso corresponden a la base `427a5c1`; los tests
unitarios se repitieron tras la adaptación SSH en `e31efcb`.

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
| gentle-fleet-desktop | PASS | PASS | UNTESTED | UNTESTED | GPUI 0.2.2; fixture real ignorado y renderer sin validar; W3 |
| SSH Windows→Linux | UNTESTED | UNTESTED | UNTESTED | UNTESTED | P5 anterior no repetido en W0; W4 |
| SSH Linux→Windows | UNTESTED | UNTESTED | UNTESTED | UNTESTED | Bridge y shell Windows; W4 |

Los tests de proceso Linux basados en Bash, SIGKILL y permisos Unix no están
adaptados para ejecución Windows; su éxito en Linux no modifica esta matriz.
`fleetd` y `fleetctl` no tienen pruebas unitarias en los binarios (0 ejecutadas).
Los PASS de tests excluyen los probes opt-in ignorados y código `cfg(unix)`.
