# Contratos de integración

`dicta-core/src/lib.rs` y `config.rs` son la fuente de verdad de DTOs. El padre es dueño del core y del ejecutable `dicta`. No modificar estos DTOs sin coordinar.

## UI, dueño Opus 5.5

`dicta_ui::run(initial: AppSnapshot, command_tx: std::sync::mpsc::Sender<Command>, snapshot_rx: std::sync::mpsc::Receiver<AppSnapshot>) -> anyhow::Result<()>`.

La UI corre en el hilo principal. Envía Command, recibe snapshots sin bloquear y conserva el último. Tiene overlay GPUI y ajustes. Cerrar ajustes no sale del proceso. Usar `initial.demo` para mostrar DEMO cuando corresponda. Nada de audio ni red desde la UI. Cambios de tema envían SetTheme y son persistidos por el controlador. Configuración editable envía UpdateConfig. Texto editado envía EditText. Comandos con secretos no se loguean.

## Proveedor, dueño worker provider

Proponer una API pública documentada en su crate para `stream(config, bearer, audio_rx, events_tx, cancel)`, donde `audio_rx` lleva Vec<u8> PCM16 mono16000, eventos Started/Partial/Final/Error, y cancel tiene cancelación distinta de cierre normal de audio. Puede definir sus tipos en su crate. El padre adaptará su controlador. DTOs de wire siguen contrato xAI, `transcript.created` antes de enviar, `audio.done` al cerrar captura. No compilar UI para validar su trabajo.

OAuth DeviceCode + refresh; keyring propio para API key/tokens. Client ID configurable: no inferir que el cliente público de otro programa está autorizado para distribución. No llamadas reales autenticadas ni lectura de archivos de credenciales ajenos. Modo API explícito, sin fallback automático.

## Plataforma, dueño worker platform

Proponer API pública para captura PipeWire a PCM16/16k/mono por canal acotado Tokio, AudioHandle que se detiene con drop/stop, nivel de audio y errores. Solo abrir captura por orden. API para enumerar dispositivos; clipboard y pegar en Hyprland; detección de destino/foco; tray y hotkeys pueden enviar `Command` por std::sync::mpsc Sender. Usar `dicta-core` para DTOs y nombres. No modificar configuración global de Hyprland automáticamente; generar instrucciones/candidatos por separado si portales no funcionan. No probar micrófono ni pegado real sin acción consciente del usuario.

## Builds

El Cargo.toml raíz y Cargo.lock son del padre. Los workers editan solo el manifiesto de su crate, agregando dependencias directamente. Ejecutar cargo test -p SU_CRATE para validar. No fijar toolchain ni ejecutar instalaciones globales. La UI elige una revisión de GPUI reproducible y reporta restricciones. Coordinar opciones en vez de revertir cambios ajenos.

## Cierre y visibilidad (integración final)

Cerrar Ajustes debe enviar `HideSettings`; el controlador responde con `show_settings=false`. Así una orden posterior `ShowSettings` desde IPC, launcher o bandeja vuelve a abrirlos. Ocultar overlay usa `HideOverlay`. Ninguna de estas órdenes detiene el proceso residente. Quit cancela sesión y login, libera recursos y cierra el canal de snapshots; GPUI termina cuando ese canal se desconecta.

Los callbacks de UI no ejecutan comandos de escritorio ni I/O de credenciales. La demo los simula en el controlador. Las operaciones de pegado toman el destino al recibir la orden y lo comprueban antes de inyectar; las operaciones en cola se invalidan al cancelar o editar.


## Lectura en voz alta (12-10-2026)

Los DTOs están en `dicta-core/src/tts.rs` (`TtsConfig`, `TtsSnapshot`, `VoiceSettings`, `CharTime`) y `readings.rs`. Los comandos nuevos de `Command` están documentados en `lib.rs`.

