# P7-W1 — resultados

**Estado: ABIERTO.** Base `7701bba`; W0 CI
[#45234810431](https://github.com/javijec/gentle-fleet/actions/runs/45234810431)
PASS en Windows, backend Linux y desktop Linux.

## W1-A — token de daemon y runners

- Antes del arreglo, `pwsh -NoProfile -File tests/windows/w1a-token-isolation.ps1`
  falló determinísticamente con `daemon token changed after runner 0 started`.
- Tras el arreglo y `cargo build -p fleetd -p fleet-runner -p fleetctl --locked`,
  el mismo test pasó en Windows 11: dos runners y dos tokens distintos, token
  del daemon estable, rechazo explícito de ambos tokens de runner y de un token
  inválido por el daemon; se enviaron prompts y paradas. No se registraron
  secretos. Los procesos de fixture se terminaron por handles registrados.
- `cargo test -p fleet-protocol --locked` y `cargo fmt --all -- --check`: PASS
  en Windows. [CI 45238966908](https://github.com/javijec/gentle-fleet/actions/runs/45238966908)
  sobre `50451af`: PASS en Windows, backend Linux con E2E y desktop Linux;
  incluye la regresión de tokens W1-A.
- Rechazo cruzado: PASS en Windows con dos runners FakeDriver de fixture
  independientes; cada runner rechazó el token del otro y aceptó su propio
  token e identidad. **W1-A: PASS**, sin afirmar que TCP/token sea IPC seguro.

## W1-B–F

W1-B está pendiente de diseño e implementación segura. El bridge depende de
half-close y cancelación bidireccional, aún no adaptados a named pipes. La
prueba entre usuarios está BLOCKED: la sesión no tiene token administrativo
habilitado y no se ha configurado otra identidad de prueba. Named pipes, DACL,
persistencia, supervivencia del runner y E2E exhaustivos no se implementaron ni
probaron.
No se declara compatibilidad funcional ni seguridad del transporte Windows.

### W1-B — primera regresión de proceso

En `a151038`, con los binarios nativos de W1-A:
`pwsh -NoProfile -File tests/windows/w1b-named-pipe.ps1` **FAIL**:
`Windows daemon endpoint is not a named pipe`. El fixture arranca y termina
únicamente su daemon, en runtime y DB aislados. La prueba es intencionalmente
roja mientras W1-B no se implemente; aún no está integrada en CI. Por sí sola
no comprueba DACL, identidad, colisiones, EOF ni ausencia de puertos de red.
La versión ampliada también define comprobaciones de rechazo de token inválido
y colisión con un segundo daemon, pero todavía no llega a ejecutarlas porque
falla primero en la ausencia de named pipe. No se cuentan como PASS.

El run W0 de cierre
[#45237481862](https://github.com/javijec/gentle-fleet/actions/runs/45237481862)
sobre `7701bba` terminó **FAIL** en `windows-native`: el test
`fleet-core::tests::ack_failure_is_reported_instead_of_silently_replaying`
falló con `store_config_failed: database is locked`. Ambos jobs Linux pasaron.
El [CI posterior de W1-A](https://github.com/javijec/gentle-fleet/actions/runs/45238966908)
pasó, pero una repetición exitosa no explica ni corrige ese fallo; investigar
el bloqueo de SQLite y su posible concurrencia como parte de W1-C, sin
deshabilitar el test.

## W1-B — implementación candidata y estado de validación

Tras el fixture rojo inicial se publicó una implementación Windows del IPC
con `windows-sys`: named pipes con DACL protegida para SID propietario,
validación de identidad de proceso cliente/servidor, protección de la primera
instancia, prohibición de conexiones remotas de pipe y tokens por endpoint.
El cliente ya no usa TCP loopback. Los clones comparten handle y el transporte
interpreta un NUL reservado como EOF de la dirección de escritura, sin cerrar
la lectura; `CancelIoEx` se solicita en cancelación total.

Cambios concretos: `crates/fleet-protocol/src/windows_pipe.rs`,
`transport.rs`, `Cargo.toml` y `Cargo.lock`; regresión W1-A adaptada a
named pipes, prueba W1-B de endpoint/autenticación/colisión agregada a CI y
tests Rust de half-close/respuesta pendiente/cancelación.

**Validación en curso:** la primera iteración del backend falló compilación
Windows por imports de Win32 y Rustfmt; se corrigieron ambos defectos en
commits posteriores. El PASS de backend Linux de esas iteraciones no acredita
funcionalidad Windows. Hasta finalizar los nuevos jobs Windows y Linux no se
registra PASS de W1-B. La prueba real con un usuario SID diferente permanece
**BLOCKED** (sin segunda cuenta de prueba autorizada). La cancelación en
condiciones de backpressure y la ausencia de bloqueo de `Shutdown::Both`
requieren evidencia adicional. **W1-B ABIERTA.**

## W1-B — ejecución y hallazgos de CI durante la implementación

- Se sustituyó el transporte Windows TCP loopback por named pipes nativas. El código incorpora DACL protegida por SID, verificación de SID en ambos extremos, `FILE_FLAG_FIRST_PIPE_INSTANCE`, y separación entre medio-cierre ordenado y cancelación abortiva.
- En [CI 45249957541](https://github.com/javijec/gentle-fleet/actions/runs/45249957541), el job `windows-ipc` compiló daemon, runner, CLI y bridge, pasó el protocolo y la regresión W1-A, pero **falló W1-B** por timeout del bridge después de EOF de stdin. El fixture se instrumentó y se cambió `ListAgents` (que ejecuta sondeos nativos) por `ListWorkspaces` para aislar el transporte.
- En [CI 45251457404](https://github.com/javijec/gentle-fleet/actions/runs/45251457404), pasaron 10 de 11 tests del protocolo; el test de cierre físico falló por una carrera del fixture: el cliente cerraba antes de que `ConnectNamedPipe` terminara. Se añadió una barrera de aceptación en el test (`1413d4d`).
- **Estado al redactar:** el CI del último commit continúa pendiente. No existe aún una corrida verde completa del nuevo backend. La prueba de acceso desde una segunda cuenta Windows autorizada sigue BLOCKED, y el caso de cierre bajo backpressure no tiene evidencia exhaustiva.
- W1-B permanece **ABIERTA**. No se empezó W1-C ni se hizo merge.
