# P7-W0: auditoría de portabilidad Windows

Base auditada: `427a5c1` (`feature/p6-2-gpui-desktop`). Esta auditoría no
declara paridad Windows. Las mediciones y pendientes están en
[P7_W0_RESULTS.md](P7_W0_RESULTS.md) y [P7_WINDOWS_MATRIX.md](P7_WINDOWS_MATRIX.md).

## Hallazgos y prioridades

1. **P0, seguridad y funcionamiento:** `fleet-protocol/src/transport.rs` usa
   TCP en `127.0.0.1` con dirección y token en ficheros; no implementa el
   named pipe con DACL y comprobación del token del cliente requerido por
   `SECURITY_AND_PLATFORMS.md`. La privacidad depende de ACL heredadas que no
   se establecen ni comprueban. En Windows `bind_named` escribe siempre
   `fleetd.token`, incluso para runners; la prueba nativa confirmó que al
   iniciar un runner el cliente recibe `Unauthorized` porque el daemon
   conserva el token anterior en memoria. Las ACL
   de DB, WAL, spool y tokens tampoco están verificadas. No usar esto como
   garantía de aislamiento entre usuarios.
2. **P0, tests:** el test SSH `hello_is_rewritten_with_remote_token` llama a
   `expected_token()` sin fixture Windows; requiere un fichero real en el
   runtime y falla si no existe; fallo reproducido en `427a5c1`. El test se
   adaptó con token inyectado sin desactivar la comprobación ni cambiar el
   entorno global de otros tests; la repetición completa pasó en Windows.
3. **P1, procesos:** `fleetd/src/process_runner.rs` busca un ejecutable hermano
   llamado `fleet-runner` sin extensión explícita; el comportamiento real de
   `Command` Windows y las rutas con espacios requieren prueba. `Child` y
   stdout se usan para handshake; no hay prueba Windows de independencia del
   padre, handles, cierre o TerminateProcess y reconciliación. El runner usa
   token propio y spool SQLite; no confundir su autenticación con la del daemon.
4. **P1, almacenamiento:** SQLite se compila con `bundled`, usa WAL,
   `busy_timeout=5000` y claves foráneas por conexión. Las restricciones de
   permisos están bajo `cfg(unix)`. Si faltan `HOME` y `XDG_DATA_HOME`, la DB
   cae en el directorio temporal; su persistencia y ACL no están garantizadas.
   Falta comprobar bloqueo, rutas con espacios, unidades y UNC, recuperación
   WAL tras terminación abrupta y limpieza segura.
5. **P1, rutas de usuario y SSH:** `fleet-ssh` usa `HOME` o `/tmp` para
   hosts.json; el SDK usa `HOME`/`XDG_STATE_HOME` para operaciones durables;
   Pi también cae en `/tmp`. Windows suele proporcionar `USERPROFILE` en
   lugar de `HOME`. Se debe comprobar configuración, `known_hosts`, claves,
   rutas con espacios y shell remoto Windows sin relajar host-key checking.
   `HostConfig::validate` exige bridge absoluto iniciado por `/` y solo acepta
   caracteres inertes Unix; una ruta de unidad Windows y una ruta con espacios
   no pasan. También rechaza espacios en `identity_file`. No basta con cambiar
   la validación: el comando remoto pasa por el shell del servidor y requiere
   una estrategia segura de quoting específicamente probada.
   La validación existente Windows→Linux de P5 no demuestra Windows como host.
6. **P2, desktop:** GPUI está fijado a 0.2.2; el backend Windows y sus
   dependencias nativas, renderer, ciclo de vida y llamadas mediante
   `fleet-client` requieren resultados independientes de compilación y GUI.
   No se ha probado una ventana ni el renderer.
7. **P2, suite:** los scripts `tests/*.sh` incluyen señales, permisos y sockets
   Unix y se mantienen en Linux CI. No se han portado ni deben contarse como
   E2E Windows. Los tests Rust con `cfg(unix)` no prueban su equivalente
   Windows. Los tests opt-in de proveedores reales no son sustituidos por fake.

## Plan ejecutable posterior, sin iniciar ahora

### W1 — Core, IPC, persistencia y runners

1. Fijar pruebas de proceso aisladas para token estable entre dos conexiones,
   un runner y varios runners, y rechazo de otro usuario; reproducir el cambio
   de token antes de corregirlo. Diseñar named pipe + DACL por SID y verificación
   de cliente; preservar framing y bridge o documentar incompatibilidades.
2. Resolver directorios durables por usuario y ACL explícitas de runtime,
   tokens, DB/WAL/spool. Probar rutas con espacios, unidades y UNC, accesos
   concurrentes y recuperación ante terminación; no almacenar datos persistentes
   por defecto en temporales.
3. Probar y corregir resolución del ejecutable, handles y ciclo completo
   daemon→runner→FakeDriver→eventos→API, cierre explícito y desconexión de
   cliente; inyectar TerminateProcess únicamente en procesos de fixture.
4. Validar supervivencia del runner, identidad/reattach, spool/ACK, replay sin
   huecos, duplicados y estado incierto ante caídas. Nunca reenviar prompts
   inciertos ni autoaprobar. Ejecutar Linux y Windows en CI.

### W2 — proveedores nativos

Tras W1, probar descubrimiento y versiones reales de OpenCode, Codex, Pi y
Gentle Shell; ejecutar conformidad, procesos, permisos interactivos, errores,
interrupción y recuperación por proveedor. Registrar binario y versión reales,
sin credenciales en logs ni inferir soporte desde fixtures.

### W3 — GPUI Windows

Resolver primero errores del backend/dependencias, después tests y build GPUI
0.2.2 en MSVC; verificar creación/cierre de ventana, renderer y consultas SDK
locales con daemon aislado, DPI y recuperación de errores. No implementar chat
ni rediseñar la interfaz en W0.

### W4 — SSH bidireccional

Con W1 disponible, validar Windows→Linux y Linux→Windows y Windows→Windows
con OpenSSH real, claves y `known_hosts`, configuración de usuario, rutas
seguras y quoting del shell remoto, bridge, reconexión y cierre. Rechazar claves
desconocidas y no introducir TCP público. Registrar versiones y limitaciones.
