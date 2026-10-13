# UI nativa GPUI

Dueño: Opus 5.5. Código en `crates/dicta-ui/`. Capturas en `design/native/screens/`.

## Versión de GPUI

GPUI se fija por revisión de zed: `f16f9652ec57bf806e65b2a0d51bb92a63644914` (main del 09-10-2026), crates `gpui`, `gpui_platform` y, solo para la galería, `gpui_wgpu`. En crates.io solo existe gpui 0.2.2, de octubre de 2025, sin `WindowKind::LayerShell`. Por eso se usa git. La revisión compila con rustc 1.98.1 del sistema, sin toolchain propio. Cargo trae el repositorio de zed una vez a su caché.

## Contrato

`dicta_ui::run(initial, command_tx, snapshot_rx)` corre en el hilo principal. Un hilo `dicta-ui-snapshots` bloquea en el `Receiver` y reenvía a un canal async, así que la UI no hace polling. Si llegan snapshots en ráfaga, se aplican todos en orden dentro de una actualización de GPUI, conservando los niveles para la onda y los flancos de visibilidad de Ajustes. Cuando el emisor se cierra, la UI sale. La UI no abre audio ni red y no lee credenciales. Las claves de API viajan solo en `SaveApiKey` y el campo se vacía al guardar.

`QuitMode::Explicit`: en Linux GPUI sale por defecto al cerrar la última ventana. Dicta sigue vivo sin ventanas. Solo salen `Quit` (botón "Salir de Dicta" o Ctrl+Q) y el cierre del canal.

| Acción | Command |
|---|---|
| Dictar, Terminar, Cancelar, Nuevo dictado | `Start`, `Stop`, `Cancel`, `Start` |
| Copiar, Pegar (overlay en Listo) | `EditText` si hay cambios pendientes, luego `Copy` / `Paste` |
| Editar texto final | `EditText` con Ctrl+Enter, al salir del campo o antes de copiar o pegar |
| Estilo | `SetTheme`; se aplica al instante y el controlador lo persiste |
| Cualquier ajuste | `UpdateConfig`, validado antes con `AppConfig::validate` |
| Conectar, cancelar o desconectar | `StartLogin`, `CancelLogin`, `Disconnect` |
| Clave de API | `SaveApiKey` |
| Corrección propuesta o manual | `RememberCorrection`; "Olvidar" envía `ForgetCorrection` |
| Micrófono | `UpdateConfig(microphone)` y `RefreshDevices` |
| Cerrar ajustes (×, Ctrl+W, compositor) | `HideSettings` |
| Ocultar overlay (×) | `HideOverlay` |

## Estilos

Señal, Regolito, Noche y Órbita son paletas completas en `theme.rs`, con los tokens de `design/reference/BRANDING.md`. En Ajustes → General, cada estilo tiene una tarjeta con su vista previa real.

- Señal usa una onda continua en tinta.
- Regolito usa columnas de puntos, con acento naranja en los picos, y el planeta con grano asomando en la esquina.
- Noche usa un panel carbón, un control de micrófono verde pálido y la onda en verde pálido.

El naranja aparece solo en el planeta, en Terminar, en el punto de grabación y en los controles activos.

La interfaz usa Archivo de ancho normal para cuerpo y controles; Archivo SemiExpanded queda para títulos y marca. IBM Plex Mono se reserva para atajos y cronómetro. Las fuentes están embebidas con licencia OFL en `crates/dicta-ui/assets/fonts`. Los iconos propios de Dicta se sirven por `AssetSource` y se tiñen según el tema con `svg()`. Planetas, marcas y mosaicos de grano son SVG a color embebidos que se dibujan con `img()`. La barra lateral agrupa Dictado, Texto y Aplicación e incluye estado del proveedor y modo de atajo.

## Movimiento

No hay timers. La onda se redibuja solo cuando llega un snapshot, y el controlador los envía con frecuencia solo mientras graba. El cursor del campo no parpadea. Movimiento reducido se activa con `DICTA_REDUCE_MOTION=1` o con `org.gnome.desktop.interface enable-animations=false`, y se pasa a `App::set_reduce_motion`. En ese modo la onda se reemplaza por un medidor estático.

## Overlay y posición

Desde la revisión UX del 09-10-2026 el overlay es una ventana flotante nativa, `WindowKind::Floating`, con app_id `dicta-overlay`. En Wayland usa xdg_toplevel y el arrastre del compositor desde la cabecera. En reposo se puede arrastrar desde la tarjeta. Los botones quedan fuera del área de arrastre.

Abre centrado por defecto (`overlay_centered=true`) mediante las reglas de `packaging/hyprland.lua.example`, instaladas en esta PC. Las reglas solo afectan `dicta-overlay`: flotante, centrada al abrir, sin foco inicial, sin blur/sombra/borde del compositor y excluida de HyprGlass. El arrastre se conserva entre estados y al actualizar la transcripción.

Hyprland 0.56.2 ignora el resize solicitado por el cliente. En Wayland se conserva una ventana de 404×318, sin margen de sombra. La tarjeta cambia de alto y la región de entrada excluye el resto transparente. X11 permite redimensionar la ventana. La posición arrastrada dura mientras esa ventana siga abierta; al reabrir se aplica la configuración.

