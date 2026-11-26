# Roadmap — Gentle Fleet Headless Agent Session Engine

**Actualización W2-A (26/11/2026):** infraestructura candidata validada por
gate automatizado `0203b5c`; aceptación técnica pendiente de auditoría y
regresiones de descendientes y handles. **NO-GO W2-B** hasta resolver los
P0/P1 del alcance y cumplir el gate. Ver [resultados](P7_W2_RESULTS.md) y
[matriz](P7_W2_PROVIDER_MATRIX.md).

**Decisión de alcance (25/11/2026):** Gentle Fleet es un motor de sesiones persistentes y remotas para agentes de programación, sin interfaz gráfica propia como objetivo de producto. Los entregables son daemon, runner, CLI, SDK Rust y protocolo público. Gentle Shell GPUI y otros clientes son integraciones externas opcionales; no gestionan directamente los procesos internos de Fleet.

El motor y sus clientes headless son el único alcance activo. La evidencia histórica y las restricciones de calidad se conservan. No se declara ningún PASS nuevo por este cambio documental. P7-H integra main y retira GPUI; su aceptación requiere validación nativa del candidato. Los criterios de seguridad, IPC y recuperación no se reducen.

## Camino crítico del motor

| Etapa | Entregable | Estado / dependencia |
| --- | --- | --- |
| P0–P5 | Sesiones, persistencia, runner, proveedores y SSH | Cerrados para el alcance documentado Linux / cliente Windows SSH |
| P6-H | SDK Rust reutilizable sin GUI | Cerrado en Linux; integrado en main `7700b6a` |
| P7-W1 | Motor Windows: IPC, persistencia, procesos y recuperación | A–F implementados; gate W1-F PASS reportado; revisión formal pendiente |
| P7-W2 | Conformidad con proveedores nativos Windows | W2-A candidato abierto; W2-B–G pendientes |
| P7-H | Desacople del workspace y validación headless | Implementado; primer gate pendiente antes de continuar W1/W2 |
| P7-W4 | SSH bidireccional y configuración portable | Después de W1; validación final con motor headless |
| P7-O | Operación, límites, backup, diagnóstico y distribución | Pendiente |
| P7-S | SDK y wire versionados; prueba de consumidor externo sin GPUI | Pendiente |
| P7-M | Validación funcional macOS | Pendiente; puede avanzar en paralelo con un entorno nativo |
| Publicación | Nuevo repositorio público desde snapshot auditado | Futuro; el repositorio de desarrollo permanece privado |

P7-H está implementada, pendiente de aceptación; P7-O/S/M son etapas futuras. La primera entrega del motor puede ser una versión preliminar de alcance Linux, con cliente SSH Windows probado; no se podrá anunciar soporte nativo Windows/macOS ni cerrar P7 integral hasta cumplir sus respectivos gates. El release Windows conserva la prueba efectiva de aislamiento entre SID; Windows→Windows físico sigue pendiente hasta disponer de entorno autorizado.

## Alcance histórico y evidencia

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

**Estado P4.2 (22/11/2026): CERRADA EN LINUX.** Pi estándar 1.6.0 integrado como tercer proveedor con `provider_id=pi`; ver [`P4_2_RESULTS.md`](P4_2_RESULTS.md). Chat, herramientas, controles e interrupción verificados con modelo real; descubrimiento/historial y reattach deterministas en verde. Aprobaciones nativas UNSUPPORTED en esta instalación. P4.3 y P4.4 estaban pendientes en ese corte; ver sus cierres históricos más abajo.

## P5: local/remoto idénticos

**Salida**: bridge SSH autenticado contra fleetd remoto con los mismos envelopes; CLI puede listar workspaces/sesiones, iniciar un agente en cwd remoto, escuchar eventos, aprobar y reconectar tras cortar SSH. No compartir bases entre hosts, no abrir listener TCP público.

## P6 — SDK Rust headless

