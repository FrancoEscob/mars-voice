//! Formatting for the dashboard and the history in the interface language:
//! counts, durations, sizes, the estimated speech-to-text cost and note
//! dates. Pure functions, so the wording is pinned by tests.

use chrono::{DateTime, Datelike, Local, TimeZone};

use crate::i18n::{Lang, Msg, tf};

/// Thousands separated the local way: 12.345 or 12,345.
pub fn count(n: u64, l: Lang) -> String {
    let digits = n.to_string();
    let sep = l.thousands();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(sep);
        }
        out.push(c);
    }
    out
}

/// `v` with `places` decimals and the local decimal separator: "1,25".
pub fn decimal(v: f64, places: usize, l: Lang) -> String {
    let s = format!("{v:.places$}");
    match l.decimal() {
        '.' => s,
        sep => s.replace('.', &sep.to_string()),
    }
}

/// "0 s", "45 s", "3 min 20 s", "1 h 05 min". The same in both languages.
pub fn duration(ms: u64) -> String {
    let s = (ms + 500) / 1000;
    match s {
        0..60 => format!("{s} s"),
        60..3600 if s.is_multiple_of(60) => format!("{} min", s / 60),
        60..3600 => format!("{} min {} s", s / 60, s % 60),
        _ => format!("{} h {:02} min", s / 3600, s % 3600 / 60),
    }
}

/// Clock of a single dictation: 0:42, 12:05.
pub fn clock(ms: u64) -> String {
    let s = (ms + 500) / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

/// "US$ 1.234,50" or "US$1,234.50".
fn usd_amount(cents: u64, l: Lang) -> String {
    let whole = count(cents / 100, l);
    match l {
        Lang::Es => format!("US$ {whole},{:02}", cents % 100),
        Lang::En => format!("US${whole}.{:02}", cents % 100),
    }
}

/// True when `v` rounds to zero cents without being zero.
pub fn below_a_cent(v: f64) -> bool {
    v.is_finite() && v > 0. && v < 0.005
}

/// Estimated cost in US dollars. Values that round to zero but are not zero
/// read "menos de US$ 0,01" / "less than US$0.01".
pub fn usd(v: f64, l: Lang) -> String {
    if !v.is_finite() || v <= 0. {
        return usd_amount(0, l);
    }
    if below_a_cent(v) {
        return tf!(l, LessThan, amount = usd_amount(1, l));
    }
    usd_amount((v * 100.).round() as u64, l)
}

/// The smallest amount shown, "US$ 0,01", for "less than" labels.
pub fn usd_cent(l: Lang) -> String {
    usd_amount(1, l)
}

/// Token counts as reported by the provider; unknown stays unknown.
pub fn tokens(n: Option<u64>, l: Lang) -> String {
    match n {
        Some(n) => count(n, l),
        None => l.t(Msg::NotAvailable).into(),
    }
}

/// "512 B", "38 KB", "3,4 MB" / "3.4 MB".
pub fn bytes(n: u64, l: Lang) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    match n {
        0..KB => format!("{n} B"),
        KB..MB => format!("{} KB", (n + KB / 2) / KB),
        _ => {
            let tenths = (n * 10 + MB / 2) / MB;
            format!("{}{}{} MB", count(tenths / 10, l), l.decimal(), tenths % 10)
        }
    }
}

pub fn words(n: u64, l: Lang) -> String {
    if n == 1 {
        l.t(Msg::WordsOne).into()
    } else {
        tf!(l, WordsMany, n = count(n, l))
    }
}

const MONTHS_ES: [&str; 12] = [
    "ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sep", "oct", "nov", "dic",
];
const MONTHS_EN: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Note date relative to `now`: "Hoy 14:05", "Ayer 09:12", "3 oct 18:40",
/// "3 oct 2025"; in English "Today 14:05", "Oct 3 18:40", "Oct 3, 2025".
pub fn note_date<Tz: TimeZone>(at: DateTime<Tz>, now: DateTime<Tz>, l: Lang) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let (day, today) = (at.date_naive(), now.date_naive());
    let time = at.format("%H:%M").to_string();
    let (d, m) = (day.day(), day.month0() as usize);
    if day == today {
        tf!(l, DateToday, time = time)
    } else if today.pred_opt() == Some(day) {
        tf!(l, DateYesterday, time = time)
    } else if day.year() == today.year() {
        match l {
            Lang::Es => format!("{d} {} {time}", MONTHS_ES[m]),
            Lang::En => format!("{} {d} {time}", MONTHS_EN[m]),
        }
    } else {
        match l {
            Lang::Es => format!("{d} {} {}", MONTHS_ES[m], day.year()),
            Lang::En => format!("{} {d}, {}", MONTHS_EN[m], day.year()),
        }
    }
}

