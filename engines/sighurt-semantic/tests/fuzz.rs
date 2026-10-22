//! Random bytes and random JSON. Nothing here may panic, and a snapshot that parses has passed
//! `SemanticSnapshot::check` with the caller's document on it.

use sighurt_api::ids::DocumentGeneration;
use sighurt_semantic::parse;

/// xorshift64*, so the run does not depend on a crate and a failure repeats.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn gen(&mut self, bound: usize) -> usize {
        if bound == 0 {
            0
        } else {
            (self.next() as usize) % bound
        }
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            out.extend_from_slice(&self.next().to_le_bytes());
        }
        out.truncate(len);
        out
    }
}

fn random_json(rng: &mut Rng, depth: usize) -> String {
    if depth > 5 || rng.gen(5) == 0 {
        return match rng.gen(6) {
            0 => "null".to_owned(),
            1 => "true".to_owned(),
            2 => "false".to_owned(),
            3 => format!("{}", rng.next() as i64),
            4 => serde_json::to_string(&rng_text(rng)).unwrap(),
            _ => {
                if rng.gen(2) == 0 {
                    "[]".to_owned()
                } else {
                    "{}".to_owned()
                }
            }
        };
    }
    if rng.gen(2) == 0 {
        let count = rng.gen(5);
        let mut json = String::from("[");
        for index in 0..count {
            if index > 0 {
                json.push(',');
            }
            json.push_str(&random_json(rng, depth + 1));
        }
        json.push(']');
        json
    } else {
        let count = rng.gen(5);
        let mut json = String::from("{");
        for index in 0..count {
            if index > 0 {
                json.push(',');
            }
            let key = match rng.gen(8) {
                0 => "kind",
                1 => "id",
                2 => "children",
                3 => "text",
                4 => "value",
                5 => "password",
                6 => "nodes",
                _ => "k",
            };
            json.push_str(&serde_json::to_string(key).unwrap());
            json.push(':');
            json.push_str(&random_json(rng, depth + 1));
        }
        json.push('}');
        json
    }
}

fn rng_text(rng: &mut Rng) -> String {
    let alphabet = ["a", " ", "\n", "é", "\u{0001}", "\"", "\\", "<", "s"];
    let mut text = String::new();
    for _ in 0..rng.gen(24) {
        text.push_str(alphabet[rng.gen(alphabet.len())]);
    }
    text
}

fn assert_sane(json: &str, generation: DocumentGeneration) {
    let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| parse(json, generation)));
    let Ok(parsed) = parsed else {
        panic!("parse panicked on {json:?}");
    };
    if let Ok(snapshot) = parsed {
        assert_eq!(snapshot.document, generation);
        assert!(snapshot.check().is_ok());
        assert!(snapshot
            .nodes
            .iter()
            .all(|node| !(node.password && node.value.is_some())));
    }
}

/// `index` may land inside a multibyte character. Step back to a boundary.
fn floor_char(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[test]
fn random_inputs_do_not_panic() {
    let generation = DocumentGeneration::next();
    let mut rng = Rng(0x5eed_1234_abcd);
    for _ in 0..300 {
        let len = rng.gen(2048);
        let bytes = rng.bytes(len);
        let text = String::from_utf8_lossy(&bytes);
        assert_sane(&text, generation);
    }
    for _ in 0..200 {
        let json = random_json(&mut rng, 0);
        assert_sane(&json, generation);
        // A torn value, and one with junk after it.
        if let Some(mid) = json
            .char_indices()
            .nth(json.chars().count() / 2)
            .map(|(i, _)| i)
        {
            assert_sane(&json[..mid], generation);
        }
        assert_sane(&format!("{json} trailing"), generation);
    }
}

/// Crafted escapes and numbers whose bytes are valid UTF-8 but not ASCII where JSON
/// wants hex digits. `\u000é` used to pass the length check and slice through `é`.
#[test]
fn mutated_unicode_and_numbers_do_not_panic() {
    let generation = DocumentGeneration::next();
    let seeds = [
        r#"{"x":"\u000é"}"#,
        r#"{"x":"\u00é"}"#,
        r#"{"x":"\u0é"}"#,
        r#"{"x":"\ué"}"#,
        r#"{"x":"\uD800\uDC00"}"#,
        r#"{"x":"\uD800"}"#,
        r#"{"x":"\uD800\u"}"#,
        r#"{"x":"\uD800\uDC0"}"#,
        r#"{"x":"\uD800\u000é"}"#,
        r#"{"x":"\uDEAD"}"#,
        r#"{"x":"\u"}"#,
        r#"{"x":"\"#,
        r#"{"x":"\u12"}"#,
        r#"{"n":1é}"#,
        r#"{"n":-é}"#,
        r#"{"n":12.3é}"#,
        r#"{"n":1eé}"#,
        r#"{"n":1e+é}"#,
        r#"{"n":01}"#,
        r#"{"n":1.}"#,
        r#"{"n":-}"#,
        "\"\\u000é\"",
        "\"\\uD800\\uDC00\"",
        "\"\\uD800\\u你\"",
    ];
    for seed in seeds {
        assert_sane(seed, generation);
    }
    // A well-formed surrogate pair is one character, not an error and not a panic.
    let decoded = parse(
        r#"{"root":1,"nodes":[{"id":1,"kind":"text","text":"\uD800\uDC00"}]}"#,
        generation,
    )
    .expect("a surrogate pair is a character");
    assert_eq!(decoded.nodes[0].text.as_deref(), Some("\u{10000}"));

    let mut rng = Rng(0x00e1_5ca7);
    let mut corpus: Vec<String> = seeds.iter().map(|seed| (*seed).to_owned()).collect();
    for _ in 0..80 {
        corpus.push(random_json(&mut rng, 0));
    }
    let splices = ["é", "你", "\u{1F600}", "e", ".", "-", "+", "\\u", "\"", "0"];
    for _ in 0..400 {
        let base = &corpus[rng.gen(corpus.len())];
        let at = floor_char(base, rng.gen(base.len().saturating_add(1)));
        let splice = splices[rng.gen(splices.len())];
        let mut mutated = String::with_capacity(base.len() + splice.len());
        mutated.push_str(&base[..at]);
        mutated.push_str(splice);
        mutated.push_str(&base[at..]);
        assert_sane(&mutated, generation);
        if let Some(pos) = mutated.find("\\u") {
            let end = floor_char(&mutated, pos + 2 + rng.gen(6));
            assert_sane(&mutated[..end], generation);
        }
    }
}

#[test]
fn deep_json_is_an_error_not_a_panic() {
    let mut json = String::new();
    for _ in 0..10_000 {
        json.push('[');
    }
    for _ in 0..10_000 {
        json.push(']');
    }
    assert!(parse(&json, DocumentGeneration::next()).is_err());
}
