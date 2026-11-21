# Contrato Fleet API 2 — propuesta inicial, NO congelada

## Objetivo

Un único protocolo cliente/servidor independientemente de local (Unix socket / Windows named pipe) o remoto (SSH stdio). El contrato no contiene tipos de Gentle Shell, Codex u OpenCode. La semántica se estabiliza después del primer smoke real, no antes.

## Transporte y framing

- En la fase P1: JSONL con un frame por línea; UTF-8; máximo 1 MiB por frame con error explícito. Ampliar tamaño solo con tests.
- Una conexión se negocia con `Hello{client_id, token?}`; en Unix el socket 0600 del directorio 0700 es la garantía de usuario; en Windows `token` se lee de un fichero privado (plataforma no validada en P1.1). Cualquier request anterior a `Hello` devuelve `Unauthorized`.
- Transporte P1.1: Unix Domain Socket en Linux; TCP loopback + token en Windows (`UNTESTED`). Los envelopes no cambian para P5.
- Frames etiquetados con kind=request|response|event, id de correlación y, para eventos, sesión y secuencia. Jamás se asume que una notificación push es respuesta de otra petición.
- Un request tiene request_id para correlación **y** operation_key para idempotencia de efectos; no confundirlos.
- Operaciones asíncronas devuelven Accepted{operation_id} solo cuando una intención queda durable; la finalización llega como eventos y GetOperation.

## Comandos de primer nivel

| Operación | Semántica | Mutación |
|---|---|---|
| GetHost / ListHosts | Identidad y capacidades del host conectado | No |
| ListAgents | `id`, versión detectada (`null` si no disponible), `availability` (`Available`/`Unavailable`), `capabilities` efectivas y `mode` (`Owned`/`Attached`/`Auto`) del registro real | No |
| ListWorkspaces / RegisterWorkspace | Directorios autorizados del host; no inferir identidad por ruta | Sí, registro |
| CreateSession | Crear identidad Fleet y vincular workspace/proveedor | Sí |
| StartSession | Iniciar/reanudar runner cuando haya garantía posible | Sí |
| SendPrompt | Reservar `operation_key`, crear turno y despachar una sola vez; `TurnAlreadyRunning` si hay turno activo, `Conflict` si la clave cambia de payload/sesión | Sí |
| AnswerApproval | Responder una única solicitud pendiente vinculada a sesión/turno | Sí |
| InterruptTurn | Pedir interrupción; no promete abort instantáneo | Sí |
| StopSession | Cierre explícito; debe producir resultado observable | Sí |
| GetSession / ListSessions | Estado observable, no inferido de caches de cliente | No |
| SubscribeTimeline | Snapshot durable paginado + eventos live desde cursor, sin agujero entre ambos | No |
| ReadTimeline | Leer páginas de hasta 256 eventos desde `since_seq` (fallback/recovery) | No |
| GetOperation | Estado durable de una operación: `reserved`, `accepted`, `completed`, `failed` o `uncertain` | No |
| ListApprovals / GetApproval | Listar o consultar solicitudes de aprobación por sesión | No |
| AnswerApproval | Registrar la decisión ganadora de una aprobación; `once`/`always`/`reject` según el proveedor | Sí |

Los nombres son propuestos, no un wire publicado. Mantener separado protocolo de API y mensaje nativo de agente.

**P2:** los eventos se persisten en SQLite antes de publicarse; `seq` sigue siendo
propiedad de Fleet y monotónico por sesión. `ApprovalRequested` y
`ApprovalResolved` son eventos tipados; `native_request_id` permanece dentro del
driver. Dos clientes pueden ver la misma aprobación, pero solo la primera
decisión `AnswerApproval` se registra; las demás devuelven `Conflict`. Una
aprobación sin respuesta nunca se autoaprueba. Si la entrega al proveedor es
incierta, la aprobación y la operación pasan a `uncertain` y no se reintentan.
`GetOperation` consulta el estado durable; `ReadTimeline` pagina desde un cursor
sin cargar la timeline completa en RAM.

## Modelo mínimo de eventos tipados

Envelope: event_id, session_id, seq, timestamp, kind, payload, turn_id? y operation_id?. Secuencia entera monotonía por sesión desde 1; la deduplicación usa (host_id, session_id, seq). La marca de tiempo es metadato y **no** determina orden.

- SessionCreated / SessionStateChanged / RunnerHealthChanged
- TurnStarted / UserMessage / AssistantDelta / AssistantMessage / TurnCompleted
- ToolStarted / ToolOutput / ToolCompleted
- ApprovalRequested / ApprovalResolved / ApprovalExpired
- AgentNotice / AgentError / RecoveryNotice
- TurnUncertain (turno sin resultado terminal observable; nunca se reenvía)
- Backpressure (el suscriptor lento se desconecta; también existe `Notice{code:"Backpressure"}` fuera de la secuencia de eventos)

