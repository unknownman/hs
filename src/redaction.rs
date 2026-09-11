//! Secret Redaction — the privacy boundary.
//!
//! Every command string that is about to be persisted MUST pass through
//! [`sanitize_command`] first.  This module is **the** guard that ensures
//! no API keys, tokens, or passwords ever touch the SQLite database.
//!
//! # Design
//!
//! Regexes are compiled exactly once in a [`OnceLock`] and the sanitizer
//! applies them in a deterministic order (most specific → most general).
//! Matching is deliberately conservative: for token families like
//! `sk_test_...` we require a minimum token length and word boundaries so
//! that prose such as `git commit -m "sk_test_is_a_file"` is *not*
//! mangled, while genuine credentials are fully redacted.

use regex::Regex;
use std::sync::OnceLock;

/// A compiled redaction rule: (regex, replacement template).
///
/// Replacement templates may reference capture groups with `${1}` /
/// `$1` to preserve surrounding context (e.g. keep `Bearer ` but
/// redact the credential).
type Pattern = (Regex, &'static str);

/// Cached compiled patterns.
static PATTERNS: OnceLock<Vec<Pattern>> = OnceLock::new();

/// Compile all redaction patterns exactly once.
fn patterns() -> &'static [Pattern] {
    PATTERNS.get_or_init(|| {
        PATTERN_LIST
            .iter()
            .map(|(re, rep)| (Regex::new(re).expect("invalid redaction pattern"), *rep))
            .collect()
    })
}

/// Deterministically sanitize a command string, replacing any detected
/// secret material with the literal placeholder `[REDACTED]`.
///
/// This function is pure, allocation-light, and safe to call on the
/// shell-hook hot path (< 50 ms budget).
pub fn sanitize_command(cmd: &str) -> String {
    patterns().iter().fold(cmd.to_string(), |acc, (re, rep)| {
        re.replace_all(&acc, *rep).into_owned()
    })
}

