use once_cell::sync::Lazy;
use regex::Regex;

#[derive(Debug, Clone, PartialEq)]
pub enum PiiPattern {
    Email,
    IpV4,
    IpV6,
    CreditCard,
    Iban,
    PhoneNumber,
    SocialSecurityNumber,
}

#[derive(Debug, Clone)]
pub struct RedactionResult {
    pub redacted_text: String,
    #[allow(dead_code)]
    pub hits: Vec<PiiPattern>,
    pub hit_count: usize,
}

// Module-level Lazy static regex patterns
static EMAIL_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"[a-zA-Z0-9._%+\-]+@[a-zA-Z0-9.\-]+\.[a-zA-Z]{2,}").expect("static regex is valid")
});

static IPV4_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\b(?:(?:25[0-5]|2[0-4]\d|[01]?\d\d?)\.){3}(?:25[0-5]|2[0-4]\d|[01]?\d\d?)\b")
        .expect("static regex is valid")
});

static IPV6_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"([0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4}").expect("static regex is valid")
});

static CREDIT_CARD_REGEX: Lazy<Regex> = Lazy::new(|| {
    // Handle common credit card formats with optional dashes or spaces
    Regex::new(r"\b(?:4[0-9]{3}[- ]?[0-9]{4}[- ]?[0-9]{4}[- ]?[0-9]{4}|5[1-5][0-9]{2}[- ]?[0-9]{4}[- ]?[0-9]{4}[- ]?[0-9]{4}|3[47][0-9]{2}[- ]?[0-9]{6}[- ]?[0-9]{5}|3(?:0[0-5]|[68][0-9])[0-9][- ]?[0-9]{6}[- ]?[0-9]{4}|6(?:011|5[0-9]{2})[- ]?[0-9]{4}[- ]?[0-9]{4}[- ]?[0-9]{4})\b")
        .expect("static regex is valid")
});

static IBAN_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\b[A-Z]{2}[0-9]{2}[A-Z0-9]{4}[0-9]{7}([A-Z0-9]?){0,16}\b")
        .expect("static regex is valid")
});

static PHONE_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\b(\+?[1-9]\d{0,2}[\s.\-]?)?\(?\d{3}\)?[\s.\-]?\d{3}[\s.\-]?\d{4}\b")
        .expect("static regex is valid")
});

static SSN_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\b\d{3}-\d{2}-\d{4}\b").expect("static regex is valid"));

#[derive(Debug, Clone)]
pub struct Redactor;

impl Default for Redactor {
    fn default() -> Self {
        Self::new()
    }
}

impl Redactor {
    pub fn new() -> Self {
        Self
    }

