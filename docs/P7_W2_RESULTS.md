# P7-W2-A — resultados preliminares

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

**Estado: ABIERTO / NO-GO.** Base actualizada `8354b19`; no hay cambios de
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
