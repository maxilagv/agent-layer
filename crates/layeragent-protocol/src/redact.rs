//! Secret detection and redaction applied before content leaves the trust boundary
//! (model prompts, events, memories, artifacts, skill drafts).
use regex::Regex;
use serde_json::Value;
use std::sync::{Mutex, OnceLock};

struct Rule {
    kind: &'static str,
    re: Regex,
    /// Capture group holding the secret; 0 replaces the whole match.
    group: usize,
}

fn rules() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let table: &[(&str, &str, usize)] = &[
            ("private_key", r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?(?:-----END [A-Z ]*PRIVATE KEY-----|\z)", 0),
            ("aws_access_key", r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b", 0),
            ("github_token", r"\b(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{50,})\b", 0),
            ("anthropic_key", r"\bsk-ant-[A-Za-z0-9_\-]{20,}", 0),
            ("api_key", r"\bsk-(?:proj-|live-|test-)?[A-Za-z0-9_\-]{24,}", 0),
            ("slack_token", r"\bxox[abprs]-[A-Za-z0-9\-]{10,}", 0),
            ("google_key", r"\bAIza[0-9A-Za-z_\-]{35}\b", 0),
            ("stripe_key", r"\b[rs]k_(?:live|test)_[0-9A-Za-z]{16,}\b", 0),
            ("jwt", r"\beyJ[A-Za-z0-9_\-]{8,}\.eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}", 0),
            ("bearer", r"(?i)\bbearer\s+([A-Za-z0-9._~+/\-]{16,}=*)", 1),
            ("url_credentials", r"(?i)\b[a-z][a-z0-9+.\-]*://[^\s/:@]+:([^\s/@]{3,})@", 1),
            (
                "assigned_secret",
                r#"(?i)\b[\w.\-]*(?:secret|token|passw(?:or)?d|passwd|api[_\-]?key|apikey|private[_\-]?key|credential|auth)[\w.\-]*["']?\s*[:=]\s*["']?([^\s"',;&]{8,})"#,
                1,
            ),
            ("auth_header", r"(?i)\bauthorization\s*:\s*(?:basic|token)\s+([A-Za-z0-9._~+/\-]{8,}=*)", 1),
        ];
        table
            .iter()
            .filter_map(|(kind, pattern, group)| {
                Regex::new(pattern).ok().map(|re| Rule { kind, re, group: *group })
            })
            .collect()
    })
}

fn registered() -> &'static Mutex<Vec<String>> {
    static VALUES: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    VALUES.get_or_init(Default::default)
}

/// Registers an exact secret value (for example a configured API key) so that it is
/// masked wherever it appears, even without a recognizable shape.
pub fn register_secret(value: &str) {
    if value.is_empty() {
        return;
    }
    if let Ok(mut values) = registered().lock()
        && !values.iter().any(|known| known == value)
    {
        values.push(value.to_owned());
    }
}

fn placeholder(kind: &str) -> String {
    format!("[REDACTED:{kind}]")
}

/// Returns the text with secrets masked and the number of replacements made.
pub fn redact(text: &str) -> (String, usize) {
    let mut out = text.to_owned();
    let mut count = 0;
    if let Ok(values) = registered().lock() {
        for value in values.iter() {
            let hits = out.matches(value.as_str()).count();
            if hits > 0 {
                out = out.replace(value.as_str(), &placeholder("registered"));
                count += hits;
            }
        }
    }
    for rule in rules() {
        let mut replaced = 0;
        let next = rule.re.replace_all(&out, |caps: &regex::Captures| {
            let whole = &caps[0];
            let secret = caps.get(rule.group).map_or(whole, |m| m.as_str());
            // Already masked, or a value that is itself a placeholder.
            if secret.starts_with("[REDACTED") {
                return whole.to_owned();
            }
            replaced += 1;
            whole.replacen(secret, &placeholder(rule.kind), 1)
        });
        if replaced > 0 {
            count += replaced;
            out = next.into_owned();
        }
    }
    (out, count)
}

/// Masks every string inside a JSON value in place. Returns the replacement count.
pub fn redact_value(value: &mut Value) -> usize {
    match value {
        Value::String(text) => {
            let clean = redact_text(text);
            let n = usize::from(clean != *text);
            if n > 0 {
                *text = clean;
            }
            n
        }
        Value::Array(items) => items.iter_mut().map(redact_value).sum(),
        Value::Object(map) => {
            let original = std::mem::take(map);
            let mut count = 0;
            for (key, mut child) in original {
                let (clean, n) = redact(&key);
                count += n + mask_field_value(&key, &mut child) + redact_value(&mut child);
                // Refuse to expose secret-bearing keys; retain every value if masked
                // keys collide, rather than silently overwriting another member.
                let mut clean = clean;
                while map.contains_key(&clean) {
                    clean.push('_');
                }
                map.insert(clean, child);
            }
            count
        }
        _ => 0,
    }
}

