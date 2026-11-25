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

- SDK síncrono, independiente de frameworks de interfaz. Un consumidor externo debe invocar desde su worker si necesita evitar bloquear su event loop.
- Los cursores están separados por target local/alias SSH; el alias no pretende ser identidad criptográfica del host. OpenSSH `known_hosts` sigue siendo la autoridad de autenticación.
- `RetentionGap` se comunica explícitamente. El store actual no poda eventos ni publica earliest-retained, de modo que no se fabricó soporte de retención.
- Las operaciones inciertas no se retransmiten automáticamente; el caller debe reconciliar mediante `get_operation` y la clave registrada.
- No se validó aprobación interactiva real por el SDK; se mantienen las pruebas de decisión única existentes.
- El push inicial falló antes de ejecutar el replay porque el script perdió el bit ejecutable; P6.1-H restauró modos `100755` para replay y SSH y logró CI completo verde. No atribuir aquel fallo al algoritmo de reconstrucción sin evidencia.

## Siguiente corte

El SDK será consumido por CLI y aplicaciones externas. P6.2 gráfica se retira del alcance de producto. Antes de abrir capacidades de terminal/archivos/Git, validar una aprobación real soportada por proveedor.

## P6.1-H — Correcciones posteriores al primer push

- El cursor de `last_sequence()` solo refleja eventos recibidos y persistidos, no la cola del worker. La deduplicación interna sigue usando el último evento encolado.
- `fleetctl watch` continúa observando después de errores transitorios de transporte; errores de cursor local o huecos terminales no se silencian.
- El envío al canal acotado aplica backpressure cancelable en lugar de perder mensajes cuando la cola está llena; los errores de suscripción se entregan cuando el consumidor drena el canal.
- `RetentionGap` y los errores permanentes detienen los reintentos de reconexión.
- La prueba P5/P6 solo termina procesos SSH pertenecientes al SDK invocado en la prueba, sin afectar bridges ajenos.
- Se agregaron unitarias para eventos encolados sin ACK, consumidor lento y error terminal en una cola llena. `e2e_long_replay.sh` reporta errores de watch con diagnóstico en caso de fallo.
- CI instala OpenSSH Server para ejecutar los E2E SSH deterministas, en vez de omitirlos por falta de `sshd`.
- **CI de P6.1-H: SUCCESS** en el commit de código `26a436d` (132 tests Rust; replay 270 eventos; E2E SSH uno/dos hosts; SDK local FakeDriver). Los cuatro proveedores reales no fueron revalidados en P6.1-H; el smoke OpenCode de P6.1 original permanece como evidencia anterior.

## P6-H — preparación headless (25/11/2026)

Base `4166c4a`; origen de mejoras reutilizadas `427a5c1`; código candidato `6a8d366`. Se incorporan solo `fleet-client`, `fleet-ssh` y una aserción E2E más estricta de identidad SSH. Se conserva catálogo de hosts sin secretos y tipos públicos, clasificación de host-key y espera acotada de stderr. Si un helper local conserva stderr abierto, se deja de esperar al lector después de 200 ms; el hilo puede permanecer vivo hasta EOF. No se declara garantía de cancelación de todos los procesos auxiliares SSH. Se añade una regresión del lector que no termina y se exige el error tipado HostKeyRejected en el fixture SSH.

El workspace y lockfile se conservan de P6.1, sin GPUI. La suite de cierre usa los 21 scripts E2E explícitos del workflow heredado, más los contratos Rust y el gate de grafo headless. La cifra histórica de 22 se conserva como resultado histórico, no se inventa un script adicional. CI y ejecución local usan el mismo script; al preparar el candidato faltaba ejecutar el smoke real OpenCode tras el endurecimiento.

**Estado inicial del candidato:** compilación, Clippy, unitarias, E2E y smoke real UNTESTED al preparar la rama. Ver actualización de cierre siguiente. La validación de Windows nativo sigue en P7; no se anuncia paridad. No reducir criterios de P7.

### Cierre P6-H (25/11/2026)

El operador ejecutó el procedimiento Linux indicado en el plan P6. El primer intento confirmó metadata headless y se detuvo en rustfmt; se corrigió únicamente el formato en `b366f968fad2a5c409444a633591c56fda74f48d`. Tras indicar pull y repetición del gate y smoke, el operador confirmó que pasó todo. Se registra esa confirmación como evidencia reportada del candidato, no como ejecución independiente por el asistente. No se adjuntaron SHA local, versiones de herramientas ni logs completos; no se verifican independientemente esos datos ni se inventan contadores de tests Rust.

| Validación | Resultado |
| --- | --- |
| Gate headless Linux: metadata, fmt, check, clippy, test, build locked, 21 E2E deterministas | PASS reportado por operador |
| Smoke real opt-in OpenCode SDK | PASS reportado por operador |
| GitHub Actions | Sin PASS nuevo confirmado; ejecución previa fallida sin pasos ni logs accesibles |
| Windows nativo, macOS, otros proveedores reales tras P6-H, aprobación interactiva real SDK | UNTESTED en este corte |

Cierre de alcance Linux del SDK y motor headless; no es release ni aceptación de P7. Se acepta la evidencia reportada por el operador para esta integración limitada; conservar los logs localmente para trazabilidad y no atribuir PASS a GitHub Actions. Este commit de cierre solo modifica documentación, no altera el código o los scripts validados. El experimento gráfico permanece exclusivamente en el historial (commit `427a5c1`). Siguiente corte: P7-H, incorporar la base P6 y retirar dependencias gráficas de P7 sin reducir sus gates de seguridad.