    pub fn redact(&self, input: &str) -> RedactionResult {
        let mut hits = Vec::new();
        let mut current_text = input.to_string();
        let mut total_hits = 0;

        // Early-exit heuristics for obviously PII-free strings
        // Email requires '@', SSN/IBAN/CC/IP/Phone require digits
        let has_at_symbol = current_text.contains('@');
        let has_digits = current_text.chars().any(|c| c.is_ascii_digit());

        // Processing order: most specific first
        // 1. SSN - requires digits and dashes
        if has_digits && current_text.contains('-') {
            let matches: Vec<_> = SSN_REGEX.find_iter(&current_text).collect();
            if !matches.is_empty() {
                total_hits += matches.len();
                hits.push(PiiPattern::SocialSecurityNumber);
                current_text = SSN_REGEX.replace_all(&current_text, "[SSN]").into_owned();
            }
        }

        // 2. IBAN - requires uppercase letters and digits
        if has_digits && current_text.chars().any(|c| c.is_ascii_uppercase()) {
            let matches: Vec<_> = IBAN_REGEX.find_iter(&current_text).collect();
            if !matches.is_empty() {
                total_hits += matches.len();
                hits.push(PiiPattern::Iban);
                current_text = IBAN_REGEX.replace_all(&current_text, "[IBAN]").into_owned();
            }
        }

        // 3. CreditCard - requires digits and possibly dashes/spaces
        if has_digits {
            let matches: Vec<_> = CREDIT_CARD_REGEX.find_iter(&current_text).collect();
            if !matches.is_empty() {
                total_hits += matches.len();
                hits.push(PiiPattern::CreditCard);
                current_text = CREDIT_CARD_REGEX
                    .replace_all(&current_text, "[CREDIT_CARD]")
                    .into_owned();
            }
        }

        // 4. IpV6 - requires colons and hex digits
        if current_text.contains(':') {
            let matches: Vec<_> = IPV6_REGEX.find_iter(&current_text).collect();
            if !matches.is_empty() {
                total_hits += matches.len();
                hits.push(PiiPattern::IpV6);
                current_text = IPV6_REGEX.replace_all(&current_text, "[IPv6]").into_owned();
            }
        }

        // 5. IpV4 - requires digits and dots
        if has_digits && current_text.contains('.') {
            let matches: Vec<_> = IPV4_REGEX.find_iter(&current_text).collect();
            if !matches.is_empty() {
                total_hits += matches.len();
                hits.push(PiiPattern::IpV4);
                current_text = IPV4_REGEX.replace_all(&current_text, "[IPv4]").into_owned();
            }
        }

        // 6. Email - requires '@' symbol
        if has_at_symbol {
            let matches: Vec<_> = EMAIL_REGEX.find_iter(&current_text).collect();
            if !matches.is_empty() {
                total_hits += matches.len();
                hits.push(PiiPattern::Email);
                current_text = EMAIL_REGEX
                    .replace_all(&current_text, "[EMAIL]")
                    .into_owned();
            }
        }

        // 7. PhoneNumber - requires digits
        if has_digits {
            let matches: Vec<_> = PHONE_REGEX.find_iter(&current_text).collect();
            if !matches.is_empty() {
                total_hits += matches.len();
                hits.push(PiiPattern::PhoneNumber);
                current_text = PHONE_REGEX
                    .replace_all(&current_text, "[PHONE]")
                    .into_owned();
            }
        }

        RedactionResult {
            redacted_text: current_text,
            hit_count: total_hits,
            hits,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_email_redaction() {
        let redactor = Redactor::new();
        let result = redactor.redact("user@example.com");
        assert_eq!(result.redacted_text, "[EMAIL]");
        assert_eq!(result.hit_count, 1);
        assert!(result.hits.contains(&PiiPattern::Email));
    }

    #[test]
    fn test_ipv4_redaction() {
        let redactor = Redactor::new();
        let result = redactor.redact("from 192.168.1.1");
        assert_eq!(result.redacted_text, "from [IPv4]");
        assert_eq!(result.hit_count, 1);
        assert!(result.hits.contains(&PiiPattern::IpV4));
    }

    #[test]
    fn test_ssn_redaction() {
        let redactor = Redactor::new();
        let result = redactor.redact("SSN 123-45-6789");
        assert_eq!(result.redacted_text, "SSN [SSN]");
        assert_eq!(result.hit_count, 1);
        assert!(result.hits.contains(&PiiPattern::SocialSecurityNumber));
    }

    #[test]
    fn test_phone_redaction() {
        let redactor = Redactor::new();
        let result = redactor.redact("call 555-867-5309");
        assert_eq!(result.redacted_text, "call [PHONE]");
        assert_eq!(result.hit_count, 1);
        assert!(result.hits.contains(&PiiPattern::PhoneNumber));
    }

    #[test]
    fn test_credit_card_redaction() {
        let redactor = Redactor::new();
        // Test with a Visa card number
        let result = redactor.redact("4111-1111-1111-1111");
        assert_eq!(result.redacted_text, "[CREDIT_CARD]");
        assert_eq!(result.hit_count, 1);
        assert!(result.hits.contains(&PiiPattern::CreditCard));
    }

    #[test]
    fn test_iban_redaction() {
        let redactor = Redactor::new();
        let result = redactor.redact("IBAN DE89370400440532013000");
        assert_eq!(result.redacted_text, "IBAN [IBAN]");
        assert_eq!(result.hit_count, 1);
        assert!(result.hits.contains(&PiiPattern::Iban));
    }

    #[test]
    fn test_multiple_pii_same_msg() {
        let redactor = Redactor::new();
        let result = redactor.redact("Email: user@example.com from 192.168.1.1");
        assert_eq!(result.redacted_text, "Email: [EMAIL] from [IPv4]");
        assert_eq!(result.hit_count, 2);
        assert!(result.hits.contains(&PiiPattern::Email));
        assert!(result.hits.contains(&PiiPattern::IpV4));
    }

    #[test]
    fn test_no_pii() {
        let redactor = Redactor::new();
        let result = redactor.redact("Hello World");
        assert_eq!(result.redacted_text, "Hello World");
        assert_eq!(result.hit_count, 0);
        assert!(result.hits.is_empty());
    }

    #[test]
    fn test_hit_count_accuracy() {
        let redactor = Redactor::new();
        let result = redactor.redact("user1@example.com user2@example.com user3@example.com");
        assert_eq!(result.hit_count, 3);
        // Check that all emails are redacted
        assert_eq!(result.redacted_text, "[EMAIL] [EMAIL] [EMAIL]");
    }
}
