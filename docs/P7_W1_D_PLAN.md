# P7-W1-D — plan de ciclo de vida nativo

**Estado: preparación; implementación y aceptación bloqueadas por revisión de
parada y por el cierre pendiente de W1-C.** Base automatizada `846a4b227d8f005125678bab7851c00b1c945da8`; el commit documental `064e6f4` no cambia código. Este plan no acredita una prueba nueva ni autoriza W1-E/F o W2.

## Hallazgos de auditoría

| Prioridad | Hallazgo y evidencia estática | Tratamiento |
| --- | --- | --- |
| P1 candidato | `fleet-core/src/lib.rs:2358`: si no hay handle, `stop_session` publica `Stopped` y marca el runner como detenido sin demostrar que el proceso haya terminado; el test `stop_after_failed_reattach_finalizes_durable_state` fija ese comportamiento. Un reattach fallido no demuestra muerte. | Reproducir con runner vivo y fallo controlado de reattach; corregir antes de avanzar si se confirma. Diferenciar ausencia de runner creado de estado incierto. |
| P1 candidato | `fleet-core/src/runner.rs:258` descarta el error de `driver.close()` y señaliza cierre en el launcher en proceso. Esto no demuestra el mismo defecto en el runner de proceso. | Prueba roja de driver que devuelve error y confirmar `StopUnconfirmed`; corregir en el propietario del contrato. |
| P1 candidato | `fleetd/src/process_runner.rs:341-357`: después de spawn, lectura de identidad no tiene plazo y los errores de lectura/parse/handshake abandonan `Child` sin terminación explícita. Riesgo de bloqueo u orphan; falta reproducción de proceso. | Fixture controlado para silencio, identidad inválida y fallo de Hello; timeout y limpieza únicamente del hijo propio con estado incierto cuando no se confirme. |
| P1 candidato | `fleetd/src/process_runner.rs:264-278`: `Shutdown` no tiene plazo explícito en `rpc`; tras dos segundos se ignoran resultados de kill/wait, y una respuesta RPC cualquiera se acepta. En reattach no existe `Child`. | Probar peer bloqueado, respuesta inesperada y errores; distinguir confirmación de driver, salida de runner y terminación forzada verificable. Sin matar por PID no autenticado. |
| P2 / sin probar | Resolución usa `FLEET_RUNNER_BIN` o vecino `fleet-runner` vía `current_exe`, luego PATH si falla `current_exe`; `Command` recibe argumentos separados. La resolución `.exe`, rutas con espacios, override inválido y comportamiento de PATH necesitan prueba nativa. | Fixture con binarios y workspace en rutas con espacios; errores explícitos, sin shell ni concatenación de argumentos. |
| P2 / sin probar | `Command` canaliza stdout, archivo stderr o stderr heredado; no hay auditoría de herencia de handles del proceso y sus descendientes. | Inspeccionar opciones reales de `std::process` y comprobar con handles centinela; no deducir que todos son privados. Mantener independencia respecto a muerte del daemon. |
| P2 / sin probar | Interrupt se transmite con clave por turno (`process_runner.rs:170-174`); un ACK de recepción no es evento terminal. | Probar turno correcto/incorrecto, error de entrega y evento terminal real; nunca inventar `Interrupted`. |
| P2 / sin probar | Dos runners tienen identidades y spools separados en W1-A; no demuestra aislamiento completo de handles, stop y procesos descendientes. | Dos sesiones activas, parar una y verificar que la otra continúa; centinela externo intacto. |

Las prioridades candidatas se basan en inspección, no en reproducción nativa.
No concluir ausencia de P0/P1 hasta investigar y resolver los confirmados.
No convertir un timeout o una desconexión en prueba de muerte.

## Diseño propuesto

Usar `Child` y handles de proceso retenidos como autoridad para hijos recién
creados, sin búsquedas globales ni kill por nombre. En cada fase de startup
definir plazo, cancelación, identidad esperada y propiedad; observar la salida
del hijo y recogerlo si se confirma su terminación. Un fallo tras efecto
incierto conserva evidencia y no relanza automáticamente. Para un runner
reattachado, verificar identidad y utilizar el protocolo autenticado; no
suponer que un PID persistido todavía identifica el mismo proceso.

