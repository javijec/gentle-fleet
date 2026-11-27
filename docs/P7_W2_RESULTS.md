# P7-W2-A — resultados preliminares

## Candidato de contención de procesos Windows (26/11/2026)

UNTESTED: se agrega un Job Object privado por proceso proveedor (sondeos,
Codex RPC, JSONL Pi/Gentle Shell y OpenCode owned). Se crea suspendido, se
asigna por handle de Child y se comprueba el único hilo inicial y su identidad
antes de reanudar. Se rechazan fallos de asignación, incluso restricciones
del job padre, sin fallback inseguro. No se cambia el lanzamiento de runners,
ni se incorpora OpenCode attached al job. Al cerrar se verifica salida del
root y cero procesos activos; kill-on-close es respaldo, no prueba de cierre.
Un root exitoso con descendientes activos produce error tras limpieza.

Fixture Windows: root real con hijo que hereda stdout, root ya salido,
limpieza comprobada por conteo de job y EOF, y un centinela independiente
que sigue vivo. Se comprueba también rechazo del sondeo con descendientes.
Cancelación del lector de sondeo con handle de hilo propio si EOF se demora.
Linux mantiene su política previa de hijo directo. Se reutiliza windows-sys
0.61.2 ya fijado; no hay dependencias externas nuevas en el lock.

Límites: no es sandbox contra inyección de procesos del mismo SID, ni
contiene servicios creados por intermediarios externos al árbol. Se requiere
validación nativa del arranque suspendido, jobs anidados y fixture de proceso,
incluida revisión de riesgo de muerte durante la asignación inicial.
No se declara PASS nuevo, cierre W1 ni W2-A; W2-B NO-GO hasta validar.

## Revisión de `0203b5c` (26/11/2026)

**W2-A sigue ABIERTO; NO-GO W2-B.** El informe original
`reports/combined/73cebebcd20d4225b500695d49ca0674/` y los informes
Windows/Linux registran `0203b5c40eb2cd39e0c393c2aaabdcf6961d345c`:
AUTOMATED_PASS, 49 checks Windows y 77 Linux, sin FAIL/BLOCKED/SKIP a
nivel de check. Los diez logs Windows de protocolo muestran 25 tests PASS cada
uno; las veinte repeticiones Lifecycle incluyen sus dos aserciones PASS.
Los logs de Cargo muestran fleet-agent 13 tests Windows y 12 Linux PASS, y
el fixture de argumentos nativos PASS en ambos. La prueba real opt-in
Gentle Shell está ignored; no se convierte en PASS. La revisión del gate
histórico `8662aea` está en P7_W1_RESULTS.md.

Un daemon Windows aislado ejecutó `fleetctl agents`, usando Node directamente
con entrypoints JS explícitos de Pi y Gentle Shell: ver la matriz. No se
enviaron prompts, instalaron proveedores ni cambiaron rangos de compatibilidad.

Auditoría parcial de contención: el sondeo común mata y espera al hijo directo
ante timeout, pero no contiene descendientes. Un descendiente que hereda
stdout puede mantener vivo el lector tras el plazo; el propio código reconoce
este límite. JSONL y OpenCode también terminan al hijo directo, no demuestran
terminación de árbol. Es un **riesgo P1 candidato**, no una reproducción
determinista completa: faltan fixture de descendiente/handles, análisis de
startup y solución verificada sin carreras ni daños a procesos ajenos.
No se modificó código de producción ni se afirma que la limpieza esté resuelta.
Se requiere corregir los defectos confirmados, pruebas de proceso Windows y
regresión Linux y un nuevo gate del mismo SHA limpio antes de cerrar W2-A.

## Evidencia histórica — base `8354b19`

**Estado histórico: ABIERTO / NO-GO.** Base actualizada `8354b19`; no hay cambios de
infraestructura común ni pruebas nuevas acreditadas en este registro.

La inspección inicial encontró rutas distintas de descubrimiento: Codex
resuelve explícitamente `.exe` en Windows, mientras Pi y Gentle Shell invocan
nombres sin extensión y sus instalaciones presentes exponen shims `.cmd`.
Pi usa `HOME` o `/tmp` para localizar sesiones; OpenCode usa `HOME` o temporal
para el estado de servicio. Son hallazgos de portabilidad que requieren
reproducción y pruebas, no garantías de funcionamiento. `fleet-rpc-jsonl`
acepta CRLF y mantiene stdout y stderr separados; sus pruebas de proceso
existentes incluyen un fixture exclusivamente Unix. Los argumentos y la
limpieza de procesos Windows aún necesitan pruebas negras específicas.

Los comandos `--version` reales se ejecutaron sin instalar ni actualizar
proveedores; ver [matriz](P7_W2_PROVIDER_MATRIX.md). No se ejecutó una sesión,
descubrimiento mediante API Fleet, ni conformidad real. El gate W2-A Windows y
Linux sobre el mismo SHA limpio está pendiente. No se han verificado todos
los componentes ni corregido los defectos P0/P1 de este alcance.

## W2-A — candidato de infraestructura

