pub fn assess(
    head: &str,
    expected_digest: &str,
    observation: &serde_json::Value,
    now_ms: i64,
) -> serde_json::Value {
    fn hex(value: &str, len: usize) -> bool {
        value.len() == len && value.bytes().all(|b| b.is_ascii_hexdigit())
    }

    fn digest(value: &str) -> bool {
        value
            .strip_prefix("sha256:")
            .is_some_and(|suffix| hex(suffix, 64))
    }

    fn result(state: &str, count: usize) -> serde_json::Value {
        serde_json::json!({ "state": state, "count": count })
    }

    if observation.is_null() {
        return result("absent", 0);
    }

    let replicas = observation.get("replicas").and_then(|v| v.as_array());
    let count = replicas.map_or(0, |items| items.len());

    if !hex(head, 40) || !digest(expected_digest) || now_ms <= 0 {
        return result("unknown", count);
    }

    let observed_ms = match observation.get("observed_ms").and_then(|v| v.as_i64()) {
        Some(value) if value > 0 && value <= now_ms => value,
        _ => return result("unknown", count),
    };

    if i128::from(now_ms) - i128::from(observed_ms) > 120_000
        || observation.get("complete").and_then(|v| v.as_bool()) != Some(true)
    {
        return result("unknown", count);
    }

    let replicas = match replicas {
        Some(items) if (1..=64).contains(&items.len()) => items,
        _ => return result("unknown", count),
    };

    let mut first: Option<(&str, &str)> = None;
    let mut mixed = false;
    let mut mismatch = false;
    let mut not_ready = false;

    for replica in replicas {
        let revision = match replica.get("revision").and_then(|v| v.as_str()) {
            Some(value) if hex(value, 40) => value,
            _ => return result("unknown", count),
        };
        let pinned_digest = match replica.get("digest").and_then(|v| v.as_str()) {
            Some(value) if digest(value) => value,
            _ => return result("unknown", count),
        };
        let ready = match replica.get("ready").and_then(|v| v.as_bool()) {
            Some(value) => value,
            _ => return result("unknown", count),
        };

        match first {
            Some(pair) => mixed |= pair != (revision, pinned_digest),
            None => first = Some((revision, pinned_digest)),
        }

        mismatch |= revision != head || pinned_digest != expected_digest;
        not_ready |= !ready;
    }

    result(
        if mixed {
            "mixed"
        } else if mismatch {
            "mismatch"
        } else if not_ready {
            "not_ready"
        } else {
            "match"
        },
        count,
    )
}

#[cfg(test)]
mod tests {
    use super::assess;
    use serde_json::{json, Value};

    const NOW: i64 = 200_000;

    fn head() -> String {
        "a".repeat(40)
    }

    fn digest() -> String {
        format!("sha256:{}", "b".repeat(64))
    }

    fn replica() -> Value {
        json!({
            "revision": head(),
            "digest": digest(),
            "ready": true
        })
    }

    fn observation() -> Value {
        json!({
            "observed_ms": NOW,
            "complete": true,
            "replicas": [replica()]
        })
    }

    fn check(value: &Value, state: &str, count: usize) {
        assert_eq!(
            assess(&head(), &digest(), value, NOW),
            json!({ "state": state, "count": count })
        );
    }

    #[test]
    fn absent() {
        check(&Value::Null, "absent", 0);
    }

    #[test]
    fn matches() {
        check(&observation(), "match", 1);
        let mut value = observation();
        value["replicas"] = json!(vec![replica(); 64]);
        check(&value, "match", 64);
    }

    #[test]
    fn freshness_boundaries() {
        let mut value = observation();
        value["observed_ms"] = json!(NOW - 120_000);
        check(&value, "match", 1);

        for timestamp in [NOW - 120_001, 0, -1, NOW + 1] {
            value["observed_ms"] = json!(timestamp);
            check(&value, "unknown", 1);
        }
    }

    #[test]
    fn incomplete_or_invalid_metadata() {
        for replacement in [json!(false), Value::Null, json!("true")] {
            let mut value = observation();
            value["complete"] = replacement;
            check(&value, "unknown", 1);
        }

        for replacement in [Value::Null, json!("200000"), json!(1.5)] {
            let mut value = observation();
            value["observed_ms"] = replacement;
            check(&value, "unknown", 1);
        }

        check(&json!({}), "unknown", 0);
        check(&json!([]), "unknown", 0);
    }

    #[test]
    fn replica_count_limits() {
        let mut value = observation();
        value["replicas"] = json!([]);
        check(&value, "unknown", 0);

        value["replicas"] = json!(vec![replica(); 65]);
        check(&value, "unknown", 65);

        value["replicas"] = json!({});
        check(&value, "unknown", 0);
    }

    #[test]
    fn validates_every_replica() {
        for (field, replacement) in [
            ("revision", json!("a".repeat(39))),
            ("revision", json!("g".repeat(40))),
            ("digest", json!(format!("sha256:{}", "b".repeat(63)))),
            ("digest", json!(format!("sha512:{}", "b".repeat(64)))),
            ("digest", json!(format!("sha256:{}", "g".repeat(64)))),
            ("ready", json!("true")),
            ("ready", Value::Null),
        ] {
            let mut value = observation();
            let mut invalid = replica();
            invalid[field] = replacement;
            value["replicas"] = json!([replica(), invalid]);
            check(&value, "unknown", 2);
        }
    }

    #[test]
    fn revision_and_digest_are_independently_pinned() {
        let mut value = observation();
        value["replicas"][0]["revision"] = json!("c".repeat(40));
        check(&value, "mismatch", 1);

        value = observation();
        value["replicas"][0]["digest"] = json!(format!("sha256:{}", "d".repeat(64)));
        check(&value, "mismatch", 1);
    }

    #[test]
    fn mixed_revisions_or_digests() {
        for field in ["revision", "digest"] {
            let mut value = observation();
            let mut other = replica();
            other[field] = if field == "revision" {
                json!("c".repeat(40))
            } else {
                json!(format!("sha256:{}", "d".repeat(64)))
            };
            other["ready"] = json!(false);
            value["replicas"] = json!([replica(), other]);
            check(&value, "mixed", 2);
        }
    }

    #[test]
    fn readiness_and_precedence() {
        let mut value = observation();
        value["replicas"][0]["ready"] = json!(false);
        check(&value, "not_ready", 1);

        value["replicas"][0]["revision"] = json!("c".repeat(40));
        check(&value, "mismatch", 1);
    }

    #[test]
    fn exact_comparison() {
        let mut value = observation();
        value["replicas"][0]["revision"] = json!(head().to_uppercase());
        check(&value, "mismatch", 1);
    }

    #[test]
    fn invalid_expected_values_and_clock() {
        let value = observation();
        for (expected_head, expected_digest, now) in [
            ("invalid".to_owned(), digest(), NOW),
            (head(), "invalid".to_owned(), NOW),
            (head(), digest(), 0),
            (head(), digest(), -1),
        ] {
            assert_eq!(
                assess(&expected_head, &expected_digest, &value, now),
                json!({ "state": "unknown", "count": 1 })
            );
        }

        let mut value = observation();
        value["observed_ms"] = json!(1);
        assert_eq!(
            assess(&head(), &digest(), &value, i64::MAX),
            json!({ "state": "unknown", "count": 1 })
        );
    }
}
