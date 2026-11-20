//! Comparable relevance signals for one ranking across all search sources.
use std::path::Path;

pub(crate) const SEMANTIC_FLOOR: f64 = 0.30;

/// Every query word starts a word of the filename: the user is naming a file.
/// Content evidence may lift such files, but must not reorder equally named ones.
pub(crate) const KNOWN_NAME: f64 = 0.50;

/// Metadata already supplied by GNOME; no activation-history database.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct SearchMetadata {
    pub fts_rank: f64,
    pub modified: i64,
    pub accessed: i64,
}

fn comparable(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    glib::casefold(text)
        .nfkd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .collect()
}

/// NautilusQuery's global filename match: prefer earlier, tighter matches.
pub(crate) fn name_match(query: &str, name: &str) -> f64 {
    let name = comparable(name);
    let query = comparable(query);
    let mut tail = 0;
    let mut position = 0;
    let mut found = false;
    for word in query.split_whitespace() {
        let Some(offset) = name.find(word) else {
            return 0.0;
        };
        position = offset;
        tail += name.len() - offset - word.len();
        found = true;
    }
    if found {
        (50.0 - position as f64 - tail as f64 / 100.0).max(10.0)
    } else {
        0.0
    }
}

/// Same global recency bands as NautilusSearchHit (query_location = NULL).
pub(crate) fn recency(metadata: &SearchMetadata, now: i64) -> f64 {
    let recent = metadata.modified.max(metadata.accessed);
    if recent <= 0 {
        return 0.0;
    }
    let days = now.saturating_sub(recent).max(0) / 86400;
    match days {
        0..=1 => 100.0,
        2..=7 => 70.0,
        8..=14 => 50.0,
        15..=30 => 30.0,
        31..=90 => 10.0,
        _ => 0.0,
    }
}

pub(crate) fn context(query: &str, name: &str, metadata: &SearchMetadata, now: i64) -> f64 {
    // Secondary to the hard exact-name/title bands (3 / 2). Ordinary matches
    // are soft evidence: like GNOME global search, recency may outweigh a small
    // prefix/word-quality difference. Do not shrink recency into a tie-break.
    let fts = if metadata.fts_rank.is_finite() {
        metadata.fts_rank.clamp(0.0, 50.0)
    } else {
        0.0
    };
    0.001 * (name_match(query, name) + fts + recency(metadata, now)) + fresher(metadata, now)
}

/// Inside one GNOME recency band, the newer copy should still win. This stays
/// below one filename-position step (0.001), so it only orders otherwise-tied names.
fn fresher(metadata: &SearchMetadata, now: i64) -> f64 {
    let recent = metadata.modified.max(metadata.accessed);
    if recent <= 0 {
        return 0.0;
    }
    let days = now.saturating_sub(recent).max(0) as f64 / 86400.0;
    0.0009 / (1.0 + days)
}

/// Strip the suffixes file managers and browsers add to duplicates:
/// `(1)`, `(copy)`, `(another copy)`, `(3rd copy)`, ` - Copy`, ` copy 2`.
fn without_copy_marker(stem: &str) -> &str {
    fn marker(inner: &str) -> bool {
        let inner = inner.trim();
        let ordinal = |n: &str| {
            let digits = n.trim_end_matches(|c: char| c.is_ascii_alphabetic());
            !digits.is_empty()
                && digits.chars().all(|c| c.is_ascii_digit())
                && n.len() == digits.len() + 2
        };
        inner == "copy"
            // "(1)".."(999)"; a year such as "(2024)" is part of the title.
            || (1..=3).contains(&inner.len()) && inner.chars().all(|c| c.is_ascii_digit())
            || inner
                .strip_suffix(" copy")
                .is_some_and(|n| n == "another" || ordinal(n))
    }
    let separators: &[char] = &[' ', '-', '_'];
    let mut stem = stem.trim_end();
    loop {
        let before = stem;
        // "(1)", "(copy)", "(another copy)", "(3rd copy)"
        if let Some((base, inner)) = stem.strip_suffix(')').and_then(|s| s.rsplit_once('('))
            && marker(inner)
        {
            stem = base.trim_end();
        }
        // " - Copy", " copy", " copy 2"
        let unnumbered = stem
            .trim_end_matches(|c: char| c.is_ascii_digit())
            .trim_end();
        for candidate in [stem, unnumbered] {
            if let Some(base) = candidate.strip_suffix("copy")
                && base.ends_with(separators)
                && !base.trim_end_matches(separators).is_empty()
            {
                stem = base.trim_end_matches(separators);
                break;
            }
        }
        if stem == before {
            return stem;
        }
    }
}

fn words(text: &str) -> Vec<&str> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect()
}

pub(crate) fn filename(query: &str, name: &str, path: &Path) -> f64 {
    let query = comparable(query.trim());
    let name = comparable(name);
    if query == name {
        return 3.0;
    }
    let query_words = words(&query);
    if query_words.is_empty() {
        return 0.0;
    }
    let stem = Path::new(&name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&name);
    // A downloaded or duplicated copy ("Passport (1).pdf") names the same file.
    if words(stem) == query_words || words(without_copy_marker(stem)) == query_words {
        return 2.0;
    }
    let name_words = words(&name);
    if name_words
        .windows(query_words.len())
        .any(|part| part == query_words)
    {
        return 0.70;
    }
    if query_words.iter().all(|word| name_words.contains(word)) {
        return 0.60;
    }
    if query_words
        .iter()
        .all(|term| name_words.iter().any(|word| word.starts_with(term)))
    {
        return 0.50;
    }
    if query.split_whitespace().all(|term| name.contains(term)) {
        return 0.35;
    }
    let path = comparable(&path.to_string_lossy());
    if query.split_whitespace().all(|term| path.contains(term)) {
        0.15
    } else {
        0.0
    }
}

