# Decisiones arquitectónicas — baseline nuevo

**Todas las decisiones están propuestas.** Los spikes P0 ya se ejecutaron con los tres motores reales (20/11/2026, ver [`P0_FINDINGS.md`](P0_FINDINGS.md) y [`research/PROVIDER_COMPARISON.md`](research/PROVIDER_COMPARISON.md)); las propuestas P0-D1…D6 requieren ratificación explícita. No se trasladan automáticamente los ADR antiguos.

| ID | Decisión | Motivo | Alternativa rechazada por ahora |
|---|---|---|---|
| D01 | Monorepo Rust con crates independientes y Cargo.lock único | Contratos, binarios y CI cambian juntos mientras el wire evoluciona | Tres repos privados y Protocol congelado prematuramente |
| D02 | SessionManager como autoridad de negocio por sesión | Reducir estados contradictorios y lugares con side effects | Daemon, worker y dos DBs decidiendo simultáneamente el estado |
| D03 | Un Fleet DB por host y spool runner solo para eventos no confirmados | Simplificar reconciliación; permitir daemon crash sin replicación de catálogos | Dos DB autoritativas por sesión |
| D04 | AgentDriver con capacidades declaradas y native IDs | Soportar Pi, Codex, OpenCode sin falsa paridad | Suponer que todos hablan Pi RPC o interpretar ANSI como chat |
| D05 | Procesos runner independientes de clientes | UI se desconecta sin terminar agentes | UI dueña de procesos o Herdr como runtime obligatorio |
| D06 | Protocolo tipado orientado a conversaciones/turnos | Contrato de producto primero | Copiar eventos RPC del agente o congelar solo operaciones CRUD |
| D07 | Modelo local de usuario confiable | Evitar inflación de complejidad de UID/SQLite | Aislamiento entre procesos del mismo UID como requisito del MVP |
| D08 | Acceso remoto SSH primero, mismo Fleet API | Seguridad y portabilidad con OpenSSH | TCP expuesto, token propio y relay obligatorio desde P1 |
| D09 | Nunca auto-reenviar efectos inciertos | No duplicar prompts, comandos ni permisos | Prometer exactly-once de procesos externos con una transacción SQLite |
| D10 | Linux/Windows por interfaz de plataforma desde P1 | Evitar reescritura de Unix-only runtime | Codificar /proc, flock y setsid en domain/core |
| D11 | Pruebas verticales antes de blindar subsistemas | Encontrar incompatibilidades reales temprano | Completar módulos horizontales con 100+ tests aislados antes del chat |
| D12 | Versionar wire después del primer smoke | Evitar un Protocol que impide la función central | Publicar v1 inmutable sin mensajes de conversación ni subscribe |

## Concreciones propuestas tras P0 (pendientes de ratificación)

Los IDs, la justificación, las alternativas y las consecuencias de P0-D1…D6 están
desarrollados en
[`research/PROVIDER_COMPARISON.md`](research/PROVIDER_COMPARISON.md#propuestas-p0-d1d6--pendientes-de-ratificación).
Resumen: P0-D1 OpenCode como primer driver real; P0-D2 mantener D04 con
capacidades por versión; P0-D3 Runner que posee o se adjunta; P0-D4 reconciliar
antes de reenviar; P0-D5 fixtures por versión; P0-D6 cursor Fleet propio.
**Ninguna está ratificada.**

## Deudas de arquitectura aceptadas temporalmente

- Primer release no aísla procesos del mismo usuario.
- No garantiza ejecución exactly-once tras corte entre red y proceso externo.
- No garantiza recuperar procesos tras reiniciar el host; resume dependerá del agente.
- Retención de eventos sin snapshot se pospone hasta demostrar replay completo; el tamaño se limita al período de prueba y se monitorea.
- No todas las capacidades son equivalentes entre agentes.

## Regla para reabrir decisiones

Exigir un caso de uso real no resuelto, una reproducción y comparación de al menos dos alternativas con costo de implementación y pruebas. Documentar la decisión en ADR breve. No se permite cambiar D01–D12 solo para eliminar un error teórico fuera del modelo.
