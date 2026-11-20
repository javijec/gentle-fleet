# P1 — resultados reales (20/11/2026)

Estado: **P1 implementado y validado**. Un CLI (`fleetctl`) crea workspace y
sesión, envía prompt y observa texto y herramientas a través de la API compartida
de `fleetd`. Dos clientes reciben la misma secuencia de eventos. No hay
persistencia durable, remoto, aprobaciones ni GPUI: esos son P2/P3/P5/P6.

## Historia vertical observable

`API (fleet-protocol)` → `SessionManager (fleet-core)` → `AgentDriver` →
`OpenCodeDriver` → proceso/servicio OpenCode → eventos SSE → `AgentDriver` →
`SessionManager` → `EventBus` → dos clientes `fleetctl`.

Ejecutable real: `opencode v2.3.2`, attach al servicio de usuario detectado en
`$XDG_STATE_HOME/opencode/service.json`. No se inició un segundo servidor
OpenCode durante el smoke (P0-D1/P0-D3).

## Crates implementados

| Crate | Rol |
|---|---|
| `fleet-domain` | IDs, `ProviderVersion`, `VersionRange`, capacidades, `Event`/`EventKind`. Sin I/O |
| `fleet-protocol` | envelopes JSONL compartidos por daemon y CLI; `read_frame`/`write_frame` |
| `fleet-agent` | trait `AgentDriver`, `ProviderRegistry`, `FakeDriver` y su fábrica |
| `fleet-core` | `SessionManager`, `EventBus`, timeline en memoria, sequencer y dedup por `operation_key` |
| `fleet-driver-opencode` | driver HTTP/SSE real; IDs nativos confinados en el crate |
| `fleetd` | daemon: TCP loopback, registro de drivers, despacho de requests y difusión de eventos |
| `fleetctl` | CLI: workspace, session, prompt, watch, stop, agents |

`fleet-runner` como proceso independiente y `fleet-store-sqlite` siguen siendo
P3/P2: en P1 la modalidad `Owned`/`Attached` vive en el `LaunchContext` del
driver y la timeline es solo memoria del daemon. `FakeDriver` está dentro de
`fleet-agent` (no en un crate propio) porque aún no justifica un límite de crate.

## Contrato P1

- Transporte: TCP en `127.0.0.1`, puerto efímero escrito en
  `$FLEET_RUNTIME_DIR/fleetd.addr` (por defecto `/tmp/gentle-fleet`). No hay TCP
  público ni autenticación de cliente: la frontera de confianza es el usuario
  local (D07). Sustituye a Unix socket/named pipe por portabilidad Linux/Windows;
  el framing es el mismo.
- Request/response/event son envelopes tipados. El cliente nunca ve JSON de
  OpenCode: el driver traduce `session.text.delta`, `session.tool.*` y
  `session.execution.*` a `AssistantDelta`, `ToolStarted`/`ToolOutput`/
  `ToolCompleted` y `TurnCompleted`.
- `seq` lo asigna `SessionManager` por sesión, desde 1 (P0-D6). El dedup de
  clientes se hace por `seq`; la suscripción entrega snapshot y eventos vivos
  sin hueco porque se registra antes de leer el snapshot.
- Un `operation_key` repetido devuelve el mismo recibo y no reenvía el prompt
  (P0-D4). No hay retry automático tras timeout.
- Compatibilidad OpenCode: `>=2.3,<3` (P0-D5). La versión 2.4 o 2.9 se acepta
  sin nueva fixture; 3.x se rechaza con `incompatible_version`.

## Comandos usados

```bash
cargo fmt --all
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
tests/e2e_fake.sh
tests/e2e_opencode.sh
```

Flujo CLI:

```bash
export FLEET_RUNTIME_DIR=/tmp/gf-p1
fleetd &
fleetctl workspace add <directorio>
fleetctl session create --workspace <id> --provider opencode
fleetctl watch <sesión> --until-turn-end &
fleetctl prompt <sesión> "Run exactly this shell command: echo gf-fleet-p1 > p1-marker.txt. Then stop."
fleetctl stop <sesión>
```

