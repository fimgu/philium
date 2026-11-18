use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::Deserializer;
use serde_json::{Map, Number, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fmt;

const MAX_DOCUMENT_BYTES: usize = 1_048_576;
const MAX_DEPTH: usize = 64;
const MAX_POINTER_BYTES: usize = 4_096;

struct StrictJson {
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for StrictJson {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        if self.depth > MAX_DEPTH {
            return Err(de::Error::custom("maximum depth exceeded"));
        }
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for StrictJson {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JSON without duplicate object keys")
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Value, E> {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("non-finite number"))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(value))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(StrictJson {
            depth: self.depth + 1,
        })? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut object: A) -> Result<Value, A::Error> {
        let mut seen = HashSet::new();
        let mut values = Map::new();

        while let Some(key) = object.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                return Err(de::Error::custom("duplicate object key"));
            }
            let value = object.next_value_seed(StrictJson {
                depth: self.depth + 1,
            })?;
            values.insert(key, value);
        }

        Ok(Value::Object(values))
    }
}

fn pointer_tokens(pointer: &str) -> Option<Vec<String>> {
    if pointer.len() > MAX_POINTER_BYTES {
        return None;
    }
    if pointer.is_empty() {
        return Some(Vec::new());
    }
    if !pointer.starts_with('/') {
        return None;
    }

    pointer[1..]
        .split('/')
        .map(|token| {
            let mut decoded = String::with_capacity(token.len());
            let mut chars = token.chars();
            while let Some(character) = chars.next() {
                match character {
                    '~' => match chars.next()? {
                        '0' => decoded.push('~'),
                        '1' => decoded.push('/'),
                        _ => return None,
                    },
                    character => decoded.push(character),
                }
            }
            Some(decoded)
        })
        .collect()
}

fn resolve<'a>(root: &'a Value, tokens: &[String]) -> Option<&'a Value> {
    let mut current = root;
    for token in tokens {
        current = match current {
            Value::Object(object) => object.get(token)?,
            Value::Array(array) => {
                let bytes = token.as_bytes();
                if bytes.is_empty()
                    || (bytes.len() > 1 && bytes[0] == b'0')
                    || !bytes.iter().all(u8::is_ascii_digit)
                {
                    return None;
                }
                array.get(token.parse::<usize>().ok()?)?
            }
            _ => return None,
        };
    }
    Some(current)
}

fn hash_length(hash: &mut Sha256, length: usize) {
    hash.update((length as u64).to_be_bytes());
}

fn hash_bytes(hash: &mut Sha256, bytes: &[u8]) {
    hash_length(hash, bytes.len());
    hash.update(bytes);
}

// Tags and length prefixes make types, boundaries, and presence unambiguous.
// Object keys are sorted independently of serde_json's map configuration.
fn hash_value(hash: &mut Sha256, value: &Value) {
    match value {
        Value::Null => hash.update([0]),
        Value::Bool(false) => hash.update([1]),
        Value::Bool(true) => hash.update([2]),
        Value::Number(number) => {
            hash.update([3]);
            hash_bytes(hash, number.to_string().as_bytes());
        }
        Value::String(string) => {
            hash.update([4]);
            hash_bytes(hash, string.as_bytes());
        }
        Value::Array(array) => {
            hash.update([5]);
            hash_length(hash, array.len());
            for value in array {
                hash_value(hash, value);
            }
        }
        Value::Object(object) => {
            hash.update([6]);
            hash_length(hash, object.len());
            let mut entries: Vec<_> = object.iter().collect();
            entries.sort_unstable_by(|left, right| left.0.cmp(right.0));
            for (key, value) in entries {
                hash_bytes(hash, key.as_bytes());
                hash_value(hash, value);
            }
        }
    }
}

pub(crate) fn strict_value(document: &str) -> Option<Value> {
    if document.len() > MAX_DOCUMENT_BYTES {
        return None;
    }
    let mut deserializer = serde_json::Deserializer::from_str(document);
    let root = StrictJson { depth: 0 }
        .deserialize(&mut deserializer)
        .ok()?;
    deserializer.end().ok()?;
    Some(root)
}
pub fn observe(document: &str, case: &serde_json::Value) -> Option<String> {
    let root = strict_value(document)?;
    let case = case.as_object()?;
    let expected = case.get("expect")?;
    let tokens = pointer_tokens(case.get("pointer")?.as_str()?)?;

    let observed = resolve(&root, &tokens);
    // The JSON decoder rounds non-integer and oversized numbers to f64. Do
    // not credit equality or hash equality after that loss of precision.
    fn exact(v: &Value) -> bool {
        match v {
            Value::Number(n) => !n.is_f64(),
            Value::Array(a) => a.iter().all(exact),
            Value::Object(o) => o.values().all(exact),
            _ => true,
        }
    }
    if !exact(expected) || observed.is_some_and(|v| !exact(v)) {
        return None;
    }
    let passed = observed.is_some_and(|value| value == expected);

    let mut hash = Sha256::new();
    hash.update(b"observe-json-v1\0");
    match observed {
        None => hash.update([0]),
        Some(value) => {
            hash.update([1]);
            hash_value(&mut hash, value);
        }
    }

    let digest = hash.finalize();
    let mut result = String::with_capacity(73);
    result.push_str(if passed { "pass:" } else { "mismatch:" });

    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in digest {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 15) as usize] as char);
    }
    Some(result)
}

