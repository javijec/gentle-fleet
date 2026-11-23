# P7-W0: resultados Windows nativo

**Estado: ABIERTO.** Base `427a5c1`; fecha 23/11/2026. Consultar
[matriz](P7_WINDOWS_MATRIX.md) y [auditoría](P7_W0_AUDIT.md).

## Entorno

- Windows 11 Home Single Language 10.0.26300 x64; AMD Ryzen 5 5600X.
- PowerShell 7.7.0; Git 2.45.1.windows.1.
- `rustc 1.98.1 (48a229cea 2026-09-01)`, LLVM 22.1.8;
  `cargo 1.98.1 (797e8a9bc 2026-08-05)`; stable y target
  `x86_64-pc-windows-msvc`; rustfmt y clippy instalados.
- Build Tools 2022: `VC/Tools/MSVC/14.44.35207`, compilador x64 encontrado;
  Windows SDK incluye 10.0.26100.0. `cl` no está en PATH; Cargo compiló y
  enlazó SQLite bundled en una revisión previa. `vswhere -all` devolvió `[]`;
  no se ha verificado la instalación mediante su inventario.
- OpenSSH client 9.5p2, `sshd.exe` presente, servicio sshd activo. La consulta
  de capacidades Windows requirió elevación, no solicitada. Presencia no prueba
  claves, `known_hosts` ni conectividad bidireccional.
- `rusqlite` usa SQLite bundled. GPUI 0.2.2: dependencias y backend Windows en
  evaluación; ninguna prueba de renderer o ventana validada aún.
- No se instalaron herramientas globales ni se modificó PATH.

## Línea base y CI

- `cargo fmt --all -- --check`: PASS sobre `427a5c1`.
- `cargo check --workspace --locked`: PASS para los 18 crates, incluido GPUI.
  Esto no valida el enlace ni la ejecución. Advertencia de incompatibilidad
  futura: `proc-macro-error2 v2.0.1`.
- `cargo build --workspace --locked`: PASS, incluido enlace GPUI; no se abrió
  ninguna ventana.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: PASS.
- Primera ejecución `cargo test --workspace --locked`: FAIL en
  `fleet_ssh::tests::hello_is_rewritten_with_remote_token` con
  `Protocol("remote token unavailable: El sistema no puede encontrar la ruta especificada. (os error 3)")`.
  Los tests anteriores al fallo pasaron; los posteriores no se ejecutaron.
  El probe real opt-in Gentle Shell se ignoró por requerir instalación real.
  Adaptación W0: el test usa un token inyectado y aislado y verifica también
  error de lectura; el bridge real sigue usando `expected_token()`. Tras la
  adaptación, `cargo test --workspace --locked`: PASS. Se ignoraron dos probes
  ya opt-in: Gentle Shell real y GPUI con fixture Linux `fleetd/sshd`.
  Repetición de Clippy tras la adaptación: PASS.
- Tras la adaptación SSH en `e31efcb` también se repitieron fmt, check y
  build completos: PASS.
- Linux en base exacta `427a5c1`: [run 45220359567](https://github.com/javijec/gentle-fleet/actions/runs/45220359567)
  PASS en jobs principal y `desktop-gpui-linux`; no es evidencia del commit W0.
- Se agregó job `windows-native` con los cinco comandos del workspace y pasos
  diagnósticos independientes. En el
  [run 45233915713](https://github.com/javijec/gentle-fleet/actions/runs/45233915713)
  sobre `5183d32`, Linux `check` PASS; Windows y Linux GPUI seguían en curso al
  consultar. El run de `e31efcb` también estaba en curso; no se presupone el
  resultado final.
- Los scripts Bash de proceso existentes corren en Linux CI; no se han portado
  a Windows ni desactivado para simular éxito. Los tests `cfg(unix)` y el
  probe real opt-in no prueban Windows.

## Exploración funcional y límites

Con binarios de la base, directorio temporal y DB aislados, `FLEET_RUNNER_BIN`
apuntando explícitamente a `fleet-runner.exe`:

1. Arranque de `fleetd.exe`: PASS; conexión de `fleetctl.exe` y descubrimiento
   de FakeDriver 0.1.0 Available: PASS.
2. Registro del workspace temporal: PASS; ruta devuelta en formato `\\?\C:\...`.
   Creación de sesión FakeDriver: PASS, `Running / Idle`.
3. Prompt, timeline y stop: FAIL, cada uno devolvió
   `Unauthorized: local token does not match`. El runner cambia `fleetd.token`
   al arrancar, pero el daemon conserva el token anterior. No se probó cierre
   ordenado de sesión; se terminaron solo los PIDs del daemon y runner de esta
   prueba y se conservaron los archivos temporales para diagnóstico.
4. El launcher por defecto sin `FLEET_RUNNER_BIN`, rutas con espacios, crash y
   reconexión siguen UNTESTED. El descubrimiento listó Codex 0.170.0 como
   Unavailable; ningún proveedor real fue ejecutado.

Sin pruebas de ventana, renderer, SSH hacia servidor Windows ni ACL multiusuario.
No se afirma equivalencia entre loopback/token y named pipe/DACL. W0 sigue abierto.
