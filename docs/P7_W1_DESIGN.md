# P7-W1 — diseño y límites actuales

**ABIERTO.** W1-A está en validación; W1-B–F no están implementados.
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
