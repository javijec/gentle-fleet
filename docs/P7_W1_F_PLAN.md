# P7-W1-F — aceptación integral nativa Windows

Estado vigente: gate automatizado final PASS, con informes originales revisados parcialmente en P7_W1_RESULTS.md; aceptación técnica integral pendiente. El siguiente texto describe el plan histórico. El propietario reportó PASS de D y E y de los gates
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


### W1-F — regresión intermitente SQLite SHM (26/11/2026)

El operador aportó `reports/both/96d9576a21f94221a28348d7137f68b0/lifecycle-19.log`: tras completar ambos turnos, timeline falló con `insecure_store_file: SQLite -shm validation: Fleet data file must be a regular, unlinked file`. El gate W1-F no se acepta: se alcanzó la repetición 19, no 20/20. HEAD observado `322a328`, SHA ejecutado pendiente de informe. La condición anterior mezclaba directorio/reparse y todo número de enlaces distinto de uno. Hipótesis principal: una conexión SQLite cerró y eliminó su sidecar mientras el handle de metadata seguía vivo. No se dispone del contador de enlaces original ni reproducción sincronizada del fallo exacto.

Corrección candidata: separar atributos inseguros y contador; cero enlaces únicamente en WAL/SHM/journal provoca ERROR_DELETE_PENDING para reutilizar el retry acotado existente (10 reintentos de 10 ms). Se vuelve a abrir y validar por nombre, incluidos propietario, DACL y exactamente un enlace; la desaparición real del sidecar sigue siendo normal. Cero enlaces persistente falla; base principal, journals SDK y demás archivos no admiten este retry nuevo. Directorios, reparse points y múltiples enlaces se rechazan inmediatamente. Se conservan checks ACL y límites, sin tocar SQLite ni datos del usuario. Regresión determinista de clasificación/retry y reemplazo por hardlink; se conserva churn concurrente existente. Estado UNTESTED: ejecutar tests de protocolo/store Windows y gate W1-F completo sobre nuevo SHA limpio. No se declara resuelto el fallo original hasta evidencia; W1-C mantiene su aceptación histórica pero la regresión bloquea W1-F. No se modifica main ni se inicia W2.


### W1-F — gate final PASS reportado (26/11/2026)

Tras la corrección de la carrera de metadata SHM en `c464520`, el propietario confirmó «paso todo» en respuesta al comando del gate `both.py --mode w1f --auto-format` Windows/Linux. HEAD remoto observado: `8662aea23cb16744bbf2a9714494d1682dcc4f22`. Se registra PASS integral reportado, incluido el gate que exige protocolo 10/10 y Lifecycle 20/20; no se recibieron los tres JSON originales ni logs que permitan verificar independientemente SHA, conteos y ejecuciones. No se afirma reproducción exacta de la carrera previa. No repetir pruebas válidas solo por este commit documental. El trabajo de implementación W1-A–F queda completado para desarrollo; la aceptación técnica formal de W1 requiere la revisión de evidencia ya generada conforme al plan W1-F, sin omitir defectos P0/P1 confirmados. Los requisitos de release, pruebas reales de proveedores, dos SID, identidad adversa y SSH físico no se dan por validados. Próximo corte: W2 tras esa revisión. Main y workflows sin cambios; no se declara release ni PASS de Actions.
