# Roadmap de implementación desde un árbol vacío

## P0: investigar antes de fijar contratos

**Salida**: tres programas de prueba y fixtures anonimizados, que ejercitan sesiones y permisos reales en Gentle Shell, Codex y OpenCode. Medir capacidades disponibles, IDs nativos, tipos de mensajes, cambios de versión y capacidad de recuperación. Verificar qué proveedores necesitan proceso propio o pueden conectarse a servidor de usuario. Elegir el driver más viable para P1. Ningún servicio persistente ni esquema definitivo todavía.

**Estado (20/11/2026): CERRADO.** Programas en `spikes/p0/`, capturas anonimizadas, resumen en `docs/P0_FINDINGS.md` y detalle por motor en `docs/research/`. Los tres probes pasan con los binarios instalados. P0-D1…D6 ratificadas en `DECISIONS.md`: OpenCode es el primer driver de P1. No se creó ningún servicio persistente ni esquema definitivo.

## P1: primera conversación completa desde CLI

**Salida**: un solo proceso `fleetd`, un Runner con modalidad propia/adjunta, `FakeDriver` y **`OpenCodeDriver` como primer proveedor real**. `fleetctl` registra workspace, crea sesión con `provider_id`, envía texto, observa respuesta y fin de turno. Dos conexiones reciben el mismo timeline en vivo con `seq` asignado por Fleet. Se congela el esquema de eventos P1 tras el smoke real. No agregar persistencia durable, remoto, aprobaciones ni GPUI aquí.

**Estado (20/11/2026): CERRADO.** Ver [`P1_RESULTS.md`](P1_RESULTS.md): fmt, clippy y tests en verde; e2e con FakeDriver y e2e real con OpenCode 2.3.2 pasan con dos clientes y secuencias idénticas.

## P2: persistencia y aprobación

**Salida**: SQLite única con sesiones, turnos, operaciones, eventos y aprobaciones; idempotencia antes de efecto y timeline reconstruible por cursor; aprobaciones de OpenCode con una sola decisión ganadora. El cliente puede cerrarse, reiniciarse y leer eventos con cursor. No prometer supervivencia al daemon crash todavía.

**Estado (20/11/2026): CERRADO EN LINUX.** Ver [`P2_RESULTS.md`](P2_RESULTS.md): WAL y migraciones, `ReadTimeline`, `GetOperation`, aprobaciones (`once`/`reject`) y e2e de persistencia, reinicio, carreras y backpressure en verde. Windows/macOS quedan para validación en P7.

## P3: runner persistente y reconexión de daemon

**Salida**: proceso fleet-runner independiente de fleetd, identidad autenticada, spool acotado de eventos no ACK, retransmisión deduplicada y reattachment. Plataforma de referencia: Linux x86_64. Windows y macOS quedan para validación funcional en P7; no se declara compatibilidad con esas plataformas en P3.

**Estado (22/11/2026): CERRADA EN LINUX.** Ver [`P3_RESULTS.md`](P3_RESULTS.md): supervivencia ante SIGKILL real de `fleetd` y de runner, reattach autenticado, replay durable sin duplicados, aprobaciones consistentes, spool acotado con backpressure, CI verde y smokes reales de OpenCode. Se conservan las comprobaciones de compilación cruzada disponibles. Windows/macOS: pendientes de validación en P7.

## P4: tres motores y capacidad de proveedor

**Salida**: tests de conformidad comunes para **Codex y Gentle Shell/Pi** reutilizando el contrato ya ejercitado por OpenCode; descubrimiento de versión, inicio, send, salida, permisos/capacidades y parada. Reutilizar los contratos Fleet sin agregar ramas de proveedor dentro de SessionManager. El soporte de resume se declara por driver/versión.

**Estado P4.0 (22/11/2026): COMPLETADO — solo investigación y contratos, sin drivers nuevos.** Versiones detectadas: Codex `0.171.0`, Pi estándar `1.6.0`, Gentle Shell `4.8.0-main.652552becbb6` (Pi embebido `1.5.2`), OpenCode `2.3.2` como referencia. Entregables: [`P4_PROVIDER_MATRIX.md`](P4_PROVIDER_MATRIX.md), [`P4_RPC_RESEARCH.md`](P4_RPC_RESEARCH.md), [`P4_CONTRACT_DESIGN.md`](P4_CONTRACT_DESIGN.md) y [`P4_TEST_PLAN.md`](P4_TEST_PLAN.md). Las 18 decisiones de [`P4_SPEC.md`](P4_SPEC.md) permanecen intactas.