/// [`note_date`] in local time for Unix milliseconds.
pub fn note_date_local(ms: u64, l: Lang) -> String {
    match Local.timestamp_millis_opt(ms as i64).single() {
        Some(at) => note_date(at, Local::now(), l),
        None => l.t(Msg::DateUnknown).into(),
    }
}

/// First line of a note, for lists.
pub fn preview(text: &str, l: Lang) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() {
        l.t(Msg::NoText).into()
    } else {
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    const ES: Lang = Lang::Es;
    const EN: Lang = Lang::En;

    #[test]
    fn counts_durations_and_sizes_read_in_spanish() {
        assert_eq!(count(0, ES), "0");
        assert_eq!(count(1234567, ES), "1.234.567");
        assert_eq!(duration(0), "0 s");
        assert_eq!(duration(44_600), "45 s");
        assert_eq!(duration(200_000), "3 min 20 s");
        assert_eq!(duration(120_000), "2 min");
        assert_eq!(duration(3_900_000), "1 h 05 min");
        assert_eq!(clock(65_400), "1:05");
        assert_eq!(bytes(512, ES), "512 B");
        assert_eq!(bytes(38 * 1024, ES), "38 KB");
        assert_eq!(bytes(3_565_158, ES), "3,4 MB");
        assert_eq!(words(1, ES), "1 palabra");
        assert_eq!(words(2500, ES), "2.500 palabras");
        assert_eq!(decimal(1.25, 2, ES), "1,25");
    }

    #[test]
    fn counts_and_sizes_read_in_english() {
        assert_eq!(count(1234567, EN), "1,234,567");
        assert_eq!(bytes(3_565_158, EN), "3.4 MB");
        assert_eq!(words(1, EN), "1 word");
        assert_eq!(words(2500, EN), "2,500 words");
        assert_eq!(decimal(1.25, 2, EN), "1.25");
    }

    #[test]
    fn estimated_cost_never_reads_as_free_or_as_a_bill() {
        // US$0.20 per hour of audio sent: 3 minutes is one cent.
        assert_eq!(usd(0.2 * 3. / 60., ES), "US$ 0,01");
        assert_eq!(usd(0., ES), "US$ 0,00");
        assert_eq!(usd(f64::NAN, ES), "US$ 0,00");
        assert_eq!(usd(0.2 * 10. / 3600., ES), "menos de US$ 0,01");
        assert_eq!(usd(1234.5, ES), "US$ 1.234,50");
        assert_eq!(usd(1234.5, EN), "US$1,234.50");
        assert_eq!(usd(0.2 * 10. / 3600., EN), "less than US$0.01");
    }

    #[test]
    fn unknown_tokens_are_never_invented() {
        assert_eq!(tokens(None, ES), "No disponible");
        assert_eq!(tokens(None, EN), "Not available");
        assert_eq!(tokens(Some(0), ES), "0");
        assert_eq!(tokens(Some(18_204), ES), "18.204");
    }

    #[test]
    fn note_dates_are_relative_to_today() {
        let now = Utc.with_ymd_and_hms(2026, 10, 9, 15, 0, 0).unwrap();
        let at = |d, h, m| Utc.with_ymd_and_hms(2026, 10, d, h, m, 0).unwrap();
        assert_eq!(note_date(at(9, 14, 5), now, ES), "Hoy 14:05");
        assert_eq!(note_date(at(8, 9, 12), now, ES), "Ayer 09:12");
        assert_eq!(note_date(at(3, 18, 40), now, ES), "3 oct 18:40");
        let old = Utc.with_ymd_and_hms(2025, 12, 31, 8, 0, 0).unwrap();
        assert_eq!(note_date(old, now, ES), "31 dic 2025");
        assert_eq!(note_date(at(9, 14, 5), now, EN), "Today 14:05");
        assert_eq!(note_date(at(8, 9, 12), now, EN), "Yesterday 09:12");
        assert_eq!(note_date(at(3, 18, 40), now, EN), "Oct 3 18:40");
        assert_eq!(note_date(old, now, EN), "Dec 31, 2025");
    }

    #[test]
    fn previews_collapse_whitespace() {
        assert_eq!(preview("  Hola\n\nmundo  ", ES), "Hola mundo");
        assert_eq!(preview(" \n", ES), "Sin texto");
        assert_eq!(preview(" \n", EN), "No text");
    }
}
