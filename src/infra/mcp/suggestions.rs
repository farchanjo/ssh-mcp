//! Closest-match suggestions for v4.7-step6 `NOT_FOUND` errors.
//!
//! Smaller LLMs frequently typo `SESSION_ID` / `SHELL_ID` /
//! `COMMAND_ID` / `TRANSFER_ID` / `FORWARD_ID` values. Without a hint,
//! the recovery path requires a follow-up `ssh_list_*` round-trip just
//! to discover that `s-abe` was meant rather than `s-abf`. This module
//! provides a tiny pure-Rust Levenshtein distance implementation plus
//! a wrapper that returns the top-N candidates given a target id and
//! an iterator of live ids.
//!
//! ## Cost
//!
//! `O(N * |target| * |max-id|)` per call. With N ≤ 1024 ids and id
//! lengths ≤ 64 bytes the worst case is ~4M character comparisons —
//! well under a millisecond on modern hardware. `NOT_FOUND` is a slow
//! path (the LLM already paid a round-trip), so we trade compute for
//! conversational UX.

/// Levenshtein edit distance between two byte slices.
///
/// Standard dynamic programming table; `O(|a| * |b|)` time and space.
/// Returns `0` when the inputs are byte-equal and `max(|a|, |b|)` for
/// disjoint inputs. Operates on bytes rather than chars: every id we
/// compare comes from server-issued UUID-style identifiers (ASCII), so
/// the byte-level distance is identical to the char-level distance and
/// is faster to compute.
#[must_use]
pub fn levenshtein(a: &str, b: &str) -> usize {
    use std::mem;
    let ab = a.as_bytes();
    let bb = b.as_bytes();
    if ab.is_empty() {
        return bb.len();
    }
    if bb.is_empty() {
        return ab.len();
    }
    let cols = bb.len() + 1;
    // Single rolling buffer keeps space at O(min(|a|, |b|)).
    let mut prev: Vec<usize> = (0..cols).collect();
    let mut curr: Vec<usize> = vec![0; cols];
    for (i, &ca) in ab.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in bb.iter().enumerate() {
            let cost = usize::from(ca != cb);
            // deletion / insertion / substitution
            let del = prev[j + 1] + 1;
            let ins = curr[j] + 1;
            let sub = prev[j] + cost;
            curr[j + 1] = del.min(ins).min(sub);
        }
        mem::swap(&mut prev, &mut curr);
    }
    prev[bb.len()]
}

/// Pick the top `n` closest matches to `target` from `ids`.
///
/// Returns the candidates sorted by ascending Levenshtein distance,
/// breaking ties on lexicographic order so the output is deterministic.
///
/// Excludes exact matches (distance 0) — when the target is already in
/// `ids` the caller would have hit a different branch (`NOT_FOUND` would
/// not have fired).
///
/// Returns at most `n` entries; an empty `ids` iterator yields an empty
/// vector.
#[must_use]
pub fn closest_ids<I, S>(target: &str, ids: I, n: usize) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    if n == 0 {
        return Vec::new();
    }
    let mut scored: Vec<(usize, String)> = ids
        .into_iter()
        .map(|s| {
            let owned = s.as_ref().to_string();
            let d = levenshtein(target, &owned);
            (d, owned)
        })
        .filter(|(d, _)| *d > 0)
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    scored.into_iter().take(n).map(|(_, id)| id).collect()
}

/// Render a `closest matches: a, b, c` suffix to append onto the
/// existing DETAIL line. Returns `None` when `matches` is empty so the
/// caller can leave DETAIL untouched.
#[must_use]
pub fn render_closest_matches(matches: &[String]) -> Option<String> {
    if matches.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(32 + matches.iter().map(String::len).sum::<usize>());
    out.push_str("closest matches: ");
    for (i, m) in matches.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(m);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::{closest_ids, levenshtein, render_closest_matches};

    #[test]
    fn levenshtein_empty_inputs() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("", "abc"), 3);
    }

    #[test]
    fn levenshtein_equal_inputs() {
        assert_eq!(levenshtein("session-1", "session-1"), 0);
    }

    #[test]
    fn levenshtein_single_substitution() {
        assert_eq!(levenshtein("abe", "abf"), 1);
    }

    #[test]
    fn levenshtein_multi_op_distance() {
        // "kitten" -> "sitting": substitute k->s, substitute e->i, insert g.
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }

    #[test]
    fn closest_ids_finds_neighbors() {
        let ids = ["s-abc", "s-abe", "s-abd", "s-xyz", "s-aba"];
        let result = closest_ids("s-abf", ids.iter().copied(), 3);
        assert_eq!(result.len(), 3);
        // s-abe / s-abc / s-abd / s-aba are all distance 1; tie-breaking
        // sorts them lexicographically, so we get s-aba, s-abc, s-abd.
        // s-xyz (distance 3) is excluded.
        assert!(!result.contains(&"s-xyz".to_string()));
        assert_eq!(
            result,
            vec![
                "s-aba".to_string(),
                "s-abc".to_string(),
                "s-abd".to_string(),
            ]
        );
    }

    #[test]
    fn closest_ids_excludes_exact_match() {
        let ids = ["s-abc", "s-abd"];
        let result = closest_ids("s-abc", ids.iter().copied(), 3);
        assert_eq!(result, vec!["s-abd".to_string()]);
    }

    #[test]
    fn closest_ids_empty_when_input_empty() {
        let ids: [&str; 0] = [];
        let result = closest_ids("missing", ids.iter().copied(), 3);
        assert!(result.is_empty());
    }

    #[test]
    fn closest_ids_caps_at_n() {
        let ids: Vec<String> = (0..50_u32).map(|i| format!("s-{i:02}")).collect();
        let result = closest_ids("s-04", ids.iter(), 3);
        assert!(result.len() <= 3);
    }

    #[test]
    fn render_closest_matches_returns_none_for_empty() {
        assert!(render_closest_matches(&[]).is_none());
    }

    #[test]
    fn render_closest_matches_joins_with_comma_space() {
        let v = vec!["s-1".to_string(), "s-2".to_string()];
        assert_eq!(
            render_closest_matches(&v),
            Some("closest matches: s-1, s-2".to_string())
        );
    }
}
