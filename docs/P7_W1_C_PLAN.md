# P7-W1-C — persistencia segura del motor

**Estado (25/11/2026): gate automatizado W1-B/P7-H PASS reportado; preparación de primera regresión W1-C y revisión de logs pendiente.** Base auditada `10d2912`. La decisión de diferir dos SID al release permanece; no equivale a PASS. No modificar los contratos IPC de W1-B ni iniciar W1-D/E/F en este corte.

## Entrada reproducible

Con checkouts Windows y Linux limpios, en el mismo HEAD de P7, ejecutar desde Windows:

```powershell
python scripts/ci/w1c_entry.py --host javiomarchi --remote-root /home/javijec/dev/gentle-fleet
```

El wrapper ejecuta el full conjunto existente con SSH estricto y, solo si pasa, diez ejecuciones de fleet-protocol con paralelismo normal. Conserva salida de cada repetición en reports/ y un entry.json ligado al SHA. No hace pull ni modifica el entorno del usuario. AUTOMATED_PASS no certifica identidad, cancelación del kernel ni aislamiento entre SID: revisar logs completos, resultados W1-A/B y hallazgos P0/P1; no aceptar un fallo, SKIP, ejecución incompleta o informe antiguo. No iniciar implementación sin aceptar ese gate. El wrapper es nuevo y aún no se ha ejecutado nativamente.

## Auditoría del árbol actual

| Componente | Hallazgo | Acción propuesta |
| --- | --- | --- |
| fleetd/src/main.rs | database_path usa HOME/XDG y fallback temporal; abre SQLite antes del bind | Resolver ruta durable explícita, validar raíz antes de abrir y no mutar datos sin autoridad del daemon |
| fleet-store-sqlite/src/lib.rs | ACL Windows ausente; SQLite abre DB y sidecars por nombre | Directorio privado validado y mantenido, DB/WAL/SHM y journal rollback protegidos desde creación y durante reaperturas |
| fleet-runner/src/main.rs y spool.rs | spool SQLite vive bajo runtime, no raíz durable dedicada | Ruta por runner estable bajo estado; no migrar ni borrar spool activo de forma implícita |
| fleet-client/src/lib.rs | journals HOME/XDG; permisos Windows ausentes | Ruta nativa por usuario, validación también al leer, escrituras concurrentes y flush/errores explícitos |
| fleet-ssh/src/lib.rs | config_path cae en /tmp; save_hosts escribe directamente | Ruta privada por usuario, escritura atómica y validación; no debilitar known_hosts |
| fleet-protocol/src/windows_pipe.rs | private_runtime protege directorios de endpoints y write_private_file reemplaza archivos | Reutilizar primitivas auditadas según corresponda; no usar un helper que borra/recrea credenciales para DB o journals durables |
| fleet-driver-pi/src/lib.rs | directorio nativo y sesiones propias tienen decisiones temporales | Auditar distinción de estado propio, sesiones nativas y probes efímeros; no cambiar formato ni adoptar historial sin prueba |

## Diseño de corte (propuesto; no código implementado)

- Una política compartida de rutas/seguridad del filesystem, sin crear dependencias circulares ni duplicar autoridad. Windows: raíz durable bajo LocalAppData del usuario (resolver ruta conocida y validar identidad; no confiar solo en una variable arbitraria). Linux: conservar XDG_DATA_HOME/XDG_STATE_HOME/XDG_CONFIG_HOME y compatibilidad con HOME. Runtime separado de datos durables. Respetar overrides FLEET_DB_PATH, FLEET_RUNTIME_DIR, FLEET_OPERATION_JOURNAL, FLEET_CURSOR_JOURNAL y FLEET_SSH_CONFIG solo si pasan la política de seguridad aplicable. Ausencia de raíz válida: error explícito, nunca fallback temporal durable.
- Creación privada antes de escribir: SID propietario y DACL protegida para directorios durables, con herencia de archivos/descendientes controlada para SQLite; Unix 0700/0600 al crear, no chmod de rutas arbitrarias. No reutilizar sin adaptar la DACL de runtime de W1-B (no concede herencia a hijos) ni su helper que elimina archivos. Validar propietarios, DACL efectivas, reparse points, hard links, directorios antecesores/DELETE_CHILD, sustitución y sidecars preexistentes. Mantener guardas y rechazar rutas inseguras; no tomar propiedad de datos ajenos ni modificar ACL del sistema.
- SQLite: conservar schema y WAL/migraciones, transacciones y busy timeout. Probar que todos los ficheros DB, WAL, SHM y rollback journal se crean privados desde el primer acceso, incluso con conexiones concurrentes. Definir durabilidad y recuperación de commit; no borrar ni truncar WAL para recuperar. Si la garantía exige cambios de VFS o de política, detener y revisar el diseño antes de afirmar seguridad.
- Spool: asociar ruta estable a runner_id autenticado, no perder eventos pendientes, mantener ACK, dedup y límites. No prometer reattach Windows ni recuperación de proceso completo hasta W1-D/E.
- Journals SDK: asegurar concurrencia entre clientes, no almacenar prompts ni credenciales, no avanzar cursor antes de persistencia, distinguir línea final incompleta de corrupción intermedia y preservar operación incierta sin reenvío automático. Configuración de hosts: reemplazo atómico privado, evitar pérdida de cambios concurrentes y redacción de diagnósticos.
- Migración: no mover, eliminar o apropiarse de un almacén anterior automáticamente. Definir reconocimiento de rutas antiguas y migración explícita, respaldada, con exclusividad y verificación. Mantener pruebas Linux y evitar cambiar su almacenamiento histórico sin contrato.