**Estado P4.1 (22/11/2026): CERRADA EN LINUX tras P4.1-H.** Driver Codex real integrado con la misma API, SessionManager, runner, aprobaciones y timeline que OpenCode; ver [`P4_1_RESULTS.md`](P4_1_RESULTS.md). Controles por turno, descubrimiento e historial importados con procedencia. La adopción con escritura sigue bloqueada: Codex no demuestra exclusividad frente a carreras. Revalidación final en `docs/P4_1_RESULTS.md` §8: 8 smokes reales PASS sobre `d1eab2a`, V3→V4 verificada y control efectivo comprobado en el thread nativo.

**Estado P4.2 (22/11/2026): CERRADA EN LINUX.** Pi estándar 1.6.0 integrado como tercer proveedor con `provider_id=pi`; ver [`P4_2_RESULTS.md`](P4_2_RESULTS.md). Chat, herramientas, controles e interrupción verificados con modelo real; descubrimiento/historial y reattach deterministas en verde. Aprobaciones nativas UNSUPPORTED en esta instalación. P4.3 y P4.4 pendientes.

## P5: local/remoto idénticos

**Salida**: bridge SSH autenticado contra fleetd remoto con los mismos envelopes; CLI puede listar workspaces/sesiones, iniciar un agente en cwd remoto, escuchar eventos, aprobar y reconectar tras cortar SSH. No compartir bases entre hosts, no abrir listener TCP público.

## P6: SDK reusable + interfaz GPUI

**Estado P6.1 (22/11/2026): CERRADA EN LINUX** en `feature/p6-sdk-gpui`. `fleetctl` consume `fleet-client`; el SDK comparte el wire y `fleet-ssh`, con observación cancelable, replay/reconexión y cursor durable por host/sesión. La suite unitaria y E2E determinista, chat FakeDriver local/SSH y smoke real opt-in OpenCode pasaron; ver [`P6_1_RESULTS.md`](P6_1_RESULTS.md). P6.2 GPUI no iniciado.