**CERRADA EN LINUX e integrada en main** mediante PR #2 (`7700b6a`). El operador confirmó el gate determinista de P6-H y el smoke real OpenCode tras la corrección de formato `b366f96`. Evidencia y límites en [P6_1_RESULTS.md](P6_1_RESULTS.md). El SDK ofrece catálogo de hosts, tipos públicos, operaciones, timeline, reconexión, cursores y cancelación; CLI y clientes externos comparten el contrato. La evidencia del experimento gráfico, retirado del producto, permanece en el commit histórico [427a5c1](https://github.com/javijec/gentle-fleet/tree/427a5c1c26377805755930eadaa95e7e68166d63), incluido su código y documentación. No requiere conservar ninguna rama P6.

## P7: robustez operacional y release

**Salida**: límites configurables y métricas de runners/conexiones/eventos/spool; backups y migraciones; pruebas de consumo bajo carga; **validación funcional completa de Windows (daemon, runners, IPC, procesos) y macOS, más ampliación de los smokes de cliente Windows de P5** (incluyendo ACL/permisos de transporte y toolchain completo); installers/upgrades; logs seguros; documentación de recuperación y compatibilidad; versión de wire publicada. No convertir P7 en una auditoría infinita: definir presupuesto de riesgos y criterios de release medibles.

