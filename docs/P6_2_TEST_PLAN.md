# P6.2-A — plan de pruebas

## Pruebas automatizadas

`cargo test -p gentle-fleet-desktop --locked` cubre el modelo visual, la
conversión de tokens y las transiciones de foco/interacción de ventana:

1. Estado inicial en bienvenida, panel visible, hosts/sesiones vacíos y sin
   sesión iniciada.
2. Navegación a ajustes y a la vista informativa de sesión.
3. Abrir/cerrar panel.
4. Selección de paleta clara/oscura y persistencia del tema visual.
5. Navegación/panel no crean estado Fleet ni sesión.
6. Tab recorre Ajustes; Enter activa la acción enfocada y el foco se restaura en
   la página destino y al volver.

Las pruebas GPUI simulan KeyDown y KeyUp por separado: el helper
`simulate_keystrokes` solo genera KeyDown y el click de teclado de GPUI se activa
en KeyUp.

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

Reintento en la sesión Hyprland activa: monitor físico `eDP-1` encendido,
1920×1080. El binario actualizado se inspeccionó visible y apto para input.
Capturas en `/tmp/opencode/p62-67-final-welcome.png`,
`p62-73-settings-followup.png`, `p62-75-back-followup.png`,
`p62-82-light-latest.png` y `p62-80-resized.png` documentan bienvenida,
Ajustes, regreso, tema claro y layout redimensionado. Se verificó tema oscuro
antes del cambio a claro. «Abrir sesión» llegó a la pantalla informativa; no se
inició ningún agente.

El registro de trabajo conserva la limitación del intento con dispatcher
sintético; posteriormente el usuario confirmó PASS para el ciclo del panel y
para el cierre gráfico, que eran los dos criterios pendientes de P6.2-A. El
usuario también confirmó PASS del CI del último commit de A y después se verificó
[run 45195572977](https://github.com/javijec/gentle-fleet/actions/runs/45195572977)
en SHA `870c8ed`. Windows/macOS no se validaron. P6.2-A queda cerrada con esa
procedencia de evidencia, sin extender la afirmación a esas plataformas.

## Dependencias Linux para CI

Se deben instalar los headers de X11, XCB, Wayland, xkbcommon, fontconfig y
freetype, además de las dependencias de Vulkan/renderer del runner Ubuntu. Las
pruebas unitarias no necesitan Xvfb; un smoke GUI automatizado separado sí
necesitaría Xvfb o Weston y un dispositivo/render software disponible. No se
declara ese smoke automatizado hasta probarlo.
