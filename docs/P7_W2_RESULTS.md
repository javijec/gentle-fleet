# P7-W2-A — resultados preliminares

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
