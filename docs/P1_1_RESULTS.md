# P1.1 — resultados reales de estabilización y concurrencia

Rama: `fix/p1-session-concurrency`. Plataforma validada: **Linux x86_64**.
Windows: **no validado**; el token local depende de la ACL del fichero y no se
probó con otro usuario.

## 1. Problemas originales y causas

| # | Problema | Causa raíz |
|---|---|---|
| 1 | Eventos de una sesión llegaban a clientes de otra | `EventBus` era un broadcast global; el daemon no filtraba por `SessionId` |
| 2 | Secuencias y snapshot podían solaparse | La suscripción se registraba fuera del lock que asigna `seq`; el snapshot y el stream podían solaparse |
| 3 | Un cliente podía dejar receptores/hilos huérfanos | No había cancelación explícita de suscripción ni `join` controlado al reemplazarla |
| 4 | Dos prompts concurrentes podían activar dos turnos | No existía estado de turno en `SessionManager`; solo un `disposition` del driver |
| 5 | `operation_key` no era atómico ni carpaba sesión/payload | Se consultaba y se insertaba en dos locks distintos; solo se guardaba el recibo |
| 6 | Cualquier `AgentError` parecía un error de turno | No existía distinción entre aviso informativo y fallo terminal |
| 7 | `current_turn`/`turn_finished` del driver OpenCode podían asociar eventos viejos | El driver no tenía noción de turno activo y el `turn_end` del manager era informativo |
| 8 | `StopSession` podía colgarse | Se hacía `join()` incondicional tras esperar el cierre del driver |
| 9 | TCP en loopback sin autenticación | Se confundía «solo local» con «autenticado por usuario» |
| 10 | `ListAgents` devolvía versión `"per-version"` y capacidades falsas | El daemon construía resúmenes hardcodeados en vez de usar el registro |
| 11 | Suscriptor lento acumulaba eventos sin límite | Canal `mpsc` ilimitado y sin política de backpressure |

## 2. Correcciones implementadas

- **EventBus por sesión:** `subscribe(session_id, limit)`; `publish` enruta solo a
  suscriptores de ese `session_id`. Cada sesión conserva su `seq` desde 1.
- **Snapshot sin huecos:** la suscripción se crea bajo el mismo lock que produce
  el snapshot; las publicaciones quedan bloqueadas hasta que termina. El daemon
  envía snapshot y después arranca el reenviador, deduplicando por `seq`.
- **Cancelación:** `SubscriptionHandle` con `AtomicBool`; al reemplazar o cerrar
  una conexión se cancela y se da de baja. La cola es `sync_channel(256)` y el
  desbordamiento emite `SubscriberSignal::Overflow` → `Notice{Backpressure}`.
- **Turnos:** `TurnState` en `SessionManager`. Un segundo prompt devuelve
  `TurnAlreadyRunning`. Los eventos con `turn_id` diferente del activo se
  descartan. `AwaitingApproval` queda reservado para P2.
- **Idempotencia:** `operation_key` reservado en un solo lock con fingerprint
  `{session_id}\0{text}`; igual → mismo recibo, distinto → `Conflict`; el
  registro precede al despacho y un fallo queda marcado como `Failed`.
- **Semántica de errores:** `AgentNotice`/`AgentError` informativos,
  `TurnFailed` terminal, `TurnUncertain` cuando no hay evidencia. Solo
  `TurnCompleted`/`TurnUncertain` son resultados de turno.
- **Cierre acotado:** ack de 500 ms; si el driver no responde se publica el
  estado `Stopped` y un aviso, y se suelta el hilo sin `join()` bloqueante.
- **Transporte:** Unix socket 0700/0600 en Linux; token local en Windows
  (no validado). `Hello` es obligatorio antes de cualquier request.
- **ListAgents real:** versión detectada, disponibilidad, capacidades y modo
  desde `ProviderRegistry::descriptors()`; OpenCode hace `probe()` leyendo
  `service.json` y `GET /api/info`.
- **OpenCode:** `permission.asked` → `AgentNotice`; pérdida de SSE → `TurnFailed`;
  eventos sin turno activo se descartan. El texto del prompt vuelve a viajar
  correctamente al driver (se corrigió un despacho con texto vacío).

## 3. Pruebas ejecutadas y resultados

