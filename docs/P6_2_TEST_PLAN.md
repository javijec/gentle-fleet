# P6.2-A — plan de pruebas

## Pruebas automatizadas

`cargo test -p gentle-fleet-desktop` cubre el modelo visual sin necesitar servidor
gráfico:

1. Estado inicial en bienvenida, panel visible, hosts/sesiones vacíos y sin
   sesión iniciada.
2. Navegación a ajustes y a la vista informativa de sesión.
3. Abrir/cerrar panel.
4. Selección de paleta clara/oscura y persistencia del tema visual.
5. Navegación/panel no crean estado Fleet ni sesión.

Las pruebas de estado deliberadamente no afirman interacciones gráficas: cuando
la API de prueba de GPUI 0.2.2 y el entorno de CI se validen, se agregarán pruebas
de ventana/eventos con `gpui::test`.

## Validaciones de build

Ejecutar desde la raíz:

```sh
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --locked
```

Estas validaciones compilan la UI pero no requieren abrir una ventana. La suite
del backend debe seguir ejecutándose sin un display server.

## Smoke gráfico manual Linux

1. Ejecutar `cargo run -p gentle-fleet-desktop`.
2. Confirmar ventana nativa redimensionable y título/identidad.
3. Ocultar/mostrar panel; abrir ajustes; alternar claro/oscuro; volver.
4. Pulsar «Abrir sesión» y confirmar mensaje informativo, no agente.
5. Cerrar Desktop y confirmar que termina sin panic.

Registrar el resultado con sesión de pantalla/servidor gráfico y evidencia
observada. La existencia de `DISPLAY` por sí sola no constituye un smoke.

Intento de este corte: Hyprland reportó la ventana GPUI mapeada, visible y con
input habilitado. `grim` creó una imagen, pero todos sus píxeles son transparentes;
por eso no se pudo inspeccionar el contenido renderizado. No se verificaron las
interacciones manualmente y ambas cosas siguen pendientes.

## Dependencias Linux para CI

Se deben instalar los headers de X11, XCB, Wayland, xkbcommon, fontconfig y
freetype, además de las dependencias de Vulkan/renderer del runner Ubuntu. Las
pruebas unitarias no necesitan Xvfb; un smoke GUI automatizado separado sí
necesitaría Xvfb o Weston y un dispositivo/render software disponible. No se
declara ese smoke automatizado hasta probarlo.