Se agrega módulo común de lanzamiento y sondeo acotado en fleet-agent,
composición Command en transporte JSONL, ruta de usuario portable, soporte
explícito Node+JS sin shell y limpieza de arranque OpenCode. Las pruebas
unitarias del sondeo ejecutan el propio binario de tests como fixture; incluyen
exit no cero, exceso de salida, timeout y resolución EXE en Windows.
No hay ejecución de Cargo ni de CI nativo acreditada. Estado UNTESTED / NO-GO.
Quedan por validar entrypoints de las instalaciones reales, descubrimiento
Fleet, argumentos de proceso adversos, descendientes y recuperación; W1-F
formal permanece pendiente de evidencia original. No cambios de versiones,
main, workflows, ni gates de release.

Segundo candidato: el ejecutable/entrypoint del sondeo se conserva durante
open de Pi y Gentle Shell; las consultas nativas RPC de Codex y Gentle Shell
rechazan versiones incompatibles. Se agrega fixture negro argv Windows/Linux,
limpieza del hijo directo ante caída de RPC y error de startup y drenaje de
stdout de OpenCode después de readiness. UNTESTED; no confundir fixture Node
con una instalación real ni hijo directo con contención de descendientes.

Regresiones adicionales candidatas: terminación del root bloqueado con hijo
activo y prueba de kill-on-close cuando el propietario del job muere sin
Drop. Se da un margen acotado de 200 ms al contador del job tras el exit
normal del root. Sigue UNTESTED; no acredita atomicidad durante spawn/assign.

## Gate del candidato de contención: fallo del fixture CloseBlocked

El operador aportó captura de w1d-lifecycle.log (Windows, ejecución
20261126T180500100518Z). Lifecycle, MissingRunner, InvalidRunner, SilentRunner,
Interrupt, InterruptFailure, ActiveStop y CloseFailure PASS; CloseBlocked
FAIL en Invoke-FixtureCli línea 113: CLI stop exit 1, stderr unavailable.
HEAD remoto observado c74e856; SHA ejecutado no verificado por la captura.
La inspección confirma una carrera de diagnóstico: el exit del proceso no
implica que ReadToEndAsync de stderr haya terminado. El fixture descartaba
stderr si la tarea aún no estaba completada. Se sustituye esa comprobación
instantánea por espera acotada de 5s (igual que stdout), conservando el regex
StopUnconfirmed, estado durable, runner vivo y ausencia de Stopped. No se
modifican plazos ni producción. La causa del error real de stop sigue sin
confirmar; si stderr no cierra o no coincide, el fixture sigue fallando.
UNTESTED: repetir CloseBlocked dirigido varias veces y después gate w1f
sobre mismo SHA limpio. No declarar W2-A aceptada con este fallo.

## Validación y avance paralelo solicitado por propietario

PASS del gate w1f y repeticiones CloseBlocked reportado tras el candidato
4a7d44c; no se adjuntó nuevo informe original ni SHA ejecutado. Se autoriza
por el propietario comenzar verticales reales B–E en paralelo sin cerrar
formalmente W1/W2-A ni reducir gates. Candidato común: smoke Windows con
un daemon y workspace por proveedor, chat real, streaming, operación y
replay tipado JSONL, lectura de controles, stop; opcionales herramienta de
lectura y recovery tras restart de daemon. Modo OpenCode explícito; attached
sin fallback owned. UNTESTED; no se ejecutaron prompts ni CI aquí. Pi sigue
bloqueado si su versión instalada es 1.0.4. Aprobaciones, interrupt,
aislamiento simultáneo y controles efectivos requieren pruebas adicionales.

Se añade interrupt opt-in para proveedores que anuncian cancel_turn; una
carrera ganada por el terminal Completed se registra BLOCKED. OpenCode
queda UNSUPPORTED para interrupt. Se agrega regresión FakeDriver de
replay JSONL exacto en Lifecycle. UNTESTED; no sustituye aprobación
interactiva, aislamiento simultáneo ni la aceptación formal W2.

## Primer smoke conjunto real (11a943d, 26/11/2026)

Informe JSON aportado por el propietario: OpenCode 2.4.2 Attached PASS
(7 checks, interrupt UNSUPPORTED); Codex 0.172.0 PASS discovery/session/chat/
controls, FAIL herramienta y stop; Pi 1.6.1 PASS discovery/session, FAIL chat
y stop; Gentle Shell 4.8.0 PASS discovery/session/chat/controls/herramienta/
recovery/interrupt, FAIL stop. Total 20 PASS, 5 FAIL, 1 UNSUPPORTED; 6 etapas
no ejecutadas. Los tres fixtures fallidos se retuvieron. No publicar rutas
ni texto nativo. No atribuir la causa exacta del FAIL a este informe agregado.

