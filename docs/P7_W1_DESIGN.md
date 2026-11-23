# P7-W1 — diseño y límites actuales

**ABIERTO.** W1-A pasó su regresión nativa y CI. W1-B tiene una implementación inicial de named pipes en revisión y CI pendiente; W1-C–F no se iniciaron.
Este documento registra el comportamiento real, no una garantía futura.

## Identidades actuales

`LocalListener::bind()` crea el listener daemon con nombre `fleetd`; en
Windows crea `fleetd.token`, que `fleetd` lee una vez como token esperado y que
usan clientes locales y bridge. `fleet-runner` llama `bind_named` con nombre
`fleet-runner-<runner_id>` y `prepare_token` crea una credencial distinta en
`fleet-runner-<runner_id>.token`. `ProcessRunnerLauncher` lee ese token para
el handshake `RunnerRequest::Hello`, que también verifica la identidad.

En W0, `bind_named` escribía `fleetd.token` para cualquier nombre. W1-A limita
esa escritura al listener `fleetd`; los runners mantienen su propio token.
La corrección W1-A se validó originalmente sobre TCP. En W1-B se sustituyó la capa Windows por named pipes, pero todavía no hay evidencia completa de su seguridad. No confundir el PASS histórico W1-A con la validación de W1-B.

## Puerta de seguridad W1-B

Se implementó en la rama un backend named pipe con DACL protegida para el SID del usuario, comprobación del SID del proceso remoto y FILE_FLAG_FIRST_PIPE_INSTANCE. Falta validar el funcionamiento, la semántica de cancelación y el rechazo real de otra cuenta. No existe fallback TCP y **W1-B no está cerrada**.

El bridge `fleet-ssh::run_bridge` necesita `Shutdown::Write` tras EOF de
stdin mientras el hilo de salida continúa leyendo; también usa
`Shutdown::Both` para cancelar ante errores. Un `DuplicateHandle` de una pipe
no proporciona por sí solo ese half-close: no se puede sustituir por un cierre
incondicional ni por un no-op sin romper EOF, drenaje o cancelación. W1-B
requiere una estrategia explícita y pruebas de proceso para esos casos antes de
sustituir TCP.

La sesión Windows actual tiene el grupo Administradores marcado solo para
denegar; no se ha dispuesto una segunda identidad controlada ni ejecutado una
prueba entre usuarios. Esa prueba es **BLOCKED**, no una validación de DACL.

Según la documentación de Microsoft, una pipe con descriptor de seguridad
predeterminado puede conceder lectura a Everyone y Anonymous; se debe pasar
un descriptor explícito al crearla. Además, `FILE_GENERIC_WRITE` incluye
`FILE_CREATE_PIPE_INSTANCE`, por compartir bit con `FILE_APPEND_DATA`, por lo
que se deben evaluar derechos individuales y creación de la primera instancia
para evitar nuevos servidores no autorizados. Fuente:
[Named Pipe Security and Access Rights](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights).
Estos requisitos son criterios de diseño, no evidencia de implementación.

## Contrato observado por plataforma

En Unix, `LocalListener` y `LocalStream` usan sockets de dominio Unix; el
`try_clone()` permite lectura y escritura simultáneas y `shutdown(Write)`
propaga EOF sin cerrar el canal de respuesta. En Windows actual se usan TCP
loopback y `TcpStream`, con la misma semántica de mitad de conexión, pero el
token y la dirección se publican en ficheros sin DACL comprobada. Esto no
implementa la política de aislamiento de Windows.

Una pipe Windows dúplex no ofrece el mismo shutdown direccional por duplicar
un handle. La solución deberá especificar, probar y acotar cómo se representa
el EOF de entrada y cómo se despierta una lectura o escritura bloqueada sin
perder respuestas; no basta convertir `shutdown(Write)` en no-op. `fleetd` y
`fleet-runner` leen frames por una clonación y escriben por otra, y el SDK,
CLI, conformance y bridge también consumen el transporte; los cambios deben
mantener esas concurrencias y no reenviar prompts inciertos.

## Backend Windows propuesto (pendiente de validación)

- `crates/fleet-protocol/src/windows_pipe.rs` usa la API Win32 mediante `windows-sys 0.61.2`. Cada listener publica una ruta `\\\\.\\pipe\\gentle-fleet-...`, bajo nombre derivado del runtime/identidad local.
- `CreateNamedPipeW` usa una DACL explícita protegida `D:P(A;;GA;;;<SID>)`, handles no heredables, `PIPE_REJECT_REMOTE_CLIENTS` y `FILE_FLAG_FIRST_PIPE_INSTANCE` al reservar el primer endpoint. La comprobación de SID se repite en servidor y cliente mediante el PID del proceso par y su token Windows.
- Los tokens de W1-A siguen separados por daemon/runner. La identidad de Windows **no** se infiere de la autenticación JSONL: ambas barreras son independientes.
- `LocalStream` comparte el handle entre sus clones, con bloqueos de lectura y escritura independientes. El mensaje de control interno de medio-cierre es un único byte NUL, que no puede aparecer sin escape en JSONL. `Read` lo interpreta como EOF sin impedir recibir la dirección contraria. `Shutdown::Both` además solicita cancelar I/O pendiente por `CancelIoEx`.
- Se agregaron pruebas Rust nativas de half-close, respuesta pendiente y cancelación, y se adaptó W1-A para conectarse por `NamedPipeClientStream`. W1-B ejecuta un fixture de daemon, token inválido y segundo daemon competidor en CI.
- La propiedad de handles y el efecto de cancelación ante un peer que no consume datos exigen prueba de carga adicional. El cierre completo puede bloquear al transmitir el marcador EOF si el buffer de salida está lleno. **No declarar cancelación no bloqueante en cualquier estado** hasta cerrar esa brecha.
- La ACL de archivos (token, DB, WAL, spool) y la prueba contra otra cuenta Windows siguen pendientes. El modelo de confianza sigue considerando al mismo usuario como confiable; esto no prueba aislamiento entre usuarios distintos.
