# Roadmap — Gentle Fleet Headless Agent Session Engine

**Alcance vigente (25/11/2026):** motor headless, daemon/runner, CLI, SDK y protocolo para clientes externos. No se desarrolla una interfaz gráfica propia. P6.2 gráfica queda como referencia histórica; P6 headless recupera sus mejoras de backend sin incorporar código gráfico. P6 se integra a main solo tras aceptar la validación del nuevo candidato, sin esperar P7. P7-H pasa a ser el primer corte de reorganización de P7 después de integrar P6: desacoplar el escritorio sin reducir los gates IPC, persistencia y recuperación. Hasta validar ese corte se conservan los perfiles de P7 existentes.

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

**Estado P4.2 (22/11/2026): CERRADA EN LINUX.** Pi estándar 1.6.0 integrado como tercer proveedor con `provider_id=pi`; ver [`P4_2_RESULTS.md`](P4_2_RESULTS.md). Chat, herramientas, controles e interrupción verificados con modelo real; descubrimiento/historial y reattach deterministas en verde. Aprobaciones nativas UNSUPPORTED en esta instalación. P4.3 y P4.4 estaban pendientes entonces; ver cierres posteriores.

## P5: local/remoto idénticos

**Salida**: bridge SSH autenticado contra fleetd remoto con los mismos envelopes; CLI puede listar workspaces/sesiones, iniciar un agente en cwd remoto, escuchar eventos, aprobar y reconectar tras cortar SSH. No compartir bases entre hosts, no abrir listener TCP público.

## P6: SDK reutilizable y cierre headless

**Estado P6.1 (22/11/2026): CERRADA EN LINUX** en `feature/p6-sdk-gpui`. `fleetctl` consume `fleet-client`; el SDK comparte el wire y `fleet-ssh`, con observación cancelable, replay/reconexión y cursor durable por host/sesión. La suite unitaria y E2E determinista, chat FakeDriver local/SSH y smoke real opt-in OpenCode pasaron; ver [`P6_1_RESULTS.md`](P6_1_RESULTS.md). La evidencia P6.1-H es histórica; el nuevo candidato debe revalidarse.

**P6-H (25/11/2026): CANDIDATA, UNTESTED.** Se rescatan catálogo de hosts, tipos públicos, clasificación SSH y manejo acotado de diagnósticos; no se incluye GPUI ni P7 experimental. Gate: [plan P6](P6_TEST_PLAN.md). La interfaz de usuario es responsabilidad del consumidor externo.

## P7: robustez operacional y release

**Salida**: límites configurables y métricas de runners/conexiones/eventos/spool; backups y migraciones; pruebas de consumo bajo carga; **validación funcional completa de Windows (daemon, runners, IPC, procesos) y macOS, más ampliación de los smokes de cliente Windows de P5** (incluyendo ACL/permisos de transporte y toolchain completo); installers/upgrades; logs seguros; documentación de recuperación y compatibilidad; versión de wire publicada. No convertir P7 en una auditoría infinita: definir presupuesto de riesgos y criterios de release medibles.

## Regla para ampliar el alcance

Nuevas capacidades como git/worktree, filesystem remoto, delegación multiagente, notificaciones, plugins, MCP y automatizaciones entran solo después de P6 y mediante ADR más prueba vertical. La arquitectura debe admitirlas mediante interfaces claras; no se implementan anticipadamente.

## Etapas de migración desde el desarrollo anterior

No existe migración automática. No se hacen cherry-picks de M0–M4, no se arrastra su schema ni se repite su fichero `PLAN.md`. Si una pieza vieja demuestra ser útil, se reescribe con su contrato y nueva prueba, con revisión explícita. Mantener la historia anterior solo como referencia técnica accesible. No publicar Core v2 hasta que P1 tenga chat real.


**Estado P4.3-H (22/11/2026): CERRADA EN LINUX con limitaciones explícitas.** El endurecimiento corrigió controles nativos efectivos, ACK inciertos, backpressure Desktop RPC, resets conservadores y aislamiento de lectura de historial. El CI [`44953487212`](https://github.com/javijec/gentle-fleet/actions/runs/44953487212) sobre `5ba188a` y los siete scripts Gentle Shell (lifecycle, recovery, concurrency, chat, tools, interrupt y controls) finalizaron PASS según los resultados compartidos por el usuario. Importación histórica no verificada deshabilitada, modo de permisos de solo lectura, permisos interactivos reales y perfiles UNTESTED. Ver [`P4_3_RESULTS.md`](P4_3_RESULTS.md); P4.4 estaba pendiente entonces.


**Estado P4.4-H (22/11/2026): CERRADA EN LINUX sobre `4c6b0e3`.** Tras la auditoría del framework común se corrigieron falsos PASS en concurrencia y terminalidad, se exigió workspace explícito, dos clientes autenticados distintos, una única ejecución de matriz y timeouts con paginación. Los nuevos E2E deterministas se incorporan al CI en modalidad FakeDriver explícita; la matriz real de 56 celdas, las 12 direcciones de aislamiento y los smokes de los cuatro proveedores se repitieron sobre `dd9a2f4` y la revalidación posterior al hotfix fue reportada con 56 comprobaciones, 12/12 direcciones y CI `44992082537` PASS. Ver [`P4_4_RESULTS.md`](P4_4_RESULTS.md). P5 estaba pendiente entonces.


### Cierre histórico P5: local/remoto idénticos

**Estado (22/11/2026): CERRADA — servidor Linux x86_64, cliente CLI Linux y Windows 11.** Bridge SSH stdio, mismo wire Fleet, `fleetctl --host`, hosts configurables y replay durable pasaron CI ([#45076748228](https://github.com/javijec/gentle-fleet/actions/runs/45076748228)) y recorrido manual real Windows → Linux (FakeDriver, OpenCode, Codex, Pi, Gentle Shell; streaming y recuperación por cursor). Criterio «dos Linux físicos» sustituido explícitamente por **dos equipos físicos Windows → Linux**: prueba más relevante para transporte cliente multiplataforma; dos Linux físicos siguen UNTESTED. Quedan fuera del cierre daemon/runner Windows, macOS, aprobación interactiva remota en Windows y reconexión automática, documentados en [`P5_RESULTS.md`](P5_RESULTS.md) §6–7. El cierre P5 ya está integrado en main.

## Orden vigente tras P6-H

- P7-H: separar el workspace de P7 de GPUI, lockfile, CI y dependencias gráficas; preservar evidencia histórica y tests del motor. No eximir gates de seguridad.
- P7-W1: validación técnica Windows IPC (incluido W1-B), persistencia, procesos y recuperación; W2: proveedores reales Windows.
- P7-W4: SSH bidireccional sin relajar host key checking; declarar combinaciones sin probar.
- P7-O/S: operación, distribución y SDK/wire versionado con consumidor headless externo.
- P7-M: validación nativa macOS. Una entrega Linux limitada no certifica Windows/macOS.
- PUBLIC-READINESS: auditoría independiente antes de abrir el repositorio; no implica aceptación técnica ni release.

La rama P7 conserva su evidencia y gates específicos de plataforma; este documento no adelanta sus resultados ni autoriza su merge.
