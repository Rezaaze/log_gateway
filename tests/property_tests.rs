use log_gateway::cache::SemanticCache;
use log_gateway::redactor::Redactor;
use proptest::prelude::*;

// ── Deterministische Einzelfälle ─────────────────────────────────────────────

#[test]
fn redactor_empty_input_produces_no_hits() {
    let redactor = Redactor::new();
    let result = redactor.redact("");
    assert_eq!(result.hit_count, 0);
    assert_eq!(result.redacted_text, "");
    assert!(result.hits.is_empty());
}

// ── SemanticCache::make_key Invarianten ──────────────────────────────────────

proptest! {
    /// Gleicher Input → gleicher Key (Determinismus)
    #[test]
    fn prop_cache_key_deterministic(s in ".*") {
        let k1 = SemanticCache::make_key(&s);
        let k2 = SemanticCache::make_key(&s);
        prop_assert_eq!(k1, k2);
    }

    /// Key ist immer 64 Hex-Zeichen (SHA-256)
    #[test]
    fn prop_cache_key_is_64_hex_chars(s in ".*") {
        let key = SemanticCache::make_key(&s);
        prop_assert_eq!(key.len(), 64);
        prop_assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// Groß-/Kleinschreibung erzeugt gleichen Key
    #[test]
    fn prop_cache_key_case_insensitive(s in "[a-zA-Z0-9 ]{1,64}") {
        let lower = SemanticCache::make_key(&s.to_lowercase());
        let upper = SemanticCache::make_key(&s.to_uppercase());
        prop_assert_eq!(lower, upper);
    }

    /// Trim-Invarianz: Leerzeichen am Rand ändern den Key nicht
    #[test]
    fn prop_cache_key_trim_invariant(s in "[a-z0-9]{1,32}") {
        let base  = SemanticCache::make_key(&s);
        let left  = SemanticCache::make_key(&format!("   {s}"));
        let right = SemanticCache::make_key(&format!("{s}   "));
        let both  = SemanticCache::make_key(&format!("  {s}  "));
        prop_assert_eq!(&base, &left);
        prop_assert_eq!(&base, &right);
        prop_assert_eq!(&base, &both);
    }
}

// ── Redactor Invarianten ─────────────────────────────────────────────────────

proptest! {
    /// Beliebiger Text ohne PII → hit_count == 0 und Text bleibt unverändert
    /// Strategie: nur Buchstaben/Leerzeichen — kein Zeichen das PII-Pattern triggern kann
    #[test]
    fn prop_redactor_no_pii_text_unchanged(
        s in "[a-zA-Z ]{1,200}"
    ) {
        let redactor = Redactor::new();
        let result = redactor.redact(&s);
        prop_assert_eq!(result.hit_count, 0);
        prop_assert_eq!(result.redacted_text, s);
        prop_assert!(result.hits.is_empty());
    }


    /// hit_count ist immer >= hits.len()
    /// (ein Pattern kann mehrfach treffen, hits enthält jeden Typ nur einmal)
    #[test]
    fn prop_redactor_hit_count_ge_hits_len(s in ".*") {
        let redactor = Redactor::new();
        let result = redactor.redact(&s);
        prop_assert!(result.hit_count >= result.hits.len());
    }

    /// Redacted text enthält niemals '[' am Anfang eines PII-Tokens ohne ']'
    /// Grundlegende Ausgabe-Wohlgeformtheit
    #[test]
    fn prop_redactor_brackets_balanced(s in ".*") {
        let redactor = Redactor::new();
        let result = redactor.redact(&s);

        // Count brackets in input
        let open_before = s.chars().filter(|&c| c == '[').count();
        let close_before = s.chars().filter(|&c| c == ']').count();

        // Count brackets in output
        let open_after = result.redacted_text.chars().filter(|&c| c == '[').count();
        let close_after = result.redacted_text.chars().filter(|&c| c == ']').count();

        // The redactor adds complete [TOKEN] pairs, so the difference
        // between opening and closing brackets should remain the same
        prop_assert_eq!(
            open_after as i32 - close_after as i32,
            open_before as i32 - close_before as i32
        );
    }

    /// Doppelte Redaktion ist idempotent — Text ändert sich nicht beim zweiten Durchlauf
    #[test]
    fn prop_redactor_idempotent(s in ".*") {
        let redactor = Redactor::new();
        let first  = redactor.redact(&s);
        let second = redactor.redact(&first.redacted_text);
        prop_assert_eq!(first.redacted_text, second.redacted_text);
    }
}
