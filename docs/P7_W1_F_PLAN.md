# P7-W1-F — aceptación integral nativa Windows

Estado: candidato UNTESTED. El propietario reportó PASS de D y E y de los gates
conjuntos anteriores. No se recibieron los informes originales completos; este
corte consolida la evidencia pendiente y cubre los requisitos restantes de F.
No introduce cambios de producción ni proveedores reales.

## Criterio observable

En Windows nativo, dos runners FakeDriver propios del fixture, API pública
JSONL sobre named pipes: dos solicitudes concurrentes de turno para A y una
para B. A admite exactamente una, B progresa independientemente; la clave
ganadora devuelve el mismo recibo, una mutación de payload produce Conflict y
la clave perdedora no se reserva. Ambas sesiones llegan a aprobación pendiente
sin efectos de herramienta ni terminal. Dos clientes compiten por resolver A,
exactamente una decisión gana y se persiste; B permanece pendiente hasta una
denegación explícita. Se verifican los estados finales, las operaciones y la
ausencia de herramientas tras rechazo. No hay autoaprobación ni efectos
duplicados. Los procesos y archivos pertenecen exclusivamente al fixture.

El probe Rust usa la respuesta Subscribed para sincronizar una suscripción
antes de iniciar los turnos. El límite de cola es uno y se utiliza la demora
debug de forwarding ya existente (5s, solo fixture). Exige Notice Backpressure,
no un timeout ni un cierre silencioso. Una nueva suscripción desde el cursor
previo debe reproducir todos los Event tipados de la timeline (incluidos ID,
sesión, secuencia, timestamp y contenido). La prueba no afirma saturación
física del kernel ni aislamiento entre SID; esas garantías conservan sus gates.
Tras el probe se confirma salida exitosa de los dos runners y el reinicio no
revive sesiones paradas. Límite exterior de 60s para el probe.

## Gate reproducible

Desde Windows, sobre rama limpia actualizada y SSH Linux configurado:

    python scripts/ci/both.py --mode w1f --auto-format --windows local --linux javiomarchi --linux-root /home/javijec/dev/gentle-fleet

El modo w1f ejecuta full nativo de ambas plataformas, incluidos W1-A/B/C/D/E/F
en Windows y todas las regresiones Linux existentes; conserva protocolo 10/10
y añade veinte ejecuciones consecutivas del escenario positivo Lifecycle en
Windows. Cada corrida tiene log separado y registro de exit_code; se rechaza
un PASS que no contenga las repeticiones exactas. No se altera el paralelismo
normal Rust ni el timeout de producción. No se ejecuta el gate release ni
se cuentan manuales como PASS.

Prueba dirigida opcional, tras compilar los binarios del workspace:

    pwsh -NoProfile -File tests/windows/w1f-acceptance.ps1

## Cierre

Revisar combined.json, windows.json, linux.json del mismo run y SHA limpio,
los logs de los checks y repeticiones, omisiones internas y los riesgos
pendientes. Requiere todos PASS, ausencia de defectos P0/P1 confirmados y
evidencia D 20/20 y protocolo 10/10. Si se satisface, aceptar D/E/F y W1 con
alcance técnico de desarrollo; no implica soporte de producción ni release.
GitHub Actions conserva su estado independiente. Dos SID, DELETE_CHILD real,
casos adversos de identidad/cancelación no reproducidos, proveedores reales
y SSH físico faltante siguen pendientes según QUALITY.md y LOCAL_CI.md.
No crear cuentas, no requerir otra PC Windows, no GPUI, no merge a main.
W2 se inicia después de aceptación W1; no se implementa en este corte.