`Stopped` requiere evidencia verificable del cierre pertinente; un error,
timeout, respuesta no válida o imposibilidad de verificar resulta en
`StopUnconfirmed`/reconciliación, no éxito. Una escalada forzada solo puede
afectar procesos cuya propiedad esté demostrada; documentar por separado
runner y proveedor, especialmente descendientes. No usar un Job Object
kill-on-close que destruya la supervivencia al daemon exigida en P3.
Limitar esperas sin mantener locks globales; compatibilidad Unix sin cambiar
sus garantías. La política exacta de herencia y limpieza se decide con pruebas
negras antes de implementar.

## Primera historia vertical preparada

Fixture propuesto `tests/windows/w1d-lifecycle.ps1`, inicialmente **no
implementado / UNTESTED**: copiar binarios Fleet de la misma build a una ruta
aislada con espacios; crear runtime, DB y workspace privados aislados; lanzar
`fleetd.exe`, registrar workspace, crear sesión `fake`, enviar prompt por
`fleetctl.exe`, consultar timeline hasta `AssistantDelta` y `TurnCompleted`
del turno esperado y luego solicitar stop. Exigir resultado público y salida
del runner de esa sesión mediante handle verificable; comprobar que daemon y
centinela ajeno siguen vivos. Registrar handles/PID de fixtures desde su
creación, plazos y stdout/stderr redactados; cleanup exclusivamente de
procesos propios. No depender de proveedor externo: FakeDriver es fixture
integrado, no un ejecutable ni versión real de agente. Proveedores y versiones
reales se reservan a W1/W2 según su gate, sin declarar integración real aquí.

Primero escribir una variante roja dirigida al defecto que se reproduzca
(p. ej. launcher fixture que no anuncia identidad o cierre no confirmado),
antes de modificar producción; conservar la historia positiva como control.
No etiquetar roja por un error de compilación o de infraestructura.

## Regresiones y aceptación

1. Spawn normal, ejecutable ausente, rutas con espacios, stdout ausente/
   truncado/identidad incorrecta, Hello inválido, proceso que sale o no publica
   identidad; control de tiempo y procesos propios, sin tocar spools ajenos.
2. Cierre normal, fallo de `close`, peer bloqueado, ACK no válido, timeout,
   kill/wait fallidos, reattach fallido y proceso de identidad incierta; probar
   `StopUnconfirmed` y no afirmar que terminó cuando no hay evidencia.
3. Turno en curso: interrupción entregada, rechazada y sin terminal; stop con
   incertidumbre y sin reenvío; dos runners y proceso externo centinela.
4. Inspección de handles heredados con sincronización determinista, cleanup
   tras fallo de startup y tras terminación confirmada; no usar kill global.
5. Ejecutar la vertical Windows y regresiones específicas; conservar full
   Windows/Linux del mismo SHA limpio, W1-A/B/C y protocolo 10/10, más pruebas
   Linux equivalentes para cualquier ruta compartida. Quick solo diagnóstico.
6. Sin P0/P1 confirmados pendientes; revisión de logs y limitaciones. No
   sustituir requisitos manuales de release, dos SID, proveedores reales,
   SSH físico ni gate de seguridad por FakeDriver. No iniciar W1-E/F ni W2.

## Corrección candidata previa al cierre W1-C — confirmación de parada

Inspección confirmó dos caminos lógicos que reportan éxito sin evidencia: ausencia de handle y error de close descartado por launcher en proceso. Se sustituyen las expectativas anteriores por regresiones que exigen StopUnconfirmed en API, timeline y runner durable; el caso sin handle también verifica persistencia tras reinicio. FakeConfig añade fail_close (FLEET_FAKE_FAIL_CLOSE para futuros fixtures). SQLite conserva intención de stop incierta al reiniciar; recuperación no convierte StopUnconfirmed/Failed en runner Stopped ni revive sesiones. Stop simultáneo se rechaza mientras uno está en curso. Cambios compartidos Linux/Windows, sin nuevo wire ni tablas. UNTESTED: pruebas no ejecutadas aquí, no se declara RED observado ni PASS. Pendientes: revisar transición y persistencia ante fallo de storage, startup/Child, RPC shutdown/timeout y verificación del proceso real; primera vertical W1-D sigue especificada, no implementada. No iniciar W1-D ni cerrar W1-C hasta resolver y validar bloqueantes.

