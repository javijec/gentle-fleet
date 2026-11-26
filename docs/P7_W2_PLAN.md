# P7-W2 — proveedores reales Windows

## Alcance vigente solicitado (26/11/2026)

El orden de cortes siguiente sustituye la nomenclatura histórica de este
documento, que se conserva debajo: **A infraestructura común**, **B OpenCode**,
**C Codex**, **D Pi**, **E Gentle Shell**, **F conformidad conjunta** y
**G aceptación final**. El trabajo Codex ya publicado en `8354b19` se conserva;
no constituye aceptación de W2-A ni de W2-C.

W2-A requiere pruebas negras deterministas de descubrimiento y lanzamiento,
rutas Unicode/con espacios, argumentos adversos, errores y limpieza en Windows
y Linux; coherencia del descubrimiento y ejecución; y gate full de ambos
hosts sobre un mismo SHA limpio. No ejecutar `.cmd` con argumentos no
confiables mediante una cadena de shell sin un diseño y pruebas de quoting.
Los shims no son equivalentes a ejecutables nativos. La validación de cada
proveedor real y su compatibilidad funcional corresponde a B–F; no modificar
configuración ni instalar proveedores automáticamente. Estado: **ABIERTO**.

Inicio de implementación por instrucción del propietario tras PASS integral
W1-F reportado. La revisión formal de los JSON/logs W1 sigue pendiente;
no se reduce su gate ni se declara release. Rama feature/p7-windows-native.

## Cortes (orden de trabajo elegido)

- W2-A Codex: ejecutable nativo, descubrimiento/versionado, chat y streaming,
  replay, cierre. Después herramientas, controles, aprobación interactiva,
  interrupción, errores y supervivencia real.
- W2-B Pi: resolución segura del launcher/Node si corresponde, sesión aislada,
  chat, herramientas, controles, interrupción y recuperación; capacidades
  y UI de extensiones según evidencia.
- W2-C Gentle Shell: RPC, negociación de versión y permisos; no ampliar
  rango 4.8–<5 sin pruebas de contrato. Perfiles e historial seguro mantienen
  sus pendientes P4.3.
- W2-D OpenCode: servicio nativo/owned, rutas de estado reales Windows,
  versión, chat/herramientas, aprobaciones y recuperación; no asumir que
  un OpenCode de otra API/version es compatible.
- W2-E conformidad cruzada, aislamiento y gate full Windows/Linux.

## W2-A primer corte: Codex chat

Rango actual del driver >=0.171,<1. La versión concreta instalada aún no
se conoce. En Windows se resuelve un codex.exe absoluto desde PATH o desde
FLEET_CODEX_BIN explícito (no ejecutar .cmd/.ps1, ni buscar cwd); en Linux
se conserva codex por PATH. Discovery y App Server usan el mismo resolver;
open usa el mismo pathname que su --version. No instalar ni actualizar
proveedores automáticamente. No cambiar versión soportada sin fixture real.
Cierre normal de App Server exige exit exitoso, no solo observar un exit.

Prueba negra opt-in (una llamada real, puede tener costo), desde Windows:

    cargo build -p fleetd -p fleet-runner -p fleetctl --locked
    python tests/windows/w2-codex-smoke.py --real

Si npm expone solo un shim, identificar el codex.exe nativo real de la
instalación y pasar --codex-bin con ruta absoluta. El script no descarga nada.
Usa copia aislada de binarios, workspace temporal con espacios, DB/runtime y
journals propios, sandbox read-only para chat sin herramientas; no modifica
credenciales ni relaja aprobación. Discovery ListAgents también consulta
el resto del catálogo, pero solo evalúa Codex. Exige versión disponible,
turno Completed, delta real y marcador, sin herramientas/autoaprobación,
timeline contigua, operación durable, replay idéntico y stop confirmado.
Si el stop no se confirma conserva fixture para reconciliación; no mata
proveedores por nombre o PID persistido. No imprime prompts/errores crudos
del proveedor. No acredita herramientas, recovery durante turno, control
efectivo, aprobación real, ni descendientes; son los siguientes checks.

Gate automatizado existente sigue determinista/fake-only y sin consumo de
modelo; ejecutar full Windows/Linux mismo SHA (modo w1f conserva estabilidad
de W1). El smoke real no se añade a CI por credenciales y costo. Registrar
versión, SHA, evidencia por proveedor, resultados FAIL/BLOCKED/UNTESTED.
No declarar W2-A cerrada con este único smoke. Estado UNTESTED; no main/W4.
