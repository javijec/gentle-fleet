# P6.2-B — plan de pruebas

## Automatizadas

### Contrato y adapter

- `fleet-client::configured_hosts` proyecta solo alias ordenados; destino, ruta
  del bridge, archivo de identidad y estado SSH no se filtran.
- `MachineId` distingue Local/SSH; `SessionIdentity` combina host + session ID.
- Clasificación de clave SSH rechazada no filtra el diagnóstico privado.
- La cancelación es compartida entre vistas/trabajador y se consulta entre
  operaciones SDK.
- Una operación bloqueada respeta el deadline de respuesta de GPUI, **sin liberar** el permiso mientras el hilo SDK siga activo.
- Dos llamadas bloqueadas tras timeout/cancelación mantienen ocupados los dos permisos: 100 reintentos son rechazados sin crear transportes adicionales.
- Al terminar realmente ambos hilos, los dos permisos vuelven a estar disponibles.
- Una operación ya cancelada no inicia el trabajo SDK ni consume un permiso.
- Separar explícitamente límite de espera de UI y límite efectivo del transporte: el timeout del primero no implica que el segundo haya terminado.
- La generación y el ID de máquina descartan respuestas de selección obsoleta.
- Desconectar o fallar elimina el snapshot anterior, y el detalle no muestra
  datos de una fila que no está `Connected`.
- Prueba GPUI de render/selección de host SSH sin configuración; verifica que el
  evento vuelve sin bloquear y no declara una conexión exitosa.
- Prueba GPUI de «Volver al inicio» con una máquina seleccionada: Welcome se
  muestra y la selección se limpia.
- Claves de elementos estables derivadas de host/sesión, independientes de la
  posición en la lista.

### Validaciones de regresión

```sh
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --locked
tests/e2e_p51_ssh.sh
tests/e2e_p52_two_hosts.sh
tests/e2e_p61_sdk.sh
tests/e2e_p62b_desktop_gpui.sh
```

`tests/e2e_p62b_desktop_gpui.sh` inicia explícitamente un `fleetd` local aislado y
otro remoto tras un `sshd` de fixture; después ejecuta la prueba GPUI opt-in que
atraviesa catálogo, conexión, consulta, refresco, desconexión, reintento y SSH.
Los E2E P5/P6.1 son deterministas con SSH local/FakeDriver y no equivalen a un
smoke contra una segunda máquina física ni contra proveedor pago.

## Manual Omarchy / Hyprland

1. Verificar monitor físico activo y ventana Desktop visible/acepta input.
2. Confirmar solo lectura del catálogo en arranque, con Local y alias SSH reales.
3. Conectar Local y confirmar proveedores/workspaces/sesiones reales.
4. Conectar manualmente un alias SSH conocido; revisar estados seguros para
   host-key/auth/bridge/daemon y probar refresh, cancelación y cambio de host.
5. Elegir sesión y confirmar aislamiento del contexto por máquina.
6. Cerrar Desktop y comprobar que Fleet no recibe `StopSession` ni termina
   procesos preexistentes.
7. Guardar capturas de catálogo, éxito local/remoto y error de host no disponible.

No iniciar `fleetd` ni crear datos falsos para hacer pasar un smoke: si el
servicio o alias remoto no existe en el entorno, registrar el bloqueo y usar los
E2E deterministas solo para la parte de transporte que cubren.
