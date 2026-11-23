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
Si CI sigue bloqueado por facturación, registrar BLOCKED; no usar PASS local
como sustituto del CI. Eliminar fallas reales de seguridad antes de W1-C.
