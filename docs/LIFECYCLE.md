# Sesiones, turnos, operaciones y recuperación

## Cuatro conceptos separados

1. **Sesión Fleet:** relación duradera entre workspace/proveedor y un historial de conversación.
2. **Runner:** proceso de control asociado a una sesión; puede morir o cambiar sin cambiar necesariamente el ID Fleet.
3. **Turn:** una unidad de trabajo del agente que comienza con entrada del usuario y termina con éxito, fallo o cancelación.
4. **Operation:** intento de mutación solicitado por un cliente, con clave idempotente y resultado verificable o incierto.

No codificar estas cuatro dimensiones en un único enum gigante. La conectividad no determina el estado de una sesión: un runner vivo puede no responder; un proveedor puede estar esperando permisos con socket conectado.

## Estados propuestos

Session: Created → Starting → Running → Stopping → Stopped. Ramas de fallo: Failed, NeedsReconciliation. Running puede coexistir con un Turn Pending/Running/WaitingApproval; el estado de conexión se modela aparte (Online, Unresponsive, Disconnected). Una sesión Stopped nunca se relanza con el mismo intento por accidente.

Turn: Queued → Running → WaitingApproval ↔ Running → Completed / Failed / Cancelled / OutcomeUnknown.

Operation: Prepared → Dispatched → Confirmed / Rejected / OutcomeUnknown → Reconciled. Un resultado confirmado es durable y se puede consultar después de reiniciar.

## Algoritmo de StartSession

1. Autenticar cliente y autorizar host/workspace.
2. En una transacción crear sesión/operación, reservar runner_id y registrar intención; devolver Accepted únicamente si commit ok.
3. El SessionManager serializa mutaciones de esa sesión, valida límites de runners y asigna un proceso/runner concreto.
4. Runner arranca bajo perfil de usuario, publica identidad de instancia y confirma posesión.
5. El primer evento durable indica Running solamente después de handshake y evidencia real del driver.
6. Si hay caída en pasos 3–5, no lanzar otro runner por un retry. Reconciliar runner_id y estado comprobables; de lo contrario NeedsReconciliation.

## SendPrompt y efectos externos

El desafío principal no es deduplicar un INSERT; es no ejecutar dos veces un prompt. Se persiste la intención primero y se envía usando operation_id. Cuando el agente acepta un identificador correlacionable, se persiste. Si la respuesta es ambigua, el estado permanece OutcomeUnknown. Una capacidad de consulta/reanudación del proveedor puede resolverla; si no, se requiere intervención o el usuario asume conscientemente el riesgo de enviar de nuevo como operación diferente. No declarar entrega exactly-once universal.

## Aprobaciones

El driver entrega solicitud estructurada; SessionManager la registra, crea approval_id y la expone. Dos clientes pueden verla; solo una transacción acepta la primera decisión válida. Runner confirma entrega de decisión; el request no se considera resuelto hasta evidencia suficiente. Timeout de transporte = resultado incierto, nunca aprobación tácita. Una aprobación que venció o fue cancelada no puede revivirse por reconexión del cliente.

## Matriz de fallos

| Evento | Expectativa MVP / fase | Acción |
|---|---|---|
| Se cierra CLI o GPUI | P1/P2 | Runner y agente continúan; otra UI reanuda timeline |
| Cae conexión SSH | P5 | Runner remoto continúa; cliente reautentica y usa cursor |
| Muere fleetd | P3 | Runner vivo conserva conexión y spool de eventos; daemon nuevo reanuda tras autenticar runner |
| Muere runner | P3 | No fingir que el agente continúa; intentar recuperación nativa solo si driver la garantiza; en otro caso Failed/NeedsReconciliation |
| Muere agente | P1 | Evento de fallo, turn fallido y runner recolectado; no relanzar prompt sin petición explícita |
| Reinicio del host | Futuro, distinto de P3 | Proveedor reanudable puede reconstruir sesión; el proceso original no sobrevive |
| SQLite cae antes de commit de operación | P2 | No efecto externo admitido antes del commit; reintento crea una sola intención |
| SQLite cae después de commit y antes de ejecutar | P2/P3 | Intención duradera; reconciliar en inicio, no ejecutar dos veces |
| Daemon pierde ACK de un evento | P3 | Runner reenvía su seq, BD deduplica; evento ya persistido no se duplica |
| Cliente pierde eventos por lag | P2 | Gap y read timeline; nunca omisión silenciosa |

## Recuperación por reinicio

Al arrancar, fleetd abre el store, marca operaciones InFlight como Recovering y consulta runner descriptors locales; usa handshake autenticado con identidad persistida y nonce para saber cuál runner controla cada sesión. Si no se encuentra, diferencia process_dead, descriptor_missing y unknown; un timeout no equivale a muerte. Después consume unacked runner events, reconstruye el estado del proveedor y publica RecoveryNotice. Una recuperación nunca reescribe la historia pasada como si el proveedor hubiera contestado algo que no se observó.

## Reglas de parada

StopSession es una operación explícita y auditable: pide al driver cancelación o shutdown según sus capacidades, espera un plazo acotado y puede escalar a matar el proceso bajo una política configurable. La salida del cliente no llama a StopSession. La limpieza de archivos y procesos solo ocurre cuando la propiedad y la finalización están verificadas.
