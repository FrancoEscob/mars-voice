//! Escritura: arreglar y traducir texto con la Responses API de xAI.
use crate::{CancellationToken, Result, Secret, XaiProvider};
use dicta_core::writer::{WriterOutput, WriterRequest};

/// Respuesta interpretada y uso de tokens que devolvió xAI.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WriterResult {
    pub output: WriterOutput,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

impl XaiProvider {
    /// Un pedido sin herramientas, sin `store` y sin reintentos automáticos.
    /// El texto del usuario nunca se interpreta como instrucciones.
    pub async fn rewrite(
        &self,
        bearer: &Secret,
        request: &WriterRequest<'_>,
        cancel: &CancellationToken,
    ) -> Result<WriterResult> {
        let _ = (bearer, request, cancel);
        todo!("Escritura: pedido a xAI")
    }
}
