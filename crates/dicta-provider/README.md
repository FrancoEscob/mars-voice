# Proveedor xAI de Dicta

El crate implementa streaming STT, lectura en voz alta (TTS), OAuth device flow, renovación, almacenamiento en el llavero del sistema y corrección de texto optativa. No inicia sesión por sí solo ni lee credenciales de otros programas. No cambia de suscripción a API paga, no reenvía audio automáticamente y no registra audio, textos ni secretos.

## Integración del controlador

```rust,ignore
use dicta_provider::{CancellationToken, Credentials, XaiProvider, StreamEvent};
let credentials = Credentials::new();
let cancel = CancellationToken::new();
let bearer = credentials.bearer(&config, false, &cancel).await?;
let provider = XaiProvider::new()?;
provider.stream(&config, &bearer, audio_rx, events_tx, cancel.clone()).await?;
```

`audio_rx` es `tokio::sync::mpsc::Receiver<Vec<u8>>`, acotado por el controlador. Audio PCM16 little-endian, mono, 16 kHz. Bloques de 3200 bytes representan 100 ms; el último puede ser menor. La captura mantiene el ritmo real. Esperar `StreamEvent::Started` para abrir el micrófono o usar un buffer acotado. El proveedor no transmite audio antes de `transcript.created`.

`events_tx` es `tokio::sync::mpsc::Sender<StreamEvent>`. Los eventos son:

- `Started` cuando el servicio está listo.
- `Partial { text, is_final, speech_final, start: Option<f64>, duration: Option<f64> }`. Rangos en segundos. Si el evento solo trae tiempos por palabra, se calcula su rango. El ensamblador del core decide cómo reconciliar fragmentos.
- `Final { text, duration: Option<f64> }`. Un texto vacío se entrega como vacío para que el core conserve lo acumulado.

Errores por el `Result` de `stream`. El controlador conserva el texto recibido ante desconexión. Stop cierra **todos** los emisores `audio_tx`, procesa la cola restante y envía `audio.done`. Cancel llama `CancellationToken::cancel()`, devuelve `ProviderError::Cancelled` y descarta el socket sin pedir final. Cancel no debe copiar ni disparar postproceso. El estado de captura debe detenerse también cuando `stream` retorna error.

La conexión y el inicio tienen 15 s de timeout, escrituras 10 s, finalización 20 s y duración máxima de sesión 15 min. El controlador debe mostrar el error y detener el micrófono al alcanzar ese límite. No hay reconexión con repetición de audio. La ausencia de `transcript.done` nunca se presenta como éxito. `Timeouts` permite variar estos valores para tests.

## Credenciales y OAuth

`Credentials::new()` usa el almacén del sistema (`KeyringStore`, keyring 3.6), siempre con el servicio `io.dicta.Dicta`, que se conserva para no perder cuentas, y las entradas `xai-api-key`, `xai-oauth` y `xai-oauth-access`:

| Sistema | Almacén | Feature de keyring | Límite por secreto |
|---|---|---|---|
| Linux | Secret Service por D-Bus (GNOME Keyring, KWallet, KeePassXC) | `sync-secret-service` + `crypto-rust` | sin límite práctico |
| macOS | Llavero de inicio de sesión (Keychain) | `apple-native` | sin límite práctico |
| Windows | Administrador de credenciales, credencial genérica `<entrada>.io.dicta.Dicta` | `windows-native` | 2560 bytes: 1280 caracteres UTF-16 |

En otros sistemas el crate no compila (`compile_error!`): sin backend nativo, keyring 3 usaría un almacén falso en memoria y la sesión se perdería al cerrar. Las llamadas bloqueantes al llavero corren en `spawn_blocking`. Si el almacén no está disponible, se devuelve `KeyringUnavailable`, con un mensaje que nombra el almacén de cada sistema; no existe fallback de archivos planos. Si el almacén rechaza un valor por tamaño, `SecretStore::set` devuelve `SecretTooLong`.

