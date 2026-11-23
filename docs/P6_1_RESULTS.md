# P6.1 — Resultados del SDK Rust

**Estado:** **P6.1-H VALIDADA EN CI EN LINUX** (Linux x86_64); GPUI todavía no implementada. El smoke real OpenCode es evidencia anterior a P6.1-H y no se repitió tras este endurecimiento.

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
| Cargo fmt/clippy/test/build en GitHub Actions | PASS; 132 tests passed, 1 ignored (diagnóstico opt-in preexistente) |
| SDK local + FakeDriver: catálogo, workspace, sesión, prompt, stream, cursor, cancelación | PASS |
| CLI/SDK catálogo local idéntico | PASS |
| SDK remoto por SSH: catálogo y chat FakeDriver | PASS |
| Corte forzado del bridge mientras SDK observa | PASS; reattach por cursor, sin secuencias duplicadas ni prompt reenviado |
| Timeline persistente tras reconexión | PASS; un único mensaje de usuario |
| E2E regresiones P1–P5 + P6.1 | PASS, 22 scripts deterministas |
| OpenCode real opt-in, SDK sobre daemon local | PASS (`GF_E2E_REAL=1`, `tests/e2e_p61_sdk.sh`) |
| Linux/Windows | Linux ejecutado; Windows SDK no validado en este corte. La validación de P5 fue del CLI Windows. |
| GitHub Actions | **SUCCESS** [#45089008594](https://github.com/javijec/gentle-fleet/actions/runs/45089008594) sobre `26a436d`; incluye replay de 270 eventos y E2E reales de transporte SSH con FakeDriver |

## Límites conocidos

- SDK síncrono, sin dependencia GPUI. El consumidor GPUI debe invocar en su worker y publicar eventos en su propio event loop.
- Los cursores están separados por target local/alias SSH; el alias no pretende ser identidad criptográfica del host. OpenSSH `known_hosts` sigue siendo la autoridad de autenticación.
- `RetentionGap` se comunica explícitamente. El store actual no poda eventos ni publica earliest-retained, de modo que no se fabricó soporte de retención.
- Las operaciones inciertas no se retransmiten automáticamente; el caller debe reconciliar mediante `get_operation` y la clave registrada.
- No se validó aprobación interactiva real por el SDK; se mantienen las pruebas de decisión única existentes.
- El push inicial falló antes de ejecutar el replay porque el script perdió el bit ejecutable; P6.1-H restauró modos `100755` para replay y SSH y logró CI completo verde. No atribuir aquel fallo al algoritmo de reconstrucción sin evidencia.

## Siguiente corte

P6.2 puede integrar el SDK en un cliente GPUI mínimo de chat/timeline/permisos, con una tarea worker cancelable; evitar rehacer transporte/correlación. Antes de abrir capacidades de terminal/archivos/Git, validar una aprobación real soportada por proveedor. P6.2 no se implementó en este corte.

## P6.1-H — Correcciones posteriores al primer push

- El cursor de `last_sequence()` solo refleja eventos recibidos y persistidos, no la cola del worker. La deduplicación interna sigue usando el último evento encolado.
- `fleetctl watch` continúa observando después de errores transitorios de transporte; errores de cursor local o huecos terminales no se silencian.
- El envío al canal acotado aplica backpressure cancelable en lugar de perder mensajes cuando la cola está llena; los errores de suscripción se entregan cuando el consumidor drena el canal.
- `RetentionGap` y los errores permanentes detienen los reintentos de reconexión.
- La prueba P5/P6 solo termina procesos SSH pertenecientes al SDK invocado en la prueba, sin afectar bridges ajenos.
- Se agregaron unitarias para eventos encolados sin ACK, consumidor lento y error terminal en una cola llena. `e2e_long_replay.sh` reporta errores de watch con diagnóstico en caso de fallo.
- CI instala OpenSSH Server para ejecutar los E2E SSH deterministas, en vez de omitirlos por falta de `sshd`.
- **CI de P6.1-H: SUCCESS** en el commit de código `26a436d` (132 tests Rust; replay 270 eventos; E2E SSH uno/dos hosts; SDK local FakeDriver). Los cuatro proveedores reales no fueron revalidados en P6.1-H; el smoke OpenCode de P6.1 original permanece como evidencia anterior.
