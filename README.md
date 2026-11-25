# Gentle Fleet 2.0 — Headless Agent Session Engine

Motor de sesiones persistentes, locales y remotas para agentes de programación, escrito en Rust. Gestiona procesos, eventos, persistencia y reconexión independientemente de cualquier interfaz gráfica. **Proyecto experimental: no es todavía un release de producción.**

## Qué permite

Crear y observar sesiones de Gentle Shell, Pi, Codex y OpenCode, enviar prompts, recibir eventos y herramientas, resolver permisos cuando el proveedor los soporta y reconectar sin depender del ciclo de vida del cliente. El mismo contrato Fleet funciona localmente y por SSH; la ejecución y los datos permanecen en el host correspondiente.

La meta es que aplicaciones como Gentle Shell GPUI puedan utilizar Fleet como backend opcional de ejecución, no construir otra interfaz gráfica.

## Arquitectura

```text
fleetctl / clientes externos (por ejemplo Gentle Shell GPUI)
                       |
                  fleet-client
                       |
              IPC local / SSH
                       |
                    fleetd
                 /         \
            SQLite      fleet-runner
                            |
                   Driver / agente real
```

| Entregable | Responsabilidad |
| --- | --- |
| `fleetd` | Daemon por usuario y host; sesiones, operaciones y eventos |
| `fleet-runner` | Proceso independiente; ejecución y recuperación según proveedor |
| `fleetctl` | Cliente CLI y administración |
| `fleet-client` | SDK Rust sin dependencia gráfica |
| `fleet-protocol` | Contrato de comunicación versionable |

No compartir SQLite entre hosts. No abrir listeners TCP públicos por defecto. El usuario local es la frontera de confianza: procesos del mismo UID/SID no están aislados entre sí. No autoaprobar solicitudes ni reenviar prompts inciertos.

## Estado actual

| Plataforma | Servidor | Cliente |
| --- | --- | --- |
| Linux x86_64 | Validado en el alcance P0–P5 | CLI validado; SDK P6.1 validado en Linux |
| Windows 11 | Experimental, aceptación técnica P7 pendiente | CLI por SSH hacia Linux validado en P5 |
| macOS | Sin validación funcional completa | Sin validación funcional completa |

P0–P6 constituyen la base headless Linux; el cierre de P6-H se apoya en la validación local reportada por el operador. P7 se desarrolla por separado. Los resultados históricos no acreditan automáticamente HEAD. Ver [roadmap](docs/ROADMAP.md). La supervivencia a cierre del cliente y caída del daemon está validada en Linux dentro del alcance probado; no equivale a garantizar supervivencia al reinicio del host o resume de todos los proveedores.

| Proveedor | Estado Linux |
| --- | --- |
| OpenCode | Driver real, chat, herramientas y aprobaciones verificadas en alcance P1–P4 |
| Codex | Driver real; adopción con escritura bloqueada hasta demostrar exclusividad |
| Pi | Driver real; aprobaciones nativas no soportadas en instalación probada |
| Gentle Shell | Driver real; permisos interactivos y perfiles sin validar, cambio de modo e importación no verificada restringidos |

## Desarrollo sin GUI como objetivo

Este workspace no incluye `apps/desktop` ni dependencias GPUI. P6.2 gráfica queda fuera del producto y conservada en su rama histórica. El motor recupera únicamente mejoras del SDK y SSH de ese trabajo. P7, que sí contiene el cliente experimental, deberá desacoplarlo antes de continuar su camino headless.

Para desarrollar el motor, con Rust estable y las dependencias de plataforma descritas en [plan de pruebas P6](docs/P6_TEST_PLAN.md):

```sh
cargo build -p fleetd -p fleet-runner -p fleetctl --locked
cargo test -p fleet-client --locked
```

Estos comandos son un punto de entrada al backend. El gate completo Linux de P6 es `bash scripts/ci/p6-headless.sh`; requiere Python 3, OpenSSH server y herramientas de los fixtures. El smoke real OpenCode se ejecuta por separado. Consultar [AGENTS.md](AGENTS.md) y [QUALITY.md](docs/QUALITY.md) para los checks obligatorios. Las pruebas con proveedores reales son opt-in; requieren su propia instalación y autenticación. No publicar logs, perfiles o credenciales.

## Camino al primer release

1. P6 headless: cerrado en alcance Linux, con PASS local reportado.
2. P7-H: desacoplar GPUI en la rama P7 y conservar sus gates de seguridad.
3. P7-W1/W2: cerrar IPC, persistencia, procesos, recuperación y proveedores Windows.
4. P7-W4: SSH bidireccional y configuración portable.
5. P7-O/S: operación, distribución y contrato público/SDK.
6. P7-M: macOS con evidencia nativa.

Puede planificarse una entrega preliminar de alcance Linux con cliente SSH Windows, sujeta a sus propios criterios de seguridad, operación, API, instalación y publicación. Windows nativo y macOS se anunciarán únicamente con sus gates aceptados. Los gates de seguridad Windows no se relajan; no se anuncia soporte Windows nativo sin aceptación.

## Documentación

- [Producto y alcance](docs/PRODUCT.md), [arquitectura](docs/ARCHITECTURE.md), [roadmap vigente](docs/ROADMAP.md), [decisiones](docs/DECISIONS.md).
- [API](docs/API.md), [ciclo de vida](docs/LIFECYCLE.md), [drivers](docs/DRIVERS.md), [persistencia](docs/PERSISTENCE.md).
- [Seguridad y plataformas](docs/SECURITY_AND_PLATFORMS.md), [calidad](docs/QUALITY.md), [plan de pruebas P6](docs/P6_TEST_PLAN.md).
- Evidencia: [P0](docs/P0_FINDINGS.md), [P1](docs/P1_RESULTS.md), [P2](docs/P2_RESULTS.md), [P3](docs/P3_RESULTS.md), [P4](docs/P4_4_RESULTS.md), [P5](docs/P5_RESULTS.md), [P6.1](docs/P6_1_RESULTS.md).
- [Ejemplo conceptual de eventos](examples/chat-flow.jsonl), [pruebas de proceso](tests/), [referencias](docs/REFERENCES.md).

## Fuera de alcance

No desarrollar una GPUI propia ni convertir Fleet en IDE completo. Git/worktrees, filesystem y PTY remotos, orquestación autónoma, plugins y automatizaciones requieren una decisión posterior y prueba vertical. Integraciones de clientes externos son opcionales y no bloquean el producto headless. No prometer compatibilidad con el antiguo wire o las bases de datos previas al reset de v2.

No importar ni borrar historiales anteriores por fuerza. Los contratos y resultados ya documentados permanecen como evidencia; la decisión headless en el roadmap prevalece sobre objetivos gráficos históricos, sin alterar criterios de seguridad.
