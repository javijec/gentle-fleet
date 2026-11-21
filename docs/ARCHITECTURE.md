# Arquitectura de Gentle Fleet v2

## Principio principal

**La sesión es la unidad de orquestación.** Una autoridad (SessionManager) decide y registra qué debe ocurrir; una entidad ejecutora (Runner) se encarga del proceso agente; un driver conoce el protocolo específico; una timeline estable permite observar y reconectar. Los clientes no conocen SQLite, PID ni protocolo del agente.

## Estado implementado tras P1.1

El diagrama y el árbol siguiente describen el destino arquitectónico. Lo implementado en P1.1 respeta estos límites:

- `EventBus` está particionado por `SessionId`: publicar en una sesión nunca entrega a suscriptores de otra. Cada sesión mantiene su propio `seq` monotónico asignado por `SessionManager`.
- `SessionManager` posee la máquina de estados del turno (`Idle`, `Running`, `AwaitingApproval`, `Completed`, `Failed`, `Interrupted`, `Uncertain`). El driver nunca decide el estado público del turno; un evento tardío con otro `turn_id` se descarta.
- La reserva de `operation_key` es atómica e incluye `session_id` + payload; un mismo parámetro devuelve el recibo existente, un payload distinto devuelve `Conflict`. El registro precede al despacho.
- Las suscripciones tienen cola acotada (256 por defecto). Un suscriptor lento se desconecta con un `Notice{code:"Backpressure"}`; nunca se acumula memoria sin límite ni se pierde silenciosamente.
- Transporte local: Unix Domain Socket en directorio 0700 con socket 0600 en Linux (VERIFIED); en Windows, TCP loopback + token privado, **plataforma no validada** en P1.1. No se confía en loopback como autenticación.
- `RunnerMode::{Owned, Attached, Auto}` vive en el `LaunchContext` del driver; el runner como proceso independiente sigue siendo P3.
- `SessionManager` no importa `fleet-driver-opencode` ni ningún driver concreto: `ProviderRegistry` es el único punto de extensión.
- **P3:** `fleet-runner` es un proceso separado dueño del `AgentDriver` y del spool; `SessionManager` usa `RunnerLauncher` y no contiene hilos de driver. El reattachment valida `runner_id`, `boot_id`, `generation` y versión de protocolo antes de declarar recuperación. `fleetd` no muere con el runner y el runner no muere con `fleetd`.
- **P2:** `fleet-store-sqlite` implementa `fleet_domain::store::SessionStore`; `SessionManager` solo conoce ese trait (y un store falso en pruebas), nunca SQL ni `rusqlite`. Los eventos se persisten antes de publicarse y el `seq` sigue siendo propiedad de Fleet. Las aprobaciones son recursos de dominio traducidos por cada driver; `native_request_id` no sale del driver hacia el wire. `fleetd` compone store, registry, manager y transporte; `fleetctl` solo habla la API compartida.

## Diagrama de componentes

~~~mermaid
flowchart TB
    UI[GPUI Desktop] --> SDK[Fleet Client SDK]
    CLI[fleetctl] --> SDK
    SDK --> TR[Transporte local o SSH]
    TR --> API[Fleet API / fleetd]
    API --> SM[SessionManager - autoridad]
    SM --> ST[(fleet.sqlite - estado y timeline)]
    SM --> RM[RunnerManager]
    RM --> R1[fleet-runner: sesión A]
    RM --> R2[fleet-runner: sesión B]
    R1 --> D1[Gentle Shell driver]
    R2 --> D2[Codex u OpenCode driver]
    D1 --> P1[Proceso agente]
    D2 --> P2[Proceso/servidor agente]
    R1 -. spool y ack .-> SM
    R2 -. spool y ack .-> SM
~~~

## Árbol propuesto — monorepo Rust inicial

