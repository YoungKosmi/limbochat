//! „Co nowego” — lista zmian z `CHANGELOG.md` wbudowana w silnik.
//! Po aktualizacji panel pokazuje wpisy nowsze niż poprzednia wersja.

use serde::Serialize;

static CHANGELOG: &str = include_str!("../../CHANGELOG.md");
pub const NEXT: &str = "Następna wersja";

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Entry {
    pub version: String,
    pub items: Vec<String>,
}

pub fn parse(text: &str) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for line in text.lines() {
        if let Some(head) = line.strip_prefix("## ") {
            // „## 0.7.0 — 2026-10-26” → „0.7.0”
            let version = head.split(" — ").next().unwrap_or(head).trim().to_owned();
            out.push(Entry { version, items: Vec::new() });
        } else if let Some(item) = line.trim_start().strip_prefix("- ") {
            if let Some(entry) = out.last_mut() {
                entry.items.push(item.trim().to_owned());
            }
        } else if line.starts_with("  ") && !line.trim().is_empty() {
            // Kontynuacja punktu w kolejnej linii.
            if let Some(item) = out.last_mut().and_then(|e| e.items.last_mut()) {
                item.push(' ');
                item.push_str(line.trim());
            }
        }
    }
    out.retain(|e| !e.items.is_empty());
    out
}

/// Ostatnie wpisy (przycisk „Co nowego” w ustawieniach).
pub fn recent() -> Vec<Entry> {
    released(CHANGELOG, crate::version()).into_iter().take(4).collect()
}

fn released(text: &str, current: &str) -> Vec<Entry> {
    parse(text).into_iter()
        .filter(|e| e.version != NEXT && (e.version == current || crate::update::is_newer(current, &e.version)))
        .collect()
}

/// Wpisy do pokazania po aktualizacji z `from` (nowsze niż `from`, najwyżej 3 wersje).
pub fn since(from: Option<&str>) -> Vec<Entry> {
    let all = released(CHANGELOG, crate::version());
    let Some(from) = from else { return all.into_iter().take(1).collect() };
    all.into_iter()
        .filter(|e| crate::update::is_newer(&e.version, from))
        .take(3)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sections_and_filters_by_version() {
        let text = "# Tytuł\nopis\n\n## 0.7.0 — 2026-10-26\n\n- **Nowe** coś\n  dalszy ciąg\n- drugie\n\n## 0.6.0\n\n- stare\n";
        let e = parse(text);
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].version, "0.7.0");
        assert_eq!(e[0].items, ["**Nowe** coś dalszy ciąg", "drugie"]);
        assert!(!parse(CHANGELOG).is_empty(), "wbudowany CHANGELOG się parsuje");
    }

    #[test]
    fn whats_new_excludes_unreleased_and_future_changes() {
        let text = "## Następna wersja\n- szkic\n## 0.7.0\n- przyszłość\n## 0.6.6\n- obecna\n## 0.6.5\n- starsza\n";
        let entries = released(text, "0.6.6");
        assert_eq!(entries.iter().map(|e| e.version.as_str()).collect::<Vec<_>>(), ["0.6.6", "0.6.5"]);
        assert!(since(Some(crate::version())).is_empty());
    }
}
