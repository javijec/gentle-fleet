# PLAN — Gentle Fleet v2

**Estado:** **P0, P1, P1.1, P2, P3, P4.1, P4.2 y P4.3 CERRADOS EN LINUX; P4.0 COMPLETADO** (22/11/2026). P0-D1…D6 ratificadas en `docs/DECISIONS.md`. Plataforma de referencia: **Linux x86_64 hasta completar P6**; la validación funcional de Windows y macOS se realiza en P7, sin declarar compatibilidad antes. P4.4 sigue pendiente. Resultados: `docs/P1_RESULTS.md`, `docs/P1_1_RESULTS.md`, `docs/P2_RESULTS.md`, `docs/P3_RESULTS.md`, `docs/P4_1_RESULTS.md`, `docs/P4_2_RESULTS.md`, `docs/P4_3_RESULTS.md` y entregables P4.0 (`docs/P4_PROVIDER_MATRIX.md`, `docs/P4_RPC_RESEARCH.md`, `docs/P4_CONTRACT_DESIGN.md`, `docs/P4_TEST_PLAN.md`).

## Estrategia de plataforma

Linux x86_64 es la plataforma de referencia para los criterios de cierre de P1 a P6. La arquitectura permanece portable (sin suposiciones Linux-only en `fleet-core`/`fleet-domain`, abstracciones de transporte y capacidades declaradas) y se conservan las comprobaciones de compilación cruzada disponibles. Windows y macOS quedan explícitamente pendientes de validación funcional en P7; **no se afirma que funcionen**.

## Regla de avance

No hacer 12 fases horizontales antes de obtener una conversación real. Cada etapa debe producir un recorrido funcional comprobable y dejar CI verde. En cualquier momento es posible detener la etapa sin arrastrar otra a medio implementar.

| Etapa | Resultado observable | Condición para cerrarla |
|---|---|---|
| P0 — Compatibilidad real ✅ **CERRADO** | Spikes de Gentle Shell, Codex y OpenCode en versiones fijadas; fixtures de chat, herramientas, permisos y recuperación | Cumplido: 3 probes con `PASS`, capturas anonimizadas, `docs/P0_FINDINGS.md` y `docs/research/`; P0-D1…D6 ratificadas |
| P1 — Un chat local con OpenCode ✅ **CERRADO** | Un CLI registra workspace, crea sesión con `provider_id=opencode`, envía prompt y recibe texto y herramientas mediante Fleet API | Cumplido: FakeDriver + smoke real con OpenCode, dos clientes con la misma secuencia `seq`, fmt/clippy/test y e2e en verde; ver `docs/P1_RESULTS.md` |
| P1.1 — Estabilización y concurrencia ✅ **CERRADO** | Sesiones y eventos aislados, un turno activo por sesión, idempotencia atómica, stop acotado, transporte local por usuario y colas acotadas | Cumplido: 15 tests unitarios, e2e de aislamiento/carrera/backpressure/ACL y smoke real con OpenCode; ver `docs/P1_1_RESULTS.md` |
| P2 — Permisos y timeline ✅ **CERRADO** | SQLite WAL con migraciones, timeline paginada por cursor, operaciones idempotentes y aprobaciones de OpenCode | Cumplido: 25 tests unitarios, e2e de persistencia/reinicio, aprobaciones con un ganador y smoke real con `once`/`reject`; ver `docs/P2_RESULTS.md` |
| P3 — Persistencia de procesos ✅ **CERRADA EN LINUX** | `fleet-runner` independiente, reattach autenticado y spool acotado | Cumplido en Linux: supervivencia ante SIGKILL de `fleetd`, reattach autenticado, replay durable sin duplicados, aprobaciones consistentes, spool acotado con backpressure, CI y smokes reales de OpenCode. Ver `docs/P3_RESULTS.md`. Windows/macOS: validación funcional en P7 |
| P4.0 — Investigación y contratos ✅ **COMPLETADO** | Versiones reales, protocolos y contratos propuestos para Codex, Pi y Gentle Shell, sin implementar drivers | Cuatro entregables producidos y validación Rust en verde; decisiones funcionales de `docs/P4_SPEC.md` intactas |
| P4.1 — Codex ✅ **CERRADA EN LINUX** | Driver App Server real con chat, herramientas, aprobaciones tipadas, controles por turno, descubrimiento e historial | Ver `docs/P4_1_RESULTS.md` §8: 8 smokes reales, 72 tests unitarios y regresiones P1–P3 en verde; adopción con escritura bloqueada por falta de exclusividad |
| P4.2 — Pi estándar **P4.2-H EN REVALIDACIÓN** | Driver Pi RPC 1.6.0, proceso propio, `agent_settled`, controles e historial | Ver `docs/P4_2_RESULTS.md`: chat/tools/interrupt/controles reales PASS; sin Desktop RPC ni extensiones Gentle |
| P4.3 — Gentle Shell ✅ **CERRADA EN LINUX** | Proveedor propio con Desktop RPC negociado, permisos nativos y controles de solo lectura | Criterios de `docs/P4_TEST_PLAN.md` §3; subagentes y GPUI fuera de alcance |
| P4.4 — Conformidad | Suite matrix-driven para cuatro proveedores y regresiones P1–P3 | Criterios de `docs/P4_TEST_PLAN.md` §3; OpenCode como línea base |
| P5 — Cliente remoto | Cliente local conecta a Fleet en otro host vía SSH autenticado, misma API | Caída de SSH y reconexión; no se inicia worker duplicado; sin TCP abierto |
| P6 — SDK y GPUI | Cliente de escritorio consume Fleet SDK, sin acceso interno a runners | Tests SDK, chat, permisos, desconexión y cambio de host |
| P7 — Endurecimiento y release | Empaquetado, instalación, límites, backups, migraciones y telemetría local | Suite de fallas, carga, validación funcional de Windows y macOS, docs y recuperación de datos |

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
