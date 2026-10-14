# Mars voice

Aplicación nativa de dictado y lectura en voz alta, en Rust + GPUI, con xAI Grok. Interfaz en español e inglés. Tres estilos elegibles: **Señal**, **Regolito** y **Órbita**, cada uno con modo claro u oscuro.

> **Estado:** Linux con Hyprland es la plataforma validada. Windows está en pruebas (ver [docs/WINDOWS.md](docs/WINDOWS.md)); macOS y otros escritorios Linux están en desarrollo. Mars voice no está afiliado a xAI.

La marca visible es Mars voice. El ejecutable `dicta`, el identificador de escritorio y las carpetas existentes se conservan para mantener cuentas, configuración y atajos.

## Ejecutar

```bash
cd mars-voice
scripts/build-local.sh
./target/release/dicta --demo --theme signal --color-mode dark
```

La demo simula una sesión cuando presionás el micrófono. No abre audio, llavero, API ni portapapeles. Su configuración y correcciones viven en memoria, y usa un socket separado del modo real.

Para usar la aplicación real:

```bash
./target/release/dicta
```

No graba al abrir. Configurá el proveedor y el micrófono; empezá un dictado con el botón, la bandeja o un atajo. El texto final se copia por defecto. Podés editarlo y volver a copiarlo. Para pegar, enfocá el campo externo y ejecutá el atajo de pegar: el destino es el **campo enfocado**, no la ventana bajo el puntero.

## Cuenta

El modo recomendado es una **API key de xAI** propia: es la vía oficial y se paga por uso. Las credenciales se guardan en el llavero del sistema (Secret Service, Keychain o Administrador de credenciales), nunca en archivos de configuración.

También existe una **suscripción de Grok experimental y no oficial**. Usa el cliente OAuth público de compatibilidad de Grok CLI, no un registro propio de Mars voice. Puede dejar de funcionar o entrar en conflicto con los términos de xAI, y el riesgo es de tu cuenta. La app pide aceptar ese aviso antes de activarla. No hay fallback automático a API paga. El modo API se elige en Ajustes y requiere guardar su propia API key. Para desarrollo, `--allow-env-api-key` permite `DICTA_XAI_API_KEY` únicamente cuando se seleccionó API.

## Atajos e inicio

`packaging/hyprland.lua.example` contiene los binds actuales (Hyprland 0.56/Lua) de pulsación/liberación y `dicta ctl paste`. Agregalos conscientemente a tu configuración si querés usarlos; Dicta no edita Hyprland. El ejemplo `.conf` separado es solo para versiones legacy. Alternativamente, `--portal-shortcuts` solicita registro vía portal GlobalShortcuts. No uses ambos mecanismos para la misma combinación.

**Ajustes → Atajos** permite elegir **Alternar** (predeterminado: una pulsación inicia y otra termina) o **Mantener para hablar**. En este último, mantené Ctrl+Space y soltá para terminar; al completar la transcripción y el posprocesamiento se copia si «Copiar al terminar» está activado. Los botones siguen funcionando con inicio/detención explícitos. No se pega automáticamente.

Los binds Lua pasan un token por gesto para conservar el orden incluso al abrir la app en frío; soltar nunca abre Dicta. `--toggle` se conserva para integración simple, pero no implementa mantener para hablar. El ejemplo legacy `.conf` solo admite alternar.

```bash
./target/release/dicta --toggle
./target/release/dicta ctl stop
./target/release/dicta ctl paste
./target/release/dicta ctl settings
./target/release/dicta ctl quit
```

Anteponé `--demo` a `ctl` para controlar la demo. `--toggle` abre Dicta si está cerrado e inicia el dictado; si ya está abierto, alterna inicio/detención. Una segunda apertura normal muestra los ajustes de la instancia existente. `--background` inicia sin ajustes. `--headless` ejecuta el controlador sin ventanas; es útil con la bandeja/IPC. No es necesario separar daemon y UI en dos procesos.

Instalación opcional, después de compilar:

```bash
scripts/install-user.sh
```

Copia binario, launcher e icono a `~/.local`. `--with-service` también copia una unidad systemd de usuario, sin activarla. Estos scripts no se ejecutan automáticamente durante el desarrollo. La compilación predeterminada es `release`, con optimización para uso diario. Para iterar código podés usar `DICTA_PROFILE=debug` al compilar e instalar; ese perfil no es adecuado para comparar rendimiento.

## Lectura en voz alta

