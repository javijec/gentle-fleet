# Decisiones arquitectónicas — baseline nuevo

**D01–D12 y P0-D1…D6 están ratificadas (20/11/2026).** Los spikes P0 se ejecutaron con los tres motores reales (ver [`P0_FINDINGS.md`](P0_FINDINGS.md) y [`research/PROVIDER_COMPARISON.md`](research/PROVIDER_COMPARISON.md)). No se trasladan automáticamente los ADR antiguos.

| ID | Decisión | Motivo | Alternativa rechazada por ahora |
|---|---|---|---|
| D01 | Monorepo Rust con crates independientes y Cargo.lock único | Contratos, binarios y CI cambian juntos mientras el wire evoluciona | Tres repos privados y Protocol congelado prematuramente |
| D02 | SessionManager como autoridad de negocio por sesión | Reducir estados contradictorios y lugares con side effects | Daemon, worker y dos DBs decidiendo simultáneamente el estado |
| D03 | Un Fleet DB por host y spool runner solo para eventos no confirmados | Simplificar reconciliación; permitir daemon crash sin replicación de catálogos | Dos DB autoritativas por sesión |
| D04 | AgentDriver con capacidades declaradas y native IDs | Soportar Pi, Codex, OpenCode sin falsa paridad | Suponer que todos hablan Pi RPC o interpretar ANSI como chat |
| D05 | Procesos runner independientes de clientes | UI se desconecta sin terminar agentes | UI dueña de procesos o Herdr como runtime obligatorio |
| D06 | Protocolo tipado orientado a conversaciones/turnos | Contrato de producto primero | Copiar eventos RPC del agente o congelar solo operaciones CRUD |
| D07 | Modelo local de usuario confiable | Evitar inflación de complejidad de UID/SQLite | Aislamiento entre procesos del mismo UID como requisito del MVP |
| D08 | Acceso remoto SSH primero, mismo Fleet API | Seguridad y portabilidad con OpenSSH | TCP expuesto, token propio y relay obligatorio desde P1 |
| D09 | Nunca auto-reenviar efectos inciertos | No duplicar prompts, comandos ni permisos | Prometer exactly-once de procesos externos con una transacción SQLite |
| D10 | Linux/Windows por interfaz de plataforma desde P1 | Evitar reescritura de Unix-only runtime | Codificar /proc, flock y setsid en domain/core |
| D11 | Pruebas verticales antes de blindar subsistemas | Encontrar incompatibilidades reales temprano | Completar módulos horizontales con 100+ tests aislados antes del chat |
| D12 | Versionar wire después del primer smoke | Evitar un Protocol que impide la función central | Publicar v1 inmutable sin mensajes de conversación ni subscribe |

## Ratificación P0-D1…D6 (20/11/2026)

Las alternativas y consecuencias analizadas están en
[`research/PROVIDER_COMPARISON.md`](research/PROVIDER_COMPARISON.md). Se ratifican
con las condiciones siguientes:

| ID | Decisión ratificada | Condición de aplicación |
|---|---|---|
| P0-D1 | OpenCode es el **primer driver implementado** en P1 | Fleet sigue siendo independiente del proveedor: `SessionManager` y el wire no conocen OpenCode; el registro de proveedores permite añadir Codex/Pi sin tocarlos |
| P0-D2 | D04 se mantiene: `AgentDriver` con capacidades y native IDs | Las capacidades se declaran **por versión del proveedor**; el cliente no deduce funciones por el nombre del motor |
| P0-D3 | D05 se matiza: el Runner admite **procesos propios** y **conexión a servicios existentes** | En P1 OpenCode usa attach al servicio de usuario o levanta su propio `serve`; el Runner no asume un hijo exclusivo. El runner como proceso independiente sigue siendo P3 |
| P0-D4 | D09 se refuerza: no reenviar operaciones de resultado incierto | Un timeout nunca dispara un reenvío automático; antes de cualquier reintento hay que reconciliar aprobaciones pendientes y cursor de eventos |
| P0-D5 | Versión fijada por driver y evidencia por versión | La compatibilidad se comprueba con un rango (`>=mínimo, <siguiente mayor`) para **no bloquear actualizaciones compatibles**; un cambio incompatible obliga a nueva fixture |
| P0-D6 | Fleet mantiene su propio orden de eventos | `seq` monotónico por sesión lo asigna Fleet; los IDs nativos son opacos y quedan dentro del driver; no se usa timestamp como orden |

