//! Reconciles xAI's revisable hypotheses, locked chunks and stitched utterances.
//! Timestamps, when present, are authoritative; a final utterance replaces its chunks.
#[derive(Debug, Clone)]
struct Span {
    start: f64,
    end: f64,
    text: String,
}
#[derive(Debug, Clone, Default)]
pub struct Transcript {
    spans: Vec<Span>,
    untimed_prefix: String,
    untimed_utterance: String,
    pub partial: String,
}
impl Transcript {
    pub fn update(
        &mut self,
        text: &str,
        is_final: bool,
        speech_final: bool,
        start: Option<f64>,
        duration: Option<f64>,
    ) {
        if !is_final {
            self.partial = text.to_owned();
            return;
        }
        self.partial.clear();
        if text.trim().is_empty() {
            return;
        }
        if let (Some(start), Some(duration)) = (start, duration) {
            if start.is_finite() && duration.is_finite() && duration >= 0.0 {
                let end = start + duration;
                // An utterance-final event covers the earlier locked chunks. Replayed
                // events replace the same range rather than appending the words again.
                self.spans.retain(|s| {
                    !(s.start < end - 0.000_001 && s.end > start + 0.000_001
                        || (s.start - start).abs() < 0.000_001)
                });
                self.spans.push(Span {
                    start,
                    end,
                    text: text.trim().into(),
                });
                self.spans.sort_by(|a, b| a.start.total_cmp(&b.start));
                return;
            }
        }
        if speech_final {
            let utterance = merge_hypothesis(&self.untimed_utterance, text);
            append(&mut self.untimed_prefix, &utterance);
            self.untimed_utterance.clear();
        } else {
            self.untimed_utterance = merge_hypothesis(&self.untimed_utterance, text);
        }
    }
    pub fn confirmed(&self) -> String {
        let mut out = self
            .spans
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        append(&mut out, &self.untimed_prefix);
        append(&mut out, &self.untimed_utterance);
        out
    }
    pub fn finish(&mut self, text: &str) -> String {
        self.partial.clear();
        if text.trim().is_empty() {
            self.confirmed()
        } else {
            text.trim().to_owned()
        }
    }
}
fn append(target: &mut String, value: &str) {
    if value.is_empty() {
        return;
    }
    if !target.is_empty() {
        target.push(' ');
    }
    target.push_str(value);
}
fn merge_hypothesis(current: &str, new: &str) -> String {
    if current.is_empty() || new.starts_with(current) {
        return new.to_owned();
    }
    if current == new {
        return current.to_owned();
    }
    let a: Vec<&str> = current.split_whitespace().collect();
    let b: Vec<&str> = new.split_whitespace().collect();
    let overlap = (1..=a.len().min(b.len()))
        .rev()
        .find(|&n| a[a.len() - n..] == b[..n])
        .unwrap_or(0);
    let mut result = current.to_owned();
    append(&mut result, &b[overlap..].join(" "));
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chunk_and_stitched_utterance_are_not_duplicated() {
        let mut t = Transcript::default();
        t.update("hola", true, false, Some(0.0), Some(0.5));
        t.update("mundo", true, false, Some(0.5), Some(0.6));
        t.update("Hola, mundo.", true, true, Some(0.0), Some(1.1));
        assert_eq!(t.confirmed(), "Hola, mundo.");
        t.update("Otra frase.", true, true, Some(1.2), Some(1.0));
        assert_eq!(t.finish(""), "Hola, mundo. Otra frase.");
    }
    #[test]
    fn interim_is_replaced_and_empty_final_keeps_locked_text() {
        let mut t = Transcript::default();
        t.update("casa", false, false, None, None);
        t.update("Casa Caoba", false, false, None, None);
        assert_eq!(t.partial, "Casa Caoba");
        assert!(t.confirmed().is_empty());
        t.update("Casa Caoba", true, true, Some(0.0), Some(2.0));
        assert_eq!(t.finish(""), "Casa Caoba");
        assert!(t.partial.is_empty());
    }
    #[test]
    fn replay_and_intentional_repetition_have_different_time_ranges() {
        let mut t = Transcript::default();
        for _ in 0..2 {
            t.update("sí", true, true, Some(0.0), Some(0.2));
        }
        t.update("sí", true, true, Some(0.3), Some(0.2));
        assert_eq!(t.confirmed(), "sí sí");
    }
    #[test]
    fn untimed_cumulative_chunks_stitch() {
        let mut t = Transcript::default();
        t.update("hola", true, false, None, None);
        t.update("hola mundo", true, false, None, None);
        t.update("hola mundo", true, true, None, None);
        assert_eq!(t.confirmed(), "hola mundo");
    }
    #[test]
    fn session_final_can_revise_punctuation() {
        let mut t = Transcript::default();
        t.update("hola mundo", true, false, None, None);
        assert_eq!(t.finish("Hola, mundo."), "Hola, mundo.");
    }
}