Seleccioná texto en cualquier aplicación y pulsá **Ctrl+Alt+Space**. Por defecto, Mars voice copia lo seleccionado en la ventana activa una sola vez, en ese momento: le envía Ctrl+C con `send_shortcut` de Hyprland, o Ctrl+Shift+C si es una terminal, para no interrumpir procesos. Después lee el texto y devuelve tu portapapeles como estaba. Así funciona aunque Hyprland tenga apagada la selección primaria (`misc:middle_click_paste = false`). En Ajustes → Lectura también podés usar la selección primaria o solo el portapapeles. Un gestor de historial del portapapeles puede registrar el texto copiado. Después genera la voz con Grok y abre una ventana flotante y arrastrable con:

- reproducir/pausa, ±10 s, frase anterior/siguiente y barra de progreso;
- la palabra y la frase actuales resaltadas, y salto con un clic;
- velocidad de 0,5× a 2,5× sin cambiar el tono;
- cambio de voz con **Regenerar**, que sigue desde la frase actual; las versiones anteriores quedan disponibles sin volver a pagarlas;
- copiar el texto y guardar el audio como WAV.

Con la ventana enfocada (un clic), Espacio pausa, las flechas saltan, `[`/`]` cambian la velocidad y Esc cierra. Pulsar el atajo otra vez con el mismo texto pausa o reanuda; con otro texto lo reemplaza (o lo encola, si lo elegís).

- **Cuenta**: el mismo login y las mismas credenciales que el dictado. Lectura tiene su propio selector, Suscripción o API key, y nunca cambia sola a la API paga.
- **Coste**: la generación va por streaming y solo envía el texto de los próximos ~2 minutos. Detener una lectura larga no paga el resto. Desde 5 000 caracteres se pide confirmación con el coste estimado (US$15 por millón de caracteres, tarifa pública; no es un cobro).
- **Seguridad del texto**: las etiquetas de voz que traiga el texto (`[laugh]`, `<whisper>`) se neutralizan por defecto.
- **Convivencia con el dictado**: dictar pausa la lectura, para que el micrófono no la capte. Mientras grabás no se puede empezar una lectura.
- **Configuración**: todo está en Ajustes → Lectura (atajos, origen del texto, voz, idioma, estilo, velocidades, volumen, salida, pronunciaciones, ventana, historial y coste).

```bash
./target/release/dicta --read-selection   # lo que ejecuta el atajo
./target/release/dicta ctl read           # igual, con Mars voice abierto
./target/release/dicta ctl read-clipboard
./target/release/dicta ctl tts-toggle
./target/release/dicta ctl tts-stop
```

En la demo (`--demo`) la lectura usa un texto fijo y audio sintético silencioso: no lee tu selección, no usa la API ni abre la salida de audio.

## Escritura (arreglar y traducir)

Pensada para escribir en otro idioma mientras lo aprendés. Seleccioná texto en cualquier aplicación y pulsá **Ctrl+Alt+E**. Mars voice copia la selección igual que la lectura (Ctrl+C con `send_shortcut`, Ctrl+Shift+C en terminales, y te devuelve el portapapeles como estaba), se la pasa a Grok y abre una ventana flotante con:

- el texto arreglado, con las palabras quitadas y agregadas marcadas sobre el original;
- una explicación corta de cada cambio, en el idioma de la interfaz (o el que elijas), para entender la regla y no repetir el error;
- **Arreglar** o **Traducir** para volver a pedirlo, y el idioma destino (inglés por defecto);
- **Copiar**, **Reemplazar** (pega el resultado sobre tu selección original) y **Escuchar** (lo lee en voz alta con la misma voz de la lectura).

En modo automático, si el texto ya está en el idioma destino se arregla; si no, se traduce. Al terminar, el resultado se copia al portapapeles (se puede apagar). Si no había nada seleccionado, la ventana se abre vacía para que escribas o pegues el texto.

- **Reemplazar**: cierra la ventana, vuelve a la ventana de la que copiaste y pega con tu combinación de pegado (Ctrl+Shift+V en terminales). Si esa ventana ya se cerró, el resultado queda en el portapapeles y la ventana de Escritura vuelve a abrirse con el aviso. No envía Enter.
- **Cuenta**: las mismas credenciales que el dictado, con su propio selector (Suscripción o API key). Nunca cambia sola a la API paga ni reintenta un pedido fallido.
- **Coste**: un pedido por cada Arreglar, Traducir o cambio de idioma, hasta 8 000 caracteres; el resto se descarta con aviso. Los tokens que devuelve xAI se muestran en la ventana.
- **Seguridad del texto**: Grok recibe tu texto como texto, nunca como instrucciones. El texto no se guarda en el historial ni viaja por el socket de control.
- **Configuración**: Ajustes → Escritura (atajo, modo, idioma destino y de las explicaciones, cuánto cambiar, tono, instrucciones propias, ventana y portapapeles).

