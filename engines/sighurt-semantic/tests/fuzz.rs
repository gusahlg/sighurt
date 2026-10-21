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
    if let Ok(snapshot) = parse(json, generation) {
        assert_eq!(snapshot.document, generation);
        assert!(snapshot.check().is_ok());
        assert!(snapshot
            .nodes
            .iter()
            .all(|node| !(node.password && node.value.is_some())));
    }
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
