# PLAN — Gentle Fleet v2

**Estado:** **P0, P1, P1.1, P2 y P3 CERRADOS** (21/11/2026). P0-D1…D6 ratificadas en `docs/DECISIONS.md`. Runner independiente con supervivencia demostrada ante SIGKILL de `fleetd`. Codex y Gentle Shell/Pi quedan para P4. Resultados: `docs/P1_RESULTS.md`, `docs/P1_1_RESULTS.md`, `docs/P2_RESULTS.md` y `docs/P3_RESULTS.md`.

## Regla de avance

No hacer 12 fases horizontales antes de obtener una conversación real. Cada etapa debe producir un recorrido funcional comprobable y dejar CI verde. En cualquier momento es posible detener la etapa sin arrastrar otra a medio implementar.

| Etapa | Resultado observable | Condición para cerrarla |
|---|---|---|
| P0 — Compatibilidad real ✅ **CERRADO** | Spikes de Gentle Shell, Codex y OpenCode en versiones fijadas; fixtures de chat, herramientas, permisos y recuperación | Cumplido: 3 probes con `PASS`, capturas anonimizadas, `docs/P0_FINDINGS.md` y `docs/research/`; P0-D1…D6 ratificadas |
| P1 — Un chat local con OpenCode ✅ **CERRADO** | Un CLI registra workspace, crea sesión con `provider_id=opencode`, envía prompt y recibe texto y herramientas mediante Fleet API | Cumplido: FakeDriver + smoke real con OpenCode, dos clientes con la misma secuencia `seq`, fmt/clippy/test y e2e en verde; ver `docs/P1_RESULTS.md` |
| P1.1 — Estabilización y concurrencia ✅ **CERRADO** | Sesiones y eventos aislados, un turno activo por sesión, idempotencia atómica, stop acotado, transporte local por usuario y colas acotadas | Cumplido: 15 tests unitarios, e2e de aislamiento/carrera/backpressure/ACL y smoke real con OpenCode; ver `docs/P1_1_RESULTS.md` |
| P2 — Permisos y timeline ✅ **CERRADO** | SQLite WAL con migraciones, timeline paginada por cursor, operaciones idempotentes y aprobaciones de OpenCode | Cumplido: 25 tests unitarios, e2e de persistencia/reinicio, aprobaciones con un ganador y smoke real con `once`/`reject`; ver `docs/P2_RESULTS.md` |
| P3 — Persistencia de procesos ✅ **CERRADO** | `fleet-runner` independiente, reattach autenticado y spool acotado | Cumplido en Linux: SIGKILL real de `fleetd` con turno OpenCode en curso, replay sin duplicación y aprobaciones tras desconexión; ver `docs/P3_RESULTS.md`. Windows no validado |
| P4 — Drivers adicionales | Codex y después Gentle Shell/Pi integrados usando la interfaz de capacidades, sin cambiar el gestor de sesiones | Misma suite de contrato para los tres drivers; funciones no soportadas visibles; resume declarado por versión |
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

## Orden de trabajo de P1

1. Crear la estructura de crates definida en ARCHITECTURE, con dependencias públicas fijadas y CI sin repositorios privados.
2. Contratos mínimos de dominio y `AgentDriver`, `FakeDriver` determinista y API compartida entre `fleetd` y `fleetctl`.
3. `OpenCodeDriver` contra la API HTTP/SSE investigada en P0, smoke real y prueba de dos clientes. Codex y Pi se registran en P4, no en P1.