## Resultados de validación

| Validación | Resultado |
|---|---|
| `cargo fmt --all --check` | PASS |
| `cargo clippy --workspace --all-targets -- -D warnings` | PASS |
| `cargo test --workspace` | PASS (8 tests unitarios: dominio, protocolo, agente/fake, core, driver) |
| `tests/e2e_fake.sh` | PASS: dos clientes, secuencias idénticas `1..11`, delta + herramienta + `turn_completed Completed` |
| `tests/e2e_opencode.sh` | PASS: dos clientes, secuencias idénticas, `tool_started shell`, `turn_completed Completed`, archivo `p1-marker.txt` creado con `gf-fleet-p1` |
| Limpieza OpenCode | `GET /api/session` sin sesiones `gentle-fleet-p1` residuales |

Timeline real observado en el smoke con OpenCode (IDs de evento y `seq` dados por
Fleet):

```text
1 session_created opencode
2 session_state Running
3 user Run exactly this shell command: echo gf-fleet-p1 > p1-marker.txt. Then stop.
4 turn_started
5 tool_started shell
6 tool_output shell (no output)
7 tool_completed shell true
8..11 delta ...
12 assistant Done — I ran `echo gf-fleet-p1 > p1-marker.txt` ...
13 turn_completed Completed
```

Los dos clientes imprimieron exactamente la misma secuencia; `diff` sin cambios.

## Lo que funciona y lo que no

**Funciona (VERIFIED en Linux x86_64):**

- Registrar un workspace real y crear sesión con `provider_id=opencode`.
- Enviar un prompt y recibir deltas, mensaje final, eventos de herramienta y
  fin de turno correcto.
- Dos clientes observando la misma sesión sin duplicar mensajes.
- Cerrar una sesión explícitamente (`fleetctl stop`) tiene efecto verificable:
  el driver elimina la sesión nativa OpenCode y el runner termina.
- `FakeDriver` permite el mismo flujo sin servicios externos (CI).
- Registro de proveedores abierto: `ProviderRegistry::register` permite añadir
  Codex y Pi sin tocar `SessionManager`.

**No implementado en P1 (no prometido):**

- Persistencia durable: al morir `fleetd` se pierde la timeline y no hay replay.
- Supervivencia a caída del daemon o del runner (P3).
- Aprobaciones interactivas: el driver no autoaprueba; un `permission.asked`
  se reporta como `AgentError` y no se responde (P2).
- Cancelación de turno, reconexión con turno en vuelo y dedup de prompts
  más allá de `operation_key` en memoria (P2/P3).
- Acceso remoto, SDK público, GPUI, base de datos y TCP distinto de loopback.

**Desconocido / no probado:** Windows (aunque el transporte usa `std::net` y no
`procfs`), SIGKILL, clientes lentos, lag de SSE y versiones de OpenCode distintas
de 2.3.2.

## Seguridad

No se guardan credenciales en el repo. El password de OpenCode se lee de
`service.json` o de stdout del proceso y solo se usa en la cabecera HTTP; no se
registra en logs ni en eventos. Los IDs nativos (`ses_*`, `msg_*`, `call_*`) no
salen del crate del driver. No se abre ningún puerto más allá de loopback.

## Referencias

- Código: `crates/`, `tests/e2e_fake.sh`, `tests/e2e_opencode.sh`,
  `.github/workflows/ci.yml`
- Investigación en que se basa el driver:
  [`research/OPENCODE.md`](research/OPENCODE.md) y
  [`spikes/p0/fixtures/opencode.jsonl`](../spikes/p0/fixtures/opencode.jsonl)
- Decisiones aplicadas: [`DECISIONS.md`](DECISIONS.md) (D01–D12, P0-D1…D6)
