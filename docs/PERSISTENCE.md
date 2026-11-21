# Persistencia, transacciones y replay

## Decisión

**Una sola base autoritativa por daemon/usuario/host**, fleet.sqlite con SQLite WAL, y ownership de escritura en fleetd. El runner no es un segundo catálogo: solo lleva un spool acotado de eventos pendientes de confirmación durante una desconexión del daemon. Se evitan dos bases SQLite con estados de sesión Fleet potencialmente contradictorios.

## Esquema lógico mínimo (no migración final)

~~~sql
hosts(host_id PK, created_at, schema_version)
workspaces(workspace_id PK, path, label, created_at)
sessions(session_id PK, workspace_id FK, provider_id, native_session_id NULL,
         state, runner_id NULL, created_at, updated_at)
operations(operation_id PK, principal_id, operation_key, kind, session_id NULL,
           request_hash, status, result NULL, created_at, expires_at)
turns(turn_id PK, session_id FK, provider_turn_id NULL, status, operation_id FK)
approvals(approval_id PK, session_id FK, turn_id FK NOT NULL, native_request_id,
          status, decision NULL, operation_id NULL)
events(session_id, seq, event_id, kind, payload_json, timestamp,
       runner_id NULL, runner_seq NULL,
       PRIMARY KEY(session_id, seq), UNIQUE(runner_id, runner_seq))
runner_bindings(runner_id PK, session_id UNIQUE, generation, endpoint,
                secret_ref, status, last_ack_seq, updated_at)
~~~/

En el primer prototipo no se crean tablas que no use el recorrido vertical. Este esquema define **propietarios de datos**, no SQL listo para correr. Se permiten migraciones forward-only y backups probados desde P2.

## Eventos y commit

- La timeline Fleet tiene secuencia entera creciente por sesión. Un solo writer lógico en fleetd ordena eventos.
- El runner emite runner_seq monótono por su identidad persistente; fleetd inserta evento normalizado y asocia la correspondencia en la misma transacción; ACK del runner después del commit.
- Ante reenvío tras crash se reconoce UNIQUE(runner_id,runner_seq) y se retorna el ACK anterior sin generar un evento doble.
- El spool del runner almacena el mínimo para retransmitir mensajes no reconocidos; está acotado por bytes/eventos. Si se agota, el runner deja de aceptar nuevas operaciones cuando el proveedor no puede pausarse, o emite un error explícito de pérdida irreversible. **Nunca llama exitoso a un evento perdido**.
- Solo publicar un evento al socket después de persistirlo. Un cliente puede recibirlo dos veces al reconectar; deduplicación por (host_id,session_id,seq).

## Idempotencia de operaciones

Un identificador de operación separa solicitud aceptada y efecto del proveedor. La clave repetida con misma operación y payload recupera su estado; payload diferente devuelve Conflict. `SendPrompt` es el caso crítico: una transacción SQLite nunca vuelve atómico el envío externo al proceso agente. Si el efecto fue transmitido y se perdió la confirmación, la operación queda OutcomeUnknown y **no se reenvía automáticamente**. Un driver con correlación nativa puede reconciliarla; sin capacidad nativa, se requiere decisión explícita.

La retención de operaciones se anuncia y se prueba. No borrar operaciones que aún referencian turnos inciertos solo para mantener la DB pequeña. La recuperación no usa timestamps como prueba de unicidad.

## Capacidad y retención

Configurar límites para eventos, imágenes, tool output y spool. Las entradas enormes se almacenan como blobs separados con referencia tipada y permisos privados. La política de retención se introduce después de demostrar replay completo: para recortar, primero producir snapshot y marcador de corte consistentes, después purgar; cualquier cursor viejo produce Gap. No truncar silenciosamente.

## Integridad y recuperación

- Migrations y `PRAGMA integrity_check` en tests de upgrade; backup/restore en pruebas de corte de proceso. `user_version=2` añade foreign keys en `approvals` y `user_version=3` añade `runners`/`runner_events`; una DB con aprobaciones huérfanas bloquea la migración en lugar de descartar datos silenciosamente.
- SQLite WAL requiere manejo correcto de -wal/-shm, fsync y directorio privado; no inventar identity fingerprint inode ni crear VFS propio.
- No considerar la modificación por un proceso malicioso del mismo usuario como frontera de aislamiento. Sí defender contra datos corruptos, permisos accidentales y bugs normales.
- Los vínculos de runner a sesión se validan al reconectar con secreto/identity de runtime y generación; PID por sí solo no es prueba de ownership.
- La protección contra reemplazo de DB por procesos del mismo usuario queda fuera del modelo local, expresamente documentada.

## Test obligatorio de cortes

Inyectar caída: antes de aceptar mutación, después del commit, antes de enviar al proveedor, tras envío pero antes de ACK, después de persistir evento y antes de avisar al cliente. Cada interleaving debe recuperar un resultado confirmado o incierto **sin reejecutar automáticamente un efecto ambiguo**.


## P3: spool del runner frente a base autoritativa

`fleet.sqlite` sigue siendo la única autoridad para sesiones, turnos,
operaciones, aprobaciones y timeline. Cada runner posee un spool SQLite propio
en el directorio de runtime con `spool(runner_seq, event, bytes, acked)` y
`operations(operation_key, operation_id)`. El spool **no** es un catálogo Fleet:
sólo conserva eventos aún no confirmados por el daemon. El daemon persiste cada
evento y registra `(runner_id, runner_seq)` en la misma transición lógica antes
de enviar el ACK; un reenvío se deduplica y no produce un segundo evento Fleet.
El `seq` público continúa siendo propiedad del SessionManager.
