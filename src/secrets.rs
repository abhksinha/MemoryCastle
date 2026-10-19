//! Refusing files that look like secrets. Patterns are deliberately few and
//! obvious; `--force` overrides.

pub fn suspicious_name(name: &str) -> bool {
    let n = name.rsplit('/').next().unwrap_or(name);
    n == ".env"
        || n.starts_with(".env.")
        || n.ends_with(".pem")
        || n.ends_with(".key")
        || n.ends_with(".p12")
        || n.ends_with(".pfx")
        || n == "id_rsa"
        || n == "id_ed25519"
        || n == "credentials"
        || n == "credentials.json"
}

/// Returns a short description of the first secret-like pattern in `text`.
pub fn suspicious_content(text: &str) -> Option<&'static str> {
    if text.contains("PRIVATE KEY-----") {
        return Some("private key block");
    }
    for (i, _) in text.match_indices("AKIA") {
        if text[i..].len() >= 20 && text[i + 4..i + 20].bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()) {
            return Some("AWS access key id");
        }
    }
    for prefix in ["ghp_", "github_pat_", "xoxb-", "xoxp-", "sk-ant-", "sk-proj-", "AIza"] {
        for (i, _) in text.match_indices(prefix) {
            let rest = &text[i + prefix.len()..];
            let run = rest.bytes().take_while(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-').count();
            if run >= 20 {
                return Some("API token");
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects() {
        assert!(suspicious_name("config/.env"));
        assert!(suspicious_name("server.pem"));
        assert!(!suspicious_name("notes/decisions.md"));
        assert!(suspicious_content("-----BEGIN RSA PRIVATE KEY-----").is_some());
        assert!(suspicious_content("key AKIAABCDEFGHIJKLMNOP x").is_some());
        assert!(suspicious_content("token ghp_abcdefghijklmnopqrstuvwxyz0123").is_some());
        assert!(suspicious_content("we tried ghp_ prefix matching, rejected").is_none());
        assert!(suspicious_content("ordinary note about AKIA airline").is_none());
    }
}
