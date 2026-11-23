# P6.2-A — plan de pruebas

## Pruebas automatizadas

`cargo test -p gentle-fleet-desktop --locked` cubre el modelo visual y la
conversión de tokens de color sin necesitar servidor gráfico:

1. Estado inicial en bienvenida, panel visible, hosts/sesiones vacíos y sin
   sesión iniciada.
2. Navegación a ajustes y a la vista informativa de sesión.
3. Abrir/cerrar panel.
4. Selección de paleta clara/oscura y persistencia del tema visual.
5. Navegación/panel no crean estado Fleet ni sesión.

Las pruebas de estado deliberadamente no afirman interacciones gráficas. GPUI
0.2.2 implementa la activación Enter/Espacio en elementos enfocados, pero todavía
no hay una prueba de ventana fiable en este proyecto.

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

Intento previo: Hyprland reportó la ventana GPUI mapeada, visible y con input
habilitado, pero la captura resultó transparente y no sirvió como evidencia.
Reintento en la sesión activa: la ventana se mapeó y se inspeccionó una captura
opaca de 1920×1080; se distinguieron bienvenida, panel vacío y tema oscuro. Se
enviaron Tab/Return con `wtype`, pero no se observó una transición de pantalla.
No se completaron las comprobaciones de controles, foco/teclado, redimensionado
ni cierre; el smoke sigue incompleto.

## Dependencias Linux para CI

Se deben instalar los headers de X11, XCB, Wayland, xkbcommon, fontconfig y
freetype, además de las dependencias de Vulkan/renderer del runner Ubuntu. Las
pruebas unitarias no necesitan Xvfb; un smoke GUI automatizado separado sí
necesitaría Xvfb o Weston y un dispositivo/render software disponible. No se
declara ese smoke automatizado hasta probarlo.
