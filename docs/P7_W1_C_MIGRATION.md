# W1-C — política de actualización y recuperación de datos

## Alcance y advertencias

La actualización no mueve, borra, fusiona ni cambia ACL de datos antiguos. No se admite una migración transparente de sesiones o runners vivos. Esta guía describe una decisión explícita de operador sobre datos offline, no una herramienta de backup/restauración certificada. Las pruebas de restauración completa y actualización/rollback de release pertenecen también a P7-O. No usar datos de producción como fixture.

El modelo de amenaza considera al SID local como frontera de confianza. No proteger contra un administrador, contra código malicioso ejecutado por el mismo SID, ni prometer garantías de pérdida de energía o de filesystem remoto. No confundir los guardas de directorio con una auditoría de todas las DACL de antecesores. Evidencia efectiva DELETE_CHILD y otro SID permanece requerida antes de release.

## Inventario previo (sin escribir)

Registrar los overrides vigentes, rutas absolutas, usuario y versión del motor. Resolver el LocalAppData real del usuario en Windows (Known Folder; no confiar solo en LOCALAPPDATA). Los defaults actuales de DB, journals y catálogo Windows viven en gentle-fleet bajo esa raíz:

| Estado | Override | Nombre |
|---|---|---|
| DB del daemon | FLEET_DB_PATH | fleet.sqlite |
| Endpoints y tokens efímeros | FLEET_RUNTIME_DIR | runtime separado |
| Spools de Windows | raíz de FLEET_DB_PATH | fleet-runner-*.spool.sqlite |
| Intentos de operación | FLEET_OPERATION_JOURNAL | operations.jsonl |
| Cursores | FLEET_CURSOR_JOURNAL | cursors.jsonl |
| Catálogo SSH | FLEET_SSH_CONFIG | hosts.json |

Antes de W1-C, Windows usaba XDG_DATA_HOME o HOME/.local/share, con fallback al temporal para DB; los journals usaban XDG_STATE_HOME o HOME/.local/state; hosts.json usaba HOME/.config (o /tmp si HOME faltaba). Los spools vivían en runtime. Linux mantiene XDG/HOME para DB y journals y admite XDG_CONFIG_HOME para hosts. Consultar los overrides y la configuración de la instalación histórica; no asumir que los defaults antiguos fueron usados. No crear un almacén vacío nuevo para ocultar uno antiguo inaccesible.

## Respaldo y decisión

1. Identificar todos los clientes, daemons, runners, proveedores y runtimes asociados. Confirmar que terminaron de forma controlada y que no quedan operaciones/aprobaciones inciertas sin registrar. Si no se puede confirmar el estado, detener la migración.
2. Hacer un respaldo offline protegido de la raíz completa, DB, WAL/SHM/journal si existen, lease/identidad, spools, catálogos y journals; inventariar también runtimes antiguos con evidencia de runner. No incluir tokens efímeros en material compartido. No copiar únicamente fleet.sqlite si existe WAL.
3. Verificar integridad SQLite y claves foráneas sobre una copia de respaldo, conservando su WAL y cualquier otra evidencia necesaria. Revisar por separado la identidad y estado de sesiones/runners; un integrity_check correcto no demuestra ausencia de operaciones inciertas. Conservar la copia original sin modificar.
4. Elegir una ruta local privada. No usar una carpeta con ACL heredada amplia, junctions/reparse points, enlaces duros o antecesores controlados por terceros. No tomar propiedad ni ampliar ACL para conseguir que una prueba pase.

## Caso limitado: adopción de una DB legacy existente

Solo se permite cuando el directorio ya cumple la política, el lease no contiene identidad, la DB es la autoridad correcta y se ha comprobado offline que no hay runners ni spools asociados sin reconciliar (incluidos runtimes anteriores). La comprobación automática de nombres fleet-runner-* cubre la raíz seleccionada, no todos los runtimes ni el historial de otras bases. El operador debe verificar el resto antes de adoptar.

El consentimiento es FLEET_ADOPT_LEGACY_DB=1 exclusivamente para ese arranque; quitarlo después. Mantener FLEET_DB_PATH y FLEET_RUNTIME_DIR explícitos y correctos. La variable no anula ninguna ACL ni permite reemplazar una DB ya vinculada. El proceso escribe la identidad física y nombre exacto en fleetd.lease bajo lock exclusivo antes de abrir SQLite; por ello incluso una inicialización fallida puede dejar la marca vinculada. No borrarla para reintentar. Validar el workspace y datos esperados por API antes de habilitar nuevos prompts. Si falla, detenerse y conservar la evidencia.

No hay procedimiento automatizado para adoptar una raíz con spools previos, DB sustituida/renombrada, marca incompleta u huérfanos WAL. En esos casos se requiere reconciliación específica. No copiar spools a otra raíz para que otro boot los adopte. No usar FLEET_ADOPT_LEGACY_DB para recuperar identidad de una DB restaurada desde copia.

## Catálogo y journals

Los archivos previos deben pasar las mismas comprobaciones de seguridad que los nuevos. Una instalación insegura necesita preparar una copia en una ubicación privada por un procedimiento offline revisado; no relajar ACL ni modificar permisos de rutas arbitrarias. No combinar catálogos o journals sin validar identidades de host, duplicados, secuencias y referencias. Mantener claves de host SSH y operación incierta originales; nunca desactivar StrictHostKeyChecking. Un journal de cursores no es una confirmación de procesamiento de negocio; una caída entre persistencia y entrega puede requerir reconciliación explícita. No compartir un cursor persistido entre consumidores independientes como garantía de entrega para todos.

Si la cola de operaciones queda parcial, fleetctl pending --inspect-torn muestra solo registros completos válidos bajo lock y retorna error si la cola sigue incompleta. No modifica el original; reconciliar las claves conocidas mediante fleetctl --host ALIAS operation CLAVE. No volver a enviar por defecto una operación incierta. Corrupción intermedia exige revisión de copia, no truncado silencioso. La reparación de cola y restauración integral no están automatizadas.

## Evidencia y límites

Las regresiones nativas W1-C comprueban rechazo de cambio de identidad, exclusividad, persistencia del workspace, transacciones SQLite ante kill, ACL/sidecars, concurrencia de catálogos y journals. Las nuevas pruebas de protocolo comprueban aceptación explícita/rechazo de adopción, spool legacy, WAL huérfano y marca incompleta sobre archivos de fixture. No sustituyen un ensayo real de migración de datos del usuario ni un ataque de otro SID. La adopción de archivos copiados con formato SQLite real, verificación de todas las DACL efectivas de antecesores y respaldo/restauración de sesiones completas necesitan evidencia separada. No usar esta guía para declarar release PASS.
