use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::model::Packument;

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

    pub async fn tarball(&self, url: &str) -> Result<Vec<u8>> {
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .with_context(|| format!("failed to download {}", url))?
            .error_for_status()
            .with_context(|| format!("download failed for {}", url))?;
        let bytes = resp.bytes().await?;
        Ok(bytes.to_vec())
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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
