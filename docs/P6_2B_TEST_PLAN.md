# P6.2-B — plan de pruebas

## Automatizadas

### Contrato y adapter

- `fleet-client::configured_hosts` proyecta solo alias ordenados; destino, ruta
  del bridge, archivo de identidad y estado SSH no se filtran.
- `MachineId` distingue Local/SSH; `SessionIdentity` combina host + session ID.
- Clasificación de clave SSH rechazada no filtra el diagnóstico privado.
- La cancelación es compartida entre vistas/trabajador y se consulta entre
  operaciones SDK.
- La generación y el ID de máquina descartan respuestas de selección obsoleta.
- Prueba GPUI de render/selección de host SSH sin configuración; verifica que el
  evento vuelve sin bloquear y no declara una conexión exitosa.

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
```

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
