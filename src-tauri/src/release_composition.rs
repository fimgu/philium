pub fn composition(
    parent: i64,
    commits: &serde_json::Value,
    associations: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    reduce(parent, commits, associations, false)
}
// graph() enumerates ALL of base..head, rejects truncation and shallow checkouts.
// Every parent outside this range is reachable from base and therefore cannot
// contain an included commit. Never apply this rule to an arbitrary partial DAG.
pub fn verified_range(
    parent: i64,
    commits: &serde_json::Value,
    associations: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    reduce(parent, commits, associations, true)
}
fn reduce(
    parent: i64,
    commits: &serde_json::Value,
    associations: &serde_json::Value,
    target_excluded: bool,
) -> Result<serde_json::Value, String> {
    use std::collections::{BTreeMap, BTreeSet};

    const MAX_NODES: usize = 512;
    const MAX_WORK: usize = 1_048_576;

    fn charge(work: &mut usize) -> Result<(), String> {
        if *work >= MAX_WORK {
            return Err("composition work limit exceeded".to_owned());
        }
        *work += 1;
        Ok(())
    }

    fn ancestry(
        starts: &[String],
        graph: &BTreeMap<String, Vec<String>>,
        work: &mut usize,
    ) -> Result<(BTreeSet<String>, bool), String> {
        let mut visited = BTreeSet::new();
        let mut pending: Vec<&str> = starts.iter().map(String::as_str).collect();
        let mut complete = true;

        while let Some(oid) = pending.pop() {
            charge(work)?;
            if !visited.insert(oid.to_owned()) {
                continue;
            }
            match graph.get(oid) {
                Some(parents) => {
                    for predecessor in parents {
                        charge(work)?;
                        pending.push(predecessor.as_str());
                    }
                }
                None => complete = false,
            }
        }

        Ok((visited, complete))
    }

    let commits = commits
        .as_array()
        .ok_or_else(|| "commits must be an array".to_owned())?;
    let associations = associations
        .as_object()
        .ok_or_else(|| "associations must be an object".to_owned())?;

    let mut work = 0;
    let mut graph = BTreeMap::<String, Vec<String>>::new();

    for commit in commits {
        charge(&mut work)?;
        let oid = commit
            .get("oid")
            .and_then(serde_json::Value::as_str)
            .filter(|oid| !oid.is_empty())
            .ok_or_else(|| "commit oid must be a nonempty string".to_owned())?;
        let raw_parents = commit
            .get("parents")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| format!("parents for {oid} must be an array"))?;

        let mut parents = Vec::new();
        let mut seen = BTreeSet::new();
        for predecessor in raw_parents {
            charge(&mut work)?;
            let predecessor = predecessor
                .as_str()
                .filter(|oid| !oid.is_empty())
                .ok_or_else(|| format!("parent oid for {oid} must be a nonempty string"))?;
            if seen.insert(predecessor) {
                parents.push(predecessor.to_owned());
            }
        }

        if let Some(previous) = graph.get(oid) {
            if previous != &parents {
                return Err(format!("conflicting commit records for {oid}"));
            }
        } else {
            if graph.len() >= MAX_NODES {
                return Err("composition node limit exceeded".to_owned());
            }
            graph.insert(oid.to_owned(), parents);
        }
    }

    // Reject cycles: ancestry subtraction requires a commit DAG.
    fn visit(
        oid: &str,
        graph: &BTreeMap<String, Vec<String>>,
        active: &mut BTreeSet<String>,
        done: &mut BTreeSet<String>,
        work: &mut usize,
    ) -> Result<(), String> {
        charge(work)?;
        if done.contains(oid) {
            return Ok(());
        }
        let Some(parents) = graph.get(oid) else {
            return Ok(());
        };
        if !active.insert(oid.to_owned()) {
            return Err(format!("commit ancestry contains a cycle at {oid}"));
        }
        for predecessor in parents {
            visit(predecessor, graph, active, done, work)?;
        }
        active.remove(oid);
        done.insert(oid.to_owned());
        Ok(())
    }

    let mut active = BTreeSet::new();
    let mut done = BTreeSet::new();
    for oid in graph.keys() {
        visit(oid, &graph, &mut active, &mut done, &mut work)?;
    }

    let mut landed = BTreeMap::<String, i64>::new();
    let mut missing = 0usize;
    let mut unmapped = 0usize;
    let mut ambiguous = 0usize;

    for oid in graph.keys() {
        charge(&mut work)?;
        let Some(entries) = associations.get(oid) else {
            missing += 1;
            continue;
        };
        let entries = entries
            .as_array()
            .ok_or_else(|| format!("associations for {oid} must be an array"))?;
        let mut candidates = BTreeSet::new();

        for entry in entries {
            charge(&mut work)?;
            if entry.get("merged").and_then(serde_json::Value::as_bool) != Some(true)
                || entry.get("merge").and_then(serde_json::Value::as_str) != Some(oid.as_str())
            {
                continue;
            }
            let number = entry
                .get("number")
                .and_then(serde_json::Value::as_i64)
                .filter(|number| *number > 0)
                .ok_or_else(|| format!("matching association for {oid} has invalid number"))?;
            candidates.insert(number);
        }

        match candidates.len() {
            0 => unmapped += 1,
            1 => {
                let number = *candidates.iter().next().unwrap();
                if number != parent {
                    landed.insert(oid.clone(), number);
                }
            }
            _ => ambiguous += 1,
        }
    }

    let mut edges = BTreeSet::new();
    for (oid, outer) in &landed {
        let parents = &graph[oid];
        if parents.len() < 2 {
            continue;
        }

        // Unknown ancestry beyond the first-parent boundary might contain
        // any candidate inner commit. Its absence cannot prove an edge.
        let (baseline, complete) = ancestry(&parents[..1], &graph, &mut work)?;
        if !complete && !target_excluded {
            continue;
        }

        let (introduced, _) = ancestry(&parents[1..], &graph, &mut work)?;
        for inner_oid in introduced.difference(&baseline) {
            charge(&mut work)?;
            if let Some(inner) = landed.get(inner_oid) {
                if inner != outer {
                    edges.insert((*outer, *inner));
                }
            }
        }
    }

    let landed: Vec<_> = landed
        .into_iter()
        .map(|(oid, pr)| serde_json::json!({ "pr": pr, "oid": oid }))
        .collect();
    let edges: Vec<_> = edges
        .into_iter()
        .map(|(outer, inner)| serde_json::json!({ "outer": outer, "inner": inner }))
        .collect();

    Ok(serde_json::json!({
        "landed": landed,
        "edges": edges,
        "missing": missing,
        "unmapped": unmapped,
        "ambiguous": ambiguous
    }))
}

