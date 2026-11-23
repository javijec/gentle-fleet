# Seguridad, plataformas y red

## Modelo de confianza

MVP **personal, un usuario por host**. Fleet daemon, runners y agentes operan con el usuario propietario. El entorno de ese mismo UID (Unix) / token de sesión (Windows) se considera confiable para sus propios archivos y credenciales. No se afirma aislamiento entre procesos de igual usuario. Se protegen las fronteras entre hosts, usuarios distintos y clientes que carecen de permisos.

Las solicitudes de herramienta y approvals del agente mantienen una segunda frontera lógica: incluso un cliente autorizado no obtiene autoaprobación, y el SessionManager decide cuándo se permite responder a una aprobación. Nunca confiar en `actor_claim` de JSON como identidad autenticada.

## IPC local

- Linux: Unix domain socket en runtime privado del usuario; autenticación por SO_PEERCRED (o equivalente soportado), modos y permisos apropiados.
- Windows: named pipe con DACL restringida al usuario propietario; derivar identidad del token del cliente y verificar acceso. No usar ACL equivalente a Everyone.
- Ni named pipe ni Unix socket interno se exponen directamente por TCP; transporte público dentro de Fleet API tipado.
- La interfaz de plataforma está confinada a fleet-transport/fleet-runtime. fleet-domain/core/protocol compilan sin `/proc`, `setsid` ni señales Unix directas.

## Acceso remoto

Primera implementación: **SSH estándar** hacia un daemon Fleet existente en el host remoto. El cliente usa SSH de la máquina, agente de claves y host-key verification; el binario `fleet-ssh-bridge` en el host remoto traduce los frames autenticados al socket local de `fleetd`, reescribiendo el `Hello` con el token del host remoto. No implementar autenticación propia sobre TCP en P5 ni exponer 0.0.0.0.

Cada host posee su DB, directorios y sus procesos. La UI puede agregar vistas multi-host pero no sincroniza el estado internamente. Los IDs de sesión se interpretan como (host_id,session_id) para evitar colisiones. No se migran sesiones vivas entre hosts en v2 MVP.

## Workspace y credenciales

El workspace registrado debe ser directorio existente y accesible para el usuario; se establece de forma efectiva como `cwd` al iniciar el agente. No basta con registrar una ruta en SQLite. Evitar seguir symlinks en operaciones de seguridad sensibles cuando corresponda; no prometer protección frente a mutaciones del mismo UID. Las credenciales permanecen en las configuraciones nativas del proveedor; Fleet no las copia a su state ni las imprime.

Solo guardar secrets temporales de IPC cuando sean indispensables, con permisos de usuario, rotación por runner y eliminación segura si se verifica que pertenecen al proceso. Logs de debug nunca deben registrar env completo, auth headers o payloads privados sin opt-in explícito.

## Controles de permisos

Un AgentDriver puede informar permisos de ejecución, filesystem, red, selección o input. Cada solicitud se tipa con decisiones válidas y se vincula a un único turno/proveedor. La autorización de Fleet protege quién puede responder, pero **no sustituye** las decisiones/sandbox nativas de Codex, OpenCode o Pi.

## Compatibilidad de plataforma

| Tema | Linux | Windows |
|---|---|---|
| Daemon usuario | proceso de usuario; systemd --user opcional | proceso por sesión de usuario, autostart opcional |
| IPC | Unix socket | named pipe |
| Procesos persistentes | runner desacoplado y supervisado | runner con manejo explícito de job/process handles; probar que no muere con el padre |
| Descubrimiento | handshake + descriptor, sin depender de /proc para Core | handshake + handles/token/SID, no emular /proc |
| Rutas | PathBuf, case sensitivity según FS | PathBuf, drive/UNC verificados; no concatenar strings |
| SSH remoto | OpenSSH | OpenSSH client de Windows |
| Crash tests | SIGKILL y restart | TerminateProcess y restart |

El **cliente** Windows `fleetctl.exe` fue probado con OpenSSH en P5 contra Linux, lo que verifica la interoperabilidad de ese cliente pero **no** la ejecución de `fleetd`/runners nativos en Windows. No afirmar paridad de procesos Windows sin ejecutar los smokes nativos de P7. macOS queda fuera de release inicial aunque la capa Unix debe facilitar incorporarlo.

## No objetivos de seguridad inicial

Aislamiento de UID dedicado, contenedores por agente, host remoto multiusuario compartido, cero confianza entre procesos del mismo usuario y autenticar reinicio de procesos frente a root/administrador. Si se requiere alguno, escribir un ADR y añadir pruebas antes de declararlo admitido; no incrustarlo como excepción en el MVP.

**Implementación preliminar W1-B (no validación de seguridad):** el código
Windows incorporó named pipes con DACL protegida para el SID propietario,
comprobación del SID del peer, protección de primera instancia y un marcador
interno de half-close independiente de JSONL. La prueba entre usuarios
Windows distintos, los permisos de archivos sensibles y la cancelación ante
backpressure continúan sin validación completa. Consultar
`P7_W1_DESIGN.md` y `P7_W1_RESULTS.md`; no declarar IPC Windows seguro
solo porque el endpoint tenga un nombre de pipe.
