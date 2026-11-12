use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::model::Packument;

/// download progress hook: (bytes done, total from content-length; None when
/// the server sent none). plain std fn trait so low-level crates stay free of
/// any ui dependency
pub type DownloadHook<'a> = &'a (dyn Fn(u64, Option<u64>) + Send + Sync);

#[derive(Clone)]
pub struct Registry {
    client: reqwest::Client,
    base: String,
    cache: PathBuf,
    offline: bool,
    refresh: bool,
    prefer_offline: bool,
    ttl_secs: u64,
}

impl Registry {
    pub fn new(
        offline: bool,
        refresh: bool,
        prefer_offline: bool,
        ttl_hours: u64,
        registry_override: Option<&str>,
    ) -> Result<Self> {
        // precedence: explicit override / config / env > saved "current" registry > npmjs
        let base = registry_override
            .map(|s| s.to_string())
            .or_else(|| std::env::var("ZAP_REGISTRY").ok())
            .or_else(saved_current_registry)
            .unwrap_or_else(|| "https://registry.npmjs.org".to_string())
            .trim_end_matches('/')
            .to_string();
        let mut builder = reqwest::Client::builder()
            .user_agent("zap/0.1.0 (node package manager)")
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .gzip(true);
        if let Ok(token) = std::env::var("ZAP_TOKEN").or_else(|_| std::env::var("NPM_TOKEN")) {
            if let Ok(hv) = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", token)) {
                let mut headers = reqwest::header::HeaderMap::new();
                headers.insert(reqwest::header::AUTHORIZATION, hv);
                builder = builder.default_headers(headers);
            }
        }
        Ok(Self {
            client: builder.build()?,
            base,
            cache: crate::util::meta_cache()?,
            offline,
            refresh,
            prefer_offline,
            ttl_secs: ttl_hours * 3600,
        })
    }

    /// fetch a packument, with a disk cache (fresh for the configured TTL, always
    /// used offline or with prefer_offline). negative results (404) are cached too
    pub async fn packument(&self, name: &str) -> Result<Arc<Packument>> {
        match self.try_packument(name).await? {
            Some(p) => Ok(p),
            None => anyhow::bail!("package `{}` not found on registry {}", name, self.base),
        }
    }

    /// like packument but Ok(None) for 404s
    pub async fn try_packument(&self, name: &str) -> Result<Option<Arc<Packument>>> {
        let key = crate::util::sha256_hex(name.as_bytes());
        let cache_file = self.cache.join(format!("{}.json", key));
        if !self.refresh {
            if let Ok(text) = std::fs::read_to_string(&cache_file) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                    let t = v.get("t").and_then(|x| x.as_u64()).unwrap_or(0);
                    let fresh = t.saturating_add(self.ttl_secs) > now_secs();
                    if fresh || self.offline || self.prefer_offline {
                        if v.get("missing").is_some() {
                            return Ok(None);
                        }
                        if let Some(body) = v.get("body") {
                            if let Ok(p) = serde_json::from_value::<Packument>(body.clone()) {
                                return Ok(Some(Arc::new(p)));
                            }
                        }
                    }
                }
            }
        }
        if self.offline {
            anyhow::bail!("--offline: metadata for `{}` is not cached", name);
        }
        let url = format!("{}/{}", self.base, name.replace('/', "%2F"));
        let resp = self
            .client
            .get(&url)
            .header("accept", "application/json")
            .send()
            .await
            .with_context(|| format!("failed to reach registry for `{}`", name))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            let _ = std::fs::create_dir_all(&self.cache);
            let _ = std::fs::write(&cache_file, serde_json::json!({ "t": now_secs(), "missing": true }).to_string());
            return Ok(None);
        }
        let resp = resp
            .error_for_status()
            .with_context(|| format!("registry returned an error for `{}`", name))?;
        let bytes = resp.bytes().await?;
        let _ = std::fs::create_dir_all(&self.cache);
        let wrapped = serde_json::json!({
            "t": now_secs(),
            "body": serde_json::from_slice::<serde_json::Value>(&bytes)?,
        });
        let _ = std::fs::write(&cache_file, wrapped.to_string());
        let pack: Packument = serde_json::from_slice(&bytes)
            .with_context(|| format!("could not parse registry metadata for `{}`", name))?;
        Ok(Some(Arc::new(pack)))
    }

    /// the tarball's package.json is the authoritative description (a proxy
    /// mirror's cached packument can be stale): update one version's
    /// description in the cached packument; Ok(true) when the entry changed.
    /// the freshness timestamp stays untouched (ttl semantics unchanged)
    pub fn cache_update_description(name: &str, version: &str, description: &str) -> Result<bool> {
        let cache_file =
            crate::util::meta_cache()?.join(format!("{}.json", crate::util::sha256_hex(name.as_bytes())));
        let Ok(text) = std::fs::read_to_string(&cache_file) else {
            return Ok(false);
        };
        let mut v: serde_json::Value =
            serde_json::from_str(&text).context("cached packument is not valid json")?;
        let Some(entry) = v
            .get_mut("body")
            .and_then(|b| b.get_mut("versions"))
            .and_then(|vs| vs.get_mut(version))
        else {
            return Ok(false); // missing-marker or the version isn't in the cache
        };
        if !entry.is_object() {
            return Ok(false);
        }
        if entry.get("description").and_then(|d| d.as_str()) == Some(description) {
            return Ok(false);
        }
        entry["description"] = serde_json::Value::String(description.to_string());
        std::fs::write(&cache_file, v.to_string())?;
        Ok(true)
    }

    /// fetch one tarball fully; `hook` fires per chunk so a progress lane can
    /// track the transfer as it happens (chunk loop instead of one .bytes())
    pub async fn tarball(&self, url: &str, hook: Option<DownloadHook<'_>>) -> Result<Vec<u8>> {
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .with_context(|| format!("failed to download {}", url))?
            .error_for_status()
            .with_context(|| format!("download failed for {}", url))?;
        // content-length covers the ENCODED body: with the gzip feature the
        // decoded chunks can sum past it, lanes clamp
        let total = resp.content_length();
        let mut done: u64 = 0;
        let mut data: Vec<u8> = Vec::with_capacity(total.unwrap_or(0) as usize);
        let mut resp = resp;
        while let Some(chunk) = resp.chunk().await.with_context(|| format!("reading {}", url))? {
            done += chunk.len() as u64;
            data.extend_from_slice(&chunk);
            if let Some(hook) = hook {
                hook(done, total);
            }
        }
        Ok(data)
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// github releases (the update banner + `zap set-version` verification)

const GITHUB_RELEASES_API: &str = "https://api.github.com/repos/ninjaninja140/zap/releases";

fn github_blocking() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent("zap-cli") // the github api rejects requests without one
        .build()?)
}