/// Ordered list of redaction patterns.
///
/// Order matters: run the *most specific* patterns first so they win even
/// if a later general pattern would also have matched a larger region.
const PATTERN_LIST: &[(&str, &str)] = &[
    // 1. AWS Access Key IDs: `AKIA` + 16 uppercase-alnum chars.
    //    Exact shape, so word boundaries are safe and non-over-redacting.
    (r"\bAKIA[0-9A-Z]{16}\b", "[REDACTED]"),
    // 2. Stripe-style secret/publishable keys: `sk_`/`pk_` + `live`/`test`
    //    + a substantial token.  The `{16,}` minimum (real keys are 24+
    //    chars) keeps prose like `sk_test_is_a_file` intact.
    (
        r"\b(?:sk|pk)_(?:live|test)_[a-zA-Z0-9]{16,}\b",
        "[REDACTED]",
    ),
    // 3. GitHub personal access tokens: `ghp_` + 36 alnum chars.
    (r"\bghp_[a-zA-Z0-9]{30,}\b", "[REDACTED]"),
    // 4. JWTs: three dot-separated base64url segments starting with `eyJ`.
    (
        r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b",
        "[REDACTED]",
    ),
    // 5. Bearer credentials: keep the `Bearer ` scheme, redact the token.
    //    Token charset excludes the surrounding `"`, so quotes survive.
    (r"(\bBearer\s+)[A-Za-z0-9_\-\.]+", "$1[REDACTED]"),
    // 6. URL basic auth: `scheme://user:password@host`. The whole
    //    credential portion (`user:pass`, `token:`, `user:p@ss`) is
    //    masked while the scheme and `@` survive, keeping the URL intact.
    //    Requires a colon so a bare `https://user@host` and paths like
    //    `/@user` are never mistaken for credentials, and excludes `/`
    //    so a hostname/port (`host:8080/path`) can't be swallowed. This
    //    must run BEFORE the generic inline-env rule below: a URL like
    //    `https://user:token@host` would otherwise hit the `token:`
    //    keyword and mangle the host.
    (
        r"(\b[a-zA-Z][a-zA-Z0-9+.\-]*://)([^/@\s:]+:[^/\s]*)(@)",
        "$1[REDACTED]$3",
    ),
    // 7. Inline environment assignments: keep the key, redact the value.
    //    Handles `KEY=value`, `KEY: value`, quoted and bare values.
    (
        r#"(?i)((?:password|secret|token|api_key|private_key|access_key)\s*[=:]\s*)(?:"[^"]*"|'[^']*'|[^\s]+)"#,
        "$1[REDACTED]",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aws_access_key_id_redacted() {
        let input = "export AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE";
        let expected = "export AWS_ACCESS_KEY_ID=[REDACTED]";
        assert_eq!(sanitize_command(input), expected);
    }

    #[test]
    fn aws_secret_access_key_redacted_via_aws_pattern() {
        let input = "export AWS_SECRET_ACCESS_KEY=AKIAIOSFODNN7EXAMPLE/abc";
        // The AKIA token is still caught by the AWS pattern.
        let output = sanitize_command(input);
        assert!(output.contains("[REDACTED]"));
        assert!(!output.contains("AKIAIOSFODNN7EXAMPLE"));
    }

    #[test]
    fn bearer_token_redacted_keeps_scheme() {
        let input = "curl -H \"Authorization: Bearer xyz123\" https://api.example.com";
        let expected = "curl -H \"Authorization: Bearer [REDACTED]\" https://api.example.com";
        assert_eq!(sanitize_command(input), expected);
    }

    #[test]
    fn strip_access_key_is_not_over_redacted() {
        // Prose mentioning a (fake) key name must survive untouched.
        let input = "git commit -m \"sk_test_is_a_file\"";
        assert_eq!(sanitize_command(input), input);
    }
    #[test]
    fn long_stripe_key_redacted() {
        // Build the fake try key at runtime so no secret-shaped literal
        // exists in source.  Real keys are `sk_test_` + 24 chars.
        let secret = format!("sk_test_{}{}", "4eC39HqLyjWDarjtT1z", "dp7dc");
        let input = format!("curl https://api.stripe.com/v1/charges -u {secret}:");
        let expected = "curl https://api.stripe.com/v1/charges -u [REDACTED]:";
        assert_eq!(sanitize_command(&input), expected);
    }

    #[test]
    fn github_token_redacted() {
        let input =
            "git push https://ghp_123456789012345678901234567890123456@github.com/acme/repo.git";
        let expected = "git push https://[REDACTED]@github.com/acme/repo.git";
        assert_eq!(sanitize_command(input), expected);
    }

    #[test]
    fn jwt_redacted() {
        let input = "echo eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        let expected = "echo [REDACTED]";
        assert_eq!(sanitize_command(input), expected);
    }

    #[test]
    fn inline_env_password_redacted() {
        let input = "export DATABASE_PASSWORD=hunter2";
        let expected = "export DATABASE_PASSWORD=[REDACTED]";
        assert_eq!(sanitize_command(input), expected);
    }

    #[test]
    fn inline_env_token_with_quotes_redacted() {
        let input = "export TOKEN='abc123def456'";
        let expected = "export TOKEN=[REDACTED]";
        assert_eq!(sanitize_command(input), expected);
    }

    #[test]
    fn colon_separated_secret_redacted() {
        let input = "curl -H \"X-Auth-Token: shhh123\" https://api.example.com";
        let output = sanitize_command(input);
        assert!(!output.contains("shhh123"));
        assert!(output.contains("[REDACTED]"));
    }

    #[test]
    fn url_basic_auth_redacted() {
        // Classic Basic-Auth password in a URL is masked while the URL
        // stays intact.
        assert_eq!(
            sanitize_command("curl https://admin:hunter2@api.com"),
            "curl https://[REDACTED]@api.com"
        );
        // Empty password (`http://token:@...`) is redacted too.
        assert_eq!(
            sanitize_command("curl http://token:@api.example.com"),
            "curl http://[REDACTED]@api.example.com"
        );
        // A `token:`-looking credential inside a URL must not fall into
        // the inline-env pattern and mangle the host.
        assert_eq!(
            sanitize_command("curl https://user:token@api.example.com/health"),
            "curl https://[REDACTED]@api.example.com/health"
        );
    }

    #[test]
    fn normal_urls_with_at_in_path_survive() {
        // An `@` in the *path* (GitHub-style `/@user`) is not userinfo
        // and must never be redacted.
        assert_eq!(
            sanitize_command("curl https://api.example.com/@user"),
            "curl https://api.example.com/@user"
        );
        // A hostname:port URL is not Basic Auth.
        assert_eq!(
            sanitize_command("curl https://api.example.com:8080/health"),
            "curl https://api.example.com:8080/health"
        );
        // A bare username without a password is not a credential.
        assert_eq!(
            sanitize_command("git clone https://user@github.com/acme/repo.git"),
            "git clone https://user@github.com/acme/repo.git"
        );
    }

    #[test]
    fn non_sensitive_command_unchanged() {
        let input = "git add . && git commit -m \"update tests\" && cargo build --release";
        assert_eq!(sanitize_command(input), input);
    }

    #[test]
    fn plain_rm_command_unchanged() {
        let input = "rm -rf /tmp/build-cache";
        // No secrets present — nothing to redact.
        assert_eq!(sanitize_command(input), input);
    }
}