fn secret_field(key: &str) -> bool {
    let key = key.to_ascii_lowercase().replace(['-', '.'], "_");
    [
        "password",
        "passwd",
        "passphrase",
        "token",
        "secret",
        "api_key",
        "apikey",
        "private_key",
        "credential",
        "credentials",
        "authorization",
        "cookie",
    ]
    .contains(&key.as_str())
        || [
            "_password",
            "_passwd",
            "_token",
            "_secret",
            "_api_key",
            "_private_key",
        ]
        .iter()
        .any(|suffix| key.ends_with(suffix))
        || key.contains("secret_access_key")
}

fn mask_field_value(key: &str, value: &mut Value) -> usize {
    if secret_field(key)
        && let Value::String(text) = value
        && !text.is_empty()
        && !text.starts_with("[REDACTED:")
    {
        *text = placeholder("assigned_secret");
        return 1;
    }
    0
}

/// Redact plaintext, including JSON encoded inside a string, without rewriting
/// harmless text. This is important for tool messages and native JSON arguments.
pub fn redact_text(text: &str) -> String {
    if let Ok(mut value) = serde_json::from_str::<Value>(text)
        && matches!(value, Value::Object(_) | Value::Array(_))
        && redact_value(&mut value) > 0
    {
        return value.to_string();
    }
    redact(text).0
}

/// Signed provider units are atomic: changing their plaintext breaks the signature.
/// Reject a contaminated unit instead of persisting a secret or corrupting replay.
fn sanitize_native(value: &mut Value) -> anyhow::Result<()> {
    if let Value::Object(map) = value
        && (map.contains_key("signature")
            || map.contains_key("thoughtSignature")
            || map.contains_key("encrypted_content")
            || map.get("type").and_then(Value::as_str) == Some("redacted_thinking"))
    {
        let mut probe = value.clone();
        anyhow::ensure!(
            redact_value(&mut probe) == 0,
            "sensitive data in signed provider unit; refusing unsafe replay"
        );
        return Ok(());
    }
    match value {
        Value::String(text) => *text = redact_text(text),
        Value::Array(items) => {
            for item in items {
                sanitize_native(item)?;
            }
        }
        Value::Object(map) => {
            let original = std::mem::take(map);
            for (key, mut item) in original {
                mask_field_value(&key, &mut item);
                sanitize_native(&mut item)?;
                let mut clean = redact(&key).0;
                while map.contains_key(&clean) {
                    clean.push('_');
                }
                map.insert(clean, item);
            }
        }
        _ => {}
    }
    Ok(())
}

/// A safe durable/outbound copy; never use it to replace live execution arguments.
pub fn sanitize_message(message: &crate::Message) -> anyhow::Result<crate::Message> {
    let mut message = message.clone();
    anyhow::ensure!(
        ["system", "user", "assistant", "tool"].contains(&message.role.as_str()),
        "invalid message role"
    );
    let identifiers = message
        .tool_calls
        .iter()
        .flat_map(|call| [&call.id, &call.name])
        .chain(message.tool_call_id.iter());
    for id in identifiers {
        anyhow::ensure!(
            redact_text(id) == *id,
            "sensitive data in message identifier; refusing unsafe replay"
        );
    }
    message.content = redact_text(&message.content);
    for call in &mut message.tool_calls {
        redact_value(&mut call.arguments);
        sanitize_native(&mut call.provider_data)?;
    }
    sanitize_native(&mut message.provider_data)?;
    Ok(message)
}

pub fn sanitize_tools(
    tools: &[crate::ToolDefinition],
) -> anyhow::Result<Vec<crate::ToolDefinition>> {
    tools
        .iter()
        .map(|tool| {
            anyhow::ensure!(
                redact_text(&tool.name) == tool.name,
                "sensitive data in tool identifier"
            );
            let mut tool = tool.clone();
            tool.description = redact_text(&tool.description);
            redact_value(&mut tool.parameters);
            Ok(tool)
        })
        .collect()
}