Candidato: se corrige desalineación del plazo de close del runner (500ms,
menor que los 2s de EOF del transporte) a 3s, dentro de RPC Shutdown de 5s.
El worker entrega resultado explícito después de drop del driver; no cambia
la política de exit, job ni StopUnconfirmed. Fixture SlowClose (1000ms) y
CloseFailure/CloseBlocked conservados. Se corrige otro defecto confirmado
estáticamente: un delta vacío marcaba falsamente el mensaje como streamed
y suprimía el texto final en los tres mappers RPC. Regresiones de mensaje
final, sin inventar deltas. El smoke registra conteos y presencia del marker,
no texto, y etiquetas de error de stop; --stop-only evita llamadas de modelo.
La causa real de ausencia de texto y de cada stop aún requiere confirmación
nativa. UNTESTED; no se declara cierre de W2 ni cambios a main.

Se añade inspección de fixtures retenidos por API pública de solo lectura,
sin arrancar daemon, enviar prompt, reintentar stop ni imprimir texto de
proveedor. Si el daemon original no responde, informa BLOCKED. El reporte
contiene conteos de deltas, mensajes, herramientas, terminales y razones
clasificadas de parada. No se lee DB SQLite por fuera de Fleet.

## Stop-only real — 9932566 (26/11/2026)

Informe aportado por propietario: OpenCode Attached y Codex Owned PASS
(discovery/open/stop); Pi 1.6.1 FAIL stop por provider_nonzero_exit, estados
Running -> Stopping -> StopUnconfirmed; Gentle Shell BLOCKED discovery,
version desconocida. No hubo turnos reales. No repetir OpenCode por ahora.
Candidato: JSONL conserva lectores durante EOF y drena stdout/stderr,
desconectando colas solo al cierre explícito. Se conserva exit no cero como
StopUnconfirmed y se añade código numérico y clasificación fija de stderr
(mejor esfuerzo). Fixture nativo emite salida abundante después de EOF y
comprueba exit 0 frente a 7. Gentle Shell dispone de sondeo read-only de 10s
en Windows y diagnóstico clasificado de discovery; no se modifica el rango.
La causa de exit no cero de Pi y de versión desconocida de Gentle Shell no
está confirmada; el candidato no acredita que sean causados por pipes o
latencia. UNTESTED; validar stop-only de Pi/Gentle Shell y gate full.

## Bundle Windows Gentle Shell — a8b8413

Stop-only Gentle Shell BLOCKED exe_not_found. El propietario confirmó
instalación alternativa en ~/.gentle-shell-bundle: current 5.0.0-1.6.1,
launcher generado apunta a Node 24.21.0 y versions/<current>/node_modules/
gentle-pi/bin/gentle-shell.mjs. No ejecutar CMD. Resolver ambas raíces del
bundle, la versión activa y el runtime exacto del launcher como datos,
con paths contenidos y lectura acotada; fallback npm y overrides explícitos.
El 5.0.0 es un nuevo major no aceptado. Solo se admite como candidato exacto
si FLEET_GENTLE_SHELL_EXPERIMENTAL_5=1 (harness --experimental-gentle-shell-5),
con negociación Desktop RPC existente. No se amplía rango estable ni se
acepta otro 5.x. UNTESTED; confirmar --version real, stop-only y smoke full.

## CI posterior al bundle — cda68bb

Operador reporta FAIL en clippy Windows/Linux y e2e_concurrency Linux;
contenido de logs no aportado, causas exactas pendientes. Incompatibilidad
estática: espera de Shutdown de 3s frente al contrato de stop bajo driver
ocupado <2s en e2e_concurrency. Candidato separa inicio de close (500ms)
de close iniciado (3s); mantiene StopUnconfirmed en ambos timeouts,
no modifica el límite de e2e ni la confirmación de salida. Regresión de
fases y resultado de error. Limpieza de logs CI previos por defecto,
con --keep-logs y JSON preservados. UNTESTED; solicitar logs originales.

## Smoke real conjunto — 2e07fcb (27/11/2026 UTC)

Informe propietario: 22 PASS, 3 FAIL, 1 UNSUPPORTED, 6 checks no ejecutados.
OpenCode 2.4.2 Attached 7 PASS + interrupt UNSUPPORTED; Gentle Shell 5.0.0
Owned (opt-in experimental) 8 PASS; Codex 0.172.0 discovery/session/chat/
controls PASS, herramienta FAIL (15 deltas, 76 caracteres, marcador ausente,
2 herramientas iniciadas, 0 éxitos), stop FAIL StopUnconfirmed por
active_descendants. Pi 1.6.1 discovery/session/stop PASS, chat FAIL (0 deltas,
0 mensajes, terminal Completed). No demuestra causas de errores de modelo o
herramientas; la cuenta original de agent_errors solo miraba turn_id y no
incluía errores de sesión. El fixture Codex sigue retenido para inspección.
El propietario también reporta CI full/w1f PASS en ambas plataformas; falta
JSON nativo para vincular formalmente el resultado al SHA. W2 no cerrado.
Candidato diagnóstico: categorías fijas de error y conteos de herramientas
fallidas/salida; incluir notices/errors sin turn_id dentro de ventana del
prompt, conservar fixtures fallidos aun con stop exitoso (daemon apagado).
No modificar sandbox, autoaprobar, reintentar mutaciones ni admitir cierre con
descendientes activos. Gentle Shell permanece experimental hasta gate formal.