- **Sesión OAuth dividida.** `xai-oauth` guarda `{refresh_token, client_id, expires_at}` y `xai-oauth-access` guarda `{access_token, expires_at}`. Así la sesión entra en Windows aunque el access token sea un JWT largo. La segunda entrada es de mejor esfuerzo: si no entra o falla, el access token queda solo en memoria durante la sesión y el siguiente inicio renueva una vez. `expires_at` vincula las dos entradas, así un access token viejo nunca se combina con un login más nuevo. Si lo que no entra es `xai-oauth` (un refresh token de más de unos 1200 caracteres), el login devuelve `SecretTooLong` y se conserva la sesión anterior.
- **Migración.** Las versiones anteriores guardaban el `TokenSet` completo en `xai-oauth`. Esa entrada se sigue leyendo sin escribir ni usar la red; la próxima escritura (renovación o login nuevo) la reescribe en el formato dividido. Una versión anterior de la app no entiende el formato nuevo y pide volver a conectar Grok.
- **Memoria.** `Credentials` recuerda el último `TokenSet` leído o escrito y lo usa solo mientras coincide con `xai-oauth` (refresh token, client ID y vencimiento). En Linux eso mantiene una sola lectura del llavero por pedido. `clear(Subscription)` borra las dos entradas y la copia en memoria.
- **macOS.** Sin firma de código estable, macOS puede volver a pedir permiso para el llavero después de cada compilación.
- **Sin verificar todavía:** Windows y macOS se configuran por `cfg` y no se compilaron ni probaron en esas plataformas; queda para el CI con runners nativos y la validación manual.

Métodos asíncronos:

- `has_credentials(mode, allow_env_api_key)` consulta presencia. No hace red ni renueva. No demuestra autenticación exitosa ni cobertura de suscripción.
- `save_api_key(Secret::new(value))` guarda una key propia.
- `store_tokens(TokenSet)` guarda el resultado de OAuth.
- `clear(mode)` elimina la credencial del proveedor seleccionado.
- `bearer(&config, allow_env_api_key, &cancel)` devuelve `Secret`; renueva OAuth 60 s antes de vencer. Los clones de `Credentials` comparten el bloqueo de refresh. Mantener una instancia en el controlador.

Con `allow_env_api_key=true` y modo API explícito se permite `DICTA_XAI_API_KEY`. Esto es para desarrollo y debe habilitarse explícitamente desde CLI. No se consulta `XAI_API_KEY` de otros programas ni se considera el env en modo suscripción. `Secret` oculta `Debug` y borra su allocation al liberar, aunque las bibliotecas de HTTP/JSON pueden tener copias temporales en memoria.

`OAuthClient::from_config(&config)` llama a `resolve_oauth_client_id(&config) -> Result<String>`. Dicta trae configuración OAuth nativa y funciona sin Node, npm ni Grok CLI instalados. El usuario pulsa Conectar Grok y autoriza en el navegador; no debe introducir un ID. No hay búsqueda de instalaciones, lexer JavaScript, lectura de fuentes locales ni subprocesos de autenticación.

