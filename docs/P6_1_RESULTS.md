# P6.1 — Resultados del SDK Rust

**Estado:** **P6.1 CERRADA EN LINUX** (Linux x86_64). No se implementó GPUI.

## Cambios

- Nueva crate `crates/fleet-client`, miembro del workspace.
- `fleetctl` usa el SDK para handshake, transporte y correlación; sus comandos y salida se conservan. `watch` usa `TimelineSubscription`.
- El SDK tiene API tipada para hosts, agentes/capacidades, workspaces, sesiones, prompts con `operation_key`, replay/stream, operaciones, aprobaciones, interrupt, stop y controles.
- Journal remoto durable y cursores JSONL privados bajo el estado local; los prompts no se escriben al journal.
- `fleet-ssh` expone cierre del transporte de cliente para cancelar suscripciones sin tocar Fleet remoto.
- E2E negra `tests/e2e_p61_sdk.sh`; integración del SDK en `tests/e2e_p51_ssh.sh`.

## Evidencia

| Validación | Resultado |
|---|---|
| Cargo fmt/check/clippy/test/build, workspace locked | PASS; 129 tests passed, 1 ignored (diagnóstico opt-in preexistente) |
| SDK local + FakeDriver: catálogo, workspace, sesión, prompt, stream, cursor, cancelación | PASS |
| CLI/SDK catálogo local idéntico | PASS |
| SDK remoto por SSH: catálogo y chat FakeDriver | PASS |
| Corte forzado del bridge mientras SDK observa | PASS; reattach por cursor, sin secuencias duplicadas ni prompt reenviado |
| Timeline persistente tras reconexión | PASS; un único mensaje de usuario |
| E2E regresiones P1–P5 + P6.1 | PASS, 22 scripts deterministas |
| OpenCode real opt-in, SDK sobre daemon local | PASS (`GF_E2E_REAL=1`, `tests/e2e_p61_sdk.sh`) |
| Linux/Windows | Linux ejecutado; Windows SDK no validado en este corte. La validación de P5 fue del CLI Windows. |
| GitHub Actions | No ejecutado: no se hizo push por instrucción del usuario; `tests/e2e_p61_sdk.sh` agregado al workflow para CI posterior |

## Límites conocidos

- SDK síncrono, sin dependencia GPUI. El consumidor GPUI debe invocar en su worker y publicar eventos en su propio event loop.
- Los cursores están separados por target local/alias SSH; el alias no pretende ser identidad criptográfica del host. OpenSSH `known_hosts` sigue siendo la autoridad de autenticación.
- `RetentionGap` se comunica explícitamente. El store actual no poda eventos ni publica earliest-retained, de modo que no se fabricó soporte de retención.
- Las operaciones inciertas no se retransmiten automáticamente; el caller debe reconciliar mediante `get_operation` y la clave registrada.
- No se validó aprobación interactiva real por el SDK; se mantienen las pruebas de decisión única existentes.
- CI remota no se ejecutó porque el usuario indicó explícitamente no hacer push; las validaciones de calidad y E2E se ejecutaron localmente.

## Siguiente corte

P6.2 puede integrar el SDK en un cliente GPUI mínimo de chat/timeline/permisos, con una tarea worker cancelable; evitar rehacer transporte/correlación. Antes de abrir capacidades de terminal/archivos/Git, validar una aprobación real soportada por proveedor. P6.2 no se implementó en este corte.
