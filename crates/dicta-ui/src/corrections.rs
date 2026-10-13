//! Correction proposals. When the user edits a finished dictation, the UI
//! compares it word by word with what the recogniser produced and offers each
//! substitution as a correction to learn. Nothing is learned without the user
//! pressing "Recordar" (`Command::RememberCorrection`).

use dicta_core::Correction;

const MAX_SPAN_WORDS: usize = 4;
const MAX_PROPOSALS: usize = 5;
const TRIM: &[char] = &[
    '.', ',', ';', ':', '!', '?', '¿', '¡', '"', '\'', '(', ')', '«', '»',
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proposal {
    pub from: String,
    pub to: String,
}

fn clean(words: &[&str]) -> String {
    words.join(" ").trim_matches(TRIM).to_string()
}

/// Substitutions between `original` and `edited`, skipping pure insertions or
/// deletions and anything already learned.
pub fn propose(original: &str, edited: &str, known: &[Correction]) -> Vec<Proposal> {
    let a: Vec<&str> = original.split_whitespace().collect();
    let b: Vec<&str> = edited.split_whitespace().collect();
    if a.is_empty() || b.is_empty() || a.len() * b.len() > 250_000 {
        return Vec::new();
    }
    // LCS table over words.
    let mut lcs = vec![vec![0u16; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut out: Vec<Proposal> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            i += 1;
            j += 1;
            continue;
        }
        let (si, sj) = (i, j);
        while (i < a.len() || j < b.len()) && !(i < a.len() && j < b.len() && a[i] == b[j]) {
            if j >= b.len() || (i < a.len() && lcs[i + 1][j] >= lcs[i][j + 1]) {
                i += 1;
            } else {
                j += 1;
            }
        }
        let (del, ins) = (&a[si..i], &b[sj..j]);
        if del.is_empty()
            || ins.is_empty()
            || del.len() > MAX_SPAN_WORDS
            || ins.len() > MAX_SPAN_WORDS
        {
            continue;
        }
        let (from, to) = (clean(del), clean(ins));
        if from.is_empty() || to.is_empty() || from == to {
            continue;
        }
        let learned = known
            .iter()
            .any(|c| c.from.eq_ignore_ascii_case(&from) && c.to == to);
        if !learned && !out.iter().any(|p| p.from == from && p.to == to) {
            out.push(Proposal { from, to });
        }
        if out.len() == MAX_PROPOSALS {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitution_is_proposed_without_punctuation() {
        let p = propose(
            "probé ge pe u i en jiperland.",
            "probé GPUI en Hyprland.",
            &[],
        );
        assert_eq!(
            p,
            vec![
                Proposal {
                    from: "ge pe u i".into(),
                    to: "GPUI".into()
                },
                Proposal {
                    from: "jiperland".into(),
                    to: "Hyprland".into()
                },
            ]
        );
    }

    #[test]
    fn pure_insertions_and_deletions_are_not_corrections() {
        assert!(propose("mañana a las diez", "mañana temprano a las diez", &[]).is_empty());
        assert!(propose("che mañana a las diez", "mañana a las diez", &[]).is_empty());
    }

    #[test]
    fn already_learned_and_long_rewrites_are_skipped() {
        let known = [Correction {
            id: 1,
            from: "jiperland".into(),
            to: "Hyprland".into(),
        }];
        assert!(propose("abrí jiperland", "abrí Hyprland", &known).is_empty());
        assert!(
            propose(
                "uno dos tres cuatro cinco",
                "seis siete ocho nueve diez",
                &[]
            )
            .is_empty()
        );
    }

    #[test]
    fn capitalisation_change_counts() {
        let p = propose("usá gpui", "usá GPUI", &[]);
        assert_eq!(
            p,
            vec![Proposal {
                from: "gpui".into(),
                to: "GPUI".into()
            }]
        );
    }
}
