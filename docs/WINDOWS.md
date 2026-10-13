# Probar Mars voice en Windows

> **Versión de prueba.** Windows todavía no está validado. Esta compilación sirve para encontrar problemas y no está firmada.

## Instalar

1. Descargá `mars-voice-windows-x64.zip` (te lo pasa quien mantiene el proyecto, o está en *Actions → CI → Artifacts* si tenés acceso al repo).
2. Descomprimilo en una carpeta tuya, por ejemplo `C:\Users\<vos>\Apps\MarsVoice`.
3. Abrí `MarsVoice.exe`. Como no está firmado, Windows puede mostrar **«Windows protegió tu PC»**: elegí **Más información → Ejecutar de todos modos**.
4. Si no te deja usar el micrófono: **Configuración → Privacidad y seguridad → Micrófono**, y activá *Permitir que las aplicaciones de escritorio accedan al micrófono*.

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

Se cambian en **Ajustes → Atajos** y **Ajustes → Lectura**. Si otro programa ya usa la combinación, Mars voice avisa.

## Qué probar

1. **Dictado**: atajo → hablar → atajo. El texto aparece y se copia. Probá también «Mantener para hablar».
2. **Pegar**: enfocá un campo de texto (Bloc de notas, navegador) y usá el atajo de pegar.
3. **Lectura en voz alta**: seleccioná texto en el navegador y pulsá `Ctrl+Alt+Space`. Probá pausa, saltos, velocidad y Regenerar con otra voz.
4. **Ventanas**: ¿el overlay y el lector quedan arriba y se pueden arrastrar? ¿Roban el foco a la app donde estabas escribiendo?
5. **Historial e Inicio**: ¿se guardan los dictados y las lecturas?
6. **Cerrar y volver a abrir**: ¿se conserva la cuenta? Al abrir el `.exe` por segunda vez, ¿se muestran los Ajustes de la instancia que ya está abierta?

## Reportar problemas

Para cada problema, anotá:

- versión de Windows;
- qué hiciste y qué esperabas;
- qué pasó, con una captura si se puede.

Los datos están en `%APPDATA%\dicta` y `%LOCALAPPDATA%\dicta`. **No compartas** tu API key ni capturas que la muestren.