## Candidato previo al cierre — propiedad y plazo de startup

PASS de core y gate conjunto reportado por operador después de 6e6499f; HEAD remoto observado d61c96f, sin JSON/logs nuevos. Se conserva el Child en launcher hasta completar identidad/Hello; startup lee como máximo 16 KiB con newline y plazo 70s (runner interno 60s); compara runner/session/provider/host/PID/boot/generation/wire. Hello tiene watchdog de 5s que cancela la conexión, también en reattach. Fallo de lectura/identidad/Hello o salida temprana termina y comprueba exclusivamente el hijo propio (try_wait/kill/try_wait acotado); error de limpieza explícito, no declara cierre de proveedor ni borra evidencia. stdin null, .exe explícito en Windows y override OsString. Pruebas de parsing, timeout por canal y dos procesos de test (owned y centinela). No ejecutadas aquí; no hay RED/PASS observado. Límites: conexión inicial y operaciones de filesystem no sujetas a plazo global; lector stdout puede quedar bloqueado si descendiente hereda pipe; no se garantiza limpieza de descendientes, persistencia de identidad de startup fallido ni bounded RPC general. Pendiente shutdown y fixture vertical real; no declarar W1-D iniciado ni cierre W1-C.

## Candidato previo al cierre — shutdown confirmado

PASS de fleetd y gate conjunto reportado tras de121f5 (HEAD c1eb647), sin nuevos informes. RPC acotado reutilizado por Hello y Shutdown: incluye espera del lock por runner y cancela IPC al vencer el plazo; no abandona una llamada en segundo plano. Shutdown exige variante Shutdown con estado Stopped y salida exitosa verificada del Child propio dentro de 2s; no fuerza kill ni pierde handle si no confirma salida. Para reattach sin Child, ACK de driver no prueba salida del proceso: devuelve StopUnconfirmed conservador hasta implementar prueba de identidad/vida (W1-E); cambio deliberado de comportamiento, no inferir muerte del PID. Errores RPC/estado/exit quedan StopUnconfirmed. Tests nuevos de respuesta, proceso vivo/no exitoso/exitoso y bloqueo de lock/peer Unix; sin ejecución aquí, sin RED observado. No cambia wire ni políticas de Windows. El plazo del RPC general (Submit/Pending/Ack) aún no se modifica; cancelación de Shutdown interrumpe ese IPC si contiende. Falta demostrar cancelación en IPC Windows real, fixture vertical y revisión de logs; no cerrar W1-C ni iniciar formalmente W1-D sin evidencia. No declarar limpieza de descendientes.

## Primera vertical de proceso — candidato de regresión de entrada

Operador reportó PASS de fleetd y full conjunto tras corrección de compilación ca787ce, HEAD remoto observado b0f5a9b. Los cuatro hallazgos prioritarios tienen correcciones con PASS reportado, no revisión independiente de nuevos logs. Se prepara tests/windows/w1d-lifecycle.ps1, sin tocar producción: copia los tres binarios a ruta única con espacios, usa resolución sibling por defecto, dos sesiones FakeDriver y prompts con claves únicas, exige delta/terminal/operación Completed, parada con respuesta Stopped y exit code 0 del runner observado mediante handle propio, verifica segunda sesión operativa y centinela ajeno intacto. Cleanup por handles de hijos del daemon vivo y ejecutable exacto de fixture, sin kill por nombre. Se incorpora al full local Windows y workflow; total esperado en modo w1c 27 Windows + 77 Linux = 104 si todos pasan. UNTESTED: script y gate no ejecutados aquí. Es una regresión positiva de los riesgos de parada/startup, no prueba roja reproducida ni aceptación integral W1-D. No cubre cierre de proveedor real, descendientes, fallos de Hello/IPC Windows ni reattach. W1-C sigue pendiente de revisión de evidencia del SHA vigente; no avanzar a otras fases.

## Resultado de primera vertical nativa

