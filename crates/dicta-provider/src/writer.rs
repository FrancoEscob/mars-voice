//! Escritura: arreglar y traducir texto con la Responses API de xAI.
use crate::{CancellationToken, ProviderError as Error, Result, Secret, XaiProvider, stream};
use dicta_core::writer::{
    MAX_INPUT_CHARS, WriterExplain, WriterMode, WriterOutput, WriterRequest, parse_output,
    response_schema, system_prompt,
};
use serde_json::json;

/// Respuesta interpretada y uso de tokens que devolvió xAI.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WriterResult {
    pub output: WriterOutput,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

/// Salida máxima: una traducción de `MAX_INPUT_CHARS` más las notas.
const MAX_OUTPUT_TOKENS: u32 = 16_384;

impl XaiProvider {
    /// Un pedido sin herramientas, sin `store` y sin reintentos automáticos.
    /// El texto del usuario nunca se interpreta como instrucciones.
    pub async fn rewrite(
        &self,
        bearer: &Secret,
        request: &WriterRequest<'_>,
        cancel: &CancellationToken,
    ) -> Result<WriterResult> {
        if request.text.trim().is_empty() {
            return Err(Error::Configuration("no hay texto para Escritura"));
        }
        if request.text.chars().count() > MAX_INPUT_CHARS {
            return Err(Error::Configuration(
                "el texto supera el límite de Escritura",
            ));
        }
        if !is_language_code(request.target_language) || !is_language_code(request.explain_language)
        {
            return Err(Error::Configuration("idiomas de Escritura inválidos"));
        }
        let config = request.config;
        config
            .validate()
            .map_err(|_| Error::Configuration("preferencias de Escritura inválidas"))?;
        // Salida estructurada de xAI: `text.format` con `json_schema` estricto.
        let mut body = json!({
            "model":config.model,"store":false,"stream":false,
            "input":[
                {"role":"system","content":system_prompt(request)},
                {"role":"user","content":request.text}
            ],
            "text":{"format":{"type":"json_schema","name":"writer_output","schema":response_schema(),"strict":true}},
            "max_output_tokens":MAX_OUTPUT_TOKENS
        });
        if config.model == "grok-4.3" || config.model == "grok-4.3-latest" {
            body["reasoning"] = json!({"effort":"none"});
        }
        const STAGE: &str = "Escritura Grok";
        stream::bounded(cancel, self.timeouts.writer, STAGE, async {
            let response = self
                .http
                .post(&self.endpoints.responses)
                // El cliente compartido corta antes; este pedido tiene su propio límite.
                .timeout(self.timeouts.writer)
                .bearer_auth(bearer.expose())
                .json(&body)
                .send()
                .await
                .map_err(|e| {
                    if e.is_timeout() {
                        Error::Timeout(STAGE)
                    } else {
                        Error::Network
                    }
                })?;
            if !response.status().is_success() {
                return Err(Error::http(response.status().as_u16()));
            }
            let result: serde_json::Value = response
                .json()
                .await
                .map_err(|_| Error::Protocol("Escritura no es JSON"))?;
            if result.get("status").and_then(|s| s.as_str()) != Some("completed") {
                return Err(Error::Protocol("Escritura incompleta"));
            }
            let output = result
                .get("output")
                .and_then(|v| v.as_array())
                .ok_or(Error::Protocol("Escritura sin salida"))?;
            let text = output
                .iter()
                .filter(|v| v["type"] == "message")
                .flat_map(|v| v["content"].as_array().into_iter().flatten())
                .filter(|v| v["type"] == "output_text")
                .filter_map(|v| v["text"].as_str())
                .collect::<Vec<_>>()
                .join("");
            if text.trim().is_empty() {
                return Err(Error::Protocol("Escritura vacía"));
            }
            let mut parsed = parse_output(&text)
                .map_err(|_| Error::Protocol("Escritura con formato inválido"))?;
            // Un modo explícito es lo que se hizo; solo `Auto` lo decide el modelo.
            if request.mode != WriterMode::Auto {
                parsed.mode = request.mode;
            }
            if config.explain == WriterExplain::Off {
                for change in &mut parsed.changes {
                    change.why.clear();
                }
            }
            Ok(WriterResult {
                output: parsed,
                input_tokens: result["usage"]["input_tokens"].as_u64(),
                output_tokens: result["usage"]["output_tokens"].as_u64(),
            })
        })
        .await
    }
}
fn is_language_code(code: &str) -> bool {
    code.len() == 2 && code.bytes().all(|b| b.is_ascii_lowercase())
}
