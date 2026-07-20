use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::Instant;

use axum::extract::{Request, State};
use axum::http::{Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::state::{AppState, WebError};

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct KeyConfig {
    id: String,
    sha256: String,
    scopes: Vec<Scope>,
    #[serde(default = "all_tables")]
    tables: Vec<String>,
    not_before: Option<DateTime<Utc>>,
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Scope {
    Read,
    Write,
    Maintenance,
    Admin,
}

#[derive(Debug, Default, Deserialize)]
struct KeyFile {
    keys: Vec<KeyConfig>,
}

pub(crate) struct AuthState {
    keys: RwLock<Vec<KeyConfig>>,
    config_path: Option<PathBuf>,
    legacy_key: Option<KeyConfig>,
}

impl AuthState {
    pub(crate) fn open(
        config_path: Option<PathBuf>,
        legacy_key: Option<String>,
    ) -> anyhow::Result<Self> {
        let mut keys = load_keys(config_path.as_deref())?;
        let legacy_key = legacy_key.map(|secret| KeyConfig {
            id: "legacy".into(),
            sha256: hex::encode(Sha256::digest(secret.as_bytes())),
            scopes: vec![Scope::Admin],
            tables: all_tables(),
            not_before: None,
            expires_at: None,
        });
        if let Some(key) = &legacy_key {
            keys.push(key.clone());
        }
        Ok(Self {
            keys: RwLock::new(keys),
            config_path,
            legacy_key,
        })
    }

    pub(crate) fn reload(&self) -> anyhow::Result<usize> {
        let mut keys = load_keys(self.config_path.as_deref())?;
        if let Some(key) = &self.legacy_key {
            keys.push(key.clone());
        }
        let count = keys.len();
        *self
            .keys
            .write()
            .map_err(|_| anyhow::anyhow!("auth lock was poisoned"))? = keys;
        Ok(count)
    }

    fn authorize(&self, token: &str, required: Scope, table: Option<&str>) -> Option<String> {
        let digest = hex::encode(Sha256::digest(token.as_bytes()));
        let now = Utc::now();
        self.keys
            .read()
            .ok()?
            .iter()
            .find(|key| {
                constant_time_eq(digest.as_bytes(), key.sha256.as_bytes())
                    && key.not_before.is_none_or(|value| value <= now)
                    && key.expires_at.is_none_or(|value| value > now)
                    && (key.scopes.contains(&Scope::Admin) || key.scopes.contains(&required))
                    && table.is_none_or(|name| {
                        key.tables
                            .iter()
                            .any(|pattern| table_matches(pattern, name))
                    })
            })
            .map(|key| key.id.clone())
    }

    fn is_disabled(&self) -> bool {
        self.keys.read().is_ok_and(|keys| keys.is_empty())
    }
}

pub(crate) async fn require_bearer(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let started = Instant::now();
    let method = request.method().to_string();
    let path = request.uri().path().to_owned();
    let (scope, table) = required_access(request.method(), &path);
    if state.auth.is_disabled() {
        let response = next.run(request).await;
        if scope != Scope::Read {
            let _ = state.jobs.audit(
                None,
                &method,
                &path,
                response.status().as_u16(),
                started.elapsed().as_secs_f64() * 1_000.0,
            );
        }
        return response;
    }
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if let Some(key_id) =
        token.and_then(|token| state.auth.authorize(token, scope, table.as_deref()))
    {
        request.extensions_mut().insert(key_id.clone());
        let response = next.run(request).await;
        if scope != Scope::Read {
            let _ = state.jobs.audit(
                Some(&key_id),
                &method,
                &path,
                response.status().as_u16(),
                started.elapsed().as_secs_f64() * 1_000.0,
            );
        }
        response
    } else {
        let response = WebError::unauthorized().into_response();
        let _ = state.jobs.audit(
            None,
            &method,
            &path,
            response.status().as_u16(),
            started.elapsed().as_secs_f64() * 1_000.0,
        );
        response
    }
}

fn required_access(method: &Method, path: &str) -> (Scope, Option<String>) {
    let table = path
        .split("/tables/")
        .nth(1)
        .and_then(|tail| tail.split('/').next())
        .map(str::to_owned);
    let direct_table_delete = method == Method::DELETE
        && path
            .split("/tables/")
            .nth(1)
            .is_some_and(|tail| !tail.contains('/'));
    let scope = if path.contains("/reindex")
        || path.contains("/optimize")
        || path.contains("/jobs")
        || path.contains("/backups")
    {
        Scope::Maintenance
    } else if path.contains("/schema-changes")
        || path.contains("/auth/")
        || path.ends_with("/audit")
        || direct_table_delete
        || (path.ends_with("/tables") && method == Method::POST)
    {
        Scope::Admin
    } else if matches!(*method, Method::GET | Method::HEAD)
        || path.ends_with("/query")
        || path.ends_with("/aggregate-intermediate")
        || path.ends_with("/aggregate-merge")
        || path.ends_with("/explain")
        || path.ends_with("/explain-score")
    {
        Scope::Read
    } else {
        Scope::Write
    };
    (scope, table)
}

fn load_keys(path: Option<&Path>) -> anyhow::Result<Vec<KeyConfig>> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    let mut file: KeyFile = serde_json::from_slice(&std::fs::read(path)?)?;
    for key in &mut file.keys {
        anyhow::ensure!(
            key.sha256.len() == 64 && key.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "invalid SHA-256 for key {}",
            key.id
        );
        anyhow::ensure!(!key.scopes.is_empty(), "key {} has no scopes", key.id);
        key.sha256.make_ascii_lowercase();
    }
    Ok(file.keys)
}

fn table_matches(pattern: &str, table: &str) -> bool {
    pattern == "*"
        || pattern
            .strip_suffix('*')
            .is_some_and(|prefix| table.starts_with(prefix))
        || pattern.eq_ignore_ascii_case(table)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        difference |= usize::from(
            left.get(index).copied().unwrap_or(0) ^ right.get(index).copied().unwrap_or(0),
        );
    }
    difference == 0
}

fn all_tables() -> Vec<String> {
    vec!["*".into()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashed_keys_enforce_scope_and_table_allowlist() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("keys.json");
        let digest = hex::encode(Sha256::digest(b"high-entropy-test-token"));
        std::fs::write(&path, format!(r#"{{"keys":[{{"id":"reader","sha256":"{digest}","scopes":["read"],"tables":["public_*"]}}]}}"#)).unwrap();
        let auth = AuthState::open(Some(path), None).unwrap();
        assert_eq!(
            auth.authorize("high-entropy-test-token", Scope::Read, Some("public_items"))
                .as_deref(),
            Some("reader")
        );
        assert!(
            auth.authorize(
                "high-entropy-test-token",
                Scope::Write,
                Some("public_items")
            )
            .is_none()
        );
        assert!(
            auth.authorize(
                "high-entropy-test-token",
                Scope::Read,
                Some("private_items")
            )
            .is_none()
        );
    }
}