| Prueba | Resultado |
|---|---|
| `cargo fmt --all -- --check` | PASS |
| `cargo clippy --workspace --all-targets -- -D warnings` | PASS |
| `cargo test --workspace` | PASS — 15 tests unitarios |
| `cargo build --workspace` | PASS |
| `tests/e2e_fake.sh` | PASS — dos clientes, secuencia `1..11` idéntica |
| `tests/e2e_concurrency.sh` | PASS — sesiones aisladas, `TurnAlreadyRunning`, clave duplicada, stop en 503 ms |
| `tests/e2e_backpressure.sh` | PASS — `Notice{Backpressure}` y desconexión del cliente lento |
| `tests/e2e_unix_permissions.sh` | PASS — socket 0600, directorio 0700, `runuser -u nobody` rechazado (exit 1) |
| `tests/e2e_opencode.sh` | PASS — OpenCode 2.3.2 real, dos clientes, `tool_started shell`, `turn_completed Completed`, archivo creado y sesión nativa limpiada |

`fleetctl agents` devolvió información real del registro:

```text
fake      0.1.0  Available  Owned      true
opencode  2.3.2  Available  Attached   true
```

La prueba real con OpenCode es opt-in y requiere el servicio local del usuario;
no se guardan credenciales.

## 4. Limitaciones pendientes

- **Windows no validado:** el token se lee de `fleetd.token`; no se probó su ACL
  con otro usuario. No se declara compatible.
- Idempotencia y timeline siguen siendo **solo en memoria**. Al morir `fleetd`
  se pierden; P2 las hará duraderas.
- `AwaitingApproval` está reservado: un `permission.asked` se informa como
  `AgentNotice` y no se responde. No se autoaprueba nada (P2).
- No hay replay persistente ni `GetOperation` durable (P2).
- No se probó SIGKILL del daemon ni supervivencia del runner (P3).
- El límite de 256 eventos por suscriptor es fijo en P1.1; se podrá configurar
  en P7.

## 5. Compatibilidad con P2

Los cambios están deliberadamente dentro de las responsabilidades actuales:

- El `seq` por sesión y el snapshot sin huecos son la base del cursor de P2.
- `OperationStatus` y el fingerprint de `operation_key` son la semilla de la
  tabla de operaciones durable; no hace falta cambiar `SessionManager`.
- `TurnState::Uncertain` y `AwaitingApproval` ya están modelados para P2.
- `AgentNotice` permite exponer aprobaciones pendientes sin inventar eventos.
- El EventBus particionado permite sustituir la timeline en memoria por el store
  sin tocar el contrato público.

## 6. Independencia de SessionManager

`fleet-core` no importa `fleet-driver-opencode`, `reqwest`, `serde_json` ni
ningún tipo HTTP/SSE. `SessionManager` recibe un `ProviderRegistry` y solo
conoce `AgentDriver`, `AgentEvent` y `DomainEvent`. Los IDs nativos de OpenCode
siguen confinados en `fleet-driver-opencode`. Codex y Gentle Shell/Pi podrán
registrarse en P4 sin modificar `SessionManager`.

## Revisión posterior P1.1.1 — 20/11/2026

Cambios realizados en la misma rama, pendientes de validación final en CI:

- `StopSession` solo devuelve éxito y `Stopped` si el driver confirmó `close()`; un timeout es `StopUnconfirmed`, y el worker puede confirmar `Stopped` más tarde.
- OpenCode comprueba la respuesta HTTP del `DELETE` en lugar de ignorarla; un resultado incierto no se reporta como cerrado.
- `Overflow` se notifica antes de cancelar y el reenviador comprueba la señal antes de terminar.
- Cada `SubscribeTimeline` tiene cursor propio; el reenviador anterior no publica después de cancelarse.
- El driver OpenCode espera `session.execution.started`, usa la secuencia nativa observada y `assistantMessageID` del paso para descartar eventos viejos. No se afirma que estos campos sean un contrato universal de otras versiones.
- Windows genera token aleatorio UUID v4 y lo lee después de crear el listener. Sigue **NO VALIDADO** el aislamiento de ACL en Windows; no publicar soporte de seguridad Windows aún.
- El test de otro usuario Linux declara `UNTESTED` cuando no dispone de privilegios para ejecutar `runuser`; ya no confunde falta de permiso para ejecutar `runuser` con rechazo del socket.

**Pendientes explícitos:** no hay todavía una garantía criptográfica de vinculación entre el prompt y todos los eventos de ejecución de OpenCode; P2 deberá implementar reconciliación basada en el historial nativo cuando existan huecos. La muerte real de un runner atascado es P3, no un cierre simulado. El transporte Windows no está validado para otro usuario.
