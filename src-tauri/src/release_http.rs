use std::time::Duration;

const MAX_BODY: usize = 32 * 1024;

pub fn valid_path(path: &str) -> bool {
    if path.len() < 2 || path.len() > 160 || !path.starts_with('/') {
        return false;
    }

    path[1..].split('/').all(|segment| {
        !segment.is_empty()
            && segment != "."
            && segment != ".."
            && segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    })
}

fn valid_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
}

fn single_header<'a>(
    headers: &'a reqwest::header::HeaderMap,
    name: &str,
) -> Option<&'a reqwest::header::HeaderValue> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    Some(value)
}

fn json_utf8_content_type(headers: &reqwest::header::HeaderMap) -> Option<()> {
    let value = single_header(headers, "content-type")?.to_str().ok()?;
    let mut parts = value.split(';');

    if !parts
        .next()?
        .trim()
        .eq_ignore_ascii_case("application/json")
    {
        return None;
    }

    if let Some(parameter) = parts.next() {
        let (name, value) = parameter.trim().split_once('=')?;
        if !name.trim().eq_ignore_ascii_case("charset") {
            return None;
        }

        let value = value.trim();
        if !value.eq_ignore_ascii_case("utf-8") && !value.eq_ignore_ascii_case("\"utf-8\"") {
            return None;
        }
    }

    if parts.next().is_some() {
        return None;
    }

    Some(())
}

pub async fn sample(port: u16, path: &str, head: &str, digest: &str) -> Option<String> {
    if port < 1024 || !valid_path(path) || !valid_hex(head, 40) || !valid_hex(digest, 64) {
        return None;
    }

    tokio::time::timeout(Duration::from_secs(2), async {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(2))
            .build()
            .ok()?;

        let expected_artifact = format!("sha256:{digest}");
        let mut response = client
            .get(format!("http://127.0.0.1:{port}{path}"))
            .header(reqwest::header::CACHE_CONTROL, "no-store")
            .send()
            .await
            .ok()?;

        if response.status() != reqwest::StatusCode::OK {
            return None;
        }

        let headers = response.headers();
        if single_header(headers, "x-philium-revision")?.as_bytes() != head.as_bytes()
            || single_header(headers, "x-philium-artifact")?.as_bytes()
                != expected_artifact.as_bytes()
        {
            return None;
        }

        json_utf8_content_type(headers)?;

        if response
            .content_length()
            .is_some_and(|n| n > MAX_BODY as u64)
        {
            return None;
        }

        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.ok()? {
            if chunk.len() > MAX_BODY - body.len() {
                return None;
            }
            body.extend_from_slice(&chunk);
        }

        String::from_utf8(body).ok()
    })
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::valid_path;

    #[test]
    fn accepts_valid_paths() {
        for path in [
            "/a",
            "/status",
            "/api/v1/artifact.json",
            "/A0-z_9/file.name",
            "/.../.hidden/a..b",
        ] {
            assert!(valid_path(path), "{path:?}");
        }
    }

    #[test]
    fn rejects_invalid_paths() {
        for path in [
            "",
            "/",
            "status",
            "//a",
            "/a/",
            "/a//b",
            "/.",
            "/..",
            "/a/./b",
            "/a/../b",
            "/a?b",
            "/a#b",
            "/a%2fb",
            "/a\\b",
            "/a b",
            "/a\nb",
            "/a\0b",
            "/café",
            "/a:b",
            "http://127.0.0.1/a",
        ] {
            assert!(!valid_path(path), "{path:?}");
        }
    }

    #[test]
    fn enforces_byte_length_limit() {
        assert!(valid_path(&format!("/{}", "a".repeat(159))));
        assert!(!valid_path(&format!("/{}", "a".repeat(160))));
    }
}