- **Proveedor**: `XaiProvider::tts_session(request, bearer, utterances_rx, events_tx, cancel)` emite `TtsEvent::{Ready, Audio, Marks, Done}`; las marcas son relativas a cada locución, en ms. También están `synthesize` (REST), `list_voices` y `Credentials::bearer_for(config, mode, …)`. Aplica el estilo y `replace`.
- **Plataforma**: `selection::read_text(source, max_bytes)` solo por orden explícita. `playback::Player` (reset/append/set_finished/play/pause/seek/set_rate/set_volume/status) y `enumerate_outputs`. `desktop::place_reader` para la clase `dicta-reader`.
- **UI**: ventana `dicta-reader` (`set_reader_placer`), sección Ajustes → Lectura, historial Dictados/Lecturas y métricas de lectura en Inicio. Solo envía `Command`; no hace audio, red ni portapapeles.
- **IPC**: `read`, `read-clipboard`, `tts-toggle`, `tts-stop`. Nunca transporta texto.

## Escritura (14-10-2026)

Los DTOs están en `dicta-core/src/writer.rs` (`WriterConfig`, `WriterSnapshot`, `WriterOutput`, `DiffSpan`) y los comandos `Command::Writer*` en `lib.rs`.

- **Proveedor**: `XaiProvider::rewrite(bearer, request, cancel) -> WriterResult`. Un pedido, sin reintentos.
- **Plataforma**: `selection::copy_selection_from(options) -> CopiedSelection { text, source }` (como `copy_selection`, más la ventana de origen) y `desktop::paste_into(text, target, keys, extra_terminal_classes, is_current)` para Reemplazar. `desktop::set_writer_placement_enabled` / `place_writer` para la clase `dicta-writer` (`FloatingWindow::Writer`).
- **UI**: ventana `dicta-writer` (`set_writer_placer`) y Ajustes → Escritura. Solo envía `Command`.
- **IPC**: `write`, `write-clipboard`. Nunca transporta texto.

## Plataformas (MP-01, 12-10-2026)

`dicta-platform` compila en Linux, Windows y macOS. Lo exclusivo de Linux (wl-clipboard, wtype, hyprctl, grupos de procesos y señales, ksni y ashpd; el crate no usa `/proc`) está detrás de `cfg(target_os = "linux")`. En el resto hay stubs que devuelven errores accionables en español («… todavía no está disponible en este sistema»). Las firmas públicas no cambiaron.

- **Capacidades**: `dicta_platform::detect() -> Capabilities` (también `capabilities::detect`). Es barato y no bloquea: lee variables de entorno (`HYPRLAND_INSTANCE_SIGNATURE`, `SWAYSOCK`, `WAYLAND_DISPLAY`, `XDG_CURRENT_DESKTOP`, `DISPLAY`) y busca programas en `PATH`. No lanza procesos ni usa D-Bus. Informa lo que soporta el código **actual**:
  - Campos: `session` (`hyprland`, `sway`, `kde_wayland`, `gnome_wayland`, `other_wayland`, `x11`, `windows`, `macos`, `unknown`), `clipboard`, `paste_injection`, `copy_selection`, `primary_selection`, `focus_tracking`, `window_placement`, `global_hotkeys` (`none`/`toggle`/`hold`), `tray`, `compositor_binds` y `missing_programs` (por ejemplo `wl-copy`).
  - Es serializable (serde, snake_case). Los campos que falten se completan como «no disponible».
  - Hyprland con wl-clipboard, wtype y hyprctl: todo. Otras sesiones Wayland: portapapeles y lectura de la selección, y solo alternar mediante un atajo personalizado que ejecute `dicta --toggle`. X11: bandeja y alternar. Windows: todo salvo la selección primaria y los binds de compositor (ver «Windows (MP-13)»). macOS: nada todavía.
- **Costuras**: `backend::current() -> &'static Backend`, con un trait por servicio: `Clipboard`, `FocusTracker`, `Injector` (`paste`, `prepare_paste`, `paste_prepared`), `SelectionReader` (`read_text`, `copy_selection`) y `WindowPlacer` (`FloatingWindow::{Overlay, Reader}`).
  - Las funciones libres de `desktop` y `selection` son envoltorios finos sobre `current()`; el controlador no cambia.
  - En Linux, el backend es el código Wayland/Hyprland de siempre (`desktop/hyprland.rs`, `selection/wl_paste.rs`, `selection/copy.rs`), con sus mismas comprobaciones y mensajes. En Windows, `backend/windows.rs` (ver «Windows (MP-13)»). En macOS, `backend/unsupported.rs`: falla antes de tocar nada y la colocación de ventanas no hace nada.
  - Las fases 1 a 3 sustituyen servicios por sesión sin tocar al controlador. Hay que mantener `capabilities.rs` sincronizado.
