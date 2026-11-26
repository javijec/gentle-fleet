# P7-W1 — plan de pruebas

**W1-A:** `cargo build -p fleetd -p fleet-runner -p fleetctl --locked`;
`pwsh -NoProfile -File tests/windows/w1a-token-isolation.ps1`. El fixture crea
daemon y dos runners FakeDriver en un runtime aislado; comprueba que el token
del daemon no cambia, que los tokens son distintos y que el daemon rechaza el
token de cada runner y un token inválido. Usa timeouts y termina solo procesos
registrados. Los tokens no se imprimen.

El fixture también inicia dos runners independientes y comprueba rechazo
cruzado y aceptación del token propio. W1-A y regresión Linux pasaron en
[CI 45238966908](https://github.com/javijec/gentle-fleet/actions/runs/45238966908).
La prueba actual
utiliza TCP y deberá adaptarse explícitamente al nuevo transporte en W1-B.

**W1-B:** autenticar mismo usuario y rechazar otro usuario en una instalación
controlada, DACL, colisiones, rechazo de tokens e identidad, handles, EOF y
ausencia de listeners de red. Acceso a otra cuenta sin autorización apropiada
se registra BLOCKED, no PASS.

Primera prueba negra: `pwsh -NoProfile -File tests/windows/w1b-named-pipe.ps1`.
Actualmente FAIL contra TCP como se espera antes de implementar W1-B; todavía
no se ha añadido a CI y no sustituye las pruebas de seguridad enumeradas. Tras
detectar pipe, la prueba intenta conexión con token inválido y arranque de un
segundo daemon en el mismo runtime: ambas comprobaciones están UNTESTED hasta
que exista el backend; no demuestran aislamiento entre usuarios.

**W1-C–F:** rutas, permisos, WAL, crash real, reattach, deduplicación,
concurrencia, aprobaciones, backpressure, CI y smoke manual enumerados en
`P7_W0_AUDIT.md`; siguen UNTESTED.

**Backend W1-B en revisión:** ejecutar `cargo test -p fleet-protocol --locked`
en Windows y los dos scripts nativos `w1a-token-isolation.ps1` y
`w1b-named-pipe.ps1`, ahora incorporados al job Windows. El test Rust
`half_close_drains_pending_reply_without_truncation` comprueba EOF de escritura
y respuesta tardía; `full_shutdown_wakes_pending_reader_without_dropping_client`
comprueba cancelación. Agregar prueba de otro SID y de backpressure con
`Shutdown::Both` antes de cerrar W1-B. Todos los resultados pendientes son
UNTESTED o BLOCKED, nunca PASS por inspección de código.

### Pruebas posteriores a corrección ACL

Antes de marcar W1-B como PASS, ejecutar en Windows 11 nativo
`cargo fmt --all -- --check`, `cargo check --workspace --locked`,
`cargo clippy --workspace --all-targets --locked -- -D warnings`,
`cargo test --workspace --locked` y `cargo build --workspace --locked`.
Luego ejecutar ambos scripts `tests/windows/w1a-token-isolation.ps1` y
`tests/windows/w1b-named-pipe.ps1`. Ambos ahora comprueban ACL protegida
de archivos de credenciales/dirección además de la autenticación y bridge.

Registrar SID y `AreAccessRulesProtected` para el directorio de runtime,
archivos .addr y .token (sin imprimir tokens), los permisos efectivos de
`DELETE_CHILD` del directorio y un intento real de acceso desde otra
cuenta local. El cambio de archivo con `CREATE_NEW` debe probarse al
reiniciar tras un archivo obsoleto y frente a un competidor de endpoint.
Si Actions sigue bloqueado por facturación, registrar Actions como BLOCKED y
aplicar el gate local equivalente de QUALITY.md. No atribuir PASS a Actions.
Eliminar fallas reales de seguridad antes de W1-C.

## Alternativa de ejecución local

Mientras Actions no arranque, se aplica la política de CI local de QUALITY.md y LOCAL_CI.md: full en Windows y Linux sobre el mismo SHA limpio, consolidación y revisión de logs. Esto sustituye únicamente el gate automatizado; Actions sigue BLOCKED y la prueba entre usuarios, integridad del directorio, identidad y cancelación bajo backpressure continúan siendo requisitos independientes. No hay PASS de W1-B por crear estos scripts.


### Prueba manual con dos usuarios ordinarios autorizados

Compilar binarios en el checkout del propietario con el perfil full.
En la sesión Windows no elevada del propietario ejecutar:

    pwsh -NoProfile -File tests/windows/w1b-cross-user.ps1 -Mode Owner

Mantener esa consola abierta. En otra sesión Windows no elevada, de una
cuenta de prueba ya autorizada y SID diferente, ejecutar una copia del mismo
script (no copiar binarios, tokens ni archivos runtime del propietario):

    pwsh -NoProfile -File w1b-cross-user.ps1 -Mode Probe -OwnerSid '<SID>' -Address '<Address>' -Runtime '<Runtime>'

Usar exactamente los tres valores no secretos que imprime Owner. El probe
exige Access Denied explícito; timeout o archivo inexistente no son PASS.
Solo se intenta borrar un archivo de dirección del fixture aislado, nunca
del runtime real. Volver a la consola Owner, pulsar Enter y verificar el
control positivo posterior. Conservar ambas salidas sin credenciales.
No crear cuentas ni alterar ACL del sistema sin autorización. Si no existe
la segunda sesión autorizada, registrar BLOCKED. La prueba no certifica
reparse points en antecesores ni el caso PID reutilizado.

El fixture actual no cubre todos los intentos requeridos de modificación,
reemplazo, DACL y renombrado, ni verifica que un antecesor no cause la
denegación. Debe ampliarse antes de utilizarlo como evidencia de cierre.
Además falta una prueba sincronizada de la carrera real timeout/completado.


### Fixture ampliado (reemplaza los comandos Owner anteriores)

Desde la segunda cuenta ordinaria autorizada, obtener solo su SID:

    [Security.Principal.WindowsIdentity]::GetCurrent().User.Value

Desde la cuenta propietaria ordinaria:

    pwsh -NoProfile -File tests/windows/w1b-cross-user.ps1 -Mode Owner -ProbeSid '<SID de segunda cuenta>'

El padre por defecto es CommonDocuments; si no admite creación/traversal,
usar `-FixtureParent` con una carpeta de prueba autorizada accesible desde
ambas cuentas, sin modificar permisos de carpetas del sistema. El script
concede acceso únicamente a su raíz y control desechables, no al runtime.
En la segunda cuenta ejecutar Probe como arriba. Debe pasar el control
positivo de traversal y derechos de su carpeta control, y Access Denied
para todos los accesos de pipe, archivos y runtime. WRITE_DAC, DELETE,
DELETE_CHILD y escritura se prueban abriendo handles con derechos exactos,
sin modificar las ACL ni el contenido. La comprobación del propietario
antes/después usa Hello real vía bridge. La prueba es manual y no equivale
a probar reutilización de PID real ni escenarios de administradores.

## Criterio vigente de traspaso W1-B → W1-C (24/11/2026)

Esta sección prevalece sobre los criterios históricos de cierre de este
documento. El propietario decidió no crear otra cuenta Windows. El fixture
entre dos SID queda intacto, UNTESTED y diferido al release P7; no bloquea
el traspaso administrativo ni la aceptación técnica limitada por sí solo.
No se sustituye por una simulación ni se declara PASS.

Gate de entrada obligatorio a la implementación W1-C: full Windows y Linux
del mismo SHA limpio, reporte consolidado PASS, revisión de logs, protocolo
Windows 10/10 con paralelismo normal, W1-A y W1-B PASS, ACL automatizadas,
identidad, cancelación/backpressure, runtime y reinicio sin defectos P0/P1
confirmados. Registrar resultados de identidad y cancelación que aún no se
puedan reproducir; no ocultar fallos ni eximirlos por el diferimiento.
Usar `javiomarchi` para SSH con host key checking. No crear cuentas. El
perfil release conserva `cross-user-ipc-acl` y los demás gates manuales.

## Ejecución conjunta del gate W1-C

Usar `scripts/ci/w1c_entry.py` desde Windows con el checkout Linux en el mismo SHA; ver [plan W1-C](P7_W1_C_PLAN.md). El resultado AUTOMATED_PASS requiere todavía revisión de logs y criterios de seguridad. No modifica el gate ni registra PASS manual.


## W1-F — aceptación integral de desarrollo

Ver [P7_W1_F_PLAN.md](P7_W1_F_PLAN.md). Gate modo w1f conserva full nativo de ambas plataformas, protocolo Windows 10/10 y agrega Lifecycle Windows 20/20. La prueba negra W1-F se ejecuta dentro de full Windows; no sustituye los requisitos manuales de release. Estado candidato UNTESTED.
