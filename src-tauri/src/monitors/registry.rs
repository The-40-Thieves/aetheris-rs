//! Registry v2 image-update detection via manifest-digest comparison.
//!
//! Pure parsers land first (this task); the HTTP client that fetches
//! manifest digests and the poller that wires this into `Container` land in
//! later tasks on this branch. Until then these are unused from the crate's
//! perspective, hence the `dead_code` allows.

#[derive(Debug, PartialEq)]
#[allow(dead_code)] // constructed by parse_image_ref, consumed by the manifest client in Task 6
pub struct ImageRef {
    pub registry: String,   // host used for the v2 API, e.g. registry-1.docker.io
    pub repository: String, // e.g. library/postgres, coollabsio/sentinel
    pub tag: String,
}

/// Parse a docker image reference into (registry, repository, tag), applying
/// Docker Hub defaulting (implicit docker.io + library/ for single-segment names).
#[allow(dead_code)] // consumed by the manifest client added in Task 6
pub fn parse_image_ref(image: &str) -> Option<ImageRef> {
    if image.is_empty() {
        return None;
    }

    // A ref may carry a digest suffix (`@sha256:...`), with or without an
    // explicit tag: `name@sha256:...` (digest-only) or `name:tag@sha256:...`
    // (both). Strip the digest before tag parsing so its `:` is never
    // mistaken for the tag separator.
    let has_digest = image.contains('@');
    let name_and_maybe_tag = image.split_once('@').map_or(image, |(before, _)| before);

    let (name, tag) = match name_and_maybe_tag.rsplit_once(':') {
        // A ':' after the last '/' is the tag; a ':' before a '/' is a port.
        Some((n, t)) if !t.contains('/') => (n, t.to_string()),
        _ if has_digest => {
            // Digest-pinned with no explicit tag — nothing to compare a
            // running image's tag against, so this ref isn't checkable here.
            return None;
        }
        _ => (name_and_maybe_tag, "latest".to_string()),
    };
    let first = name.split('/').next().unwrap_or("");
    let is_registry = first.contains('.') || first.contains(':') || first == "localhost";
    let (registry, repository) = if is_registry {
        let (host, repo) = name.split_once('/')?;
        let host = if host == "docker.io" { "registry-1.docker.io".to_string() } else { host.to_string() };
        (host, repo.to_string())
    } else if name.contains('/') {
        ("registry-1.docker.io".to_string(), name.to_string())
    } else {
        ("registry-1.docker.io".to_string(), format!("library/{name}"))
    };
    Some(ImageRef { registry, repository, tag })
}

/// Parse a Bearer `WWW-Authenticate` challenge into (realm, service, scope).
#[allow(dead_code)] // consumed by the manifest client added in Task 6
pub fn parse_www_authenticate(header: &str) -> Option<(String, String, Option<String>)> {
    let h = header.trim();
    let rest = h.strip_prefix("Bearer ").or_else(|| h.strip_prefix("bearer "))?;
    let mut realm = None;
    let mut service = None;
    let mut scope = None;
    for part in rest.split(',') {
        let (k, v) = part.trim().split_once('=')?;
        let v = v.trim().trim_matches('"').to_string();
        match k.trim() {
            "realm" => realm = Some(v),
            "service" => service = Some(v),
            "scope" => scope = Some(v),
            _ => {}
        }
    }
    Some((realm?, service?, scope))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_image_refs() {
        assert_eq!(parse_image_ref("postgres:16").unwrap(),
            ImageRef { registry: "registry-1.docker.io".into(), repository: "library/postgres".into(), tag: "16".into() });
        assert_eq!(parse_image_ref("ghcr.io/coollabsio/sentinel:0.0.21").unwrap(),
            ImageRef { registry: "ghcr.io".into(), repository: "coollabsio/sentinel".into(), tag: "0.0.21".into() });
        assert_eq!(parse_image_ref("crazymax/diun").unwrap(),
            ImageRef { registry: "registry-1.docker.io".into(), repository: "crazymax/diun".into(), tag: "latest".into() });
        // registry with port is not mistaken for a tag
        assert_eq!(parse_image_ref("localhost:5000/app:v1").unwrap(),
            ImageRef { registry: "localhost:5000".into(), repository: "app".into(), tag: "v1".into() });
        // digest-only refs (no tag to compare against) aren't checkable
        assert_eq!(parse_image_ref("postgres@sha256:abcd1234ef"), None);
        // empty input is not checkable
        assert_eq!(parse_image_ref(""), None);
        // a tag alongside a digest is parsed; the digest is ignored
        assert_eq!(parse_image_ref("ghcr.io/coollabsio/sentinel:0.0.21@sha256:abc123").unwrap(),
            ImageRef { registry: "ghcr.io".into(), repository: "coollabsio/sentinel".into(), tag: "0.0.21".into() });
    }

    #[test]
    fn parses_challenge() {
        let h = r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:library/postgres:pull""#;
        let (realm, service, scope) = parse_www_authenticate(h).unwrap();
        assert_eq!(realm, "https://auth.docker.io/token");
        assert_eq!(service, "registry.docker.io");
        assert_eq!(scope.unwrap(), "repository:library/postgres:pull");
    }
}
