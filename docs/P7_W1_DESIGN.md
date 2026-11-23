# P7-W1 — diseño y límites actuales

**ABIERTO.** W1-A pasó su regresión nativa y CI; W1-B–F no están implementados.
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
No cambia el wire ni la autenticación. El transporte sigue siendo TCP
loopback, y los ficheros no tienen DACL explícita: **no cumple** el modelo de
seguridad objetivo. No usar el arreglo como demostración de IPC seguro.

## Puerta de seguridad W1-B

Falta diseñar e implementar named pipes con DACL explícita, verificación SID,
defensa contra apropiación del endpoint, propiedad y cierre de handles y una
decisión probada sobre half-close. No hay fallback TCP seguro validado ni
prueba de usuario Windows distinto. No se declara implementado W1-B.

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
