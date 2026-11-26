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
