//! Deterministic seeded stress/property tests. These are bounded CI campaigns,
//! not coverage-guided fuzzing and not evidence of complete parser correctness.
use anyhow::Result;
use layeragent_providers::FrameDecoder;

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

fn decode_chunks(bytes: &[u8], ndjson: bool, random: &mut Seed) -> Result<Vec<String>> {
    let mut decoder = FrameDecoder::default();
    let mut frames = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let end = (at + 1 + random.below(257)).min(bytes.len());
        frames.extend(decoder.push(&bytes[at..end], ndjson)?);
        at = end;
    }
    Ok(frames)
}

#[test]
fn seeded_valid_utf8_sse_and_ndjson_survive_random_byte_fragmentation() -> Result<()> {
    let mut random = Seed(0xc1a5_871e_d982_430b);
    for case in 0..128 {
        let expected: Vec<_> = (0..8)
            .map(|n| format!("{{\"text\":\"acción 🦀 漢字 {case}/{n}\"}}"))
            .collect();
        let ndjson = format!("{}\n", expected.join("\n"));
        assert_eq!(
            decode_chunks(ndjson.as_bytes(), true, &mut random)?,
            expected
        );
        let mut sse = String::new();
        for text in &expected {
            let end = if random.below(2) == 0 {
                "\n\n"
            } else {
                "\r\n\r\n"
            };
            sse.push_str(&format!(
                ": ordinary comment\nevent: message\ndata: {text}{end}"
            ));
        }
        assert_eq!(
            decode_chunks(sse.as_bytes(), false, &mut random)?,
            expected,
            "SSE boundaries changed in seeded case {case}"
        );
    }
    Ok(())
}

#[test]
fn seeded_malformed_bytes_are_bounded_and_never_panic() {
    let mut random = Seed(0x94b8_e67a_2d01_53fc);
    for case in 0..512 {
        let size = random.below(513);
        let mut bytes: Vec<_> = (0..size).map(|_| random.next() as u8).collect();
        if case % 2 == 0 {
            bytes.extend_from_slice(b"\n\n");
        }
        let ndjson = case % 3 == 0;
        if let Ok(frames) = decode_chunks(&bytes, ndjson, &mut random) {
            assert!(
                frames.iter().map(String::len).sum::<usize>() <= bytes.len(),
                "decoder amplified malformed bytes in case {case}"
            );
            assert!(frames.len() <= bytes.len(), "unbounded number of frames");
        }
    }
    let mut decoder = FrameDecoder::default();
    assert!(
        decoder
            .push(&vec![b'x'; 2 * 1024 * 1024 + 1], false)
            .is_err(),
        "oversized unfinished frame must fail closed"
    );
    let mut decoder = FrameDecoder::default();
    assert!(
        decoder.push(&[0xff, b'\n', b'\n'], false).is_err(),
        "malformed UTF-8 must not be silently transformed"
    );
}

#[test]
fn coalesced_sse_events_with_mixed_line_endings_keep_their_boundaries() -> Result<()> {
    let stream = b"data: first\r\n\r\ndata: second\n\ndata: third\r\n\r\n";
    let mut decoder = FrameDecoder::default();
    assert_eq!(decoder.push(stream, false)?, ["first", "second", "third"]);
    // A split inside the CRLF separator must produce the identical event order.
    let mut decoder = FrameDecoder::default();
    let split = b"data: first\r\n\r".len();
    let mut frames = decoder.push(&stream[..split], false)?;
    frames.extend(decoder.push(&stream[split..], false)?);
    assert_eq!(frames, ["first", "second", "third"]);
    Ok(())
}
