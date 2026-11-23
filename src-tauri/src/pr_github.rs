use serde_json::{json, Value};

const MARKER: &str = "<!-- philium-pr-review -->";
const QUERY: &str = r#"query($owner:String!,$repo:String!,$pr:Int!,$comments:String,$reviews:String,$threads:String,$checks:String){viewer{login} repository(owner:$owner,name:$repo){pullRequest(number:$pr){number title body url isDraft state author{login __typename} baseRefOid headRefOid baseRefName headRefName comments(first:100,after:$comments){totalCount nodes{databaseId author{login __typename} body url createdAt} pageInfo{hasNextPage endCursor}} reviews(first:100,after:$reviews){totalCount nodes{databaseId author{login __typename} state body url} pageInfo{hasNextPage endCursor}} reviewThreads(first:100,after:$threads){totalCount nodes{id isResolved isOutdated path line comments(first:20){totalCount nodes{author{login __typename} body url} pageInfo{hasNextPage endCursor}}} pageInfo{hasNextPage endCursor}} commits(last:1){totalCount nodes{commit{statusCheckRollup{contexts(first:100,after:$checks){totalCount nodes{... on CheckRun{name conclusion status detailsUrl} ... on StatusContext{context state targetUrl}} pageInfo{hasNextPage endCursor}}}}}}}}}"#;

async fn graphql(origin: &str, query: &str, variables: Value) -> Result<Value, String> {
    let input = json!({"query":query,"variables":variables}).to_string();
    let output = crate::release_intake::gh_as(
        origin,
        &["api", "graphql", "--input", "-"],
        Some(&input),
        false,
        45,
    )
    .await?;
    let value: Value = serde_json::from_str(&output).map_err(|e| e.to_string())?;
    if value.get("data").is_none_or(Value::is_null) {
        if let Some(errors) = value.get("errors") {
            return Err(format!("GitHub GraphQL: {errors}"));
        }
    }
    Ok(value)
}

fn repository(origin: &str) -> Result<(&str, &str), String> {
    let (owner, repo) = origin.split_once('/').ok_or("Expected owner/repo")?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return Err("Expected owner/repo".into());
    }
    Ok((owner, repo))
}

