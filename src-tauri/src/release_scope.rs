pub fn scoped_result(
    local: &serde_json::Value,
    live: &serde_json::Value,
    before: &str,
    after: &str,
    same_origin_pins: bool,
) -> &'static str {
    if !same_origin_pins {
        return "superseded";
    }

    let pins_match = before == "match" && after == "match";
    let local_items = local.as_array();
    let live_items = live.as_array();

    fn state(item: &serde_json::Value) -> Option<&str> {
        item.as_object()
            .and_then(|object| object.get("state"))
            .and_then(serde_json::Value::as_str)
    }

    if local_items.is_some_and(|items| {
        items.iter().any(|item| {
            matches!(
                state(item),
                Some("composed_head_regression" | "release_range_regression")
            )
        })
    }) {
        return "blocked";
    }

    if pins_match
        && live_items.is_some_and(|items| items.iter().any(|item| state(item) == Some("mismatch")))
    {
        return "blocked";
    }

    let local_passes = local_items.is_some_and(|items| {
        !items.is_empty()
            && items
                .iter()
                .all(|item| state(item) == Some("candidate_pass"))
    });
    let live_passes = live_items.is_some_and(|items| {
        !items.is_empty() && items.iter().all(|item| state(item) == Some("pass"))
    });

    if pins_match && local_passes && live_passes {
        "scoped_pass"
    } else {
        "inconclusive"
    }
}

#[cfg(test)]
mod tests {
    use super::scoped_result;
    use serde_json::json;

    #[test]
    fn all_evidence_must_pass() {
        assert_eq!(
            scoped_result(
                &json!([
                    {"state": "candidate_pass"},
                    {"state": "candidate_pass"}
                ]),
                &json!([{"state": "pass"}, {"state": "pass"}]),
                "match",
                "match",
                true,
            ),
            "scoped_pass"
        );
    }

    #[test]
    fn moved_origin_takes_precedence_over_everything() {
        for local in [
            json!(null),
            json!([]),
            json!([{"state": "composed_head_regression"}]),
            json!([{"state": "release_range_regression"}]),
        ] {
            assert_eq!(
                scoped_result(
                    &local,
                    &json!([{"state": "mismatch"}]),
                    "match",
                    "match",
                    false,
                ),
                "superseded"
            );
        }
    }

    #[test]
    fn known_local_regressions_block_even_with_incomplete_evidence() {
        for regression in ["composed_head_regression", "release_range_regression"] {
            assert_eq!(
                scoped_result(
                    &json!([null, {"state": regression}]),
                    &json!(null),
                    "unknown",
                    "unknown",
                    true,
                ),
                "blocked"
            );
        }
    }

    #[test]
    fn live_mismatch_blocks_only_with_both_pin_checks_matching() {
        for before in ["match", "mismatch", "unknown", ""] {
            for after in ["match", "mismatch", "unknown", ""] {
                assert_eq!(
                    scoped_result(
                        &json!([{"state": "candidate_pass"}]),
                        &json!([{"state": "unknown"}, {"state": "mismatch"}]),
                        before,
                        after,
                        true,
                    ),
                    if before == "match" && after == "match" {
                        "blocked"
                    } else {
                        "inconclusive"
                    }
                );
            }
        }
    }

    #[test]
    fn invalid_or_incomplete_local_evidence_cannot_pass() {
        for local in [
            json!(null),
            json!({ "state": "candidate_pass" }),
            json!([]),
            json!([null]),
            json!([{}]),
            json!([{"state": 1}]),
            json!([{"state": "pass"}]),
            json!([{"state": "unknown"}]),
            json!([{"state": "candidate_pass"}, {}]),
        ] {
            assert_eq!(
                scoped_result(&local, &json!([{"state": "pass"}]), "match", "match", true,),
                "inconclusive"
            );
        }
    }

    #[test]
    fn invalid_or_incomplete_live_evidence_cannot_pass() {
        for live in [
            json!(null),
            json!({ "state": "pass" }),
            json!([]),
            json!([null]),
            json!([{}]),
            json!([{"state": false}]),
            json!([{"state": "candidate_pass"}]),
            json!([{"state": "unknown"}]),
            json!([{"state": "pass"}, {}]),
        ] {
            assert_eq!(
                scoped_result(
                    &json!([{"state": "candidate_pass"}]),
                    &live,
                    "match",
                    "match",
                    true,
                ),
                "inconclusive"
            );
        }
    }

    #[test]
    fn passing_states_require_both_pin_checks() {
        for (before, after) in [
            ("unknown", "match"),
            ("match", "unknown"),
            ("mismatch", "mismatch"),
            ("MATCH", "match"),
            ("", ""),
        ] {
            assert_eq!(
                scoped_result(
                    &json!([{"state": "candidate_pass"}]),
                    &json!([{"state": "pass"}]),
                    before,
                    after,
                    true,
                ),
                "inconclusive"
            );
        }
    }
}
