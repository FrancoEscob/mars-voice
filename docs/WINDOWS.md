# Probar Mars voice en Windows

> **Versión de prueba.** Windows todavía no está validado en equipos reales. Esta compilación sirve para encontrar problemas y no está firmada.

## Instalar

1. Descargá `mars-voice-windows-x64.zip` (te lo pasa quien mantiene el proyecto, o está en *Actions → CI → Artifacts* si tenés acceso al repo).
2. Descomprimilo en una carpeta tuya, por ejemplo `C:\Users\<vos>\Apps\MarsVoice`.
3. Abrí `MarsVoice.exe`. Como no está firmado, Windows puede mostrar **«Windows protegió tu PC»**: elegí **Más información → Ejecutar de todos modos**.
4. Si no te deja usar el micrófono: **Configuración → Privacidad y seguridad → Micrófono**, y activá *Acceso al micrófono* y *Permitir que las aplicaciones de escritorio accedan al micrófono*.

Mars voice queda abierto en segundo plano, con un icono en la **bandeja** (junto al reloj; si no lo ves, buscalo en la flecha **^**):

- **Clic** en el icono: abre los Ajustes.
- **Clic derecho**: *Abrir ajustes*, *Dictar / detener*, *Leer selección*, *Arreglar o traducir el portapapeles* y *Salir*.
- Cerrar la ventana de Ajustes **no** cierra la app. Para salir, usá *Salir* en la bandeja.
- Si abrís `MarsVoice.exe` otra vez mientras ya está abierto, se muestran los Ajustes de la instancia que ya corre.

## Configurar

- **Ajustes → Proveedor**: pegá tu **API key de xAI** (console.x.ai). La suscripción de Grok es experimental y no oficial.
- **Ajustes → Micrófono**: elegí el micrófono.
- **Ajustes → General**: idioma de la interfaz (automático, español o inglés).

## Atajos predeterminados en Windows

| Acción | Atajo |
|---|---|
| Dictar (alternar o mantener) | `Ctrl+Space` |
| Pegar el último dictado | `Ctrl+Alt+V` |
| Leer en voz alta lo seleccionado | `Ctrl+Alt+Space` |
| Arreglar o traducir lo seleccionado | `Ctrl+Alt+E` |

- Funcionan desde cualquier programa, sin configurar nada más.
- `Ctrl+Alt` es `AltGr` en muchos teclados europeos y latinoamericanos: si `AltGr+E` escribe `€` en el tuyo, cambiá el atajo de Escritura en **Ajustes → Escritura**, porque Windows lo toma antes que el programa.
- Se cambian en **Ajustes → Atajos**, **Ajustes → Lectura** y **Ajustes → Escritura**, y el cambio se aplica al guardar, sin reiniciar. Se aceptan `Ctrl`, `Alt`, `Shift` y `Win` con una letra, un número, `Space` o `F1`–`F24`.
- Si otro programa (o Windows) ya usa la combinación, Mars voice muestra un aviso con el atajo que no pudo registrar. Elegí otra combinación.
- **Mantener para hablar**: el dictado termina al soltar `Space` (la tecla principal del atajo).

## Qué probar

1. **Dictado**: atajo → hablar → atajo. El texto aparece y se copia. Probá también «Mantener para hablar».
2. **Pegar**: enfocá un campo de texto (Bloc de notas, navegador) y usá el atajo de pegar. Mars voice espera a que sueltes las teclas del atajo y envía `Ctrl+V` (nunca `Enter`). El botón *Pegar* del overlay vuelve a la ventana donde empezaste a dictar.
3. **Lectura en voz alta**: seleccioná texto en el navegador y pulsá `Ctrl+Alt+Space`. Mars voice copia la selección con `Ctrl+C` (en terminales como Windows Terminal, la consola, Git Bash o PuTTY usa `Ctrl+Shift+C`), la lee y deja el portapapeles como estaba. Probá pausa, saltos, velocidad y Regenerar con otra voz.
4. **Ventanas**: ¿el overlay y el lector quedan arriba y se pueden arrastrar? ¿Roban el foco a la app donde estabas escribiendo?
5. **Historial e Inicio**: ¿se guardan los dictados y las lecturas?
6. **Bandeja**: ¿aparece el icono? ¿Funcionan el clic y el menú? ¿*Salir* cierra la app y quita el icono?
7. **Cerrar y volver a abrir**: ¿se conserva la cuenta? Al abrir el `.exe` por segunda vez, ¿se muestran los Ajustes de la instancia que ya está abierta?

## Límites conocidos

- **Programas como administrador**: Windows no deja que Mars voice pegue ni copie la selección en ventanas abiertas «como administrador» (salvo que Mars voice también lo esté). El texto queda copiado igual; pegalo con `Ctrl+V`.
- **Volver a la ventana de destino**: Windows solo deja traer otra ventana al frente en ciertos casos. Si no lo permite, Mars voice avisa y el texto queda copiado.
- **Selección primaria**: no existe en Windows. En **Ajustes → Lectura** usá *Copiar la selección automáticamente* (predeterminado) o *Solo el portapapeles*.
- **Historial del portapapeles** (`Win+V`): la selección que Mars voice copia para leer puede quedar en ese historial, como cualquier `Ctrl+C`.
- **Portapapeles con archivos u otros formatos**: si al leer en voz alta el portapapeles tenía algo que no es texto ni imagen (por ejemplo, archivos copiados en el Explorador), queda reemplazado por la selección.
- **Antivirus**: algunos pueden desconfiar de un programa sin firmar que registra atajos globales y envía teclas. Si lo bloquea, anotá el mensaje.

## Desde una terminal

`MarsVoice.exe --help` muestra las opciones y `MarsVoice.exe ctl status` (o `toggle`, `settings`, `read`, `quit`…) controla la instancia abierta. La salida aparece en la terminal desde la que lo lanzaste.

## Reportar problemas

Para cada problema, anotá:

- versión de Windows;
- qué hiciste y qué esperabas;
- qué pasó, con una captura si se puede.

Los datos están en `%LOCALAPPDATA%\dicta` (la configuración en `%LOCALAPPDATA%\dicta\Dicta\config\config.toml`). La API key y la sesión se guardan en el **Administrador de credenciales** de Windows, en entradas que contienen `io.dicta.Dicta`. **No compartas** tu API key ni capturas que la muestren.

## Desinstalar

*Salir* desde la bandeja, borrar la carpeta de `MarsVoice.exe` y `%LOCALAPPDATA%\dicta`, y quitar las entradas `io.dicta.Dicta` del Administrador de credenciales.