#[cfg(test)]
mod composition_tests {
    use super::composition;
    use serde_json::json;

    #[test]
    fn distinguishes_mapping_outcomes_and_excludes_parent() {
        let result = composition(
            10,
            &json!([
                {"oid":"a","parents":[]},
                {"oid":"b","parents":[]},
                {"oid":"c","parents":[]},
                {"oid":"d","parents":[]},
                {"oid":"e","parents":[]}
            ]),
            &json!({
                "b":[
                    {"number":2,"merge":"elsewhere","merged":true},
                    {"number":2,"merge":"b","merged":false}
                ],
                "c":[
                    {"number":3,"merge":"c","merged":true},
                    {"number":4,"merge":"c","merged":true}
                ],
                "d":[{"number":10,"merge":"d","merged":true}],
                "e":[
                    {"number":5,"merge":"e","merged":true},
                    {"number":5,"merge":"e","merged":true}
                ]
            }),
        )
        .unwrap();

        assert_eq!(
            result,
            json!({
                "landed":[{"pr":5,"oid":"e"}],
                "edges":[],
                "missing":1,
                "unmapped":1,
                "ambiguous":1
            })
        );
    }

    #[test]
    fn subtracts_all_first_parent_ancestry_and_deduplicates() {
        let result = composition(
            99,
            &json!([
                {"oid":"root","parents":[]},
                {"oid":"old","parents":["root"]},
                {"oid":"base","parents":["root","old"]},
                {"oid":"inner","parents":["old"]},
                {"oid":"outer","parents":["base","inner","inner"]},
                {"oid":"outer","parents":["base","inner"]}
            ]),
            &json!({
                "root":[],
                "old":[{"number":1,"merge":"old","merged":true}],
                "base":[],
                "inner":[{"number":2,"merge":"inner","merged":true}],
                "outer":[{"number":3,"merge":"outer","merged":true}]
            }),
        )
        .unwrap();

        assert_eq!(result["edges"], json!([{"outer":3,"inner":2}]));
        assert_eq!(result["landed"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn unknown_first_parent_boundary_prevents_proof() {
        let result = composition(
            99,
            &json!([
                {"oid":"base","parents":["external"]},
                {"oid":"inner","parents":[]},
                {"oid":"outer","parents":["base","inner"]}
            ]),
            &json!({
                "base":[],
                "inner":[{"number":1,"merge":"inner","merged":true}],
                "outer":[{"number":2,"merge":"outer","merged":true}]
            }),
        )
        .unwrap();

        assert_eq!(result["edges"], json!([]));
    }

    #[test]
    fn unknown_side_ancestry_does_not_erase_positive_evidence() {
        let result = composition(
            99,
            &json!([
                {"oid":"base","parents":[]},
                {"oid":"inner","parents":["external"]},
                {"oid":"outer","parents":["base","inner"]}
            ]),
            &json!({
                "base":[],
                "inner":[{"number":1,"merge":"inner","merged":true}],
                "outer":[{"number":2,"merge":"outer","merged":true}]
            }),
        )
        .unwrap();

        assert_eq!(result["edges"], json!([{"outer":2,"inner":1}]));
    }

    #[test]
    fn rejects_cycles_conflicts_and_invalid_input() {
        assert!(composition(0, &json!({}), &json!({})).is_err());
        assert!(composition(0, &json!([{"oid":"a","parents":["a"]}]), &json!({})).is_err());
        assert!(composition(
            0,
            &json!([
                {"oid":"a","parents":[]},
                {"oid":"a","parents":["b"]}
            ]),
            &json!({})
        )
        .is_err());
    }

    #[test]
    fn enforces_node_and_work_limits() {
        let commits: Vec<_> = (0..513)
            .map(|i| json!({"oid":i.to_string(),"parents":[]}))
            .collect();
        assert!(composition(0, &json!(commits), &json!({}))
            .unwrap_err()
            .contains("node limit"));

        let associations = json!({
            "a": vec![json!({"number":1,"merge":"a","merged":true}); 1_048_576]
        });
        assert!(
            composition(0, &json!([{"oid":"a","parents":[]}]), &associations)
                .unwrap_err()
                .contains("work limit")
        );
    }
}