- **Atajos y bandeja**: `shortcuts::run` usa el portal en Linux; en el resto devuelve un error (Windows usa `shortcuts::Hotkeys`). `shortcuts::event_stamp` usa `CLOCK_MONOTONIC` en Linux y macOS y `QueryPerformanceCounter` en Windows; en los tres casos es comparable entre procesos del mismo arranque. `tray::spawn` devuelve un error en macOS, y `tray::TrayHandle` nombra el handle en todos los sistemas.
- **Pendiente para controlador/UI**: llamar a `detect()` al arrancar, y no ofrecer pegar, copiar la selección ni la colocación manual cuando la capacidad sea `false`. Si `missing_programs` no está vacío, mostrar qué instalar. Para mostrarlo en la UI hace falta un DTO en `dicta-core` (o en `AppSnapshot`), que es del padre.

### Windows (MP-13, 13-10-2026)

- **Backend** (`backend/windows.rs`): portapapeles con arboard; ventana activa con `GetForegroundWindow`, y `PasteTarget` con el HWND en hexadecimal como `address` y `stable_id`, el pid, la clase (`GetClassNameW`) y el ejecutable (`notepad.exe`) como `initial_class`. Las ventanas propias se reconocen por pid. Pegar espera hasta 1 s a que se suelten Ctrl/Alt/Shift/Win, comprueba el foco y envía `CTRL+V`, `CTRL+SHIFT+V` o `SHIFT+INSERT` con `SendInput`; nunca Enter. Volver a la ventana recordada usa `SetForegroundWindow` (mejor esfuerzo: Windows solo lo permite mientras Mars voice tiene el foco).
- **Copiar la selección**: guarda el portapapeles (texto o imagen), anota `GetClipboardSequenceNumber`, envía `Ctrl+C` (`Ctrl+Shift+C` a `CASCADIA_HOSTING_WINDOW_CLASS`, `ConsoleWindowClass`, `mintty`, `PuTTY`, algunos ejecutables de terminal y `terminal_classes`) y espera hasta 700 ms a que cambie el número. Sin cambio es `Ok(None)`: el portapapeles no se tocó y nunca se toma contenido viejo. Después restaura lo guardado si `restore_clipboard`, fuera del historial de Windows. No hay selección primaria.
- **Lógica probada en todos los sistemas**: `backend/win_flow.rs` (pegado, foco y copia contra un escritorio falso) y `shortcuts/keys.rs` (atajos como `Ctrl+Alt+V` → combinación, acción y `Command`).
- **Hilo de escritorio** (`shell_windows.rs`): un hilo con bucle `GetMessageW` es dueño de `global-hotkey` y `tray-icon`. Dictar envía `ShortcutPressed`/`ShortcutReleased`, así que alternar y mantener pasan por el controlador; pegar → `Paste`, leer → `ReadSelection`, pausa → `TtsToggle`, detener → `TtsStop`. Un atajo inválido u ocupado produce un aviso en español con su nombre. El controlador los registra siempre (sin `--portal-shortcuts`, salvo en la demo) y los vuelve a registrar al guardar Ajustes.
- **Bandeja**: icono del ejecutable (recurso 1, que incrusta `crates/dicta/build.rs` junto con la versión), clic → `ShowSettings`, menú *Abrir ajustes*, *Dictar / detener*, *Leer selección* y *Salir*.
- **Ejecutable**: en release es una app GUI (`windows_subsystem = "windows"`). `ctl`, `--help` y `--version` se conectan a la consola de la terminal que los lanzó; un error sin consola se muestra en un diálogo. Un segundo lanzamiento cede el foco (`AllowSetForegroundWindow`) y pide `settings` por el named pipe.
- **Micrófono**: cpal/WASAPI. `E_ACCESSDENIED` (privacidad) se traduce a un error que indica Configuración > Privacidad y seguridad > Micrófono.
- **Colocación**: no hace nada; GPUI abre el overlay y el lector donde lo pide la UI.