Las suscripciones están ligadas a un `SessionId`; cada sesión tiene su propio `seq` desde 1. Snapshot y streaming se registran bajo el mismo lock para evitar huecos; el cliente deduplica por `seq`. `AgentError` es informativo y **no** cierra un turno: solo `TurnCompleted`/`TurnUncertain` son resultados de turno. Delatas, tokens y eventos de herramientas se emiten en límites configurables. Los payloads privados no se envían tal cual. Las capacidades del driver determinan qué eventos existen: si el proveedor solo da mensajes completos, no fabricar deltas falsos.

## Cursores y suscripción

El cursor se representa como {host_id,session_id,sequence}; no se mezclan épocas de runners con la timeline Fleet. En una suscripción el servidor calcula snapshot en una lectura consistente, captura su seq de corte y envía solamente eventos posteriores a ese corte. Si el cursor está retenido fuera del historial, devuelve Gap{earliest_available} y requiere un snapshot/resync; jamás avanza silenciosamente.

Un suscriptor lento recibe Backpressure/Gap y debe reconectar con cursor; nunca se amplía su cola sin límite. Reentrega es at-least-once y el cliente deduplica por secuencia.

## Operaciones idempotentes

Cada operación de cambio tiene operation_key generado por cliente y scope {principal, host, operation_kind, target}. La implementación almacena hash canónico de argumentos relevantes y resultado: misma clave con mismo contenido devuelve operación existente; misma clave con otro contenido = Conflict. El plazo de retención debe publicarse y ser suficiente para reintentos razonables; no prometer exactly-once de efectos externos.

Si la comunicación se corta después de transmitir un prompt y no puede probarse si el agente lo recibió, estado = OutcomeUnknown o NeedsReconciliation. El cliente consulta GetOperation y el gestor interroga al driver cuando es viable; **nunca reenvía automáticamente ese prompt**.

## Permisos

ApprovalRequest es un recurso de dominio (approval_id, session_id, turn_id, challenge, allowed_decisions, expires_at?). La respuesta es atómica; un único ganador. Las decisiones permitidas dependen del proveedor y del request específico. Ausencia de respuesta produce pendiente, cancelación o denegación según la política documentada, pero nunca Allow. No convertir input/editor/select/confirm en un booleano genérico si perdería semántica.

## Errores

NotFound; UnsupportedCapability; InvalidArgument; Unauthorized; Forbidden; Conflict; Backpressure; RetentionGap; ProviderUnavailable; Timeout; OutcomeUnknown; NeedsReconciliation; Internal. Los errores no deben revelar secretos ni confundir fallo de permiso con proceso perdido. Un error por timeout no prueba que la operación no ocurrió.

## Esquema conceptual (no normativo)

~~~json
{"kind":"request","request_id":"req-14","operation_key":"op-7","command":{"type":"send_prompt","session_id":"session-1","text":"Resumí los cambios"}}
{"kind":"response","request_id":"req-14","result":{"type":"accepted","operation_id":"operation-7"}}
{"kind":"event","session_id":"session-1","sequence":41,"payload":{"type":"assistant_delta","turn_id":"turn-1","text":"Encontré "}}
{"kind":"event","session_id":"session-1","sequence":42,"payload":{"type":"turn_completed","turn_id":"turn-1","status":"completed"}}
~~~

Los ejemplos no son fixtures definitivos. No escribir SDK público hasta validar estas transiciones con un driver real.


**P3:** el protocolo cliente (`fleetctl` ↔ `fleetd`) no cambia. Internamente
`fleetd` usa `fleet_protocol::runner` (`fleet-runner-v1`) sobre socket local
autenticado. Los eventos del runner llegan con `runner_seq`, se deduplican y se
publican al cliente con el `seq` Fleet. `RunnerHealthChanged` y `RecoveryNotice`
son los eventos públicos previstos para el estado del runner; la recuperación
sin evidencia suficiente mantiene `NeedsReconciliation` y no reenvía prompts.


**P4.1:** el contrato público incorpora `InterruptTurn`, `ListControls`, `SetControl`, `ListNativeSessions` e `ImportNativeHistory`. `AnswerApproval` acepta `arguments` opcionales (`name` + `values`) y `ApprovalRequested` publica `options` con `option_id` Fleet y esquema de argumentos; los IDs y payloads nativos siguen siendo privados. `ListAgents` reporta `controls` descubiertos por versión. Los nuevos campos son opcionales y compatibles con clientes anteriores. La importación de historial emite `history` con `provenance=imported_from_provider`; nunca se presenta como evento observado en vivo. Ver [`P4_1_RESULTS.md`](P4_1_RESULTS.md).