El valor predeterminado es el identificador **público de compatibilidad Grok CLI**, `b1a00492-073a-47ea-816f-4c329264a828`, confirmado en el [código oficial de xAI, config.rs línea 380](https://github.com/xai-org/grok-build/blob/2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8/crates/codegen/xai-grok-login/src/config.rs#L380). No es un secreto de cuenta ni un cliente registrado exclusivamente para Dicta. El repositorio oficial tiene [licencia Apache-2.0](https://github.com/xai-org/grok-build/blob/2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8/LICENSE). Se referencia ese dato público; el flujo Rust de Dicta es propio.

Para usos avanzados, `oauth_client_id` y `DICTA_OAUTH_CLIENT_ID` pueden sustituir el valor incorporado, en ese orden, si tienen formato válido. No son requisitos de instalación. Device flow, refresh y almacenamiento de credenciales corren en código Rust de Dicta. Sus tokens se guardan exclusivamente en `io.dicta.Dicta`; no se importan sesiones ajenas.

```rust,ignore
let oauth = OAuthClient::from_config(&config)?;
let device = oauth.begin_device_login(&cancel).await?;
// Mostrar device.verification_uri y device.user_code.
// No enviar device completo a logs. Abrir URI solo por acción del usuario.
let tokens = oauth.poll_device_login(&device, &cancel).await?;
credentials.store_tokens(tokens).await?;
```

Endpoints de auth: `https://auth.x.ai/oauth2/device/code` y `/oauth2/token`, formularios URL-encoded. El polling espera el intervalo incluso antes de la primera consulta, respeta pending, aumenta 5 s con slow_down y termina al expirar/denegar/cancelar. El refresh conserva el refresh token si el servidor no lo rota. Cambiar el client ID exige autorizar otra vez. Las URL de verificación deben ser HTTPS, puerto 443, en `accounts.x.ai` o `auth.x.ai`. El endpoint de device code usa `auth.x.ai`, pero la página para autorizar la cuenta que devolvió el servicio el 09-10-2026 usa `accounts.x.ai/oauth2/device`. Ambos campos, `verification_uri` y `verification_uri_complete`, se validan con coincidencia exacta de dominio y sin credenciales en la URL.

La implementación del transporte OAuth **no confirma** que xAI autorice este cliente ni que STT esté incluido en una suscripción determinada. Esa prueba real queda pendiente; no se efectuó login, conexión de STT autenticada ni consumo cobrado durante el desarrollo.

## Corrección final

`provider.polish(&config, &bearer, text, &cancel).await` devuelve `Result<String>`. Con `polish=false` no hace red y devuelve el original. Habilitado, usa Responses API, `store=false`, sin herramientas, con el dictado separado en un mensaje de usuario y las instrucciones editoriales en el mensaje system. Para `grok-4.3` o su alias latest envía `reasoning: { effort: "none" }`. Tiene timeout de 10 s y rechaza resultados vacíos o incompletos. No hay retry ni otro bearer. Ante fallo, el controlador devuelve el original/reglas locales y un aviso. Esto reduce, pero no elimina, el riesgo de que un LLM altere contenido o siga instrucciones dictadas.

## Lectura en voz alta (TTS)

Módulo `tts.rs`. Mismo `XaiProvider`, mismas credenciales y ninguna llamada real en tests. No registra texto, audio ni bearer; los errores no copian cuerpos ni mensajes remotos. No hay reintentos cobrados ni cambio de suscripción a API key.

```rust,ignore
use dicta_provider::{TtsEvent, TtsRequest, TtsSessionEnd};
let bearer = credentials.bearer_for(&config, config.tts.provider, false, &cancel).await?;
let request = TtsRequest { settings: config.tts.voice_settings(), with_timestamps: true };
// utterances: mpsc::Receiver<(u64, String)>; events: mpsc::Sender<TtsEvent>
let end = provider.tts_session(&request, &bearer, utterances, events, cancel.clone()).await?;
let audio = provider.synthesize(&request, &bearer, "Probando la voz.", &cancel).await?; // TtsAudio { pcm, marks }
let voices = provider.list_voices(&bearer, &cancel).await?; // Vec<TtsVoice>, ordenadas por nombre
```

- **Credenciales.** `Credentials::bearer_for(&config, mode, allow_env, &cancel)` usa el modo pedido (el selector propio de la lectura), no `config.provider`. Mismas entradas del llavero y el mismo bloqueo de refresh que el dictado. Sin fallback en ningún sentido. `bearer_for_with_oauth` inyecta el transporte OAuth en tests.
- **Streaming** `wss://api.x.ai/v1/tts`, `Authorization: Bearer`. Query: `language` (`auto` pasa tal cual), `voice`, `codec=pcm`, `sample_rate`, `speed`, `optimize_streaming_latency` (1 si `low_latency`, si no 0), `text_normalization`, `with_timestamps`. Si hay pronunciaciones, primero `{"type":"session.update","replace":{origen: reemplazo}}` y espera `session.updated`; recién entonces emite `TtsEvent::Ready`.
- **Locuciones.** Por cada `(id, texto)` envía `text.delta` con `settings.style.wrap(texto)` (partido cada 60 000 caracteres) y `text.done`. Emite `Audio{id, pcm}` (s16le mono, siempre par: un byte impar pasa al delta siguiente y uno sobrante al final se descarta), `Marks{id, marks}` y `Done{id}` en `audio.done`. Solo entonces toma la siguiente. Un texto vacío recibe `Done` sin red.
- **Marcas.** `graph_chars`/`graph_times` en segundos, como pares `[inicio, fin]` u objetos `{start, end}`; se convierten a ms enteros no negativos. El esquema WS dice que son relativas a la sesión: se resta la duración del PCM ya entregado en locuciones anteriores (bytes/2/sample_rate). Si la primera marca de una locución es menor que la mitad de ese desplazamiento, se asume que el servidor ya las da relativas a la locución y no se resta. La decisión se toma una vez por locución.
- **Fin.** Cerrar `utterances` termina tras la locución en curso: `Ok(Finished)`. Si el socket se cierra sin locución en curso, o al rotar la sesión inactiva a los 570 s (antes del permiso de 600 s), `Ok(ServerClosed)`. Las locuciones que quedaban en el canal se descartan sin enviar: el controlador reenvía todo id sin `Done`. `cancel` manda `text.clear` (best effort), cierra y devuelve `Cancelled`. Un evento `error` (también la moderación, que puede llegar después del audio) o un cierre con locución en curso devuelven error; lo ya entregado queda entregado.
- **REST** `POST /v1/tts` (`synthesize`): `text`, `language`, `voice_id`, `speed`, `output_format {codec: "pcm", sample_rate}`, `optimize_streaming_latency`, `text_normalization`, `with_timestamps` y `replace`. Con marcas, la respuesta es JSON `{audio (base64), content_type, duration, audio_timestamps}`; sin marcas, PCM crudo. Rechaza textos de más de 60 000 caracteres y `content_type` de otros códecs (mp3, wav, μ-law, A-law).
- **Voces.** `GET /v1/tts/voices` es obligatoria. `GET /v1/custom-voices?limit=1000` (con `pagination_token`, hasta 10 páginas) se suma con `custom: true`; cualquier fallo ahí (403 para equipos sin acceso, 404, red, JSON) deja la lista sin voces propias. Se descartan IDs inválidos y duplicados. `language: null` llega como cadena vacía.
- **Validación previa, sin red:** voz, idioma, velocidad 0,7–1,5, frecuencias documentadas (8–48 kHz) y `replace` con los límites documentados: hasta 200 entradas, origen de hasta 100 caracteres con solo letras, números, apóstrofos y espacios, reemplazo de hasta 128 y orígenes distintos sin contar mayúsculas ni espacios.
- **Errores.** 400 → `TtsRejected`, 401 → `Unauthorized`, 403 → `TtsForbidden` (propio de la lectura: indica elegir «API key» en Ajustes → Lectura y aclara que no se cambió a API paga), 404 → `TtsVoiceNotFound`, 429 → `RateLimited`, otros → `Http(n)`. Valen para el upgrade del WebSocket y para REST. Los eventos `error` se clasifican sin guardar el mensaje: `replace` → `TtsPronunciations`, cuota → `RateLimited`, moderación → `TtsModerated`, el resto → `TtsUpstream`. Cierre a mitad → `TtsDisconnected`.
- **Timeouts** (`Timeouts`, inyectables): conexión 15 s (`connect`), `session.updated` 15 s (`ready`), escritura 10 s, primer audio 30 s (`tts_first_audio`), hueco entre deltas 30 s (`tts_gap`), REST total 120 s (`tts_rest`), rotación 570 s (`tts_session`). `Endpoints` suma `tts_ws`, `tts`, `tts_voices` y `custom_voices`.

Fuentes (12-10-2026): [esquema del streaming](https://docs.x.ai/tts-streaming.ws.json), [guía TTS](https://docs.x.ai/developers/model-capabilities/audio/text-to-speech), [referencia REST](https://docs.x.ai/developers/rest-api-reference/inference/voice), [voces propias](https://docs.x.ai/developers/model-capabilities/audio/custom-voices).

**Confirmado contra el servicio real** (D-25, 12-10-2026, prueba consciente autorizada por el titular de la cuenta con `cargo run -p dicta-provider --example tts_probe`):

- El token OAuth de la suscripción de Mars voice **cubre TTS**: sesión WebSocket sin 403.
- `GET /v1/tts/voices` devolvió 28 voces con idioma `multilingual`.
- Las marcas llegan por carácter, incluidos espacios y signos, en varios lotes por locución. Tras normalizar quedan relativas a cada locución; la segunda locución empezó en 80 ms.
- Latencia: socket listo en 0,66 s; primer audio en 1,2 s, incluidos credencial y conexión.

**Sin confirmar todavía:**

- Si las marcas describen el texto antes o después de `replace` (la guía dice después; la alineación tolera ambos casos).
- El `content_type` real del envelope con `pcm`, la forma real del evento `error` (solo se documenta `message`) y si el permiso de 600 s cierra una sesión abierta.
- Si `optimize_streaming_latency` va como número o como texto en REST (la guía dice entero y la referencia muestra `"0" | "1"`; se envía entero).

## Fuentes y diferencia con el plan histórico

- [Referencia oficial STT](https://docs.x.ai/developers/rest-api-reference/inference/speech-to-text), consultada el 9-10-2026. `smart_turn` es un umbral numérico, no booleano. Se omite para mantenerlo desactivado. La tabla histórica que proponía `smart_turn=false` no se copia al protocolo.
- [Grok 4.3](https://docs.x.ai/developers/models/grok-4.3) documenta razonamiento `none`. [Responses y razonamiento](https://docs.x.ai/developers/model-capabilities/text/reasoning) muestra `reasoning.effort` y la estructura `output/message/content/output_text`.
- [Identificador público oficial de Grok CLI](https://github.com/xai-org/grok-build/blob/2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8/crates/codegen/xai-grok-login/src/config.rs#L380), verificado en el commit indicado. 

## Validación

`cargo test -p dicta-provider` usa servidores HTTP/WebSocket en loopback y almacenes de secretos en memoria, uno de ellos limitado a 2560 bytes como el de Windows. No usa micrófono, llavero real, OAuth real ni API externa. Comprueba el orden del handshake, bloques binarios, query/vocabulario, Stop/Cancel, flags y final vacío, timeout, desconexión, 401/403/429, device flow y slow_down, expiración, refresh serializado, ausencia de fallback pago y corrección optativa/incompleta.

Cuatro pruebas cubren el formato del llavero: un access token de 4 KB queda solo en memoria y se renueva una vez al reiniciar; un refresh token de 4 KB devuelve `SecretTooLong` sin escrituras parciales y conserva la sesión anterior; la entrada combinada anterior autentica sin red ni escrituras y se divide en la siguiente renovación; y un access token viejo nunca se combina con un login más nuevo. Una prueba unitaria comprueba que el error de llavero nombre el almacén del sistema y mantenga la regla de no guardar en disco plano.

`tests/tts.rs` (13 pruebas) y 4 unitarias en `tts.rs` cubren la lectura con mocks loopback: query y cabecera, `session.update` antes del texto, locuciones en orden con `Done` por id, base64 y byte impar, marcas en ambos modos y formas, estilo aplicado, Cancel con `text.clear`, `error` a mitad, cierre en reposo y en curso, timeout del primer audio, 400/401/403/404/429 en el upgrade y en REST, envelope REST, voces propias paginadas y con 403, y `bearer_for` sin fallback y con refresh compartido.

Cinco pruebas herméticas de configuración verifican el valor nativo con config vacía y sin instalación externa, la precedencia de overrides y el descarte de valores inválidos. Inyectan el valor env sin mutar el entorno global; no usan archivos ni fixtures de paquetes externos. Las once pruebas de transporte y autenticación conservan cobertura del flujo propio.
