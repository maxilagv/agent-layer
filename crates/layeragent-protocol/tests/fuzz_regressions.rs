//! Bounded seeded property/stress campaigns, not a libFuzzer corpus or a proof.
//! Seeds and case counts are fixed so a regression is reproducible in ordinary CI.
use anyhow::Result;
use layeragent_protocol::{
    Message,
    redact::{StreamRedactor, register_secret, sanitize_message},
};
use serde_json::json;

struct Seed(u64);
impl Seed {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, limit: usize) -> usize {
        (self.next() as usize) % limit
    }
}

fn fragmented(text: &str, random: &mut Seed) -> String {
    let mut boundaries: Vec<_> = text.char_indices().map(|(at, _)| at).collect();
    boundaries.push(text.len());
    let mut redactor = StreamRedactor::default();
    let mut emitted = String::new();
    let mut start = 0;
    while start + 1 < boundaries.len() {
        let end = (start + 1 + random.below(13)).min(boundaries.len() - 1);
        emitted.push_str(&redactor.push(&text[boundaries[start]..boundaries[end]]));
        if random.below(7) == 0 {
            emitted.push_str(&redactor.push(""));
        }
        start = end;
    }
    emitted.push_str(&redactor.finish());
    assert!(
        redactor.finish().is_empty(),
        "finish must drain the stream exactly once"
    );
    emitted
}

#[test]
fn seeded_fragmentation_never_emits_registered_credentials_and_preserves_benign_controls() {
    const SECRET: &str = "seeded-opaque-credential-Q7n29d8u5R";
    const UNICODE: &str = "seeded-unicode-μ漢字-opaque-tail-T2q";
    register_secret(SECRET);
    register_secret(UNICODE);
    let mut random = Seed(0x71d3_892a_6c45_b0e7);
    for case in 0..192 {
        let prefix = format!("ordinary acción 🦀 漢字 case {case} {:x}\n", random.next());
        let text = match random.below(4) {
            0 => format!("{prefix}{SECRET}\nordinary end"),
            1 => format!("{prefix}left {UNICODE} right\nordinary end"),
            2 => json!({"ordinary": [prefix, SECRET, {"nested": UNICODE}]}).to_string(),
            _ => format!("{prefix}{SECRET}\n{SECRET}\n{UNICODE}"),
        };
        let emitted = fragmented(&text, &mut random);
        assert!(
            !emitted.contains(SECRET),
            "credential leaked in seeded case {case}"
        );
        assert!(
            !emitted.contains(UNICODE),
            "unicode credential leaked in case {case}"
        );
        assert!(
            !emitted.contains("opaque-tail-T2q"),
            "unicode suffix leaked in case {case}"
        );
        let benign = format!(
            "ordinary αβ 🦀 漢字 {case} {:x}\nsecond line\n",
            random.next()
        );
        assert_eq!(
            fragmented(&benign, &mut random),
            benign,
            "benign control changed in case {case}"
        );
    }
}

#[test]
fn seeded_native_signed_units_remain_exact_or_are_rejected() -> Result<()> {
    const SECRET: &str = "seeded-native-sensitive-value-N8p32R";
    register_secret(SECRET);
    let mut random = Seed(0x8de2_f09b_1234_976a);
    for case in 0..192 {
        let signature = format!("opaque signed unit {:x} 🦀", random.next());
        let mut message = Message::text("assistant", "ordinary response");
        let native = match random.below(4) {
            0 => {
                json!({"type":"thinking", "thinking":"ordinary reasoning", "signature": signature})
            }
            1 => {
                json!({"functionCall":{"name":"read_file","args":{"path":"src/main.rs"}}, "thoughtSignature":signature})
            }
            2 => json!({"type":"reasoning", "encrypted_content":signature}),
            _ => json!({"type":"redacted_thinking", "data":signature}),
        };
        message.provider_data = json!({"items":[native]});
        assert_eq!(
            sanitize_message(&message)?.provider_data,
            message.provider_data,
            "clean signed bytes changed in case {case}"
        );
        message.provider_data["items"][0]["extra_plaintext"] =
            json!(format!("ordinary {SECRET} tail"));
        assert!(
            sanitize_message(&message).is_err(),
            "contaminated signed unit accepted in case {case}"
        );
        // The caller's original execution record must never be modified in place.
        assert!(message.provider_data.to_string().contains(SECRET));
    }
    Ok(())
}

#[test]
fn seeded_lossy_utf8_records_are_bounded_and_do_not_emit_registered_values() {
    const SECRET: &str = "seeded-binary-stream-credential-P9r78N";
    register_secret(SECRET);
    let mut random = Seed(0x639b_d107_a824_fe52);
    for case in 0..128 {
        let count = random.below(1025);
        let bytes: Vec<_> = (0..count).map(|_| random.next() as u8).collect();
        let mut text = String::from_utf8_lossy(&bytes).into_owned();
        text.push_str(SECRET);
        text.push_str("\nordinary tail\n");
        let output = fragmented(&text, &mut random);
        assert!(
            !output.contains(SECRET),
            "lossy UTF-8 record leaked in seeded case {case}"
        );
        assert!(
            output.len() <= text.len().saturating_mul(8) + 128,
            "redaction amplified a bounded record unexpectedly"
        );
    }
    let oversized = format!("{}{}", "x".repeat(70_000), SECRET);
    let mut redactor = StreamRedactor::default();
    let output = format!("{}{}", redactor.push(&oversized), redactor.finish());
    assert!(!output.contains(SECRET));
    assert!(
        output.len() < 128,
        "oversized record should be suppressed, not buffered for emission"
    );
}