**P7-W0: CERRADO como auditoría.** El [CI 45234810431](https://github.com/javijec/gentle-fleet/actions/runs/45234810431) sobre `64dfac0` pasó en Windows, backend Linux y Desktop Linux. Ver [auditoría y plan W1–W4](P7_W0_AUDIT.md), [resultados](P7_W0_RESULTS.md) y [matriz](P7_WINDOWS_MATRIX.md). El fallo de token del runner, IPC/ACL, persistencia y supervivencia de procesos son bloqueadores de W1; la compilación no equivale a compatibilidad funcional ni a seguridad del transporte. En ese cierre histórico todavía no se habían iniciado W1–W4; W1 avanzó posteriormente. La fase de interfaz gráfica queda fuera del producto.

**P7-W1: ABIERTO.** W1-A pasó sus regresiones Windows, incluido rechazo cruzado entre runners, y el [CI 45238966908](https://github.com/javijec/gentle-fleet/actions/runs/45238966908) Windows y Linux. IPC seguro, DACL, persistencia y recuperación siguen pendientes. Ver [diseño](P7_W1_DESIGN.md), [plan](P7_W1_TEST_PLAN.md) y [resultados](P7_W1_RESULTS.md).

## Regla para ampliar el alcance

Nuevas capacidades como git/worktree, filesystem remoto, delegación multiagente, notificaciones, plugins, MCP y automatizaciones entran solo después de P6 y mediante ADR más prueba vertical. La arquitectura debe admitirlas mediante interfaces claras; no se implementan anticipadamente.

## Etapas de migración desde el desarrollo anterior

No existe migración automática. No se hacen cherry-picks de M0–M4, no se arrastra su schema ni se repite su fichero `PLAN.md`. Si una pieza vieja demuestra ser útil, se reescribe con su contrato y nueva prueba, con revisión explícita. Mantener la historia anterior solo como referencia técnica accesible. No publicar Core v2 hasta que P1 tenga chat real.


**Estado P4.3-H (22/11/2026): CERRADA EN LINUX con limitaciones explícitas.** El endurecimiento corrigió controles nativos efectivos, ACK inciertos, backpressure Desktop RPC, resets conservadores y aislamiento de lectura de historial. El CI [`44953487212`](https://github.com/javijec/gentle-fleet/actions/runs/44953487212) sobre `5ba188a` y los siete scripts Gentle Shell (lifecycle, recovery, concurrency, chat, tools, interrupt y controls) finalizaron PASS según los resultados compartidos por el usuario. Importación histórica no verificada deshabilitada, modo de permisos de solo lectura, permisos interactivos reales y perfiles UNTESTED. Ver [`P4_3_RESULTS.md`](P4_3_RESULTS.md); P4.4 estaba pendiente en ese corte; ver cierre posterior.


**Estado P4.4-H (22/11/2026): CERRADA EN LINUX sobre `4c6b0e3`.** Tras la auditoría del framework común se corrigieron falsos PASS en concurrencia y terminalidad, se exigió workspace explícito, dos clientes autenticados distintos, una única ejecución de matriz y timeouts con paginación. Los nuevos E2E deterministas se incorporan al CI en modalidad FakeDriver explícita; la matriz real de 56 celdas, las 12 direcciones de aislamiento y los smokes de los cuatro proveedores se repitieron sobre `dd9a2f4` y la revalidación posterior al hotfix fue reportada con 56 comprobaciones, 12/12 direcciones y CI `44992082537` PASS. Ver [`P4_4_RESULTS.md`](P4_4_RESULTS.md). P5 estaba pendiente en ese corte; ver cierre posterior.


### Cierre histórico P5: local/remoto idénticos

**Estado (22/11/2026): CERRADA — servidor Linux x86_64, cliente CLI Linux y Windows 11.** Bridge SSH stdio, mismo wire Fleet, `fleetctl --host`, hosts configurables y replay durable pasaron CI ([#45076748228](https://github.com/javijec/gentle-fleet/actions/runs/45076748228)) y recorrido manual real Windows → Linux (FakeDriver, OpenCode, Codex, Pi, Gentle Shell; streaming y recuperación por cursor). Criterio «dos Linux físicos» sustituido explícitamente por **dos equipos físicos Windows → Linux**: prueba más relevante para transporte cliente multiplataforma; dos Linux físicos siguen UNTESTED. Quedan fuera del cierre daemon/runner Windows, macOS, aprobación interactiva remota en Windows y reconexión automática, documentados en [`P5_RESULTS.md`](P5_RESULTS.md) §6–7. No se fusiona en main.


**CI local (P7):** se incorporó una alternativa reproducible al gate automatizado de GitHub Actions mientras el servicio esté bloqueado por facturación. Ver [CI local](LOCAL_CI.md) y [política de calidad](QUALITY.md). Los scripts se ejecutan de forma nativa en Windows y Linux sobre el mismo SHA limpio; no sustituyen la evidencia manual de seguridad. P7-W1-B sigue abierta hasta validación efectiva. No se modifican los workflows remotos.

### Traspaso W1-B → W1-C (24/11/2026)

**W1-B: cierre administrativo con alcance limitado; aceptación técnica
pendiente.** Por decisión del propietario, el aislamiento real entre dos
cuentas Windows queda UNTESTED y diferido al release P7, sin crear cuentas
ni eliminar el fixture. No hay full confirmado del candidato `69f7729`.
**W1-C: autorizado para preparación; implementación tras gate de entrada**
(full Windows/Linux, mismo SHA limpio, consolidación PASS, regresiones IPC
y ningún defecto P0/P1 confirmado pendiente). Alcance W1-C: rutas durables
por usuario, ACL de datos, DB/WAL/spool, acceso concurrente y recuperación
de persistencia tras terminación. No iniciar W1-D–F ni W2; no merge ni
release. Ver [resultados W1](P7_W1_RESULTS.md) y [calidad](QUALITY.md).
Esta actualización prevalece sobre el estado histórico W1-B anterior.

## Plan vigente de P7 sin interfaz gráfica

### P7-W1 — Windows nativo

- W1-B: validar candidato y HEAD limpio en Windows/Linux, full consolidado, protocolo Windows 10/10, regresiones W1-A/B de identidad, ACL, runtime, cancelación y backpressure. Ningún defecto P0/P1 confirmado pendiente.
- W1-C: rutas durables por usuario, protección de DB/WAL/spool y estado, concurrencia y recuperación ante terminación. No recurrir a temporales para persistencia.
- W1-D: ciclo daemon → runner → FakeDriver → cliente, resolución de ejecutables, handles, interrupción y cierre explícito.
- W1-E: supervivencia del runner tras caída del daemon, reattach autenticado, reconciliación de pérdida del runner, spool/ACK y replay sin duplicados.
- W1-F: aceptación de proceso nativa, concurrencia, aprobaciones, backpressure y recuperación con evidencia Windows/Linux. W1-D/E/F son la descomposición de trabajo propuesta; cada corte deberá tener plan y prueba vertical antes de implementarse.

No comenzar W1-C hasta su gate de entrada. No adelantar W1-D–F ni W2. El diferimiento de prueba entre dos SID hasta release permanece vigente; no se crean cuentas ni se cuentan resultados ausentes como PASS.

### P7-W2 — Proveedores nativos

Validar descubrimiento, versión, lanzamiento, chat, herramientas, streaming, interrupción, controles, permisos donde estén soportados y recuperación de Gentle Shell, Pi, Codex y OpenCode en Windows. Negociar capacidades; no simular paridad. Para Gentle Shell, permisos interactivos reales, perfiles e importación segura de historial siguen pendientes según P4.3. No bloquear un release limitado por una función no soportada si se excluye explícitamente y los criterios de seguridad se cumplen; no anunciarla como disponible.

### P7-H — Integración y validación headless

Implementación candidata: integrar main `7700b6a` y mantener un workspace exclusivo del motor; conservar todo el historial en P7.

Criterios: un clon limpio del motor compila, prueba y distribuye `fleetd`, `fleet-runner`, `fleetctl`, SDK y protocolo sin instalar GPUI ni dependencias gráficas. Actualizar workspace, CI local/remoto, documentación de calidad y perfiles de release mediante un corte independiente; conservar todos los tests de seguridad, backend, IPC, recuperación, proveedores y SSH. Reemplazar solo la evidencia visual que deja de pertenecer al producto por un consumidor externo headless verificable (CLI/SDK). El nuevo perfil full incluye control del grafo headless y todos los checks del motor anteriores; el perfil release sustituye exclusivamente `desktop-smoke` por `external-sdk-smoke` (consumidor externo CLI/SDK con evidencia real). No se permite omitir otras pruebas. Antes de W1-C, full Windows/Linux mismo SHA, protocolo Windows 10/10 y W1-A/B siguen obligatorios. No hay PASS de P7-H hasta ejecutar el candidato.

### P7-W4 — SSH portable

Verificar configuración por usuario, claves/known_hosts, rutas y quoting seguro en Windows; bridge autenticado sin abrir listener TCP público. Repetir Windows→Linux y validar Linux→Windows con equipos reales. Windows→Windows y demás combinaciones se documentan como UNTESTED mientras no exista entorno autorizado. Pruebas de reconexión, streaming, permisos soportados, EOF y recuperación por cursor. No relajar StrictHostKeyChecking.

### P7-O — Operación y distribución del motor

Servicio de usuario e inicio/parada/estado reproducibles; rutas y logs seguros; límites configurables de runners, conexiones y spool; retención y métricas; pruebas de carga; backup/restauración y migraciones con evidencia; actualización y rollback documentados. Binarios y paquetes por plataforma con verificación de integridad, instalación y desinstalación desde entorno limpio. Presupuesto de riesgos y criterios medibles, sin ampliar indefinidamente el alcance.

### P7-S — Contrato público y SDK

Documentar versiones de wire/SDK, negociación, compatibilidad, errores, operaciones idempotentes y capacidades por proveedor. Ejemplos de un consumidor Rust independiente para crear/observar/reconectar/interrumpir sesiones locales y SSH; demostrar que no importa daemon, base ni GPUI. Definir distribución reproducible de SDK y dependencias accesibles para clientes públicos. Probar compatibilidad entre versiones declaradas, no prometer estabilidad antes de fijar contrato.

### P7-M — macOS y matriz de release

Validación nativa de daemon, runner, IPC, persistencia, permisos, SDK, SSH y proveedores en macOS. Ninguna inferencia de soporte por compartir cfg(unix). Publicar matriz de plataformas y proveedores con PASS/FAIL/BLOCKED/UNTESTED y capacidades reales.

**Entrega progresiva propuesta:** versión preliminar Linux con CLI/SDK y cliente Windows→Linux solo después de sus propios gates de operación, seguridad, contrato, instalación y auditoría de publicación. Release nativo Windows tras W1/W2/W4 y evidencia de seguridad requerida (incluida prueba entre SID). macOS solo tras evidencia nativa. No confundir un release limitado con el cierre integral de P7. No hay release ni merge autorizado por este documento.

## Integraciones externas y no objetivos

Gentle Shell GPUI puede consumir Fleet como backend alternativo al RPC directo, sin modificar el motor para depender de su interfaz. La integración gráfica es responsabilidad del cliente y no bloquea un release headless. Perfiles, subagentes, jobs o eventos específicos se exponen solo mediante capacidad y contrato probados. Filesystem remoto, Git/worktrees, PTY remoto y demás funciones de IDE son extensiones fuera del MVP; requieren ADR y prueba vertical. No implementar una segunda aplicación de escritorio, ni importar runtimes gráficos en los crates del motor.

### Actualización de integración P7-H (25/11/2026)

P6-H fue integrada en main mediante PR #2 (`7700b6a`), con gate Linux y smoke OpenCode PASS reportados por el operador, no por GitHub Actions. P7 incorpora ambos padres de historial en `92d0de5`; mantiene las correcciones Windows de token/IPC y las mejoras SDK/SSH de P6-H. La aplicación gráfica queda preservada por el historial de P7, no por una rama activa. El lockfile parte del lock validado de main y añade la dependencia Windows ya resuelta `windows-sys 0.61.2` de fleet-protocol; la validez del grafo debe confirmarse mediante cargo metadata/check --locked en ambas plataformas.

**Estado: IMPLEMENTADO, UNTESTED.** Ejecutar el gate full Windows/Linux del mismo SHA limpio, revisar logs, repetir protocolo Windows 10/10 y regresiones W1-A/B antes de iniciar W1-C. No declarar un nuevo PASS por esta reorganización. No se altera el diferimiento del fixture de dos SID hasta release. La futura publicación se hará en un repositorio nuevo a partir de un árbol auditado; no abrir este repositorio privado. El trabajo previo de preparación pública se conserva como referencia en el PR #1 cerrado, sin incorporarlo a P7. No quedan ramas P6 activas.

### Preparación W1-C

Auditoría de persistencia y plan de implementación en [P7_W1_C_PLAN.md](P7_W1_C_PLAN.md). Gate de entrada automatizado mediante `scripts/ci/w1c_entry.py`, que ejecuta full conjunto y protocolo Windows 10/10 sin sustituir revisión de logs ni los criterios W1-B. Estado: preparación; no se autoriza implementar hasta aceptar el gate vigente.

### Revalidación automatizada de entrada W1-C (25/11/2026)

PASS reportado del gate full conjunto y protocolo Windows 10/10 en `c1b3d77`, tras la corrección del resumen. El detalle anterior de 100/100 está registrado en resultados W1. P7-H y W1-B tienen evidencia automatizada favorable; la revisión de logs para la aceptación técnica limitada permanece pendiente de evidencia. Se prepara la primera regresión negra W1-C de persistencia y ACL; no se ha implementado ni aceptado la protección de datos. No se rebajan los criterios ni se crean cuentas adicionales.

### Estado W1-C (25/11/2026)

Subcortes C.1 SQLite, C.2 spool, C.3 journals y C.4 catálogo SSH: gates automatizados PASS reportados por el operador. C.5 candidato de recuperación integral: unificación CLI/SDK y regresión de terminación SQLite, pendiente de validación. Ver P7_W1_C_PLAN.md para pendientes y límites; no se acepta W1-C completa ni se inicia W1-D.

W1-C.5: PASS automatizado reportado tras 2b4e80f. Nuevo candidato de exclusividad durable Windows pendiente de validación. Los cinco subcortes tienen PASS reportado, pero los pendientes de aceptación enumerados en P7_W1_C_PLAN.md mantienen W1-C abierta.

W1-C: gate completo PASS reportado tras 3d2bb26; candidato adicional de identidad física de DB Windows pendiente de validación. No se modifica main ni se inicia W1-D.

W1-C: W1-B 10/10 y full conjunto PASS reportados tras 2d38d1a. Guía de migración conservadora y regresiones adicionales de adopción explícita en candidato; revisar logs del gate y alcance de aceptación antes de cierre. No se inicia W1-D.

W1-C: gate automatizado final 103/103 PASS (Windows 26, Linux 77) sobre `846a4b227d8f005125678bab7851c00b1c945da8`, resumen conjunto proporcionado y SHA contrastado. Implementación candidata completada, pendiente únicamente decisión de aceptación técnica tras revisión de logs y riesgos, sin nuevos subcortes por defecto. W1-D a continuación de esa revisión; sin merge a main ni release. Ver P7_W1_RESULTS.md y P7_W1_C_PLAN.md.

W1-C: revalidación automatizada final tras cuatro correcciones prioritarias y primera vertical W1-D: **104/104 PASS** sobre `c4f2abcb9438a88acc6bc42501049ef639a48133` (27 Windows, 77 Linux). Solo resta revisión técnica de evidencia y riesgos para cerrar W1-C con alcance limitado; no repetir gate sin motivo concreto. W1-D preparada, primera vertical positiva PASS automatizado, aceptación integral pendiente. Main y criterios release intactos. Ver P7_W1_RESULTS.md y P7_W1_D_PLAN.md.

Tras revisión de la evidencia de `c4f2abc`, **W1-C cerrada técnicamente para desarrollo con alcance limitado; W1-D abierta**. Los requisitos manuales y reales de release permanecen sin cambios. La aceptación integral de W1-D requiere sus regresiones negativas, ciclo de vida y validación nativa; no adelantar W1-E/F ni W2. Ver resultados y plan W1-D.


### W1-D — candidato consolidado (26/11/2026)

W1-D permanece ABIERTA: prioridad de Shutdown, diagnóstico por fase y regresiones nativas negativas implementadas como candidato; falta validación dirigida y gate full del SHA limpio. No reabrir W1-C sin regresión confirmada. Próximo paso tras aceptar W1-D: W1-E. Ver P7_W1_D_PLAN.md y P7_W1_RESULTS.md. No se modifica main ni se declaran PASS nuevos.


### W1-E — candidato de recuperación (26/11/2026)

Por instrucción del propietario se prepara la implementación W1-E sobre W1-D con PASS reportado. La aceptación técnica de D sigue pendiente de evidencias; no se rebaja su gate. Primer candidato: recuperación autenticada, handle de espera Windows, orden durable de recovery, aprobación incierta y tres escenarios de proceso. Ver [plan W1-E](P7_W1_E_PLAN.md). UNTESTED; no W1-F/W2, main ni release.


### W1-E — PASS reportado (26/11/2026)

Pruebas dirigidas y gate conjunto modo w1c reportados PASS por el operador tras corregir el comparador de replay; HEAD remoto observado `a36e9a3`. W1-F a preparar como consolidación de evidencia y aceptación integral, sin dar por cerrados D/E antes de revisar sus criterios. Ver P7_W1_RESULTS.md. Sin cambios de producción ni release.


### W1-F — candidato de aceptación integral (26/11/2026)

Se incorpora una prueba negra de proceso Windows para concurrencia de turnos, aislamiento, decisión única de aprobación, backpressure tipado, replay exacto y parada durable. Se integra en full Windows y workflow. El coordinador incorpora modo w1f con full Windows/Linux, protocolo 10/10 y Lifecycle 20/20 sobre mismo SHA limpio. Ver [plan W1-F](P7_W1_F_PLAN.md). Candidato UNTESTED; no se declaran cerrados D/E/F hasta evidencia revisada. No main, W2 ni release.


### W1-F — implementación completa; gate PASS reportado (26/11/2026)

Operador confirma PASS integral modo w1f tras corrección SHM; HEAD remoto observado `8662aea`. W1-A–F implementación completa, pendiente revisión de evidencia original para aceptación técnica limitada. No repetir gate sin motivo concreto. Siguiente corte W2 (proveedores Windows nativos), tras revisión. Ver P7_W1_RESULTS.md. No release ni merge a main.


### Inicio W2 (26/11/2026)

Por instrucción del propietario se inicia W2 sobre W1-F con PASS integral reportado, sin declarar aceptación formal de W1 antes de revisión de evidencia. Primer corte W2-A: resolución nativa de Codex y smoke real chat/streaming/replay/cierre opt-in. Ver [plan W2](P7_W2_PLAN.md). Los restantes checks reales y proveedores siguen pendientes. Sin cambios de rango de compatibilidad, main, W4 ni release.

W2: nomenclatura vigente A infraestructura, B OpenCode, C Codex, D Pi, E Gentle Shell, F conformidad conjunta, G aceptación. Candidato común UNTESTED; no cerrar W2-A ni W1 formal sin sus evidencias. Ver P7_W2_PLAN.md y P7_W2_RESULTS.md.
