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

## W1-B — diagnóstico y correcciones posteriores a `2525d2b`

- El CI [45252231986](https://github.com/javijec/gentle-fleet/actions/runs/45252231986)
  falló: `fleet-conformance` aún referenciaba `LocalStream::Tcp`, Clippy
  rechazaba una conversión redundante y el bridge no terminaba tras EOF.
  Ambos jobs Linux pasaron.
- En Windows nativo se reprodujo el deadlock con
  `concurrent_read_does_not_prevent_cloned_writer_from_sending`: una lectura
  síncrona bloqueaba la escritura en un clon del mismo handle. Se cambió a
  I/O superpuesta con un evento por operación y cancelación; el test pasó.
  `fleet-conformance` usa ahora la variante pipe con timeouts de lectura y
  escritura; el nuevo test de timeout comprueba que la conexión sigue usable.
- Tras estos cambios, `cargo test -p fleet-protocol --locked`: PASS (13 tests);
  compilación de `fleet-conformance`: PASS; los fixtures de proceso W1-A y
  W1-B: PASS local, incluida recepción de las dos respuestas del bridge tras
  EOF. Estas pruebas no demuestran toda la seguridad ni recuperación.
- En Windows 11 nativo, tras `b43a22f`: fmt, check, Clippy `--all-targets`,
  test y build del workspace completo con `--locked`: PASS. Permanecen
  ignorados los dos probes opt-in existentes de Gentle Shell real y E2E GPUI
  Linux; no son evidencia de esas integraciones en Windows.
- **FAIL de seguridad:** un daemon aislado en `%TEMP%` creó `fleetd.addr` y
  `fleetd.token` con `AreAccessRulesProtected=False`, heredando permisos de
  modificación para otras identidades/grupos. Se comprobó con `Get-Acl`, sin
  mostrar el token. No existe aún protección explícita de estos archivos ni
  prueba de rechazo real entre cuentas.
- **Pendientes críticos:** corregir la ACL de los archivos de dirección y
  credenciales sin ventanas de exposición; revisar y probar la carrera entre
  registrar I/O y `CancelIoEx`, cierre bajo backpressure, e identidad del peer
  frente a reutilización de PID. El rechazo real de otro usuario está BLOCKED
  por falta de cuenta controlada. No se declara W1-B PASS.
- **CI BLOCKED:** los runs
  [45256707164](https://github.com/javijec/gentle-fleet/actions/runs/45256707164)
  y [45256771289](https://github.com/javijec/gentle-fleet/actions/runs/45256771289)
  no iniciaron sus jobs. GitHub informa: `The job was not started because
  recent account payments have failed or your spending limit needs to be
  increased`. Revisar Billing & plans; estos FAIL no son resultados de tests.

## W1-B — corrección de ACL de archivos (verificación pendiente)

Para corregir el FAIL nativo registrado sobre `fleetd.addr` y
`fleetd.token`, se agregó `windows_pipe::write_private_file`. La función
crea los archivos con `CreateFileW(CREATE_NEW)` y un descriptor de seguridad
explícito `D:P(A;;GA;;;<SID actual>)` **antes de escribir el contenido**.
No utiliza `CREATE_ALWAYS` ni modifica un archivo preexistente, porque
Windows ignora el security descriptor suministrado al abrir un archivo
existente. El archivo anterior se desvincula y una recreación inesperada
provoca fallo cerrado. Se aplica al token y address del daemon, al address de
cada runner y al token de cada runner.

Las regresiones `tests/windows/w1a-token-isolation.ps1` y
`tests/windows/w1b-named-pipe.ps1` comprueban que las ACL de archivos
sensibles no sean heredadas y que ningún ACE de permiso incluya otro SID,
sin imprimir sus contenidos. El parche está publicado, pero estas pruebas
**NO HAN SIDO EJECUTADAS contra este commit**: GitHub Actions está rechazando
jobs por facturación/límite de gastos. No contabilizarlo como PASS.

**Riesgos aún abiertos:** el directorio `FLEET_RUNTIME_DIR` puede permitir
`DELETE_CHILD` a otros sujetos aun cuando cada archivo tenga DACL privada;
debe validarse/protegerse de manera atómica antes de certificar integridad del
endpoint. Persisten la prueba desde otro usuario Windows, auditoría de
reutilización de PID en verificación SID, cancelación bajo backpressure y CI
Windows/Linux completo. **W1-B sigue ABIERTA.** Sin merge a main y sin W1-C.


## CI local — regresión SQLite durante pruebas paralelas (24/11/2026)

Evidencia proporcionada por el usuario: perfil full local sobre `e8b3383`
en Windows 11 y Linux Omarchy: todas las comprobaciones salvo rustfmt PASS.
Tras corregir formato, ambos hosts validaron `a9d1d7d`: Linux full PASS,
Windows 10/11 checks PASS (incluidos W1-A y W1-B), pero el test
`fleet-core::tests::ack_failure_is_reported_instead_of_silently_replaying`
volvió a fallar al abrir SQLite con `store_config_failed: database is locked`,
igual que el antiguo CI Windows.

Hipótesis concreta: el helper de fixtures `unique_db_path` usaba PID y
SystemTime, sin contador ni reserva exclusiva; llamadas paralelas con igual
timestamp podían abrir la misma base de prueba. Se cambia únicamente el helper
de tests para usar contador atómico y creación exclusiva de directorios
(privados en Unix), con reintento solo ante colisión de nombres y una regresión
de 16 llamadas concurrentes con timestamp idéntico. No se cambian WAL,
busy_timeout, semántica de ACK ni paralelismo de Cargo. La causa concreta
de la ejecución histórica no está demostrada mediante trazas de rutas;
la corrección elimina esa posibilidad de colisión.

**Validación pendiente:** repetir fleet-core con paralelismo normal en Windows
y full local en ambos hosts sobre el nuevo SHA. Si vuelve a ocurrir el bloqueo,
investigar apertura/configuración de SQLite en bases realmente independientes.
GitHub Actions sigue BLOCKED; no se declara W1-B cerrada ni se inicia W1-C.

## W1-B — auditoría y regresiones adicionales (24/11/2026)

Base `175fad22ed1478d0f8d8d7822361a3c2b45d114b`: el usuario
reporta full Windows y Linux PASS y 10/10 ejecuciones de fleet-core Windows
PASS. Estos resultados corresponden a esa base, no al commit que añade las
regresiones siguientes. Actions sigue **BLOCKED** por facturación.

- **PASS**, Windows nativo, `cargo test -p fleet-protocol --locked`: 14 tests,
  incluido `full_shutdown_cancels_pending_write_under_backpressure`. Este
  confirma `ERROR_IO_PENDING` para una escritura overlapped de 8 MiB hacia
  un peer que no lee, solicita `Shutdown::Both` y exige que termine en 3 s.
  No cubre todas las carreras de cancelación.
- **PASS**, Windows nativo, `pwsh -NoProfile -File
  tests/windows/w1b-named-pipe.ps1`: además de los controles existentes,
  mata el daemon, conserva `.addr` y `.token`, reinicia y comprueba un Hello
  autenticado mediante el bridge real. No es una prueba de reattach de runner.
- **UNTESTED / riesgo:** `read` y `write` pueden obtener una referencia al
  handle antes de `Shutdown::Both`; `CancelIoEx` puede ejecutarse antes de
  registrar su operación overlapped. Esa secuencia no queda cubierta por la
  prueba de escritura ya pendiente y requiere regresión sincronizada y arreglo
  si se reproduce. `Shutdown::Read` cancela todas las operaciones del handle,
  no solamente lecturas; su interacción con escritura también requiere prueba.
- **UNTESTED / riesgo:** `GetNamedPipe*ProcessId` seguido de `OpenProcess`
  no demuestra que el PID no haya sido reutilizado entre ambas llamadas.
  La DACL del pipe y el token aportan defensas separadas, pero no sustituyen
  una prueba o solución de identidad vinculada al peer.
- **UNTESTED / riesgo:** el runtime comprueba propietario y DACL protegida
  exacta y rechaza reparse points en el último componente, manteniendo un
  handle sin compartir DELETE. Esto no acredita por inspección el tratamiento
  de reparse points en componentes antecesores ni permisos efectivos
  `DELETE_CHILD` mediante otra identidad.
- **BLOCKED:** prueba efectiva entre dos SID. El usuario confirma cuentas
  autorizadas y ofrece ejecutar el componente en la segunda sesión, pero
  todavía no hay fixture ni evidencia de rechazo ejecutada entre sesiones.
  No se han creado cuentas ni registrado credenciales.
- **UNTESTED:** validación full Windows y Linux del nuevo SHA limpio y
  consolidación. W1-B permanece **ABIERTA**; no comenzar W1-C ni interpretar
  este documento como certificación de seguridad.


## W1-B — cierre de carrera de cancelación y prueba entre cuentas (candidato)

Se conservó el commit previo `a7c234a` (backpressure y reinicio con archivos
obsoletos; PASS nativo consignado en el apartado anterior). El fallo de diseño
identificado es la ventana entre clonar el handle y registrar ReadFile/WriteFile:
CancelIoEx podía ejecutarse antes de la nueva operación. El gate de envío por
stream ahora serializa únicamente el inicio de I/O y la cancelación; nunca se
mantiene durante la espera. Shutdown::Read cancela el OVERLAPPED de lectura
registrado y no todas las escrituras. Se añade una regresión con handle
retenido antes de shutdown e intento de envío posterior; se endurece la
regresión de escritura pendiente para exigir un error de cancelación.
**UNTESTED** en este SHA: compilación y pruebas Windows (incluido shutdown
direccional), Linux full y consolidación. No se ha reproducido el fallo
histórico de carrera en un binario anterior desde este entorno.

Identidad: se fija propietario SID explícito del objeto pipe y se verifica
por handle en el cliente antes del intercambio. Es una comprobación
independiente del PID y no sustituye la DACL del servidor. La consulta PID /
OpenProcess sigue siendo una defensa adicional, no una prueba exacta de
identidad ante reutilización; el análisis de esa ventana y el rechazo
entre cuentas reales permanecen abiertos. No se atribuye PASS a la auditoría.

Nuevo fixture manual `tests/windows/w1b-cross-user.ps1`: propietario y
segunda cuenta ordinaria autorizada, controles positivos antes/después y
negación explícita de conexión, enumeración, lectura y borrado de un
endpoint desechable. No crea cuentas ni imprime secretos. **BLOCKED** hasta
ejecutar ambos modos en Windows y obtener evidencia. El rechazo de reparse
points del último componente y el guard de runtime existente se conservan;
la seguridad de antecesores/reparse points de ruta completa sigue pendiente
de solución y pruebas. GitHub Actions sigue BLOCKED por facturación.
**W1-B ABIERTA; no merge ni W1-C.**


### Endurecimiento adicional (candidato, UNTESTED)

El nombre de pipe derivado de la ruta textual podía distinguir alias de la
misma carpeta (por ejemplo distinto casing en NTFS), dejando que un daemon
competidor reservara otro pipe y modificara los archivos de endpoint. Se usa
ahora la identidad de directorio del handle (volumen e índice de archivo)
para el nombre, con SID e instancia, y se conserva el guard durante el
listener. Se añade intento de colisión con spelling alternativo del runtime.
El guard abre los antecesores de raíz a hoja y rechaza reparse points en
cada componente; retiene handles sin compartir DELETE. No se afirma que
esto cubra todas las mutaciones de namespace/ACL por identidades privilegiadas,
ni rutas UNC o filesystem no probado. Se añade regresión de Shutdown::Read
con escritura pendiente para exigir que la escritura sobreviva.
Todos estos cambios requieren ejecución Windows nativa y full en ambos hosts.

El fixture de proceso añade además una junction real en un antecesor hacia un runtime nuevo: exige rechazo sin publicar endpoint. La prueba de alias verifica previamente que el spelling alternativo llega a los archivos activos. Ambos casos siguen UNTESTED en este commit.

## W1-B — revisión de timeout (candidato posterior a 4342725)

- **P1, sin cerrar:** PID reutilizado entre consulta de pipe y apertura de
  proceso; el propietario del objeto y la DACL son defensas adicionales, no
  demuestran la identidad efectiva del peer. No se ha implementado aún una
  verificación vinculada al token efectivo de la conexión.
- **P2, corrección candidata:** tras timeout o fallo de espera, una operación
  overlapped puede completar antes de `CancelIoEx`. Antes se devolvía siempre
  el error de espera aunque `GetOverlappedResult` informase éxito. Ahora se
  devuelve la cantidad realmente transferida; `ERROR_OPERATION_ABORTED`
  conserva el error de espera y otro error de operación se propaga. Se mantiene
  vivo OVERLAPPED y buffer hasta la finalización. No se añade reintento.
- **PASS parcial en Windows:** 10/10 ejecuciones de `cargo test -p
  fleet-protocol --locked --quiet` con 17 tests, incluyendo clasificación de
  éxito/cancelación/error y pruebas nativas existentes. La nueva prueba es
  unitaria; una carrera real del kernel y fallos de CancelIoEx siguen
  **UNTESTED**. La prueba se añadió antes de la función y falló compilación
  por símbolo ausente; esto no es una reproducción de proceso del defecto.
- **BLOCKED:** dos usuarios Windows. El fixture existente aún no comprueba
  todos los accesos exigidos ni descarta denegaciones de antecesores; no debe
  aceptarse como evidencia de cierre sin corregirlo y ejecutarlo.
- Alias SSH `linux`: **BLOCKED** por resolución de nombre; `javiomarchi`
  conecta con host key checking habilitado. GitHub Actions **BLOCKED** por
  facturación. El full de ambos sistemas y consolidación de este candidato
  están pendientes. **W1-B ABIERTA; NO-GO W1-C.**


## W1-B — identidad efectiva por mensaje (candidato)

Se conserva la corrección timeout de `d7e4a28` y su evidencia previa. Para
evitar que la consulta por PID sea la única comprobación de identidad del
cliente, el servidor comprueba el SID del token de seguridad del último
mensaje leído antes de entregar bytes al decoder: ImpersonateNamedPipeClient,
OpenThreadToken(TOKEN_QUERY, OpenAsSelf), RevertToSelf y consulta del token
después de revertir. Si no puede revertir, termina el proceso. No ejecuta
operaciones de aplicación bajo impersonación. Los clientes Fleet solicitan
SECURITY_IDENTIFICATION mediante SECURITY_SQOS_PRESENT para limitar lo que
el servidor puede hacer con su identidad. La comprobación previa de PID,
DACL y propietario del objeto del servidor se conservan como capas.
Se añade una regresión de cliente con nivel anónimo; sus bytes no deben
llegar al decoder. Las regresiones existentes prueban tráfico normal.
**UNTESTED:** ejecución nativa, compatibilidad de clientes .NET y todos los
gates del candidato. La identidad exacta del proceso servidor y la
delegación deliberada de handles por el propio usuario siguen fuera de
lo demostrado; no se certifica reutilización de PID por inspección.
Referencia: https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-impersonatenamedpipeclient
y https://learn.microsoft.com/en-us/windows/win32/ipc/impersonating-a-named-pipe-client.