#[cfg(test)]
#[test]
fn rounded_numbers_are_not_evidence() {
    assert!(observe(
        "18446744073709551616",
        &serde_json::json!({"pointer":"","expect":1.8446744073709552e19})
    )
    .is_none());
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(document: &str, pointer: &str, expected: Value) -> String {
        observe(document, &json!({"pointer": pointer, "expect": expected})).unwrap()
    }

    fn digest(result: &str) -> &str {
        result.split_once(':').unwrap().1
    }

    #[test]
    fn present_null_and_missing_are_distinct() {
        let present = run(r#"{"x":null}"#, "/x", Value::Null);
        let missing = run("{}", "/x", Value::Null);
        assert!(present.starts_with("pass:"));
        assert!(missing.starts_with("mismatch:"));
        assert_ne!(digest(&present), digest(&missing));
        assert_eq!(digest(&present).len(), 64);
    }

    #[test]
    fn expected_value_does_not_change_observation_hash() {
        let pass = run("42", "", json!(42));
        let mismatch = run("42", "", json!(43));
        assert!(pass.starts_with("pass:"));
        assert!(mismatch.starts_with("mismatch:"));
        assert_eq!(digest(&pass), digest(&mismatch));
    }

    #[test]
    fn strict_pointer_decoding() {
        let document = r#"{"":{"a/b":{"~key":7}},"~1":8,"%2F":9}"#;
        assert!(run(document, "//a~1b/~0key", json!(7)).starts_with("pass:"));
        assert!(run(document, "/~01", json!(8)).starts_with("pass:"));
        assert!(run(document, "/%2F", json!(9)).starts_with("pass:"));

        for pointer in ["x", "#/x", "/~", "/~2", "/missing/~3"] {
            assert!(observe(
                document,
                &json!({
                    "pointer": pointer,
                    "expect": null
                })
            )
            .is_none());
        }
    }

    #[test]
    fn arrays_require_canonical_indices() {
        assert!(run("[10,20]", "/1", json!(20)).starts_with("pass:"));
        for pointer in ["/01", "/+1", "/-", "/2", "/184467440737095516160"] {
            assert!(run("[10,20]", pointer, Value::Null).starts_with("mismatch:"));
        }
        assert!(run(r#"{"01":10}"#, "/01", json!(10)).starts_with("pass:"));
    }

    #[test]
    fn rejects_duplicate_keys_including_escaped_and_nested_keys() {
        for document in [
            r#"{"a":1,"a":2}"#,
            r#"{"a":1,"\u0061":2}"#,
            r#"{"ignored":[{"x":1,"x":2}]}"#,
        ] {
            assert!(observe(
                document,
                &json!({
                    "pointer": "",
                    "expect": null
                })
            )
            .is_none());
        }
    }

    #[test]
    fn rejects_invalid_documents_and_cases() {
        for document in ["", "null null", "{\"x\":}", "[1,]", "NaN"] {
            assert!(observe(
                document,
                &json!({
                    "pointer": "",
                    "expect": null
                })
            )
            .is_none());
        }
        for case in [
            json!(null),
            json!({}),
            json!({"pointer": ""}),
            json!({"pointer": null, "expect": null}),
        ] {
            assert!(observe("null", &case).is_none());
        }
    }

    #[test]
    fn enforces_limits() {
        let case = json!({"pointer": "", "expect": null});
        assert!(observe(&" ".repeat(MAX_DOCUMENT_BYTES + 1), &case).is_none());

        let at_limit = format!("{}null{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
        assert!(observe(&at_limit, &case).is_some());

        let too_deep = format!(
            "{}null{}",
            "[".repeat(MAX_DEPTH + 1),
            "]".repeat(MAX_DEPTH + 1)
        );
        assert!(observe(&too_deep, &case).is_none());

        let pointer = format!("/{}", "x".repeat(MAX_POINTER_BYTES - 1));
        assert!(observe(
            "{}",
            &json!({
                "pointer": pointer,
                "expect": null
            })
        )
        .is_some());

        let pointer = format!("/{}", "x".repeat(MAX_POINTER_BYTES));
        assert!(observe(
            "{}",
            &json!({
                "pointer": pointer,
                "expect": null
            })
        )
        .is_none());
    }

    #[test]
    fn hashes_types_boundaries_and_sorted_objects() {
        let values = ["null", "false", "0", r#""0""#, "[]", "{}"];
        let hashes: HashSet<_> = values
            .iter()
            .map(|document| run(document, "", Value::Null))
            .map(|result| digest(&result).to_owned())
            .collect();
        assert_eq!(hashes.len(), values.len());

        let first = run(r#"{"b":2,"a":1}"#, "", Value::Null);
        let second = run(r#"{"a":1,"b":2}"#, "", Value::Null);
        assert_eq!(digest(&first), digest(&second));

        let first = run(r#"["ab","c"]"#, "", Value::Null);
        let second = run(r#"["a","bc"]"#, "", Value::Null);
        assert_ne!(digest(&first), digest(&second));
    }
}
