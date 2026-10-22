//! Comparable relevance signals for one ranking across all search sources.
use crate::store::folded;
use std::path::Path;

fn words(text: &str) -> Vec<&str> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect()
}

pub(crate) fn filename(query: &str, name: &str, path: &Path) -> f64 {
    let query = folded(query.trim());
    let name = folded(name);
    if query == name {
        return 1.2;
    }
    let query_words = words(&query);
    if query_words.is_empty() {
        return 0.0;
    }
    let stem = Path::new(&name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&name);
    if words(stem) == query_words {
        return 1.1;
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
    let path = folded(&path.to_string_lossy());
    if query.split_whitespace().all(|term| path.contains(term)) {
        0.15
    } else {
        0.0
    }
}

pub(crate) fn combined(lexical: f64, cosine: Option<f64>) -> f64 {
    // The retrieval floor is 0.30. Do not treat a positive cosine as certainty:
    // unrelated EmbeddingGemma documents can share a substantial cosine baseline.
    let semantic = cosine.map_or(0.0, |score| ((score - 0.30) / 0.70).clamp(0.0, 1.0));
    if lexical >= 1.0 {
        // An exact name/title expresses explicit intent; semantics breaks ties.
        return lexical + 0.05 * semantic;
    }
    let strongest = lexical.max(semantic);
    strongest + 0.5 * lexical.min(semantic) * (1.0 - strongest)
}
