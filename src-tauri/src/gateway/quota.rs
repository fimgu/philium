//! Live provider usage windows, read from each upstream response's own rate-limit headers.
//! Nothing here knows a limit: window names, percent used and resets all come from the server.

use reqwest::header::HeaderMap;
use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Window {
    /// Server-derived label, e.g. "5h" or "7d" (from the header name or the window length).
    pub label: String,
    pub used_percent: f64,
    /// Unix seconds when the window resets, if the server said.
    pub resets_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct Usage {
    pub windows: Vec<Window>,
    /// Unix seconds when these headers were observed.
    pub observed_at: u64,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn num(h: &HeaderMap, k: &str) -> Option<f64> {
    h.get(k)?.to_str().ok()?.trim().parse().ok()
}

/// Reset value: unix seconds, or an RFC 3339 timestamp.
fn reset(h: &HeaderMap, k: &str) -> Option<u64> {
    let s = h.get(k)?.to_str().ok()?.trim();
    s.parse::<u64>().ok().or_else(|| rfc3339(s))
}

/// `YYYY-MM-DDTHH:MM:SS[.frac](Z|±HH:MM)` to unix seconds.
pub(crate) fn rfc3339(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (n(0..4)?, n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?, n(17..19)?);
    let mut rest = &s[19..];
    if let Some(r) = rest.strip_prefix('.') {
        rest = r.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    let off = match rest {
        "Z" | "z" => 0,
        r if r.len() == 6 => {
            let sign = if r.starts_with('-') { -1 } else { 1 };
            sign * (r[1..3].parse::<i64>().ok()? * 3600 + r[4..6].parse::<i64>().ok()? * 60)
        }
        _ => return None,
    };
    // Days from civil (Howard Hinnant).
    let yy = if mo <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    u64::try_from(days * 86400 + h * 3600 + mi * 60 + se - off).ok()
}

fn minutes_label(m: u64) -> String {
    if m >= 1440 && m % 1440 == 0 { format!("{}d", m / 1440) } else if m >= 60 && m % 60 == 0 { format!("{}h", m / 60) } else { format!("{m}m") }
}

/// Parse every rate-limit window the response advertises. Returns None when it advertises none.
pub fn parse(h: &HeaderMap) -> Option<Usage> {
    parse_at(h, now())
}

pub fn parse_at(h: &HeaderMap, now: u64) -> Option<Usage> {
    let mut windows = Vec::new();
    let names: Vec<String> = h.keys().map(|k| k.as_str().to_ascii_lowercase()).collect();
    for n in &names {
        // Anthropic subscription: anthropic-ratelimit-unified-<window>-utilization (0..1).
        if let Some(w) = n.strip_prefix("anthropic-ratelimit-unified-").and_then(|r| r.strip_suffix("-utilization")) {
            if let Some(u) = num(h, n) {
                let pct = if u <= 1.0 { u * 100.0 } else { u };
                windows.push(Window { label: w.to_string(), used_percent: pct, resets_at: reset(h, &format!("anthropic-ratelimit-unified-{w}-reset")) });
            }
        }
        // ChatGPT subscription: x-codex-<slot>-used-percent, -window-minutes, -reset-after-seconds / -reset-at.
        if let Some(slot) = n.strip_prefix("x-codex-").and_then(|r| r.strip_suffix("-used-percent")) {
            if let Some(pct) = num(h, n) {
                let p = format!("x-codex-{slot}");
                let label = num(h, &format!("{p}-window-minutes")).map(|m| minutes_label(m as u64)).unwrap_or_else(|| slot.to_string());
                let resets_at = reset(h, &format!("{p}-reset-at"))
                    .or_else(|| num(h, &format!("{p}-reset-after-seconds")).map(|s| now + s.max(0.0) as u64));
                windows.push(Window { label, used_percent: pct, resets_at });
            }
        }
        // API keys (Anthropic / OpenAI style): <prefix>-limit-<what> + <prefix>-remaining-<what>.
        for (lim_pre, rem_pre, reset_pre) in [
            ("anthropic-ratelimit-", "anthropic-ratelimit-", "anthropic-ratelimit-"),
            ("x-ratelimit-limit-", "x-ratelimit-remaining-", "x-ratelimit-reset-"),
        ] {
            let what = if lim_pre == "anthropic-ratelimit-" {
                n.strip_prefix(lim_pre).and_then(|r| r.strip_suffix("-limit")).filter(|w| !w.starts_with("unified"))
            } else {
                n.strip_prefix(lim_pre)
            };
            let Some(what) = what else { continue };
            let (rem_k, reset_k) = if lim_pre == "anthropic-ratelimit-" {
                (format!("{rem_pre}{what}-remaining"), format!("{reset_pre}{what}-reset"))
            } else {
                (format!("{rem_pre}{what}"), format!("{reset_pre}{what}"))
            };
            if let (Some(lim), Some(rem)) = (num(h, n), num(h, &rem_k)) {
                if lim > 0.0 {
                    windows.push(Window { label: what.to_string(), used_percent: ((lim - rem) / lim * 100.0).clamp(0.0, 100.0), resets_at: reset(h, &reset_k) });
                }
            }
        }
    }
    if windows.is_empty() {
        return None;
    }
    windows.sort_by(|a, b| a.label.cmp(&b.label));
    windows.dedup_by(|a, b| a.label == b.label);
    Some(Usage { windows, observed_at: now })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hm(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(reqwest::header::HeaderName::from_bytes(k.as_bytes()).unwrap(), v.parse().unwrap());
        }
        h
    }

    #[test]
    fn subscription_windows_come_from_headers() {
        let u = parse_at(&hm(&[
            ("anthropic-ratelimit-unified-5h-utilization", "0.42"),
            ("anthropic-ratelimit-unified-5h-reset", "1700000000"),
            ("anthropic-ratelimit-unified-7d-utilization", "0.9"),
        ]), 1).unwrap();
        assert_eq!(u.windows.len(), 2);
        assert_eq!(u.windows[0].label, "5h");
        assert!((u.windows[0].used_percent - 42.0).abs() < 1e-9);
        assert_eq!(u.windows[0].resets_at, Some(1_700_000_000));
        assert_eq!(u.windows[1].label, "7d");

        let c = parse_at(&hm(&[
            ("x-codex-primary-used-percent", "12.5"),
            ("x-codex-primary-window-minutes", "300"),
            ("x-codex-primary-reset-after-seconds", "60"),
            ("x-codex-secondary-used-percent", "80"),
            ("x-codex-secondary-window-minutes", "10080"),
        ]), 100).unwrap();
        assert_eq!(c.windows.iter().map(|w| w.label.as_str()).collect::<Vec<_>>(), ["5h", "7d"]);
        assert_eq!(c.windows[0].resets_at, Some(160));
    }

    #[test]
    fn api_key_limits_and_absence() {
        let u = parse_at(&hm(&[("x-ratelimit-limit-tokens", "1000"), ("x-ratelimit-remaining-tokens", "250")]), 0).unwrap();
        assert_eq!(u.windows[0].label, "tokens");
        assert!((u.windows[0].used_percent - 75.0).abs() < 1e-9);
        let a = parse_at(&hm(&[("anthropic-ratelimit-tokens-limit", "100"), ("anthropic-ratelimit-tokens-remaining", "90"), ("anthropic-ratelimit-tokens-reset", "2030-01-01T00:00:00Z")]), 0).unwrap();
        assert!((a.windows[0].used_percent - 10.0).abs() < 1e-9);
        assert_eq!(a.windows[0].resets_at, Some(1_893_456_000));
        assert_eq!(rfc3339("2030-01-01T01:00:00.5+01:00"), Some(1_893_456_000));
        assert!(parse_at(&hm(&[("content-type", "text/plain")]), 0).is_none());
    }
}
