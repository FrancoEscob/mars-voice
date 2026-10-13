# Dicta, ronda 2 de marca

"Dicta" sigue siendo un nombre provisional. Esto es una maqueta de marca y no implementa la app (Rust + GPUI).
Lámina: `concepts.html`, offline y sin dependencias. Marcas: `assets/`.

## Fuentes

- Exploraciones de Mars, solo lectura: `~/Projects/Mars/brand/explorations/round4/BRIEF.md`, `round3/BRIEF.md`, `round4/final.html`, `round4/shortlist/*.svg`.
  Son rondas de exploración de logo, no una marca final confirmada. De ahí salen el naranja, la tinta, la noche, el papel, el planeta con grano `feTurbulence` enmascarado hacia la sombra, y la lista de rechazos (órbitas, anillos, retículas regulares, líneas sobre el disco, horizontes).
- v1 de Dicta: `preview/index.html` y `review.html`. Conservamos solo el verde pálido y la onda del micrófono. Se descartan el bloc de taquigrafía, la serif y los renglones.

## Marca: planeta-micrófono

Un disco naranja con grano fino y sombra irregular es la cápsula. Debajo lo sostiene un soporte de micrófono: un arco, un pie y una base, todo fuera del disco. El arco es un soporte, no una órbita, porque no rodea el planeta y termina en el pie.

| Archivo | Uso |
|---|---|
| `assets/dicta-mark.svg` | 64 px y mayores, con grano. Trazo tinta `#16130F`, clase `ink` |
| `assets/dicta-mark-dark.svg` | Igual, trazo verde pálido `#CFE3C4` para fondos noche |
| `assets/dicta-mark-32.svg` | 32 px, degradado sin grano, trazo más grueso |
| `assets/dicta-mark-16.svg` | 16 px, disco plano y trazo 1.6 |

El planeta y la textura son estáticos. Solo la onda se mueve, y solo mientras se simula grabación.

## Tokens

| Token | Valor | Rol |
|---|---|---|
| `orange` | `#FF4F00` | Planeta, Terminar, interruptor activo. Texto encima siempre tinta (5.6:1) |
| `orange-shade` | `#8C2400` / `#B23A00` | Sombra del planeta, puntos acentuados en Regolito |
| `ink` | `#16130F` | Texto y trazo de la marca sobre claro |
| `night` | `#14110E` | Fondo Noche; panel `#211C18`, borde `#463D35` |
| `pale` | `#CFE3C4` | Control y onda en Noche, trazo de marca sobre oscuro |
| `green-ground` | `#D6E5CB` (Señal), `#CCDEBF` (Regolito) | Fondo dominante |
| `green-surface` | `#EFF5E9` / `#EAF1E2` | Overlay y paneles |
| `green-line` | `#B9CCAE` | Bordes y separadores |
| `text-2` | `#4A5545` claro, `#A6B59E` noche | Texto secundario |
| `tentative` | `#5E685A` claro, `#909C8A` noche | Palabras aún no confirmadas |
| `paper` | `#EEE8DF` | Solo para mostrar la marca junto a Mars |

Tipografía: grotesca ancha (Archivo, `font-stretch` 115 a 125 %) para la palabra y la interfaz. IBM Plex Mono solo en atajos de teclado y cronómetro. Ninguna de las dos está instalada en esta máquina, así que la lámina cae a Helvetica Neue, Noto Sans o Arial. Para GPUI hay que empaquetar Archivo e IBM Plex Mono.

Textura: ruido fractal (`feTurbulence`) irregular, nunca una retícula de puntos. Fino en Señal, más grueso y en dos capas en Regolito, claro sobre carbón en Noche.

## Direcciones

1. **Señal (recomendada).** Una píldora verde pálido con la onda continua en el centro. El planeta-micrófono hace de icono de la cápsula y el naranja aparece solo en el planeta y en Terminar. Es la que mejor aguanta 16 px, modo claro y una ventana flotante chica, y es la más fácil de llevar a GPUI con quads y paths simples.
2. **Regolito.** Una tarjeta verde claro con un planeta grande asomando por la esquina, con grano grueso y sombra. La voz se dibuja como columnas de puntos. Tiene más carácter, pero el planeta grande pide un raster o un shader de ruido en GPUI y le quita espacio al texto a 320 px.
3. **Noche.** Un panel carbón con grano claro, el micrófono como control verde pálido y la voz como una línea continua. El naranja queda reducido a un punto de grabación. Sirve como tema oscuro de Señal más que como marca propia.

Recomendación: Señal como base de la marca, con Noche como su modo oscuro. Del Regolito tomaría solo el grano más palpable del planeta para piezas grandes (icono de app a 128 px o más, pantalla de bienvenida).

## Pendiente

- Que el usuario elija dirección y confirme si el arco del soporte se lee como micrófono y no como órbita.
- Probar el icono a 16 px en una bandeja real de Linux.
- Revisar en render real, con Archivo instalado, la palabra `dicta`.


## Revisión final y aplicación

El agente principal mantuvo la onda continua de la primera preview en Señal, redujo el grano que pasa cerca del texto en Regolito y alineó la copia automática con el plan. `index.html` aplica la mezcla a la preview completa de ajustes y estados. La versión anterior sigue en `../index.html`.

La aplicación de la paleta usa verde `#EEF4E6`, tinta `#24271E`, secundario `#515D47` y naranja `#FF4F00` con texto `#21170F`. Contrastes calculados: texto principal 13,53:1; secundario 6,23:1; metadatos 4,98:1; texto sobre naranja 5,33:1. La textura se separa del texto mediante capas y baja opacidad.

La forma planeta-micrófono y las tres direcciones vienen de Opus 5.5. Son propuestas pendientes de elección, no un cambio a la marca del proyecto Mars.