Ajustes permite Centro, presets y X/Y. Elegir un offset desactiva el centrado. El callback `set_overlay_placer` se conecta al adaptador de Hyprland y coloca solo la ventana propia, identificada por PID, clase y stableId. Espera el mapeo fuera del hilo de UI y cancela solicitudes obsoletas. Los offsets negativos se resuelven en la UI contra el monitor principal; en otra pantalla de tamaño diferente el adaptador limita el resultado al área visible, por lo que esos presets pueden quedar aproximados. El centro predeterminado usa el monitor donde abre el compositor.

El dictado live ajusta el texto al ancho y sigue el final, salvo si se desplaza hacia arriba para releer. En Listo el editor tiene scroll propio, y cursor/selección/IME usan ese desplazamiento. Los controles permanecen fuera del área de texto. La escala visual de audio usa dBFS, con silencio plano; no modifica el PCM enviado.

Atajo predeterminado: `Ctrl+Space`. Pegar conserva `Super+Shift+V`. Al pegar desde el overlay se restaura el destino externo del dictado si sigue vivo; desde otra app se usa esa app. No se envía Enter.

Ver [revisión de Opus](reviews/overlay-ux-v2.md) y [prueba nativa e integración](tickets/11-overlay-ux.md).

## Correcciones

Al editar un dictado, `corrections::propose` compara palabra por palabra el original y el editado (LCS). Propone solo sustituciones de hasta 4 palabras, sin puntuación, y nunca inserciones o borrados puros. Muestra como máximo dos por dictado, cada una con "Recordar" y ×. No se aprende nada sin pulsar "Recordar".

## Pruebas

```
cargo fmt -p dicta-ui --check
cargo test -p dicta-ui                       # 31 pruebas
cargo build -p dicta-ui && cargo build -p dicta
cargo run -p dicta-ui --example gallery --features screenshots   # PNG con el renderer wgpu real, incluidos dictados largos
```

La galería usa `HeadlessAppContext` y `WgpuHeadlessRenderer` con datos DEMO. No abre ventanas en el escritorio. Encontró un NaN real en la onda (`sin(π)` negativo y raíz), ya corregido.

Pruebas posteriores en Hyprland 0.56.2 con una fixture DEMO nativa de 80 párrafos: apertura flotante y centrada sin robar foco, arrastre de 150×90 px, clic real en Terminar sin mover la ventana, scroll del editor de Listo y controles visibles. Capturas en `docs/screens/overlay-ux-v2-*.png`. No usan micrófono, red, llavero ni portapapeles real.

La galería incluye textos largos en Grabando, Puliendo y Listo, selección de varias filas y los tres temas. Quedan fuera de esa validación IME real y compositores distintos de Hyprland.


## Regolito y Órbita · 09-10-2026

Regolito conserva los puntos, ahora con altura fraccional y margen de volumen. El moteado se suaviza solo en su fondo. Órbita es un cuarto estilo GPUI independiente: Marte queda fijo y el polvo responde a los niveles de voz; sin voz termina por detenerse. El selector muestra los cuatro estilos en dos filas. Ver `docs/reviews/regolito-orbit.md` para decisiones y pruebas. No se cambió el tema guardado al instalar.


## Idiomas y plataformas · Fase 0 (12-10-2026)

- **Idiomas**: la UI es bilingüe (español/inglés) con un catálogo propio en tiempo de compilación (`src/i18n.rs`, 536 mensajes). `ui_language = auto` sigue al sistema; una config anterior a la traducción conserva el español.
- **Ventanas**: `PopUp` en macOS y Windows, `Floating` en Linux. Fuera de Wayland, la ventana mide lo mismo que la tarjeta. En Windows, la cabecera es zona de arrastre nativa. Ver `docs/PLAN-MULTIPLATAFORMA.md` §8.

## Lectura en voz alta · 12-10-2026

Diseño e implementación de Opus 5.5 (D-30).

- **Ventana de lectura**: `dicta-reader` (`src/reader.rs`), `WindowKind::Floating`, de 440 px de ancho. Abre sin foco y se arrastra desde la cabecera, igual que el overlay. `set_reader_placer` usa `config.tts.window_*`. La región de entrada se limita a la tarjeta.
- **Texto**: va en una vista cacheada que solo se redibuja cuando cambia la palabra. Resalta la frase y la palabra, sigue la lectura y, si te desplazás, muestra el chip «Seguir la lectura». Un clic en una palabra envía `TtsSeekText`.
- **Controles**: transporte (frases, ±s, play/pausa), barra con tramos generados, velocidad, voz y versiones, Regenerar, copiar, guardar WAV (diálogo GPUI), Ajustes y coste estimado. Hay modo compacto. Con movimiento reducido, un medidor estático reemplaza la onda.
- **Teclado** con la ventana enfocada: Espacio, ←/→, Mayús+←/→, `[`/`]` y Esc.
- **Ajustes → Lectura** (`src/lectura.rs`): cada campo de `TtsConfig`, más «Leer ahora» (portapapeles o texto pegado). Las voces se eligen en una grilla de chips, con «Probar voz».
- **Historial**: interruptor Dictados/Lecturas y detalle de lectura (Escuchar, Copiar, Borrar audio, Borrar). Las notas de dictado tienen «Leer en voz alta». **Inicio** suma el grupo «Lectura en voz alta».
- **Rendimiento**: con 20 000 caracteres y snapshots a 30 Hz, el lector cuesta ≈0,44 ms por cuadro y la ventana principal no se redibuja.
- **Galería**: `design/native/screens/lectura/`.
- **Límites conocidos**: el título del diálogo de guardado es el de GPUI en inglés; `[`/`]` pueden requerir AltGr en teclado español; un párrafo enorme es un solo bloque de texto.