Estas decisiones concretan D01–D12 sin modificarlas. Cualquier reapertura
requiere el caso de uso, reproducción y comparación de la regla existente.

## Decisiones P3 — runner persistente (21/11/2026)

| ID | Decisión | Motivo | Alternativa rechazada | Consecuencia |
|---|---|---|---|---|
| P3-D1 | `fleet-runner` es un **proceso Rust independiente**, una instancia por sesión, dueño del `AgentDriver` y del proceso/servicio nativo | El daemon puede morir sin matar al agente; `SessionManager` no debe poseer hilos de driver | Driver en el proceso `fleetd` (P1/P2): morir el daemon mata la sesión | `fleetd` deja de crear hilos de driver; pasa a hablar con runners por IPC autenticado |
| P3-D2 | Identidad estable del runner: `runner_id` + `session_id` + `provider_id` + `host_id` + `boot_id` + `generation`; PID sólo informativo | Un PID reutilizado no prueba propiedad; el reattachment debe validar identidad de arranque | Confiar en PID o en la existencia de un socket | La DB guarda identidad y endpoint; el handshake la verifica antes de recuperar |
| P3-D3 | Protocolo daemon↔runner tipado y versionado, separado del wire cliente y del protocolo nativo del proveedor | Evita filtrar JSON/SSE de OpenCode al cliente y permite rechazar versiones incompatibles | Reutilizar envelopes de `fleetctl` o HTTP/SSE de OpenCode | Nuevo módulo `fleet_protocol::runner` con `HELLO`, `STATUS`, `OPERATION`, `EVENT`, `ACK`, `SHUTDOWN` |
| P3-D4 | Spool **durable y acotado por runner**, propiedad del runner, con `(runner_id, runner_seq)` y ACK del daemon | El daemon puede estar caído mientras se generan eventos; no se puede perder un permiso o una respuesta | Spool en RAM o en `fleet.sqlite` (segunda autoridad) | SQLite propio del runner con límite de eventos/bytes; política `SpoolFull`: suspender operaciones y avisar, nunca descartar |
| P3-D5 | `seq` público de Fleet sigue siendo asignado por `SessionManager`/SQLite; el runner sólo emite `runner_seq` | Conserva P0-D6 y evita duplicados lógicos en replay | Usar `runner_seq` como `seq` de cliente | Ingesta deduplicada por `(runner_id, runner_seq)` antes de publicar |
| P3-D6 | Reattachment **sólo con evidencia**: handshake autenticado + estado real del runner/proveedor; si falla, `Uncertain`/`Lost` | No declarar `Running` por un PID persistido | Marcar `Running` al arrancar el daemon | Un daemon reiniciado no crea runners sustitutos ni reenvía prompts |
| P3-D7 | Aprobaciones: registro lógico sigue en `SessionManager`; el runner mantiene estado operativo y evidencia nativa mientras el daemon está ausente | Una decisión no puede resolverse dos veces ni aprobarse automáticamente | Resolver aprobaciones en el runner | Reenvío de decisiones inciertas requiere reconciliación; se conserva `Dispatching`/`Uncertain` |
| P3-D8 | Transporte daemon↔runner: Unix socket 0600 en directorio 0700 en Linux; Windows queda **no validado** | Mismo modelo de confianza local que P1.1 | TCP loopback sin autenticación | No se declara paridad Windows sin prueba real |

Estas decisiones extienden D02/D03/D05/D09 sin cambiar `AgentDriver` ni el wire público más que en lo imprescindible.

## Deudas de arquitectura aceptadas temporalmente

- Primer release no aísla procesos del mismo usuario.
- No garantiza ejecución exactly-once tras corte entre red y proceso externo.
- No garantiza recuperar procesos tras reiniciar el host; resume dependerá del agente.
- Retención de eventos sin snapshot se pospone hasta demostrar replay completo; el tamaño se limita al período de prueba y se monitorea.
- No todas las capacidades son equivalentes entre agentes.

## Regla para reabrir decisiones

Exigir un caso de uso real no resuelto, una reproducción y comparación de al menos dos alternativas con costo de implementación y pruebas. Documentar la decisión en ADR breve. No se permite cambiar D01–D12 solo para eliminar un error teórico fuera del modelo.