~~~text
gentle-fleet/
  Cargo.toml                     workspace, lock único
  crates/
    fleet-domain/                IDs, estados, eventos, capacidades; cero I/O
    fleet-protocol/              envelopes versionados + codec + contract fixtures
    fleet-core/                  SessionManager + traits de store/runner
    fleet-store-sqlite/          migraciones, transacciones, event log
    fleet-agent/                 trait AgentDriver, modelos nativos y FakeDriver
    fleet-driver-pi/             Gentle Shell --mode rpc (tras spike)
    fleet-driver-codex/          Codex app-server (tras spike)
    fleet-driver-opencode/       OpenCode server/API (tras spike)
    fleet-runner/                proceso por sesión y spool acotado
    fleetd/                      daemon por usuario, IPC, runtime
    fleet-client/                SDK Rust para CLI/GPUI
    fleetctl/                    cliente CLI y diagnósticos
    fleet-transport/             IPC Unix/Windows + SSH stdio
  apps/
    desktop/                     GPUI en etapa P6, puede residir separado después
  tests/
    contract/                    fixtures comunes del wire y los drivers
    e2e/                         procesos reales/fixtures/crash injection
  docs/
~~~

Las carpetas indicadas describen módulos futuros; **no existe código implementado en este reinicio**. Al comienzo P1 puede agrupar varias crates si mantener cada una por separado no aporta un límite efectivo. No crear abstractions en blanco solo por el árbol.

## Dirección de dependencias

1. fleet-domain no conoce DB, sockets, Tokio, clientes ni proveedores.
2. fleet-protocol importa domain; clientes y servidor consumen exactamente el mismo contrato.
3. fleet-core conoce traits de Storage, RunnerManager y DriverRegistry, no implementaciones concretas.
4. fleet-store-sqlite implementa Storage; solo fleetd escribe la base central.
5. fleet-runner supervisa un proveedor por sesión y entrega mensajes al daemon; no escribe en Fleet DB ni decide autorización pública.
6. Drivers conocen únicamente la API de su proveedor y la interfaz AgentDriver; no importan fleetd, GPUI ni SQL.
7. fleetctl y Desktop solo importan fleet-client/protocol; no conocen rutas internas de daemon o runner.

No se usan dependencias circulares. No hay una crate global utils con lógica de negocio.

## Procesos y propiedad

- **fleetd:** servicio local por usuario y host. Expone API, autoriza cliente, gestiona catálogos, idempotencia, sesiones, eventos, permisos y límites. No ejecuta consultas de proveedores desde un proceso cliente.
- **fleet-runner:** proceso hijo independiente con identidad asignada por fleetd, dueño de la conexión del agente y del buffer temporal de entrega. Solo emite eventos enumerados/acknowledgeables, no decide el ID Fleet.
- **agente:** subprocess o servidor controlado por driver. La capacidad de mantener el proceso o reanudar su conversación varía entre proveedores.
- **cliente:** puede ser local o remoto, CLI o GPUI. Sus cierres no matan los runners.

Una sesión puede tener varios observadores, pero **una sola cola serializada de operaciones mutables**. Los lectores no bloquean otras sesiones. No se utiliza lock global durante llamadas de red o al proveedor.

## Ejecución y directorios

- Se registra un workspace por ruta del host. El runner recibe una ruta canonizada y la usa como directorio de trabajo real; antes de iniciar se verifica que el proceso tiene acceso, que es un directorio y que la política del usuario permite su ejecución.
- El modelo guarda tanto el ID de workspace como el valor de ruta, pero las rutas no son IDs y pueden requerir revalidación si cambian.
- El proceso del proveedor hereda únicamente entorno explícitamente permitido; la autenticación nativa de cada CLI se mantiene dentro del perfil del usuario.
- Para worktrees paralelos se crean workspaces separados en una etapa posterior, no se mezclan agentes sobre el mismo árbol sin indicarlo.

## Persistencia y recuperación

fleetd tiene la base Fleet autoritativa. El runner puede conservar un **spool mínimo de mensajes todavía no confirmados por fleetd**, para sobrevivir a una caída del daemon sin afirmar que la base de eventos está replicada dos veces. La ingesta deduplica por (runner_id, runner_seq). No permitir confirmar un evento al runner antes del commit en Fleet DB. Los procesos activos son reconciliados por identidad del runner, no por confiar solo en un PID.

