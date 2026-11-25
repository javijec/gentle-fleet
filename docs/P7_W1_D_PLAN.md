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