pub async fn snapshot(origin: &str, pr: i64) -> Result<Value, String> {
    let (owner, repo) = repository(origin)?;
    let variables = json!({"owner":owner,"repo":repo,"pr":pr});
    let mut raw = graphql(origin, QUERY, variables.clone()).await?;
    if !raw["data"]["repository"]["pullRequest"].is_object() {
        return Err(format!("GitHub pull request not found: {}", raw["errors"]));
    }
    let mut errors = raw["errors"].as_array().cloned().unwrap_or_default();
    for (variable, path) in [
        ("comments", "/data/repository/pullRequest/comments"),
        ("reviews", "/data/repository/pullRequest/reviews"),
        ("threads", "/data/repository/pullRequest/reviewThreads"),
        (
            "checks",
            "/data/repository/pullRequest/commits/nodes/0/commit/statusCheckRollup/contexts",
        ),
    ] {
        let result: Result<(), String> = async {
            loop {
                let connection = raw.pointer(path).cloned().unwrap_or(Value::Null);
                if connection["pageInfo"]["hasNextPage"] != true {
                    break;
                }
                let cursor = connection["pageInfo"]["endCursor"]
                    .as_str()
                    .ok_or("Missing pagination cursor")?;
                let mut vars = variables.clone();
                vars[variable] = json!(cursor);
                let page = graphql(origin, QUERY, vars).await?;
                errors.extend(page["errors"].as_array().into_iter().flatten().cloned());
                let next = page.pointer(path).ok_or("Missing paginated connection")?;
                if next["pageInfo"]["endCursor"] == cursor {
                    return Err("GitHub pagination did not advance".into());
                }
                let target = raw.pointer_mut(path).ok_or("Missing connection")?;
                target["nodes"]
                    .as_array_mut()
                    .ok_or("Missing nodes")?
                    .extend(nodes(next).cloned());
                target["pageInfo"] = next["pageInfo"].clone();
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            errors.push(json!(format!("{variable}: {error}")));
        }
    }
    if let Some(threads) = raw
        .pointer_mut("/data/repository/pullRequest/reviewThreads/nodes")
        .and_then(Value::as_array_mut)
    {
        for thread in threads {
            let result: Result<(), String> = async { while thread["comments"]["pageInfo"]["hasNextPage"] == true {
                let cursor = thread["comments"]["pageInfo"]["endCursor"]
                    .as_str()
                    .ok_or("Missing thread cursor")?;
                let page = graphql(origin, r#"query($id:ID!,$after:String!){node(id:$id){... on PullRequestReviewThread{comments(first:100,after:$after){totalCount nodes{author{login __typename} body url} pageInfo{hasNextPage endCursor}}}}}"#, json!({"id":thread["id"],"after":cursor})).await?;
                errors.extend(page["errors"].as_array().into_iter().flatten().cloned());
                let next = &page["data"]["node"]["comments"];
                if next["pageInfo"]["endCursor"] == cursor {
                    return Err("GitHub thread pagination did not advance".into());
                }
                thread["comments"]["nodes"]
                    .as_array_mut()
                    .ok_or("Missing thread comments")?
                    .extend(nodes(next).cloned());
                thread["comments"]["pageInfo"] = next["pageInfo"].clone();
            } Ok(()) }.await;
            if let Err(error) = result {
                errors.push(json!(format!("thread {}: {error}", text(thread, "id"))));
            }
        }
    }
    raw["errors"] = json!(errors);
    parse_snapshot(&raw)
}

fn nodes(connection: &Value) -> impl Iterator<Item = &Value> {
    connection
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
fn author(value: &Value) -> (&str, bool) {
    let login = text(&value["author"], "login");
    (
        login,
        value["author"]["__typename"] == "Bot" || login.ends_with("[bot]"),
    )
}
fn visible(value: &Value) -> bool {
    !text(value, "body").trim().is_empty() && !text(value, "body").starts_with(MARKER)
}
fn comment(value: &Value) -> Value {
    let (login, bot) = author(value);
    json!({"author":login,"bot":bot,"body":text(value,"body"),"url":text(value,"url")})
}

// GitHub DateTime scalars use UTC ISO-8601. Avoid adding a date dependency.
fn timestamp(value: &str) -> Option<i64> {
    let value = value.strip_suffix('Z')?;
    let (date, clock) = value.split_once('T')?;
    let mut date = date.split('-');
    let mut year: i64 = date.next()?.parse().ok()?;
    let month: i64 = date.next()?.parse().ok()?;
    let day: i64 = date.next()?.parse().ok()?;
    let mut clock = clock.split(':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let second: i64 = clock.next()?.split('.').next()?.parse().ok()?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..24).contains(&hour)
        || !(0..60).contains(&minute)
        || !(0..60).contains(&second)
    {
        return None;
    }
    year -= i64::from(month <= 2);
    let era = year.div_euclid(400);
    let y = year - era * 400;
    let m = month + if month > 2 { -3 } else { 9 };
    let days = era * 146097 + y * 365 + y / 4 - y / 100 + (153 * m + 2) / 5 + day - 1 - 719468;
    Some(days * 86400 + hour * 3600 + minute * 60 + second)
}

pub fn parse_snapshot(raw: &Value) -> Result<Value, String> {
    let pr = &raw["data"]["repository"]["pullRequest"];
    if !pr.is_object() {
        return Err("GitHub pull request not found".into());
    }
    let mut errors = raw["errors"].as_array().cloned().unwrap_or_default();
    let mut coverage = json!({});
    for (name, connection) in [
        ("comments", &pr["comments"]),
        ("reviews", &pr["reviews"]),
        ("threads", &pr["reviewThreads"]),
        (
            "checks",
            &pr["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"],
        ),
    ] {
        let fetched = nodes(connection).filter(|v| !v.is_null()).count();
        let total = connection["totalCount"].as_u64().unwrap_or(fetched as u64);
        coverage[name] = json!({"fetched":fetched,"total":total});
        if (fetched as u64) < total || connection["pageInfo"]["hasNextPage"] == true {
            errors.push(json!(format!(
                "{name}: incomplete collection ({fetched}/{total})"
            )));
        }
        if connection.is_null()
            && (name != "checks"
                || pr["commits"]["nodes"][0]["commit"]["statusCheckRollup"].is_object())
        {
            errors.push(json!(format!("{name}: collection unavailable")));
        }
        if nodes(connection).any(Value::is_null) {
            errors.push(json!(format!(
                "{name}: collection contains unavailable entries"
            )));
        }
    }
    for thread in nodes(&pr["reviewThreads"]) {
        let connection = &thread["comments"];
        let fetched = nodes(connection).filter(|v| !v.is_null()).count();
        let total = connection["totalCount"].as_u64().unwrap_or(fetched as u64);
        if connection.is_null()
            || (fetched as u64) < total
            || connection["pageInfo"]["hasNextPage"] == true
        {
            errors.push(json!(format!(
                "thread {}: incomplete comments ({fetched}/{total})",
                text(thread, "id")
            )));
        }
    }
    let viewer = text(&raw["data"]["viewer"], "login");
    let (login, _) = author(pr);
    let comments: Vec<Value> = nodes(&pr["comments"])
        .filter(|v| visible(v))
        .map(|v| {
            let mut c = comment(v);
            c["key"] = json!(format!("comment:{}", v["databaseId"]));
            c["at"] = json!(timestamp(text(v, "createdAt")).unwrap_or(0));
            c
        })
        .collect();
    let reviews: Vec<Value> = nodes(&pr["reviews"])
        .filter(|v| visible(v))
        .map(|v| {
            let mut c = comment(v);
            c["key"] = json!(format!("review:{}", v["databaseId"]));
            c["state"] = v["state"].clone();
            c
        })
        .collect();
    let threads: Vec<Value> = nodes(&pr["reviewThreads"])
        .filter_map(|v| {
            let mut entries = nodes(&v["comments"]).filter(|c| visible(c));
            let first = entries.next()?;
            let mut c = comment(first);
            c["key"] = json!(format!("thread:{}", text(v, "id")));
            c["path"] = json!(text(v, "path"));
            c["line"] = json!(v["line"].as_i64().unwrap_or(0));
            c["resolved"] = json!(v["isResolved"] == true);
            c["outdated"] = json!(v["isOutdated"] == true);
            c["replies"] = json!(entries
                .map(|r| {
                    let mut c = comment(r);
                    c.as_object_mut().unwrap().remove("url");
                    c
                })
                .collect::<Vec<_>>());
            Some(c)
        })
        .collect();
    let checks: Vec<Value> = nodes(&pr["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]).map(|v| {
        let run = v.get("name").is_some();
        let name = text(v, if run {"name"} else {"context"});
        let state = if run && text(v,"status") != "COMPLETED" { "pending" } else {
            match text(v, if run {"conclusion"} else {"state"}) {
                "SUCCESS" => "pass",
                "NEUTRAL" | "SKIPPED" => "skipped",
                "FAILURE" | "ERROR" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED" | "STARTUP_FAILURE" | "STALE" => "fail",
                _ => "pending",
            }
        };
        json!({"key":format!("ci:{name}"),"name":name,"state":state,"url":text(v,if run {"detailsUrl"} else {"targetUrl"})})
    }).collect();
    Ok(
        json!({"number":pr["number"],"title":text(pr,"title"),"body":text(pr,"body"),"url":text(pr,"url"),"author":login,"viewer":viewer,"own":login==viewer,"is_draft":pr["isDraft"]==true,"state":text(pr,"state"),"head":text(pr,"headRefOid"),"base":text(pr,"baseRefOid"),"head_ref":text(pr,"headRefName"),"base_ref":text(pr,"baseRefName"),"comments":comments,"reviews":reviews,"threads":threads,"checks":checks,"claims":claims(text(pr,"body")),"coverage":coverage,"errors":errors}),
    )
}

pub fn claims(body: &str) -> Vec<Value> {
    let mut result = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    for line in body.lines() {
        let line = line.trim_start();
        let ch = line.chars().next().unwrap_or(' ');
        let count = line.chars().take_while(|c| *c == ch).count();
        if ch == '`' || ch == '~' {
            if let Some((open, length)) = fence {
                if ch == open && count >= length && line[count..].trim().is_empty() {
                    fence = None;
                }
            } else if count >= 3 {
                fence = Some((ch, count));
            }
            continue;
        }
        if fence.is_some() {
            continue;
        }
        let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(tail) = rest
            .strip_prefix("[ ]")
            .or_else(|| rest.strip_prefix("[x]"))
            .or_else(|| rest.strip_prefix("[X]"))
        else {
            continue;
        };
        if !tail.starts_with(char::is_whitespace) {
            continue;
        }
        let text = tail.trim();
        if text.is_empty() {
            continue;
        }
        result.push(json!({"key":format!("claim:{}",result.len()+1),"text":text,"checked":!rest.starts_with("[ ]")}));
    }
    result
}

pub async fn post(
    origin: &str,
    pr: i64,
    head: &str,
    event: &str,
    body: &str,
    inline: &[Value],
) -> Result<Value, String> {
    repository(origin)?;
    if !matches!(event, "APPROVE" | "REQUEST_CHANGES" | "COMMENT") {
        return Err("Invalid review event".into());
    }
    let endpoint = format!("repos/{origin}/pulls/{pr}/reviews");
    let comments: Vec<Value> = inline
        .iter()
        .map(|c| json!({"path":c["path"],"line":c["line"],"side":"RIGHT","body":c["body"]}))
        .collect();
    let mut payload = json!({"commit_id":head,"event":event,"body":format!("{MARKER}\n{body}"),"comments":comments});
    let args = ["api", "--method", "POST", endpoint.as_str(), "--input", "-"];
    let input = payload.to_string();
    let output = match crate::release_intake::gh_as(origin, &args, Some(&input), true, 45).await {
        Ok(output) => output,
        Err(error)
            if !inline.is_empty()
                && (error.contains("422") || error.to_lowercase().contains("comments")) =>
        {
            let mut expanded = format!("{MARKER}\n{body}\n\n### Inline findings\n");
            for c in &comments {
                expanded.push_str(&format!(
                    "\n**{}:{}**\n\n{}\n",
                    text(c, "path"),
                    c["line"],
                    text(c, "body")
                ));
            }
            payload["body"] = json!(expanded);
            payload.as_object_mut().unwrap().remove("comments");
            crate::release_intake::gh_as(origin, &args, Some(&payload.to_string()), true, 45)
                .await?
        }
        Err(error) => return Err(error),
    };
    let review: Value = serde_json::from_str(&output).map_err(|e| e.to_string())?;
    Ok(json!({"url":review["html_url"],"id":review["id"],"event":event}))
}

pub async fn review_requested(origin: &str) -> Result<Vec<i64>, String> {
    repository(origin)?;
    let query = r#"query($search:String!,$after:String){search(query:$search,type:ISSUE,first:100,after:$after){nodes{... on PullRequest{number}} pageInfo{hasNextPage endCursor}}}"#;
    let mut variables =
        json!({"search":format!("is:pr is:open review-requested:@me repo:{origin}")});
    let mut result = Vec::new();
    loop {
        let page = graphql(origin, query, variables.clone()).await?;
        if let Some(errors) = page["errors"].as_array().filter(|v| !v.is_empty()) {
            return Err(format!("GitHub review-request search: {}", json!(errors)));
        }
        let connection = &page["data"]["search"];
        result.extend(nodes(connection).filter_map(|v| v["number"].as_i64()));
        if connection["pageInfo"]["hasNextPage"] != true {
            break;
        }
        let cursor = connection["pageInfo"]["endCursor"]
            .as_str()
            .ok_or("Missing search cursor")?;
        if variables["after"] == cursor {
            return Err("GitHub search pagination did not advance".into());
        }
        variables["after"] = json!(cursor);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_fixture() {
        let raw = json!({"data":{"viewer":{"login":"me"},"repository":{"pullRequest":{
            "number":1,"author":{"login":"me"},"body":"- [ ] test","isDraft":false,
            "comments":{"nodes":[{"databaseId":1,"author":{"login":"robot","__typename":"Bot"},"body":"finding","createdAt":"2026-01-01T00:00:00Z"},{"databaseId":2,"author":{"login":"app[bot]"},"body":"another"},{"databaseId":3,"author":{"login":"me"},"body":"<!-- philium-pr-review --> verdict"}]},
            "reviews":{"nodes":[{"databaseId":4,"body":""},{"databaseId":5,"body":"please fix","state":"CHANGES_REQUESTED"}]},
            "reviewThreads":{"nodes":[{"id":"thread1","path":"a.rs","line":8,"isResolved":true,"isOutdated":true,"comments":{"nodes":[{"author":{"login":"app[bot]"},"body":"first","url":"thread-url"},{"author":{"login":"human"},"body":"reply"}]}}]},
            "commits":{"nodes":[{"commit":{"statusCheckRollup":{"contexts":{"nodes":[{"name":"ok","status":"COMPLETED","conclusion":"SUCCESS"},{"name":"bad","status":"COMPLETED","conclusion":"FAILURE"},{"name":"skip","status":"COMPLETED","conclusion":"SKIPPED"},{"name":"neutral","status":"COMPLETED","conclusion":"NEUTRAL"},{"name":"wait","status":"IN_PROGRESS"},{"context":"legacy","state":"ERROR","targetUrl":"ci-url"},{"context":"legacy-ok","state":"SUCCESS"},{"context":"legacy-wait","state":"PENDING"}]}}}}]}
        }}}});
        let v = parse_snapshot(&raw).unwrap();
        assert_eq!(v["own"], true);
        assert_eq!(v["errors"], json!([]));
        assert_eq!(v["coverage"]["comments"], json!({"fetched":3,"total":3}));
        assert_eq!(v["comments"].as_array().unwrap().len(), 2);
        assert_eq!(v["comments"][0]["bot"], true);
        assert_eq!(v["comments"][1]["bot"], true);
        assert!(v["comments"][0]["at"].as_i64().unwrap() > 0);
        assert_eq!(v["reviews"].as_array().unwrap().len(), 1);
        assert_eq!(
            v["threads"][0],
            json!({"key":"thread:thread1","path":"a.rs","line":8,"resolved":true,"outdated":true,"author":"app[bot]","bot":true,"body":"first","url":"thread-url","replies":[{"author":"human","bot":false,"body":"reply"}]})
        );
        assert_eq!(
            v["checks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v["state"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["pass", "fail", "skipped", "skipped", "pending", "fail", "pass", "pending"]
        );
        assert!(parse_snapshot(&json!({"data":{"repository":{"pullRequest":null}}})).is_err());
    }
    #[test]
    fn partial_coverage_is_visible() {
        let raw = json!({"errors":[{"message":"partial permission failure"}],"data":{"viewer":{"login":"me"},"repository":{"pullRequest":{
            "number":7,"comments":{"totalCount":3,"nodes":[{"body":"<!-- philium-pr-review --> old verdict"}],"pageInfo":{"hasNextPage":true}},
            "reviews":{"totalCount":0,"nodes":[]},"reviewThreads":{"totalCount":1,"nodes":[{"id":"t","comments":{"totalCount":2,"nodes":[{"body":"first"}],"pageInfo":{"hasNextPage":true}}}]}
        }}}});
        let parsed = parse_snapshot(&raw).unwrap();
        assert_eq!(
            parsed["coverage"]["comments"],
            json!({"fetched":1,"total":3})
        );
        assert_eq!(
            parsed["coverage"]["reviews"],
            json!({"fetched":0,"total":0})
        );
        assert_eq!(
            parsed["coverage"]["threads"],
            json!({"fetched":1,"total":1})
        );
        assert_eq!(parsed["coverage"]["checks"], json!({"fetched":0,"total":0}));
        assert_eq!(parsed["comments"], json!([]));
        assert_eq!(parsed["errors"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    #[ignore = "opt-in read-only GitHub integration, requires signed-in gh"]
    async fn live_snapshot() {
        let origin = std::env::var("PHILIUM_TEST_PR_ORIGIN").expect("set PHILIUM_TEST_PR_ORIGIN");
        let pr = std::env::var("PHILIUM_TEST_PR_NUMBER")
            .expect("set PHILIUM_TEST_PR_NUMBER")
            .parse()
            .unwrap();
        let started = std::time::Instant::now();
        let snapshot = snapshot(&origin, pr).await.unwrap();
        println!("snapshot {} elapsed={:.3}s coverage={} errors={} normalized_comments={} normalized_reviews={} normalized_threads={} checks={}",pr,started.elapsed().as_secs_f64(),snapshot["coverage"],snapshot["errors"],snapshot["comments"].as_array().unwrap().len(),snapshot["reviews"].as_array().unwrap().len(),snapshot["threads"].as_array().unwrap().len(),snapshot["checks"].as_array().unwrap().len());
        assert_eq!(snapshot["number"], pr);
        assert_eq!(snapshot["errors"], json!([]));
    }

    #[test]
    fn github_timestamps() {
        assert_eq!(timestamp("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(timestamp("2000-03-01T00:00:00Z"), Some(951868800));
        assert_eq!(timestamp("2026-01-01T00:00:00Z"), Some(1767225600));
        assert_eq!(timestamp("2026-01-01T00:00:01.123Z"), Some(1767225601));
        assert_eq!(timestamp("not a timestamp"), None);
    }

    #[test]
    fn markdown_claims() {
        let v = claims("- [ ] first\n * [x] second\n```md\n- [x] hidden\n````\n~~~\n* [ ] hidden too\n~~~\n- [X] third\n- [z] invalid\n- [ ]no space\n- [ ] \n");
        assert_eq!(v,json!([{"key":"claim:1","text":"first","checked":false},{"key":"claim:2","text":"second","checked":true},{"key":"claim:3","text":"third","checked":true}]).as_array().unwrap().clone());
    }
}
