# Arquitectura de Dicta

Un proceso residente, implementado en Rust. GPUI ocupa el hilo principal; el controlador posee configuración, estado de sesión y diccionario. Tokio ejecuta I/O de red y servicios de escritorio. PipeWire tiene un hilo de captura, abierto únicamente al iniciar un dictado con credenciales y conexión listas.

## Módulos

- `dicta-core`: configuración, estados, ensamblado STT, reglas y SQLite.
- `dicta-provider`: protocolo xAI, OAuth de dispositivo/refresh, Secret Service y postproceso optativo.
- `dicta-platform`: PipeWire, portapapeles, pegado, bandeja y portal de atajos.
- `dicta-ui`: overlay y ajustes GPUI. Diseño de Opus 5.5; fuentes y assets locales.
- `dicta`: controlador, lifecycle, CLI e IPC local. Sirve de único punto de integración.

## Flujo

Atajo → orden IPC/portal → credencial del modo seleccionado → conexión STT → micrófono PCM16/16 kHz mono → parciales GPUI. Detener cierra la captura y drena la cola; espera confirmación final de xAI. Aplica reglas locales, y Grok opcional. Muestra el texto y lo copia por defecto. Pegar requiere una acción explícita y una ventana externa activa; no envía Enter.

Cancelar termina audio/red y descarta eventos tardíos por identificador de sesión. Una caída de conexión produce error y conserva los fragmentos visibles, sin presentar un dictado incompleto como éxito. No se repite audio ni se cambia a API paga al agotarse la suscripción.

## Decisiones

1. **GPUI nativo**: sin navegador/WebView embebido. Tres temas seleccionables, mismos controles.
2. **Wayland/Hyprland primero**: portapapeles wl-copy; pegado wtype con validación de foco. X11 pendiente aunque GPUI pueda abrir una ventana allí. La ventana bajo el puntero no equivale al campo enfocado: el destino debe tener foco.
3. **Un proceso y módulos separados**: evita mantener IPC entre daemon y UI. El socket solo acepta órdenes locales de la sesión del usuario, sin secretos ni transcripciones.
4. **Suscripción como preferencia, acceso por verificar**: El cliente OAuth público de compatibilidad viene incorporado; el flujo es Rust propio, sin dependencias externas, sin IDs manuales ni importación de tokens ajenos. STT y cobertura de cuota requieren prueba de la cuenta autorizada. Credencial guardada no equivale a cobertura confirmada.
5. **Memoria explícita y local**: reglas aprobadas, editables; sin keylogger, lectura del clipboard ni observación de correcciones en otras apps. Excepción explícita (lectura en voz alta, 12-10-2026): la selección primaria o el portapapeles se leen **una vez y solo** al pulsar el atajo o un botón de lectura. No hay sondeo ni monitoreo, y el contenido no se registra ni viaja por el IPC. Desde el 12-10-2026 el modo predeterminado copia la selección. Al pulsar el atajo, envía `send_shortcut` Ctrl+C a la ventana activa, identificada por `stableid`; si es una terminal, envía Ctrl+Shift+C. Antes guarda y vacía el portapapeles, y después lo restaura. Nunca toma las ventanas propias, y un portapapeles sin cambios nunca se confunde con la selección. Hace falta cuando el compositor desactiva la selección primaria (por ejemplo Hyprland con `middle_click_paste = false`).
6. **Corrección local primero**: Grok optativo al final; no bloquea parciales y el original queda disponible. Sin retries cobrados automáticos.
7. **Portales por elección**: `--portal-shortcuts` solicita registro. Fragmento manual Hyprland alternativo; no usar ambos para el mismo atajo ni duplicar los binds registrados.
8. **Demo aislada**: socket separado, configuración/reglas en memoria, sin micrófono, llavero, API, bandeja o portapapeles real.

## Límites del primer corte

La interfaz y transportes se prueban con ejemplos y servidores locales. Hace falta una prueba consciente con voz/cuenta para medir latencia, calidad rioplatense, permiso OAuth y cuota. Los protocolos Wayland no ofrecen una operación atómica verificar foco + inyectar pegado: cambiar de foco justo en ese instante aún puede cambiar el destino.

El acceso directo y el servicio systemd son opcionales. No se instalan ni activan en el desarrollo. Las dependencias y la revisión GPUI están fijadas por Cargo.lock y el manifiesto UI. El overlay es un toplevel flotante arrastrable y centrado al abrir. Hyprland usa reglas exclusivas para Dicta y mantiene una superficie de altura fija con región de entrada limitada a la tarjeta. El texto live y el editor final tienen áreas de scroll que conservan los controles visibles. Ver `UI-NATIVE.md` para restricciones específicas del overlay.


