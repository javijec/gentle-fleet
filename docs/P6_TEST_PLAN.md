# P6.1 — Plan de pruebas SDK

## Contratos automatizados

| Prueba | Evidencia |
|---|---|
| SDK y CLI consultan catálogo equivalente local/SSH | `tests/e2e_p61_sdk.sh`, `tests/e2e_p51_ssh.sh` |
| SDK deterministic local smoke in CI | `tests/e2e_p61_sdk.sh` |
| Mismos envelopes local/SSH | Ambos caminos usan `fleet-protocol::Frame`; E2E FakeDriver |
| Chat tipado y timeline en vivo | SDK smoke local y SSH |
| Dos observadores / secuencias sin repetición | E2E P5 y SDK reconnect con set de `seq` |
| Caída de bridge y reconexión por cursor | E2E SSH termina el bridge mientras SDK observa |
| Cursor persistente por host/sesión | SDK smoke cancela/reanuda con `resume_timeline`; test unitario separa cursor encolado y recibido |
| ACK ambiguo sin replay automático | journal antes del envío; se verifica en E2E P5 con misma `operation_key` |
| Aprobación ganadora única | regresiones existentes P2/P4.4 sobre el wire público |
| Error SSH no vacío exitoso | regresión de host-key/bridge en E2E P5 |
| Cancelar observación sin detener sesión | SDK cancela y después `stop_session` explícito |
| Consumidor lento: no pérdida de eventos y reporte de errores con cola llena | Tests unitarios de cola acotada y error terminal no descartado |
| CLI ante desconexión transitoria | `fleetctl watch` permanece activo y utiliza reconexión del SDK |
| Recuperación de más de 256 eventos | `tests/e2e_long_replay.sh` (270 eventos) y diagnóstico de salida temprana |
| Dos hosts no se mezclan | E2E P5 dos hosts; prueba unitaria de `SessionIdentity` |

## Ejecución

```bash
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --locked
tests/e2e_p61_sdk.sh
tests/e2e_p51_ssh.sh
tests/e2e_p52_two_hosts.sh
```

Smoke opt-in con OpenCode real:

```bash
GF_E2E_REAL=1 tests/e2e_p61_sdk.sh
```

No introducir credenciales ni llamadas a modelos pagos en CI; el CI usa FakeDriver.
