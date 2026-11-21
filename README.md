# Gentle Fleet 2.0 — proyecto reiniciado desde cero

**Estado: P0, P1, P1.1, P2, P3, P4.1 y P4.2 CERRADOS EN LINUX; P4.3-H EN REVALIDACIÓN; P4.0 COMPLETADO.** Fecha: 22 de noviembre de 2026. Linux x86_64 es la plataforma de referencia hasta P6; Windows y macOS se validan funcionalmente en P7, sin declarar compatibilidad antes. P4.4 sigue pendiente; resultados Gentle Shell en `docs/P4_3_RESULTS.md`; resultados Pi en `docs/P4_2_RESULTS.md`; los resultados de Codex están en `docs/P4_1_RESULTS.md`; las 18 decisiones funcionales de P4 están en `docs/P4_SPEC.md` y la evidencia técnica en `docs/P4_PROVIDER_MATRIX.md`, `docs/P4_RPC_RESEARCH.md`, `docs/P4_CONTRACT_DESIGN.md` y `docs/P4_TEST_PLAN.md`.

Este es un **inicio limpio**: no se portan ni heredan los módulos de los repositorios anteriores. El historial de GitHub antiguo puede quedar archivado, pero sus decisiones y su código no son la base de esta versión. No se promete compatibilidad de bases SQLite ni del wire anterior; la compatibilidad comienza en el primer contrato publicado de v2.

## Producto

Gentle Fleet es un gestor multiplataforma, independiente de la interfaz, para sesiones conversacionales de agentes de programación. Una persona puede iniciar varios agentes, enviar prompts, ver mensajes y herramientas, aprobar solicitudes, cerrar una interfaz sin terminar el trabajo y reconectar desde otro cliente. El mismo modelo de sesión sirve localmente y en máquinas remotas.

### Prioridades

1. **Linux y Windows** como plataformas objetivo desde el primer diseño; desarrollo inicial sobre Linux. macOS después de las primeras pruebas de portabilidad.
2. **Gentle Shell (Pi), Codex y OpenCode** como integraciones previstas. Se implementa un único driver vertical primero y después los otros, sin modificar la lógica de sesiones.
3. **CLI y SDK primero**, cliente GPUI después. Todos hablan una única API Fleet; ni Desktop ni CLI gestionan procesos de agentes directamente.
4. **Daemon por usuario y por host**. Procesos con el mismo UID/SID forman parte de un entorno personal de confianza; no se promete aislamiento entre ellos.
5. **Persistencia y reconexión reales**, distinguiendo desconexión de cliente, caída de daemon, muerte del runner, reinicio de host y capacidad nativa de reanudar un proveedor.
6. **Core independiente de Herdr**. Herdr puede administrar las terminales del entorno de desarrollo, no el runtime de Fleet.

## Entrega primera (MVP verificable)

Abrir un proyecto local → iniciar un agente → escribir un prompt → ver texto y herramientas en orden → recibir/contestar un permiso → cerrar CLI → reabrir CLI y recuperar timeline → detener sesión. Debe funcionar con **dos clientes a la vez** y sobrevivir a la caída de la interfaz. No se anuncia supervivencia a daemon/host crash hasta que sus pruebas pasen.

## Qué NO se implementa en el primer corte

Orquestación autónoma multiagente, worktrees automáticos, CI/CD, navegador, soporte móvil, sincronización entre hosts, sistema de plugins de terceros, cuentas centralizadas, servidor público TCP, aislamiento contra procesos del mismo usuario y compatibilidad con el antiguo Protocol v1.

## Documentos normativos

- [Producto y alcance](docs/PRODUCT.md)
- [Arquitectura y límites](docs/ARCHITECTURE.md)
- [Contratos y flujo de eventos](docs/API.md)
- [Sesiones, operaciones y recuperación](docs/LIFECYCLE.md)
- [Drivers para agentes](docs/DRIVERS.md)
- [Persistencia y consistencia](docs/PERSISTENCE.md)
- [Seguridad y plataformas](docs/SECURITY_AND_PLATFORMS.md)
- [Pruebas y aceptación](docs/QUALITY.md)
- [Ruta de implementación](docs/ROADMAP.md)
- [Decisiones y alternativas](docs/DECISIONS.md)
- [Referencias investigadas](docs/REFERENCES.md)
- [Ejemplo conceptual de flujo](examples/chat-flow.jsonl)

### Investigación P0 (evidencia de motores reales)

- [Resumen ejecutivo y cierre](docs/P0_FINDINGS.md)
- [Gentle Shell / Pi](docs/research/PI.md)
- [Codex](docs/research/CODEX.md)
- [OpenCode](docs/research/OPENCODE.md)
- [Comparación y contratos de `AgentDriver`](docs/research/PROVIDER_COMPARISON.md)
- [Limitaciones conocidas](docs/research/KNOWN_LIMITATIONS.md)
- [Programas y fixtures de los spikes](spikes/p0/README.md)

### Implementación P1

- [Resultados reales de P1](docs/P1_RESULTS.md)
- [Estabilización y concurrencia P1.1](docs/P1_1_RESULTS.md)
- [Persistencia y aprobaciones P2](docs/P2_RESULTS.md)
- [Runner persistente y recuperación P3](docs/P3_RESULTS.md)
- [Código de los crates](crates/)
- [Pruebas de proceso](tests/)

P0-D1…D6 están **ratificadas** en [`docs/DECISIONS.md`](docs/DECISIONS.md). P1 usa OpenCode como primer driver; Codex y Gentle Shell/Pi están previstos para P4.

## Política de código heredado

Los repositorios y ramas previos **no son dependencias**. No copiar clases, módulos, protocolos, migraciones o patrones de recuperación sin una decisión explícita respaldada por un test vertical. El desarrollo nuevo empieza sobre un repositorio o rama de árbol limpio. **Nunca eliminar historiales antiguos por fuerza ni mezclar automáticamente las ramas anteriores**.

## Primera orden de trabajo

Seguir [PLAN.md](PLAN.md). Antes de implementar Core real hay tres experimentos cortos de interoperabilidad que deben producir fixtures verificables. Consultar [AGENTS.md](AGENTS.md) para las reglas del agente de desarrollo.