## Pruebas que deben escribirse antes del código de cada historia

| Caso | Aceptación |
| --- | --- |
| Windows sin HOME/XDG y con raíz de prueba privada | Ruta durable nativa; no cae en TEMP, respeta espacios y unidades; raíz ausente/inválida falla |
| Rutas explícitas inseguras, reparse/hard links, antecesor manipulable y sidecars preexistentes | Rechazo antes de leer/escribir; archivos de control intactos; no asumir soporte UNC sin prueba |
| Nuevo SQLite con conexiones activas | Propietario, DACL y herencia comprobados en DB/WAL/SHM mientras existen, no después de que SQLite los elimina |
| SQLite concurrente | Transacciones, busy timeout, integridad y claves foráneas sin corrupción ni acceso cruzado |
| Terminación abrupta de proceso fixture SQLite | Confirmados recuperables, no confirmados no visibles, integrity_check y foreign_key_check correctos; kill limitado a PID fixture |
| Spool SQLite | Reabrir, límites, ACK, persistencia pendiente y deduplicación con dos conexiones; no mezclar runner_id |
| Journals SDK | Creación privada, concurrent append, reapertura, no duplicados de operación ni cursor adelantado; recuperación de cola parcial y rechazo de corrupción |
| Hosts SSH | Configuración privada, actualización segura y lectores concurrentes; host key checking estricto |
| Linux | Mantener E2E deterministas existentes y pruebas de permisos; no regressions de datos previos |

Primera historia vertical: daemon + almacenamiento SQLite con ruta durable y fixture Windows aislado, FakeDriver y operaciones públicas. Antes de abrir la siguiente historia revisar los fallos y los límites. Las pruebas de proceso no se sustituyen por tests unitarios ni los proveedores reales por FakeDriver. Evidencia manual real de otro SID queda diferida al release, fixture intacto. No crear cuentas. No declarar soporte de rutas remotas UNC si no se valida.

## Gate de salida

Full nativo Windows/Linux del mismo SHA limpio, consolidación PASS, regresiones W1-A/B, protocolo Windows 10/10 y las nuevas pruebas W1-C de rutas, ACL, WAL, concurrencia y crash, con logs revisados. Cero defectos P0/P1 confirmados pendientes; registrar límites y evidencia real/fixture por separado. No iniciar W1-D–F ni W2 ni merge a main. No reintroducir GUI.

## Primera regresión de proceso (preparación, UNTESTED)

`tests/windows/w1c-persistence.ps1` crea un checkout de datos desechables con espacios, arranca un daemon nativo aislado, registra un workspace por CLI, comprueba persistencia del mismo ID después de TerminateProcess y verifica propietario, ACL privada y ausencia de reparse points en el directorio de datos y DB. Conserva evidencia separada de cada comprobación, no toca almacenes reales ni ejecuta proveedores. El fixture debe fallar contra la implementación Windows previa por la ACL de datos no protegida. No se ha ejecutado todavía y no acredita WAL/SHM vivos, transacciones concurrentes o aislamiento entre SID. Ejecutar explícitamente tras build; no se incorpora al full hasta implementar el primer corte (no ocultar un fallo de un gate existente).

    pwsh -NoProfile -File tests/windows/w1c-persistence.ps1

La historia siguiente debe resolver creación privada con herencia para sidecars antes de SQLite, validación de archivos preexistentes y resolución de rutas durables. No usar el helper de credenciales que borra archivos. No iniciar W1-D/E/F.