/// Lexicographic name buckets, as in Meilisearch's bucket sort or Typesense's
/// text-match buckets. Files in one bucket match the typed name equally well, so
/// only GNOME's metadata (recency, LocalSearch rank, match position) orders them.
/// Document text and embeddings move the whole bucket relative to content-only
/// results; they never choose which copy of a known file comes first, because
/// that evidence exists only for copies GSeek has already indexed.
///
/// Returns the bucket key and its base lexical score.
pub(crate) fn name_bucket(name: f64) -> Option<(u8, f64)> {
    if name >= 3.0 {
        Some((4, 3.0))
    } else if name >= 2.0 {
        Some((3, 2.0))
    } else if name >= 0.60 {
        // Every query word is a whole word of the name.
        Some((2, 0.70))
    } else if name >= KNOWN_NAME {
        // Every query word starts a word of the name (e.g. while still typing).
        Some((1, KNOWN_NAME))
    } else {
        None
    }
}

/// A phrase match ("tax return") is slightly better than scattered words, but
/// stays below one GNOME recency band (0.01) so a recent copy still wins.
pub(crate) fn word_quality(name: f64) -> f64 {
    if (0.60..1.0).contains(&name) {
        0.03 * (name - 0.60)
    } else {
        0.0
    }
}

/// Lexical score from filename evidence plus relative BM25 document text.
pub(crate) fn lexical(name: f64, text: f64) -> f64 {
    if name > 0.0 {
        name.max(0.35 + 0.20 * text) + 0.12 * text
    } else {
        0.35 + 0.20 * text
    }
}

// Keep explicit name/title intent above semantic scores, while allowing strong
// meaning matches to beat incidental lexical evidence. Rank-only fusion would
// discard that distinction. The 0.30 baseline and Store's 0.60/0.12 filtering
// policy belong to EmbeddingGemma; revalidate them when changing models.
pub(crate) fn combined(lexical: f64, cosine: Option<f64>) -> f64 {
    // The retrieval floor is 0.30. Do not treat a positive cosine as certainty:
    // unrelated EmbeddingGemma documents can share a substantial cosine baseline.
    let semantic = cosine.map_or(0.0, |score| {
        ((score - SEMANTIC_FLOOR) / (1.0 - SEMANTIC_FLOOR)).clamp(0.0, 1.0)
    });
    if lexical >= 1.0 {
        // An exact name/title expresses explicit intent; semantics breaks ties.
        return lexical + 0.05 * semantic;
    }
    let strongest = lexical.max(semantic);
    strongest + 0.5 * lexical.min(semantic) * (1.0 - strongest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_names_keep_priority_under_maximum_secondary_evidence() {
        let now = 1_800_000_000;
        let recent = SearchMetadata {
            fts_rank: 50.0,
            modified: now,
            accessed: now,
        };
        let path = Path::new("/tmp/report.pdf.txt");
        let full = combined(filename("report.pdf", "report.pdf", path), None);
        let title = combined(filename("report.pdf", "report.pdf.txt", path), Some(1.0))
            + context("report.pdf", "report.pdf.txt", &recent, now)
            + 0.12;
        assert!(full > title);
        let title = combined(filename("report", "report.pdf", path), None);
        let partial = combined(filename("report", "report archive.pdf", path), Some(1.0))
            + context("report", "report archive.pdf", &recent, now)
            + 0.12;
        assert!(title > partial);
    }

    #[test]
    fn copy_suffixes_name_the_same_file() {
        let path = Path::new("/tmp/x");
        for name in [
            "Passport 2025 (1).pdf",
            "Passport 2025 (copy).pdf",
            "Passport 2025 (another copy).pdf",
            "Passport 2025 (3rd copy).pdf",
            "Passport 2025 - Copy.pdf",
            "Passport 2025 - Copy (2).pdf",
            "Passport 2025 copy 2.pdf",
            "passport_2025_copy.PDF",
        ] {
            assert_eq!(filename("passport 2025", name, path), 2.0, "{name}");
        }
        // Real words and numbers that are part of the title are kept.
        assert!(filename("chapter", "Chapter 2.pdf", path) < 1.0);
        assert!(filename("copy", "copy.txt", path) == 2.0);
        assert!(filename("passport", "Passport 2025.pdf", path) < 1.0);
        assert!(filename("passport 2025", "Passport 2025 (draft).pdf", path) < 1.0);
        assert!(filename("report", "Report (2024).pdf", path) < 1.0);
    }

    #[test]
    fn accented_path_matching_uses_the_same_normalization_as_the_query() {
        assert_eq!(
            filename("café docs", "docs.txt", Path::new("/tmp/café/docs.txt")),
            filename("cafe docs", "docs.txt", Path::new("/tmp/cafe/docs.txt"))
        );
        assert!(filename("café docs", "docs.txt", Path::new("/tmp/café/docs.txt")) > 0.0);
    }

    #[test]
    fn ordinary_filename_evidence_allows_gnome_recency_to_resolve_close_matches() {
        let now = 1_800_000_000;
        let recent = SearchMetadata {
            fts_rank: 50.0,
            modified: now,
            accessed: now,
        };
        let old = SearchMetadata::default();
        let path = Path::new("/tmp/file.txt");
        let older_words = filename("project plan", "plan project.txt", path)
            + context("project plan", "plan project.txt", &old, now);
        let recent_prefix = filename("project plan", "planning project.txt", path)
            + context("project plan", "planning project.txt", &recent, now);
        assert!(recent_prefix > older_words);
    }
}