**Estado P6.2-A (23/11/2026): CERRADA en Linux.** Desktop GPUI 0.2.2 incluye layout, navegación, temas, estados informativos y gestión de foco/teclado. fmt, check, Clippy, tests/build locked y E2E P5 pasaron localmente; el usuario confirmó PASS para el smoke Omarchy pendiente (panel/cierre) y el CI. El CI se confirmó con [run 45195572977](https://github.com/javijec/gentle-fleet/actions/runs/45195572977) sobre el commit `870c8ed` que registra el cierre A. El parche Linux para `ENOATTR` y la restricción SSH `StrictHostKeyChecking=yes` se mantienen. Windows/macOS no validados; detalle en [`P6_2A_RESULTS.md`](P6_2A_RESULTS.md) y [`P6_2_TEST_PLAN.md`](P6_2_TEST_PLAN.md).

**Estado P6.2-B (23/11/2026): CERRADA EN LINUX.** Desktop demuestra consultas Fleet locales y SSH desde la vista GPUI real contra un `fleetd` aislado y un `sshd` de fixture, incluyendo refresco, desconexión, reintento y supervivencia de runners. Se corrigieron snapshots obsoletos, se acotaron las operaciones con deadline/cancelación, «Volver al inicio» limpia la selección y los elementos usan identidades estables. CI: [run 45208226815](https://github.com/javijec/gentle-fleet/actions/runs/45208226815) sobre `7ec7440`. Limitaciones y resultados PASS/FAIL/UNTESTED en [`P6_2B_RESULTS.md`](P6_2B_RESULTS.md); diseño y plan en [`P6_2B_DESIGN.md`](P6_2B_DESIGN.md) y [`P6_2B_TEST_PLAN.md`](P6_2B_TEST_PLAN.md). P6.2-C no iniciado.

**Salida**: SDK de cliente con gestión de hosts, stream cursors, backoff y deduplicación, consumido por CLI/GPUI. El cliente muestra lista de sesiones, timeline de chat, herramientas, panel de permisos y desconexión/reattach. GPUI no importa el runtime interno. Soporte de terminal/archivos se añade como capacidad adicional, nunca como sustituto del chat.

## P7: robustez operacional y release

**Salida**: límites configurables y métricas de runners/conexiones/eventos/spool; backups y migraciones; pruebas de consumo bajo carga; **validación funcional completa de Windows (daemon, runners, IPC, procesos) y macOS, más ampliación de los smokes de cliente Windows de P5** (incluyendo ALC/permisos de transporte y toolchain completo); installers/upgrades; logs seguros; documentación de recuperación y compatibilidad; versión de wire publicada. No convertir P7 en una auditoría infinita: definir presupuesto de riesgos y criterios de release medibles.

**P7-W0: CERRADO como auditoría.** El [CI 45234810431](https://github.com/javijec/gentle-fleet/actions/runs/45234810431) sobre `64dfac0` pasó en Windows, backend Linux y Desktop Linux. Ver [auditoría y plan W1–W4](P7_W0_AUDIT.md), [resultados](P7_W0_RESULTS.md) y [matriz](P7_WINDOWS_MATRIX.md). El fallo de token del runner, IPC/ACL, persistencia y supervivencia de procesos son bloqueadores de W1; la compilación no equivale a compatibilidad funcional ni a seguridad del transporte. W1–W4 y P6.2-C no se iniciaron.

**P7-W1: ABIERTO.** W1-A pasó sus regresiones Windows, incluido rechazo cruzado entre runners, y el [CI 45238966908](https://github.com/javijec/gentle-fleet/actions/runs/45238966908) Windows y Linux. IPC seguro, DACL, persistencia y recuperación siguen pendientes. Ver [diseño](P7_W1_DESIGN.md), [plan](P7_W1_TEST_PLAN.md) y [resultados](P7_W1_RESULTS.md).

## Regla para ampliar el alcance

Nuevas capacidades como git/worktree, filesystem remoto, delegación multiagente, notificaciones, plugins, MCP y automatizaciones entran solo después de P6 y mediante ADR más prueba vertical. La arquitectura debe admitirlas mediante interfaces claras; no se implementan anticipadamente.

## Etapas de migración desde el desarrollo anterior

No existe migración automática. No se hacen cherry-picks de M0–M4, no se arrastra su schema ni se repite su fichero `PLAN.md`. Si una pieza vieja demuestra ser útil, se reescribe con su contrato y nueva prueba, con revisión explícita. Mantener la historia anterior solo como referencia técnica accesible. No publicar Core v2 hasta que P1 tenga chat real.


**Estado P4.3-H (22/11/2026): CERRADA EN LINUX con limitaciones explícitas.** El endurecimiento corrigió controles nativos efectivos, ACK inciertos, backpressure Desktop RPC, resets conservadores y aislamiento de lectura de historial. El CI [`44953487212`](https://github.com/javijec/gentle-fleet/actions/runs/44953487212) sobre `5ba188a` y los siete scripts Gentle Shell (lifecycle, recovery, concurrency, chat, tools, interrupt y controls) finalizaron PASS según los resultados compartidos por el usuario. Importación histórica no verificada deshabilitada, modo de permisos de solo lectura, permisos interactivos reales y perfiles UNTESTED. Ver [`P4_3_RESULTS.md`](P4_3_RESULTS.md); P4.4 pendiente.


**Estado P4.4-H (22/11/2026): CERRADA EN LINUX sobre `4c6b0e3`.** Tras la auditoría del framework común se corrigieron falsos PASS en concurrencia y terminalidad, se exigió workspace explícito, dos clientes autenticados distintos, una única ejecución de matriz y timeouts con paginación. Los nuevos E2E deterministas se incorporan al CI en modalidad FakeDriver explícita; la matriz real de 56 celdas, las 12 direcciones de aislamiento y los smokes de los cuatro proveedores se repitieron sobre `dd9a2f4` y la revalidación posterior al hotfix fue reportada con 56 comprobaciones, 12/12 direcciones y CI `44992082537` PASS. Ver [`P4_4_RESULTS.md`](P4_4_RESULTS.md). P5 pendiente.


## P5: local/remoto idénticos

**Estado (22/11/2026): CERRADA — servidor Linux x86_64, cliente CLI Linux y Windows 11.** Bridge SSH stdio, mismo wire Fleet, `fleetctl --host`, hosts configurables y replay durable pasaron CI ([#45076748228](https://github.com/javijec/gentle-fleet/actions/runs/45076748228)) y recorrido manual real Windows → Linux (FakeDriver, OpenCode, Codex, Pi, Gentle Shell; streaming y recuperación por cursor). Criterio «dos Linux físicos» sustituido explícitamente por **dos equipos físicos Windows → Linux**: prueba más relevante para transporte cliente multiplataforma; dos Linux físicos siguen UNTESTED. Quedan fuera del cierre daemon/runner Windows, macOS, aprobación interactiva remota en Windows y reconexión automática, documentados en [`P5_RESULTS.md`](P5_RESULTS.md) §6–7. No se fusiona en main.
