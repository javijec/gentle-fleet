# PLAN — Gentle Fleet v2

**Estado:** proyecto nuevo en fase de diseño. Ninguna capacidad está implementada ni validada todavía.

## Regla de avance

No hacer 12 fases horizontales antes de obtener una conversación real. Cada etapa debe producir un recorrido funcional comprobable y dejar CI verde. En cualquier momento es posible detener la etapa sin arrastrar otra a medio implementar.

| Etapa | Resultado observable | Condición para cerrarla |
|---|---|---|
| P0 — Compatibilidad real | Spikes de Gentle Shell, Codex y OpenCode en versiones fijadas; fixtures de chat, herramientas, permisos y recuperación | Documentación comprobada y tests de proceso ejecutables, sin credenciales en fixtures |
| P1 — Un chat local | Un CLI crea sesión, escribe prompt y recibe respuestas del primer proveedor, mediante Fleet API | Smoke real + simulador; dos clientes ven la misma secuencia; CI verde |
| P2 — Permisos y timeline | Requests de permiso resueltos una sola vez, timeline durable y replay por cursor | Tests de carrera, desconexión, lag y reinicio de cliente |
| P3 — Persistencia de procesos | Runner independiente permite reconectar después de caída del daemon | SIGKILL de daemon no pierde runner; reattach+replay sin duplicación; prueba Linux y Windows |
| P4 — Drivers adicionales | Codex y OpenCode integrados usando interfaz de capacidades, sin cambiar el gestor de sesiones | Misma suite de contrato para los tres drivers; funciones no soportadas visibles |
| P5 — Cliente remoto | Cliente local conecta a Fleet en otro host vía SSH autenticado, misma API | Caída de SSH y reconexión; no se inicia worker duplicado; sin TCP abierto |
| P6 — SDK y GPUI | Cliente de escritorio consume Fleet SDK, sin acceso interno a runners | Tests SDK, chat, permisos, desconexión y cambio de host |
| P7 — Endurecimiento y release | Empaquetado, instalación, límites, backups, migraciones y telemetría local | Suite de fallas, carga, Linux/Windows, docs y recuperación de datos |

## Condiciones mínimas de producto antes de anunciar MVP

- P0–P2 completos sobre al menos un proveedor con integración real.
- Modelo de recuperación documentado y probado para la caída de la UI.
- Configuración explícita de workspace y directorio de trabajo real.
- CI con cargo fmt/check/clippy/test y al menos una prueba negra de proceso (sin servicios de pago).
- Mensajes/turnos/permisos pueden reconstruirse desde un cursor, sin pérdida silenciosa ni doble envío de prompts.
- Al menos un error inducido por cada uno: proceso del agente muere, petición duplicada, socket cortado, permiso duplicado y cliente lento.

## No objetivos de estas etapas

No construir integración remota antes de la local, sistema de plugins antes del segundo driver, sincronización host-to-host, VFS propio, multiusuario con separación de UID/SID o abstracción general de PTY. PTY puede agregarse como *vista auxiliar* y no como fuente principal de conversación.

## Primeras tres tareas para el agente

1. Crear la estructura de crates definida en ARCHITECTURE, con CI sin dependencias privadas externas.
2. Crear `fleet-driver-fake` y una prueba de contrato que recorra Start → Input → Delta → Approval → Completed → Reconnect, todo a través de un cliente Fleet.
3. Crear el driver Gentle Shell únicamente después de registrar fixtures de un motor real; el test con motor real queda opt-in pero debe ejecutarse antes de cerrar P1.
