# P7-W1-E — supervivencia y recuperación Windows

Estado: candidato de implementación solicitado por el propietario, UNTESTED.
W1-D tiene PASS reportado, no aceptación técnica independiente. La preparación
e implementación de E se adelanta por instrucción del propietario, sin cerrar D
ni eliminar su gate de estabilidad. No iniciar W1-F/W2 ni merge/release.

## Contrato

Tras terminación real de fleetd, el mismo runner continúa y mantiene su spool.
El nuevo daemon autentica identidad completa, boot_id, token y wire, recupera
el turno y persiste antes de ACK. El mismo prompt no se vuelve a enviar.
Si el runner desapareció, mantener NeedsReconciliation y aprobaciones
Uncertain; no crear sustituto. Una aprobación pendiente solo se resuelve por
decisión explícita y confirmación del driver.

En Windows la salida de un runner reattachado se acredita únicamente por
Shutdown tipado y salida exitosa del handle de proceso retenido desde el
named pipe autenticado (SYNCHRONIZE y QUERY_LIMITED_INFORMATION, sin permiso
de terminación). Se comprueba PID del endpoint y SID, liveness antes y después
de Hello. Nunca abrir un PID persistido para matar o inferir muerte. Unix
conserva StopUnconfirmed cuando no hay Child verificable. Los handles de
proveedores/descendientes reales no se certifican con FakeDriver.

## Pruebas

w1e-recovery.ps1 reutiliza la propiedad de procesos del fixture W1-D y agrega
tres escenarios: terminación de daemon con turno en curso, lectura SQLite
READ_ONLY del terminal no ACKeado mientras el daemon está ausente, replay
contiguo sin duplicados y parada confirmada tras reattach; pérdida del runner
y rechazo de prompt y aprobación; aprobación pendiente a través de reinicio
con decisión explícita e idempotente. Se comprueba que una sesión parada no
revive tras reiniciar. Fixture FakeDriver, no proveedor real.

El core persiste el estado del runner antes de activar ingress para evitar
sobrescribir Active posterior a un terminal con Recovering. Cuando falla
reattach se sincroniza el catálogo de aprobaciones inciertas con SQLite.
La identidad completa debe coincidir. Se conserva el gate D y se añade
w1e-recovery al full nativo Windows y workflow. Linux conserva las pruebas P3.

Pendientes: ejecución dirigida, inspección del spool, seguridad de identidad
adversa y carreras de conexión/ACK, validación de recuperación ante pérdida
de runner con daemon vivo, evidencia completa D, full Windows/Linux mismo
SHA limpio, protocolo 10/10, revisión de logs y cero P0/P1 confirmados.
No se declara PASS ni cierre; no se requiere otra cuenta ni otra PC Windows.

Se añade cuarto escenario RunnerLossLive: el daemon vivo observa la pérdida del
runner, la aprobación queda incierta antes y después del reinicio. La ruta
compartida marca y refresca aprobaciones antes de persistir runner Uncertain;
si falla almacenamiento mantiene el bloqueo de efectos y no recaptura su
propio mutex. El startup también reconcilia aprobaciones de registros Lost o
Uncertain históricos. El caso sigue UNTESTED. No se declara PASS de W1-D/E.
