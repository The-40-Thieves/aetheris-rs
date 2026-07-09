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

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

const ACCEPT: &str = "application/vnd.oci.image.index.v1+json, \
application/vnd.docker.distribution.manifest.list.v2+json, \
application/vnd.oci.image.manifest.v1+json, \
application/vnd.docker.distribution.manifest.v2+json";
const TTL: Duration = Duration::from_secs(45 * 60);

struct Cached {
    at: Instant,
    result: Option<bool>,
}
static CACHE: RwLock<Option<HashMap<String, Cached>>> = RwLock::new(None);

/// Compare the running image's local digest to the registry's current tag digest.
/// Returns Some(true/false) only on a successful compare; None when unknown
/// (no local digest, private/unauthorized, network/registry error).
#[allow(dead_code)] // wired into the poller in Task 7
pub async fn check_update(client: &reqwest::Client, image: &str, local_digest: Option<&str>) -> Option<bool> {
    let local = local_digest?; // no local repo digest -> undeterminable
    // cache hit?
    {
        let g = CACHE.read().unwrap();
        if let Some(m) = g.as_ref() {
            if let Some(c) = m.get(image) {
                if c.at.elapsed() < TTL {
                    return c.result;
                }
            }
        }
    }
    let result = fetch_remote_digest(client, image).await.map(|remote| remote != local);
    let mut g = CACHE.write().unwrap();
    g.get_or_insert_with(HashMap::new)
        .insert(image.to_string(), Cached { at: Instant::now(), result });
    result
}

#[allow(dead_code)] // consumed by check_update above
async fn fetch_remote_digest(client: &reqwest::Client, image: &str) -> Option<String> {
    let r = parse_image_ref(image)?;
    let url = format!("https://{}/v2/{}/manifests/{}", r.registry, r.repository, r.tag);
    let head = |token: Option<&str>| {
        let mut req = client.head(&url).header("Accept", ACCEPT);
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        req
    };
    let resp = head(None).send().await.ok()?;
    let resp = if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        let chal = resp.headers().get("www-authenticate")?.to_str().ok()?.to_string();
        let (realm, service, scope) = parse_www_authenticate(&chal)?;
        let scope = scope.unwrap_or_else(|| format!("repository:{}:pull", r.repository));
        let token: serde_json::Value = client
            .get(&realm)
            .query(&[("service", service.as_str()), ("scope", scope.as_str())])
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;
        let tok = token.get("token").or_else(|| token.get("access_token")).and_then(|t| t.as_str())?;
        head(Some(tok)).send().await.ok()?
    } else {
        resp
    };
    if !resp.status().is_success() {
        return None;
    }
    resp.headers().get("docker-content-digest")?.to_str().ok().map(|s| s.to_string())
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

    #[tokio::test]
    async fn check_update_none_without_local_digest() {
        let client = reqwest::Client::new();
        assert_eq!(check_update(&client, "postgres:16", None).await, None);
    }

    #[tokio::test]
    #[ignore = "live: hits ghcr.io anonymously"]
    async fn live_registry_probe() {
        let client = reqwest::Client::new();
        // A digest that cannot match the current one -> Some(true); a wrong-but-present flow proves the fetch+token path.
        let r = check_update(
            &client,
            "ghcr.io/coollabsio/sentinel:0.0.21",
            Some("sha256:0000000000000000000000000000000000000000000000000000000000000000"),
        )
        .await;
        eprintln!("registry check result: {r:?}");
        assert_eq!(r, Some(true), "a bogus local digest must differ from the real remote digest");
    }
}