## Mars voice: biblioteca local y apariencia

El nombre público pasa a Mars voice; el workspace, app_id, rutas XDG y secretos propios conservan los identificadores Dicta. `Theme` describe Señal, Regolito u Órbita y `ColorMode` describe claro/oscuro. La variante Night antigua solo sirve para migrar a Señal/oscuro.

El controlador mantiene la autoridad sobre `Store` SQLite, captura, archivado, playback y comandos UI. `SessionArtifacts` transporta duración PCM, audio encolado, tokens realmente devueltos por el pulidor y basename WAV; no incluye un saldo Grok. Las notas guardan proveedor/configuración efectiva de la sesión y estado completado/fallido. Texto cancelado no se persiste.

El archivo WAV se produce en un worker con cola limitada, fuera de callback PipeWire. Si el almacenamiento falla, se conserva STT y se informa. Audio deshabilitado no crea el archivo. El audio habilitado necesita historial; cancelar o apagar el guardado antes de terminar descarta el archivo temporal. Si el envío final gana la carrera a Cancel/Quit, el controlador elimina el artefacto del evento obsoleto.

Retención y borrado eliminan primero el archivo y después la referencia SQLite; un fallo de disco conserva una referencia reintentable. Borrar una nota elimina su audio. Borrar solo audio mantiene el texto. La retención actúa al abrir y cada minuto con la app viva; no se ejecuta un servicio externo si la app está cerrada. Apagar audio conserva grabaciones anteriores.

La reproducción usa `pw-play` bajo demanda, con descriptor de un fichero regular abierto sin seguir enlaces. No abre shell ni ejecuta una ruta provista libremente por UI; el controlador resuelve el ID de nota en la DB. Se detiene antes de dictar, al borrar el audio y al salir.

Dashboard: métricas del historial conservado, Hoy según medianoche local, Semana móvil7d o Todo. Coste equivalente STT usa tarifa documentadaUS$0.20/h sobrePCMencolado alproveedor; no implica bytesfacturados ni incluye pulido. Tokens desconocidos permanecenNone. No se inventa saldo de suscripción. Historial usa búsquedaSQLite, carga progresiva y detalle independiente del filtro.

Audio-reactividad: `level_seq` solo cambia con muestras nuevas; la UI no agrega muestras cuando llegan palabras o cambia un ajuste. Interpolación visual no modifica el audio enviado.


## Lectura en voz alta (TTS)

`crates/dicta/src/reader.rs` vive en el hilo del controlador. Los comandos de lectura los atiende `Reader::handle`, y `Reader::poll` corre en cada vuelta del bucle.

- **Texto**: el core limpia la selección, la divide en frases y agrupa las frases en fragmentos (`dicta_core::tts`). El primer fragmento es una sola frase, para que el audio empiece rápido.
- **Generación**: una sesión WebSocket xAI por ráfaga. Cada fragmento es una locución (`text.delta` + `text.done`) y solo se envían los que caen dentro de la ventana de anticipación. Sin trabajo pendiente, el socket se cierra.
- **Respaldo REST**: si el upgrade falla antes de enviar texto, por red o protocolo, la versión pasa a REST fragmento a fragmento. Credencial, 403 y 429 no cambian de transporte ni de modo.
- **Caché y versiones**: audio por versión (voz y ajustes) y por fragmento. Regenerar crea otra versión desde el fragmento actual; volver a una anterior es gratis.
- **Reproducción**: el reproductor PipeWire (`dicta_platform::playback`) recibe un tramo contiguo de fragmentos y el fragmento en curso por streaming. Seek dentro del tramo es directo; fuera de él se recarga desde el fragmento destino, que se genera si falta. La demo usa una salida virtual.
- **Resaltado**: las marcas por carácter de xAI se alinean con el texto mostrado (tolerando acentos y reemplazos) para obtener tiempos por palabra.
- **Cancelación**: cada lectura, versión y sesión tiene su identificador. Detener o reemplazar cancela la sesión y descarta los eventos tardíos.
- **Historial**: tabla `readings`, con caracteres enviados como métrica de coste estimado. El audio es optativo: un WAV privado del prefijo generado, con la distribución por fragmento para reproducirlo sin regenerar.
- **Exclusión**: dictar pausa o detiene la lectura, según el ajuste. El audio de notas (`pw-play`) y la lectura no suenan a la vez.