/// strip a release tag's "v" prefix ("v0.7.0" → "0.7.0")
fn tag_version(tag: &str) -> String {
    tag.trim_start_matches('v').to_string()
}

/// every published release's version, newest first (github's list order)
pub fn github_release_tags() -> Result<Vec<String>> {
    let resp = github_blocking()?
        .get(format!("{}?per_page=100", GITHUB_RELEASES_API))
        .send()?
        .error_for_status()?;
    let bytes = resp.bytes()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes)?;
    Ok(v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| r.get("tag_name").and_then(|t| t.as_str()).map(tag_version))
                .collect()
        })
        .unwrap_or_default())
}

/// the latest release's version, Ok(None) when none are published (404)
pub fn github_latest_tag() -> Result<Option<String>> {
    let resp = github_blocking()?
        .get(format!("{}/latest", GITHUB_RELEASES_API))
        .send()?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let bytes = resp.error_for_status()?.bytes()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes)?;
    Ok(v.get("tag_name").and_then(|t| t.as_str()).map(tag_version))
}

// the update check's disk cache, the meta-cache envelope pattern
// ({"t": ..., "body": ...}) with a fixed 12h ttl (the check is its own
// cadence, not registry metadata) and negative caching (a verified "no
// releases" is stored too, so the reality of an unpublished repo doesn't
// re-hit github on every command)

/// the update-check cache file inside the meta-cache dir
pub fn update_check_cache() -> Result<PathBuf> {
    Ok(crate::util::meta_cache()?.join("update-check.json"))
}