La recuperación exacta después de reinicio de host depende del proveedor, no se promete universalmente. Por ejemplo, recuperar una UI no es lo mismo que recuperar un proceso muerto. Ver LIFECYCLE y PERSISTENCE.

## Límites de escala

En MVP fijar valores configurables moderados: máximo 4 runners activos, 2 operaciones en ejecución por host en el primer driver, colas por cliente y spool con límites explícitos. Cuando se saturen, rechazar o pausar con error estable; no crear un hilo OS por cliente sin control de admisión.

## P4.0 — puntos de extensión investigados (sin cambios de código)

La investigación de P4.0 (22/11/2026) confirma que la topología actual aguanta tres proveedores nuevos sin tocar `SessionManager`: el driver vive dentro de `fleet-runner`, el daemon solo habla el wire tipado y `ProviderRegistry` es el único registro. Las brechas no son de forma general sino de contrato: faltan descriptores de control, decisiones de aprobación tipadas y operaciones de adopción/importación con procedencia. El análisis con alternativas y recomendaciones está en [`P4_CONTRACT_DESIGN.md`](P4_CONTRACT_DESIGN.md); la evidencia por proveedor y versión en [`P4_PROVIDER_MATRIX.md`](P4_PROVIDER_MATRIX.md) y [`P4_RPC_RESEARCH.md`](P4_RPC_RESEARCH.md).

Dos consecuencias arquitectónicas a respetar en los próximos cortes:

- La elección de propiedad no cambia: el runner que posee el proceso sigue siendo dueño del spool y del ACK; `fleetd` no importa protocolos nativos. Las nuevas operaciones (control, adopción, importación) deben viajar por el mismo `RunnerCommand`/`RunnerRequest` tipados y ser idempotentes por `operation_key`.
- La recuperación deja de ser un bool: importar historial, reanudar un proceso nativo, reattach de un runner vivo y recuperar un turno en vuelo son operaciones distintas. Solo el reattach está demostrado (P3); Codex no ofrece turno en vuelo entre procesos y Pi/Gentle dependen del runner poseedor.

No se han creado crates, tablas ni migraciones en P4.0: la tabla de vínculo de adopción (V4) y las operaciones wire se propondrán solo cuando un corte ejecute el caso de uso real. Tampoco se toma Gentle Shell GPUI como referencia (P4-F17).

## P4.1 — segundo driver real

`fleet-driver-codex` se inserta en el punto de extensión ya existente: `ProviderRegistry` sigue siendo la única autoridad de catálogo, la composición concreta se movió a `fleet-providers` (fuera de `fleet-agent`, sin ciclos) y `SessionManager` no conoce proveedores. El App Server corre dentro de `fleet-runner`; el canal es JSON-RPC acotado sobre stdio con un hilo lector que no bloquea las aprobaciones entrantes.

Los eventos y aprobaciones ampliados (`options`, `arguments`, controles, historial con procedencia) usan campos opcionales versionados; los frames antiguos siguen deserializando. La adopción de sesiones externas con escritura permanece deshabilitada porque el proveedor no ofrece exclusividad verificable. Ver [`P4_1_RESULTS.md`](P4_1_RESULTS.md).

## P4.2 — tercer proveedor real

`fleet-driver-pi` se inserta en el mismo punto de extensión que OpenCode y Codex: `ProviderRegistry` sigue siendo el único catálogo y la composición concreta vive en `fleet-providers`, fuera de `fleet-agent`. El runner posee el proceso Pi y su directorio de sesiones aislado; el transporte usa JSONL por LF con backpressure explícito y colas acotadas. No se introdujo una abstracción RPC compartida con Gentle Shell: solo se documenta su posible reutilización futura en P4.3. Ver [`P4_2_RESULTS.md`](P4_2_RESULTS.md).

## Puertas de revisión arquitectónica

No aceptar nueva funcionalidad si requiere que un cliente conozca internals del proveedor, que un driver escriba el catálogo Fleet o que la misma mutación tenga dos autoridades. Aceptar una refactorización solo si simplifica un límite demostrado por un test, no por anticipar riesgos imposibles de enumerar.
