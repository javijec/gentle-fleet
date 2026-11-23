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
  en Windows. Regresión Linux del cambio y CI del commit: UNTESTED por ahora.
- Rechazo cruzado: PASS en Windows con dos runners FakeDriver de fixture
  independientes; cada runner rechazó el token del otro y aceptó su propio
  token e identidad. No se declara cerrado W1-A hasta comprobar Linux y CI.

## W1-B–F

BLOCKED por las puertas de W1-A y seguridad. Named pipes, DACL, persistencia,
supervivencia del runner y E2E exhaustivos aún no se implementaron ni probaron.
No se declara compatibilidad funcional ni seguridad del transporte Windows.
