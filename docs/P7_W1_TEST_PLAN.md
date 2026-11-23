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

**W1-C–F:** rutas, permisos, WAL, crash real, reattach, deduplicación,
concurrencia, aprobaciones, backpressure, CI y smoke manual enumerados en
`P7_W0_AUDIT.md`; siguen UNTESTED.
