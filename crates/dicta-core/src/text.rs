use crate::Correction;
/// Deterministic approved replacements, longest phrase first, one pass. Neither
/// numbers, negations nor punctuation are guessed. Unicode word boundaries keep
/// a correction for `neo` from corrupting `Neovim`.
pub fn apply_rules(text: &str, rules: &[Correction]) -> String {
    let mut sorted: Vec<_> = rules.iter().filter(|r| !r.from.is_empty()).collect();
    sorted.sort_by_key(|r| std::cmp::Reverse(r.from.len()));
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let mut matched = false;
        for rule in &sorted {
            let end = i + rule.from.len();
            if end > text.len() || !text.is_char_boundary(end) {
                continue;
            }
            let candidate = &text[i..end];
            if candidate.to_lowercase() != rule.from.to_lowercase() {
                continue;
            }
            let previous = text[..i].chars().next_back();
            let next = text[end..].chars().next();
            if previous.is_some_and(word) && rule.from.chars().next().is_some_and(word) {
                continue;
            }
            if next.is_some_and(word) && rule.from.chars().next_back().is_some_and(word) {
                continue;
            }
            out.push_str(&rule.to);
            i = end;
            matched = true;
            break;
        }
        if !matched {
            let c = text[i..].chars().next().expect("valid boundary");
            out.push(c);
            i += c.len_utf8();
        }
    }
    out.trim().to_owned()
}
fn word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}
/// Conservative suggestion: a short contiguous edit, not a whole rewritten dictation.
pub fn suggest_correction(original: &str, edited: &str) -> Option<(String, String)> {
    let a: Vec<_> = original.split_whitespace().collect();
    let b: Vec<_> = edited.split_whitespace().collect();
    let mut prefix = 0;
    while prefix < a.len().min(b.len()) && a[prefix] == b[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < a.len() - prefix
        && suffix < b.len() - prefix
        && a[a.len() - 1 - suffix] == b[b.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let from = &a[prefix..a.len() - suffix];
    let to = &b[prefix..b.len() - suffix];
    if from.is_empty() || to.is_empty() || from.len() > 4 || to.len() > 4 {
        return None;
    }
    let pair = (from.join(" "), to.join(" "));
    if pair.0 == pair.1 { None } else { Some(pair) }
}
/// Reject semantic edits with changed numerical payload or removed/added negations.
/// This is a guardrail, not a semantic equivalence proof.
pub fn protected_content_matches(original: &str, proposed: &str) -> bool {
    fn numbers(s: &str) -> Vec<String> {
        let chars: Vec<char> = s.chars().collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            let numeric = chars[i].is_numeric();
            let signed = matches!(chars[i], '-' | '−' | '+')
                && chars.get(i + 1).is_some_and(|c| c.is_numeric());
            let fraction =
                matches!(chars[i], '.' | ',') && chars.get(i + 1).is_some_and(|c| c.is_numeric());
            if !numeric && !signed && !fraction {
                i += 1;
                continue;
            }
            let start = i;
            i += 1;
            while i < chars.len()
                && (chars[i].is_numeric()
                    || (matches!(chars[i], '.' | ',')
                        && chars.get(i + 1).is_some_and(|c| c.is_numeric())))
            {
                i += 1;
            }
            if chars.get(i) == Some(&'%') {
                i += 1;
            }
            out.push(chars[start..i].iter().collect());
        }
        out
    }
    fn negatives(s: &str) -> Vec<String> {
        s.split(|c: char| !c.is_alphabetic())
            .map(str::to_lowercase)
            .filter(|s| {
                ["no", "nunca", "jamás", "sin", "tampoco", "not", "never"].contains(&s.as_str())
            })
            .collect()
    }
    numbers(original) == numbers(proposed) && negatives(original) == negatives(proposed)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn rule(from: &str, to: &str) -> Correction {
        Correction {
            id: 1,
            from: from.into(),
            to: to.into(),
        }
    }
    #[test]
    fn approved_only_and_not_substrings() {
        let r = vec![rule("hiper land", "Hyprland"), rule("neo", "nuevo")];
        assert_eq!(
            apply_rules("No cambies 1.250: hiper land y Neovim.", &r),
            "No cambies 1.250: Hyprland y Neovim."
        );
    }
    #[test]
    fn replacements_do_not_cascade() {
        assert_eq!(apply_rules("a b", &[rule("a", "b"), rule("b", "c")]), "b c");
    }
    #[test]
    fn longer_rule_wins() {
        assert_eq!(
            apply_rules(
                "casa caoba",
                &[rule("casa", "hogar"), rule("casa caoba", "Casa Caoba")]
            ),
            "Casa Caoba"
        );
    }
    #[test]
    fn unicode_and_punctuation() {
        assert_eq!(
            apply_rules("¿paip wair? ñ", &[rule("paip wair", "PipeWire")]),
            "¿PipeWire? ñ"
        );
    }
    #[test]
    fn suggest_short_diff() {
        assert_eq!(
            suggest_correction("abrí hiper land ahora", "abrí Hyprland ahora"),
            Some(("hiper land".into(), "Hyprland".into()))
        );
        assert!(suggest_correction("hola", "hola").is_none());
    }
    #[test]
    fn distinguishes_signs_and_decimals_from_sentence_punctuation() {
        assert!(protected_content_matches("No enviar 12", "No enviar 12."));
        assert!(protected_content_matches("12,5 y 1.200", "12,5 y 1.200."));
        assert!(!protected_content_matches("-12", "12"));
        assert!(!protected_content_matches("12%", "12"));
        assert!(!protected_content_matches(".5", "5"));
        assert!(!protected_content_matches("1,2", "1.2"));
    }
    #[test]
    fn protects_figures_and_negation() {
        assert!(!protected_content_matches(
            "No enviar 12 euros",
            "Enviar 12 euros"
        ));
        assert!(!protected_content_matches(
            "Enviar 12 euros",
            "Enviar 21 euros"
        ));
        assert!(protected_content_matches(
            "no enviar 12 euros",
            "No enviar 12 euros."
        ));
    }
}