/// a fresh cache entry's answer: outer Some = fresh (don't refetch), inner =
/// the stored latest (None = cached "no releases"). stale/missing/unparseable
/// → outer None (caller refetches)
fn update_check_cached(cache: &std::path::Path, ttl_secs: u64, now: u64) -> Option<Option<String>> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(cache).ok()?).ok()?;
    let t = v.get("t")?.as_u64()?;
    if t.saturating_add(ttl_secs) <= now {
        return None; // stale, whatever it says
    }
    Some(match v.get("body").and_then(|b| b.get("latest")) {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        _ => None,
    })
}

/// a stale cache entry's answer regardless of age (offline fallback)
fn update_check_stale(cache: &std::path::Path) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(cache).ok()?).ok()?;
    match v.get("body").and_then(|b| b.get("latest")) {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

/// the fail-silent update check: fresh cache → serve (even the negative);
/// stale/missing → fetch + store (verified results only); fetch error →
/// serve the stale value if any. errors never surface, the banner is
/// best-effort by contract
pub fn cached_update_check() -> Option<String> {
    let cache = update_check_cache().ok()?;
    const TTL: u64 = 12 * 3600;
    if let Some(cached) = update_check_cached(&cache, TTL, now_secs()) {
        return cached;
    }
    match github_latest_tag() {
        Ok(latest) => {
            let _ = std::fs::create_dir_all(cache.parent().unwrap_or(std::path::Path::new(".")));
            let _ = std::fs::write(
                &cache,
                serde_json::json!({ "t": now_secs(), "body": { "latest": latest } }).to_string(),
            );
            latest
        }
        Err(_) => update_check_stale(&cache),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("zap-http-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry(t: u64, latest: Option<&str>) -> String {
        serde_json::json!({ "t": t, "body": { "latest": latest } }).to_string()
    }

    #[test]
    fn update_cache_respects_the_ttl_and_negatives() {
        let dir = tempdir("cache");
        let f = dir.join("update-check.json");
        let now = 1_000_000u64;
        // fresh Some
        std::fs::write(&f, entry(now, Some("0.7.0"))).unwrap();
        assert_eq!(update_check_cached(&f, 12 * 3600, now), Some(Some("0.7.0".into())));
        // fresh negative ("no releases"), served without a refetch
        std::fs::write(&f, entry(now, None)).unwrap();
        assert_eq!(update_check_cached(&f, 12 * 3600, now), Some(None));
        // stale → outer None (refetch)
        std::fs::write(&f, entry(now - 12 * 3600 - 1, Some("0.7.0"))).unwrap();
        assert_eq!(update_check_cached(&f, 12 * 3600, now), None);
        // missing file → None
        assert_eq!(update_check_cached(&dir.join("nope.json"), 12 * 3600, now), None);
        // garbage → None
        std::fs::write(&f, "not json").unwrap();
        assert_eq!(update_check_cached(&f, 12 * 3600, now), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_cache_serves_offline() {
        let dir = tempdir("stale");
        let f = dir.join("update-check.json");
        // an old entry still answers when the fetch can't
        std::fs::write(&f, entry(0, Some("0.7.0"))).unwrap();
        assert_eq!(update_check_stale(&f), Some("0.7.0".into()));
        // a stale negative stays None
        std::fs::write(&f, entry(0, None)).unwrap();
        assert_eq!(update_check_stale(&f), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tag_versions_strip_the_v() {
        assert_eq!(tag_version("v0.7.0"), "0.7.0");
        assert_eq!(tag_version("0.7.0"), "0.7.0");
    }
}

/// the saved "current" registry from `zap registry use`, name resolved through the map
fn saved_current_registry() -> Option<String> {
    let v: serde_json::Value = std::fs::read_to_string(crate::util::zap_home().ok()?.join("registries.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let current = v.get("current").and_then(|x| x.as_str())?.to_string();
    let named = v.get("named").and_then(|x| x.as_object())?;
    Some(
        named
            .get(&current)
            .and_then(|x| x.as_str())
            .unwrap_or(&current)
            .to_string(),
    )
}
