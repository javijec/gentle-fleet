# P6.1 — SDK Rust reutilizable

**Estado:** P6.1 implementado en Linux; evidencia y límites en `P6_1_RESULTS.md`.

## Diseño

`fleet-client` es una crate Rust síncrona, independiente de GPUI, que depende solo de `fleet-domain`, `fleet-protocol`, `fleet-ssh` y utilidades de serialización/IDs. La conexión posee un `Host` explícito (`Local` o alias SSH), un stream y un contador de `request_id`; no hay host global dentro del SDK. `fleetctl` conserva el parseo y formato de comandos, pero delega conexión, handshake, envelopes y correlación al SDK.

```text
fleetctl ─┐
          ├─ FleetClient ── fleet-protocol ── socket local / fleet-ssh ── fleetd
GPUI ─────┘        │
                   └─ TimelineSubscription (canal acotado + cancelación)
```

No se modifica el wire. Los IDs de solicitud `sdk-N` son independientes de `operation_key`. Cada llamada espera el `request_id` correspondiente y descarta notificaciones no correlacionadas sin confundirlas con respuestas.

## API pública

- `FleetClient::connect(Host)` y `disconnect()`.
- Agentes/capacidades, workspaces, sesiones (`create`, `get`, `list`, `stop`).
- `send_prompt(session, text, operation_key)` y `new_operation_key()`.
- Timeline paginada y `subscribe_timeline` / `resume_timeline`.
- Operaciones, aprobaciones (`list`, `get`, `answer`), interrupción y controles.
- `TimelineSubscription::{recv, recv_timeout, cancel, last_sequence, identity}` sin dependencia de UI/event loop.

`SessionIdentity` incluye el contexto `Host` y `session_id`, por lo que IDs iguales en destinos distintos no colisionan dentro del cliente. Las suscripciones están ligadas a un solo host y sesión.

## Replay, reconexión y cancelación

Cada observador tiene un canal acotado de 64 eventos. La reconexión reintenta errores transitorios de transporte y `Timeout` con backoff de 100 ms a 5 s; los huecos (`RetentionGap`), el `Backpressure` explícito del servidor y los errores permanentes necesitan intervención explícita. **Dos cursores con propósitos distintos:** el worker deduplica a partir del último `seq` encolado; el cursor público `last_sequence()` y el journal `cursors.jsonl` avanzan solo tras `recv()` y persistencia exitosos. Si se descarta la suscripción con eventos pendientes en cola, `resume_timeline` no se los salta. Un consumidor lento produce backpressure acotado, cancelable y explícito.

`cancel` cierra solo el stream SSH/socket del observador, despierta el hilo lector y espera su terminación; no envía `StopSession` ni cierra el daemon/runner. Backpressure al consumidor es acotado y se informa explícitamente.

## Operaciones inciertas

Para SSH, `SendPrompt`, `AnswerApproval` e importación de historial se agrega al journal local durable (JSONL privado) la clave/host/target antes de escribir el frame; nunca se guarda el prompt ni se reenvía automáticamente. Ante error ambiguo el caller consulta `get_operation(operation_key)` usando esa misma clave. El SDK no declara que un timeout implique que el servidor no ejecutó el efecto.

## Límites

- API síncrona, adecuada para invocación desde un worker GPUI; P6.2 decidirá si hace falta una fachada async.
- El cursor es local al usuario y alias SSH, no una identidad global compartida entre perfiles. No sincroniza bases ni cursores entre equipos.
- La API actual del servidor no anuncia earliest-retained ni implementa poda de timeline; si el wire/store reporta un hueco, el SDK lo mantiene explícito.
- No se implementan GUI, terminal, filesystem ni Git.