PASS individual reportado y gate conjunto AUTOMATED_PASS sobre `c4f2abcb9438a88acc6bc42501049ef639a48133` (Windows 27 / Linux 77). Se corrigieron dos defectos del fixture (parsing workspace y ausencia explícita de override en entorno hijo); no se rebajaron aserciones. El gate incluye `w1d-lifecycle` y protocolo Windows 10/10. La vertical positiva queda con evidencia automatizada favorable, pendiente revisión de logs. W1-D no está aceptada: quedan las pruebas negativas de startup/Shutdown IPC Windows, interrupción, herencia de handles y límites de limpieza/propiedad según el plan; reattach y recuperación integral conservan alcance W1-E. No iniciar nuevos cambios de producción hasta revisión final de entrada W1-C. No certificar proveedores reales con FakeDriver.

## Inicio formal: cancelación de Shutdown

Tras revisión de evidencia `c4f2abc`, W1-C queda cerrada técnicamente para desarrollo limitado (ver resultados). W1-D está **abierta**. Primer corte: regresión Windows de `rpc_bounded(Shutdown)` contra named pipe real; el peer confirma recepción del frame y permanece silencioso hasta la cancelación. Un proceso fixture aislado impone plazo exterior y usa runtime propio. El test dirigido pasó en Windows nativo; no fue una regresión roja ni se modificó producción. No demuestra todas las carreras del kernel, el cierre de un proveedor ni reattach. La validación full del nuevo SHA en ambas plataformas está pendiente; startup negativo, interrupción, herencia y propiedad continúan pendientes.

Gate `cef2689`: **FAIL** en W1-D lifecycle por `StopUnconfirmed`/timeout RPC tras completar turnos; Linux full PASS. La repetición dirigida pasó, sin identificar causa. Se prioriza diagnóstico de contención de lock frente a respuesta del peer, con reproducción sincronizada, antes de cualquier corrección. No marcar esta repetición como resolución ni el gate como PASS. Ver resultados para ruta de log y límites.


## Diagnóstico acotado del fallo intermitente

Candidato de diagnóstico (sin modificar los plazos, locks ni la política de cierre): `runner_rpc_timeout` distingue espera del lock RPC, escritura de la solicitud y espera de respuesta. Las regresiones existentes verifican el diagnóstico de lock y peer silencioso en Unix y de respuesta en named pipe Windows. No se registra RED observado ni PASS: falta ejecutar nativamente. El objetivo inmediato es reproducir el fallo `w1d-lifecycle` con este diagnóstico y corregir únicamente su causa demostrada; después completar las regresiones negativas de W1-D y ejecutar un único gate full final sobre el SHA limpio. No iniciar W1-E hasta aceptar W1-D. No se reabre W1-C por este hallazgo.


## Candidato de consolidación W1-D

Se evita que nuevas llamadas RPC normales (incluidos Pending, Status, Submit y
ACK) se adelanten a Shutdown: se anuncia la intención antes de esperar el mutex
y se comprueba antes y después de adquirirlo. Una llamada en curso conserva
su plazo existente; no se corta preventivamente una trama sana. Un ACK que
se rechace durante el cierre no adelanta el cursor ni borra el spool: el evento
se persiste antes de ACK. Si no se verifica el cierre, el estado sigue incierto;
no se reabre automáticamente esa conexión para prompts. No se aumentan los
plazos de producción. El runner informa StopUnconfirmed si no puede admitir
el comando de cierre, en lugar de ignorar el resultado de try_send.

La prueba de named pipe Windows sincroniza el mutex ocupado con Shutdown y
comprueba que un nuevo Status no se encola ni se transmite. El fixture
w1d-lifecycle.ps1 ejecuta por defecto siete escenarios aislados: ciclo positivo,
ejecutable ausente, ejecutable incorrecto, interrupción correcta/incorrecta,
stop con turno activo, fallo de close y close bloqueado. Para repetición dirigida
se puede ejecutar -Scenario Lifecycle. No se cambia la composición del gate
ni se cuentan escenarios como nuevos checks de CI. Los tests previos de identidad,
timeout, respuesta tipada, Child y limpieza de procesos se conservan.

Estado: candidato UNTESTED. La contención es una deficiencia estática mitigada,
no causa comprobada del fallo de cef2689. Falta evidencia nativa, pruebas
integradas de Hello inválido y silencio de startup, e inspección efectiva de
herencia de handles/descendientes; no afirmar W1-D cerrada. No se modifica
main, ni se inicia W1-E. FakeDriver no acredita proveedores reales.
