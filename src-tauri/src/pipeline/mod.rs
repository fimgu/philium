//! The generic agent-to-release pipeline (docs/PIPELINE.md). Nothing here names a repo,
//! branch, service, port or namespace: each repo declares itself in `.philium/pipeline.yaml`.
pub mod app;
pub mod cli;
pub mod config;
pub mod gate;
pub mod land;
pub mod records;
pub mod replay;
pub mod runner;
pub mod simulate;
pub mod verdict;

#[cfg(test)]
mod tests {
    /// Strings in the pipeline's own code (tests aside) name no host, port or cluster
    /// context: those belong in each repo's `.philium/pipeline.yaml`.
    #[test]
    fn the_pipeline_code_names_no_host_port_or_cluster_context() {
        let files = [
            ("cli.rs", include_str!("cli.rs")),
            ("config.rs", include_str!("config.rs")),
            ("gate.rs", include_str!("gate.rs")),
            ("land.rs", include_str!("land.rs")),
            ("records.rs", include_str!("records.rs")),
            ("replay.rs", include_str!("replay.rs")),
            ("runner.rs", include_str!("runner.rs")),
            ("simulate.rs", include_str!("simulate.rs")),
            ("verdict.rs", include_str!("verdict.rs")),
        ];
        let tlds = [".com", ".io", ".net", ".org", ".dev", ".live", ".cloud", ".internal", ".local", ".svc"];
        for (name, src) in files {
            let code = src.split("#[cfg(test)]").next().unwrap();
            for (n, line) in code.lines().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                for lit in line.split('"').skip(1).step_by(2) {
                    let low = lit.to_ascii_lowercase();
                    // `{b:02x}` is a format spec, not a port.
                    let in_braces = |i: usize| low[..i].rfind('{') > low[..i].rfind('}');
                    let port = low.match_indices(':').any(|(i, _)| !in_braces(i) && low[i + 1..].chars().take_while(char::is_ascii_digit).count() >= 2);
                    assert!(!port, "{name}:{}: a port number in code: {lit}", n + 1);
                    assert!(!low.contains("kind-"), "{name}:{}: a cluster context in code: {lit}", n + 1);
                    // A host is a name right before the domain ending (".local/bin" is a folder).
                    let host = tlds.iter().any(|t| {
                        low.match_indices(t).any(|(i, _)| {
                            let before = low[..i].chars().last().is_some_and(|c| c.is_ascii_alphanumeric());
                            let after = low[i + t.len()..].chars().next();
                            before && matches!(after, None | Some('/' | ':' | ' '))
                        })
                    });
                    assert!(!host, "{name}:{}: a host in code: {lit}", n + 1);
                }
            }
        }
    }
}