/// Stateful, bounded logical-line redaction. Incomplete lines are held until a
/// newline or finish, so fragmentation cannot expose a prefix of a credential.
/// Oversized lines are suppressed entirely. PEM bodies are suppressed across lines.
#[derive(Default)]
pub struct StreamRedactor {
    pending: String,
    dropping: bool,
    private_key: bool,
    whole_record: bool,
    assignment_continuation: bool,
}
impl StreamRedactor {
    const LIMIT: usize = 65_536;
    pub fn push(&mut self, text: &str) -> String {
        self.whole_record |= registered()
            .lock()
            .is_ok_and(|values| values.iter().any(|value| value.contains('\n')));
        if self.whole_record {
            if !self.dropping {
                if self.pending.len() + text.len() > Self::LIMIT {
                    self.pending.clear();
                    self.dropping = true;
                    return "[REDACTED:oversized_stream_record]\n".into();
                }
                self.pending.push_str(text);
            }
            return String::new();
        }
        let mut output = String::new();
        for part in text.split_inclusive('\n') {
            if !self.dropping {
                if self.pending.len() + part.len() > Self::LIMIT {
                    if self.pending.contains("-----BEGIN ") || part.contains("-----BEGIN ") {
                        self.private_key = true;
                    }
                    self.pending.clear();
                    self.dropping = true;
                    output.push_str("[REDACTED:oversized_stream_record]\n");
                } else {
                    self.pending.push_str(part);
                }
            }
            if part.ends_with('\n') {
                if !self.dropping {
                    let line = std::mem::take(&mut self.pending);
                    output.push_str(&self.line(&line));
                }
                self.dropping = false;
            }
        }
        output
    }
    fn line(&mut self, line: &str) -> String {
        if self.private_key {
            if line.contains("-----END ") && line.contains("PRIVATE KEY-----") {
                self.private_key = false;
            }
            return String::new();
        }
        if let Some(start) = line.find("-----BEGIN ")
            && line[start..].contains("PRIVATE KEY-----")
        {
            self.private_key = !line[start..].contains("-----END ");
            return format!("{}[REDACTED:private_key]\n", redact(&line[..start]).0);
        }
        if self.assignment_continuation {
            if line.trim().is_empty() {
                return String::new();
            }
            self.assignment_continuation = false;
            return "[REDACTED:assigned_secret]\n".into();
        }
        static INCOMPLETE: OnceLock<Regex> = OnceLock::new();
        let incomplete = INCOMPLETE.get_or_init(|| Regex::new(r#"(?i)\b[\w.\-]*(?:secret|token|passw(?:or)?d|passwd|api[_\-]?key|apikey|private[_\-]?key|credential|auth)[\w.\-]*["']?\s*[:=]\s*["']?\s*$"#).expect("valid static assignment pattern"));
        self.assignment_continuation = incomplete.is_match(line);
        redact_text(line)
    }
    pub fn finish(&mut self) -> String {
        let pending = std::mem::take(&mut self.pending);
        if self.dropping {
            self.dropping = false;
            return String::new();
        }
        if self.whole_record {
            return redact_text(&pending);
        }
        self.line(&pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragmented_credentials_are_redacted_at_every_split() {
        let cases = [
            "safe prefix token=abcdefgh12345678\nnext line\n",
            "safe prefix AKIAABCDEFGHIJKLMNOP tail\n",
            "before\n-----BEGIN RSA PRIVATE KEY-----\nprivate-body\n-----END RSA PRIVATE KEY-----\nafter\n",
        ];
        for text in cases {
            for split in 0..=text.len() {
                let mut stream = StreamRedactor::default();
                let mut output = stream.push(&text[..split]);
                output.push_str(&stream.push(&text[split..]));
                output.push_str(&stream.finish());
                for secret in ["abcdefgh12345678", "AKIAABCDEFGHIJKLMNOP", "private-body"] {
                    assert!(!output.contains(secret), "split {split}: {output}");
                }
                assert!(output.contains("REDACTED"));
            }
        }
        let mut stream = StreamRedactor::default();
        assert!(
            stream
                .push("ordinary complete line\n")
                .contains("ordinary complete line")
        );
        assert!(stream.push("unfinished").is_empty());
        assert_eq!(stream.finish(), "unfinished");
        let mut stream = StreamRedactor::default();
        assert!(
            stream
                .push(&"x".repeat(StreamRedactor::LIMIT + 1))
                .contains("oversized")
        );
        assert!(stream.finish().is_empty());
    }

    #[test]
    fn native_plaintext_is_sanitized_but_signatures_remain_atomic() -> anyhow::Result<()> {
        let mut message = crate::Message::text("assistant", "token=abcdefgh12345678");
        message.provider_data = serde_json::json!({"wire":"openai_responses","items":[
            {"type":"reasoning","encrypted_content":"opaque-encrypted-unit"},
            {"type":"function_call","arguments":"{\"token\":\"abcdefgh12345678\"}"}
        ]});
        let clean = sanitize_message(&message)?;
        assert!(!serde_json::to_string(&clean)?.contains("abcdefgh12345678"));
        assert_eq!(
            clean.provider_data["items"][0]["encrypted_content"],
            "opaque-encrypted-unit"
        );
        message.provider_data = serde_json::json!({"items":[{"type":"thinking","thinking":"ordinary thought","signature":"exact-signature"}]});
        assert_eq!(
            sanitize_message(&message)?.provider_data,
            message.provider_data
        );
        message.provider_data["items"][0]["thinking"] = serde_json::json!("token=abcdefgh12345678");
        assert!(sanitize_message(&message).is_err());
        let mut value = serde_json::json!({"token=abcdefgh12345678":"ordinary","nested":"{\"token\":\"abcdefgh12345678\"}"});
        redact_value(&mut value);
        assert!(!value.to_string().contains("abcdefgh12345678"));
        Ok(())
    }

    #[test]
    fn reviewer_regressions_cover_keys_identifiers_and_structured_assignments() -> anyhow::Result<()>
    {
        register_secret("review-opaque-key-987654321");
        let mut message = crate::Message::text("assistant", "ordinary text");
        message.provider_data = serde_json::json!({"wire":"anthropic","items":[{"type":"tool_use","input":{"review-opaque-key-987654321":"ordinary"}}]});
        assert!(
            !serde_json::to_string(&sanitize_message(&message)?)?
                .contains("review-opaque-key-987654321")
        );
        message.tool_calls.push(crate::ToolCall {
            id: "review-opaque-key-987654321".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path":"a"}),
            provider_data: Value::Null,
        });
        assert!(sanitize_message(&message).is_err());
        let mut value = serde_json::json!({"password":"unregistered-review-password-4429"});
        assert!(redact_value(&mut value) > 0);
        assert!(
            !value
                .to_string()
                .contains("unregistered-review-password-4429")
        );
        let mut signed = crate::Message::text("assistant", "ordinary");
        signed.provider_data = serde_json::json!({"part":{"functionCall":{"args":{"password":"unregistered-review-password-4429"}},"thoughtSignature":"atomic-signature"}});
        assert!(sanitize_message(&signed).is_err());
        for split in 0..=46 {
            let text = "password:\n  unregistered-review-stream-4429\n";
            let split = split.min(text.len());
            let mut stream = StreamRedactor::default();
            let output = format!(
                "{}{}{}",
                stream.push(&text[..split]),
                stream.push(&text[split..]),
                stream.finish()
            );
            assert!(!output.contains("unregistered-review-stream-4429"));
        }
        Ok(())
    }

    #[test]
    fn masks_common_credential_shapes() {
        let samples = [
            "key AKIAABCDEFGHIJKLMNOP end",
            "token ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "sk-ant-api03-abcdefghijklmnopqrstuvwxyz",
            "Authorization: Bearer abcdefghijklmnop1234567890",
            "DATABASE_URL=postgres://user:hunter2pass@host/db",
            "export API_KEY=\"abcd1234efgh5678\"",
            "-----BEGIN RSA PRIVATE KEY-----\nMIIB\n-----END RSA PRIVATE KEY-----",
        ];
        for sample in samples {
            let (clean, n) = redact(sample);
            assert!(n > 0, "not detected: {sample}");
            assert!(clean.contains("[REDACTED"), "{clean}");
        }
        assert!(
            !redact("password: hunter2hunter2")
                .0
                .contains("hunter2hunter2")
        );
    }

    #[test]
    fn leaves_ordinary_text_and_is_idempotent() {
        let text = "cargo build finished in 3s; see src/main.rs:42";
        assert_eq!(redact(text), (text.to_owned(), 0));
        let once = redact("token=abcdefgh12345678").0;
        assert_eq!(redact(&once).1, 0);
    }

    #[test]
    fn registered_values_and_json_are_masked() {
        register_secret("opaque-value-123456");
        let mut v = serde_json::json!({"a": ["x opaque-value-123456 y"], "n": 1});
        assert_eq!(redact_value(&mut v), 1);
        assert!(!v.to_string().contains("opaque-value"));
    }
}