```bash
./target/release/dicta --write-selection    # lo que ejecuta el atajo
./target/release/dicta ctl write            # igual, con Mars voice abierto
./target/release/dicta ctl write-clipboard  # desde el portapapeles
```

En Hyprland, el bind y la regla de ventana (`dicta-writer`) están en `packaging/hyprland.lua.example`. Desde la bandeja, *Arreglar o traducir el portapapeles* usa lo que copiaste, porque al abrir el menú la ventana activa ya no es la tuya. En la demo (`--demo`) la Escritura arregla siempre la misma frase de ejemplo, sin red ni portapapeles real, y Reemplazar se simula.

## Inicio e historial

Inicio muestra notas recientes y métricas por Hoy, últimos 7 días o Todo. Historial permite buscar, abrir, copiar y eliminar notas; los audios guardados se reproducen bajo demanda y pueden borrarse sin quitar la transcripción. Los resultados fallidos con texto o audio también se conservan para recuperación.

La duración de captura se mide con PCM real. El coste mostrado es el equivalente STT estimado a US$0,20 por hora de audio entregado a la cola de envío, no un cobro ni un saldo de la suscripción. No incluye el pulido ni confirma facturación. Los tokens solo se muestran cuando el proveedor devuelve usage real; desconocido no significa cero. Los totales se calculan sobre el historial conservado: borrarlo cambia las métricas.

## Configuración y privacidad

- Configuración: `~/.config/dicta/config.toml` (respeta XDG).
- Diccionario e historial local: `~/.local/share/dicta/corrections.sqlite3`.
- Audio optativo: `~/.local/share/dicta/audio/` (WAV mono 16 kHz; directorio privado).
- Credenciales: Secret Service del usuario, servicio propio de Dicta (las mismas para dictado y lectura).
- Lecturas: texto en el historial (desactivable); audio de lectura apagado por defecto, con retención propia en Ajustes → Lectura.
- Control local: `$XDG_RUNTIME_DIR/dicta/control.sock`, privado al usuario.
- El historial guarda transcripciones localmente; se puede desactivar. Cancelar no guarda la sesión.
- El audio está apagado por defecto. Ajustes permite 7 días, 30 días o conservar hasta borrarlo. Elegir Off no borra archivos anteriores. La limpieza se ejecuta al abrir y mientras corre la app; borrar una nota también borra su audio.

El pulido con Grok está apagado inicialmente. Las reglas locales se aplican primero; si habilitás Grok, recibe el texto final y tus instrucciones. Se conserva el original ante fallo o cambios detectados en cifras/negaciones. Esa comprobación no garantiza que un modelo preserve todo el sentido.

## Dependencias y pruebas

Rust/Cargo, pkg-config, compiladores C y C++ (la velocidad de lectura usa Signalsmith Stretch) y clang/libclang, headers de PipeWire y ALSA (`libasound2-dev` en Ubuntu, `alsa-lib` en Arch; en Ubuntu 22.04 compilá con `--no-default-features --features gui` para usar PulseAudio en lugar del PipeWire nativo), Wayland/X11/xkbcommon, bibliotecas gráficas usadas por GPUI, `wl-clipboard`, `wtype`, Hyprland y Secret Service para credenciales. La plataforma validada es EndeavourOS/Arch con Hyprland. El overlay abre centrado y se arrastra desde la cabecera. Texto en vivo y resultado largo tienen scroll con controles permanentes. El atajo predeterminado es Ctrl+Space; Super+Shift+V pega. En Hyprland requiere las reglas dedicadas de `packaging/hyprland.lua.example`,. En X11 la ventana admite arrastre, pero todavía faltan los adaptadores de clipboard/pegado.

El script detecta la ruta local de clang en esta PC. GPUI se fija a una revisión de Zed en el manifiesto; `Cargo.lock` fija el grafo completo. Las licencias de las fuentes están en `crates/dicta-ui/assets/fonts/`.

```bash
CLANG_PATH=/usr/lib/llvm21/bin/clang-21 cargo test --workspace --exclude dicta-ui --no-default-features
cargo fmt --all -- --check
```

Las pruebas de protocolo usan servidores locales y secretos ficticios. Las pruebas del motor usan captura sintética. No consumen API ni graban voz.

## Documentación

- [Arquitectura y decisiones](docs/ARQUITECTURA.md)
- [Contratos entre crates](docs/CONTRACTS.md)
- [Diseño GPUI y restricciones del overlay](docs/UI-NATIVE.md)
- [Protocolo xAI, OAuth y TTS](crates/dicta-provider/README.md)
- [Probar en Windows](docs/WINDOWS.md)
- [Referencia visual](design/reference/index.html)
