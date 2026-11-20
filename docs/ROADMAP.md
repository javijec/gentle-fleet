# Roadmap de implementación desde un árbol vacío

## P0: investigar antes de fijar contratos

**Salida**: tres programas de prueba y fixtures anonimizados, que ejercitan sesiones y permisos reales en Gentle Shell, Codex y OpenCode. Medir capacidades disponibles, IDs nativos, tipos de mensajes, cambios de versión y capacidad de recuperación. Verificar qué proveedores necesitan proceso propio o pueden conectarse a servidor de usuario. Elegir el driver más viable para P1. Ningún servicio persistente ni esquema definitivo todavía.

**Estado (20/11/2026): CERRADO.** Programas en `spikes/p0/`, capturas anonimizadas, resumen en `docs/P0_FINDINGS.md` y detalle por motor en `docs/research/`. Los tres probes pasan con los binarios instalados. P0-D1…D6 ratificadas en `DECISIONS.md`: OpenCode es el primer driver de P1. No se creó ningún servicio persistente ni esquema definitivo.

## P1: primera conversación completa desde CLI

**Salida**: un solo proceso `fleetd`, un Runner con modalidad propia/adjunta, `FakeDriver` y **`OpenCodeDriver` como primer proveedor real**. `fleetctl` registra workspace, crea sesión con `provider_id`, envía texto, observa respuesta y fin de turno. Dos conexiones reciben el mismo timeline en vivo con `seq` asignado por Fleet. Se congela el esquema de eventos P1 tras el smoke real. No agregar persistencia durable, remoto, aprobaciones ni GPUI aquí.

**Estado (20/11/2026): CERRADO.** Ver [`P1_RESULTS.md`](P1_RESULTS.md): fmt, clippy y tests en verde; e2e con FakeDriver y e2e real con OpenCode 2.3.2 pasan con dos clientes y secuencias idénticas.

## P2: persistencia y aprobación

**Salida**: SQLite única con sesiones, operaciones y eventos, idempotencia antes de efecto, ack y timeline reconstruible; solicita/aprueba/deniega permiso; el cliente puede cerrarse, reiniciarse y leer eventos con cursor. Probar cortes antes/después de commit, doble decisión, gap y colas llenas. No prometer supervivencia al daemon crash todavía.

## P3: runner persistente y reconexión de daemon

**Salida**: proceso fleet-runner independiente de fleetd, identidad autenticada, spool acotado de eventos no ACK, retransmisión deduplicada y reattachment. Probar muerte del daemon con runner vivo en Linux y Windows. Solo entonces anunciar que las sesiones sobreviven al reinicio del daemon.

## P4: tres motores y capacidad de proveedor

**Salida**: tests de conformidad comunes para **Codex y Gentle Shell/Pi** reutilizando el contrato ya ejercitado por OpenCode; descubrimiento de versión, inicio, send, salida, permisos/capacidades y parada. Reutilizar los contratos Fleet sin agregar ramas de proveedor dentro de SessionManager. El soporte de resume se declara por driver/versión.

## P5: local/remoto idénticos

**Salida**: bridge SSH autenticado contra fleetd remoto con los mismos envelopes; CLI puede listar workspaces/sesiones, iniciar un agente en cwd remoto, escuchar eventos, aprobar y reconectar tras cortar SSH. No compartir bases entre hosts, no abrir listener TCP público.

## P6: SDK reusable + interfaz GPUI

**Salida**: SDK de cliente con gestión de hosts, stream cursors, backoff y deduplicación, consumido por CLI/GPUI. El cliente muestra lista de sesiones, timeline de chat, herramientas, panel de permisos y desconexión/reattach. GPUI no importa el runtime interno. Soporte de terminal/archivos se añade como capacidad adicional, nunca como sustituto del chat.

## P7: robustez operacional y release

**Salida**: límites configurables y métricas de runners/conexiones/eventos/spool; backups y migraciones; pruebas de consumo bajo carga; installers/upgrades para Linux y Windows; logs seguros; documentación de recuperación y compatibilidad; versión de wire publicada. No convertir P7 en una auditoría infinita: definir presupuesto de riesgos y criterios de release medibles.

## Regla para ampliar el alcance

Nuevas capacidades como git/worktree, filesystem remoto, delegación multiagente, notificaciones, plugins, MCP y automatizaciones entran solo después de P6 y mediante ADR más prueba vertical. La arquitectura debe admitirlas mediante interfaces claras; no se implementan anticipadamente.

## Etapas de migración desde el desarrollo anterior

No existe migración automática. No se hacen cherry-picks de M0–M4, no se arrastra su schema ni se repite su fichero `PLAN.md`. Si una pieza vieja demuestra ser útil, se reescribe con su contrato y nueva prueba, con revisión explícita. Mantener la historia anterior solo como referencia técnica accesible. No publicar Core v2 hasta que P1 tenga chat real.
