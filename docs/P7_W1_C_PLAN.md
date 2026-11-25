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

## W1-C.1 — primer candidato de almacenamiento Windows (25/11/2026)

RED reproducido por el operador sobre el fixture anterior: PASS del workspace persistido tras terminar el daemon; FAIL por DACL del directorio no protegida. Candidato: DataDirectory reutiliza la apertura de antecesores y guardas de W1-B, crea DACL protegida owner-only con OICI para datos, valida DB y sidecars existentes antes de abrir, rechaza reparse/hard links y DACL inesperadas, sin corregir ACL de carpetas existentes ni borrar datos. Se integra la regresión de proceso a CI full y se añaden tests Rust de WAL/SHM vivos, rechazo de directorio heredado y hard link. Default daemon usa LOCALAPPDATA en Windows y mantiene XDG/HOME en Unix; no cae en temporales. LOCALAPPDATA sigue siendo variable de entorno validada por la política de directorio, no resolución Known Folder. Solo rutas locales de unidad con padre existente; UNC no validado. No se migra un almacén antiguo automáticamente. Reutilizar un directorio anterior no privado requiere migración explícita y respaldada, no relajar ACL.

Estado: **CANDIDATO UNTESTED**. Sin ejecución nativa no se confirma representación SDDL ni interacción SQLite/handles concurrentes. No se certifica integridad de ACL de antecesores fuera de las guardas, DELETE_CHILD antes de abrir, privilegios elevados, otra cuenta ni todos los sidecars/transacciones de crash. Spool, journals, SSH, resolución Known Folder, migración y pruebas de crash/concurrencia restantes siguen pendientes. Gate de entrada automatizado reportado; no sustituye revisión de logs ni aceptación técnica completa. No iniciar W1-D/E/F.

### Reparación de fixtures tras primera validación

La prueba de proceso W1-C.1 pasó en Windows sobre 770c806, pero el full no: los fixtures de fleet-core creaban de antemano el directorio de SQLite con ACL heredada. Mantener su raíz única reservada y dejar crear a SqliteStore un hijo data nuevo. No modificar la política Windows ni las pruebas negativas; aplicar los diffs rustfmt del operador. Revalidación nativa pendiente. Ver P7_W1_RESULTS.md.

## W1-C.2 — spool durable Windows, primer candidato

El operador reportó AUTOMATED_PASS de W1-C.1 (24 Windows, 77 Linux, sin FAIL/BLOCKED/SKIP) tras `1af8f10`; el resumen no incluye SHA ni logs, por lo que la asociación se basa en la secuencia de actualización y HEAD remoto, no en verificación independiente. El candidato W1-C.2 reutiliza DataDirectory para spool Windows: raíz de la base del daemon, archivo por runner_id canónico, ACL y sidecars validados antes de abrir, guardia durante la conexión. Linux mantiene la ubicación histórica. Si existe spool legacy o sus sidecars en runtime, no se borra, mueve ni abre automáticamente: falla explícitamente hasta migración autorizada. Se comparte la resolución de database_path entre daemon y runner. Se introduce busy_timeout y la reserva de secuencia/límites queda dentro de BEGIN IMMEDIATE para serializar conexiones separadas. Tests de reapertura, ACK, reserva de operaciones, notices, concurrencia y hard link. No se cambia schema, contrato wire ni se añade reattach Windows tras muerte del runner. Los fixtures W1-A/B existentes siguen obligatorios.

**UNTESTED:** fmt, compilación y ejecución de este candidato. El nombre por runner_id protege contra colisiones de nombres canónicos en una misma raíz, no certifica un archivo copiado por el propio SID, identidad de generación, exclusividad entre dos runtimes que apuntan a una misma base, migración de spool activo ni aislamiento entre cuentas; esos límites deben tratarse antes de aceptación completa. No se toca el spool anterior. Journals, SSH, Known Folder, crash y pruebas restantes W1-C pendientes. No se inicia W1-D.

Regresiones adicionales W1-C.2: ruta canónica por ID, rechazo de alias de caso/traversal/ADS, namespace reservado de spool y sidecars, conservación de legacy WAL, verificación de WAL/SHM vivos; el fixture de proceso W1-A conserva sus comprobaciones y añade dos runners con spools durables distintos y ACL privadas. Sin ejecución nativa aún.

## W1-C.3 — journals SDK, candidato

Tras PASS reportado del gate W1-C.2 con auto-format en HEAD `84f219e`, se comparte la política privada de DataDirectory para archivos Windows (sin sobrescribir archivos existentes), se usa LOCALAPPDATA/gentle-fleet para journals Windows y se mantienen XDG_STATE_HOME/HOME en Linux. Los overrides deben ser absolutos; en Unix se crean directorios 0700 y se rechazan directorios finales/ficheros accesibles a terceros, symlinks finales y hard links. Se serializan los escritores con File::lock (requiere Rust >=1.89), lectores con lock_shared, se hace seek/write/sync bajo lock y se distingue cola final sin newline de corrupción JSON/estructura intermedia al leer cursor. Una cola parcial bloquea lectura/escritura sin borrarla; recuperación explícita pendiente. No se altera el contrato de journal de intentos de operación ni se reenvía un prompt incierto. Si persistir cursor falla tras retirar evento, se cancela la suscripción para impedir saltar el hueco. Tests de contrato y unidad: reapertura por host, corrupción, cola parcial intacta, hard link, escritores simultáneos y cierre tras fallo de cursor. Linux mantiene E2E SDK/SSH existentes. No se introducen dependencias.

**UNTESTED:** compilación, fmt, Clippy y ejecución de este candidato. Locks de archivos coordinan clientes cooperantes; no se han validado contención entre procesos independientes ni ACL de antecesores Unix/Windows completa, deduplicación semántica de intentos, recuperación automática de cola parcial, límite/rotación ni crash del journal. No se considera W1-C.3 cerrada; pruebas negras nuevas de proceso/Windows y revisión de riesgos pendientes. No iniciar W1-C.4 hasta verificar el candidato. El aislamiento entre SID sigue diferido a release.

## W1-C.4 — catálogo SSH privado y actualización atómica (candidato)

Operador reportó 24/24 Windows + 77/77 Linux AUTOMATED_PASS de W1-C.3. HEAD remoto 63534b9; resumen sin SHA ni logs independientes. Catálogo Windows bajo LOCALAPPDATA/gentle-fleet, Unix XDG_CONFIG_HOME/HOME, sin fallback temporal. Overrides absolutos, directorio y archivos privados, rechazo de hard links y ACL inesperadas. Lock hermano persistente para serializar toda la mutación CLI add/remove, reemplazo por archivo temporal privado sincronizado, rename en el mismo directorio; lectores ven archivo completo previo o nuevo. save_hosts conserva semántica de reemplazo explícito; config_path ahora devuelve Result para fallar si no hay raíz válida. No modificar known_hosts, transporte ni permisos de archivos existentes. Configuración anterior insegura requiere migración explícita y respaldada. Tests de concurrencia, corrupción y hard links; nueva regresión Windows de ocho procesos CLI y ACL, agregada al full. UNTESTED: compilación y ejecución; durabilidad ante pérdida de energía Windows, contención/crash bajo condiciones adversas y ACL de antecesores completas no certificadas. Pendiente de W1-C.3: fleetctl pending aún resuelve/lee el journal por su cuenta; unificar con SDK antes del cierre integral (no modificar en este corte). No iniciar W1-C.5 hasta gate automatizado del candidato.
