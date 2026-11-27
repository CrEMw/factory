//! hub callers: every service that calls the API presents its own name and its own secret.
//!
//! Request: `X-Hub-Caller: <name>` and `Authorization: Bearer <secret>`. Secrets are files in the
//! `HUB_CALLERS_DIR` directory: the file name is the caller name (`hub-web`, later `hub-ingest`, `hub-analysis-worker`),
//! the content is its secret. In the cluster the directory is a projected volume of ExternalSecret secrets
//! (deploy/overlay/templates/hub/deployment.yaml, one secret store key per caller), a new caller
//! is added as one more source.
//!
//! hub re-reads the directory every [`RELOAD_EVERY`]: a secret change in the secret store reaches it through ESO and kubelet without
//! a restart. The caller's previous secret stays valid for [`GRACE`] until the caller has re-read its file.
//! If the directory becomes empty after there were secrets, hub keeps the last known ones and does not open up.
//!
//! Without secrets (no directory or an empty one) and without `HUB_CALLERS_REQUIRED=true` hub lets everyone in and logs a WARN:
//! that is how a local `aspire run` works. In the cluster `HUB_CALLERS_REQUIRED=true`, and hub does not start without secrets.
//! Route permissions ([`route_allowed`]): callers `docs-<repo>` may only call
//! `POST /api/documents/sync`, and only they may call that route; otherwise 403 `caller_not_allowed`. A leaked
//! document build key neither reads nor edits tickets, and the hub-web key does not write documents.
//! `/health` is always open. The verified caller name goes into the request extensions ([`Caller`]) and into the span
//! (`hub.caller`) for the actor journal; without verification it is `unauthenticated`, and the claimed name goes
//! only to `hub.caller_claimed`. Secrets are written neither to spans nor to logs.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use axum::{
    Json,
    extract::{Request, State},
    http::{Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use sha2::{Digest, Sha256};

pub const CALLER_HEADER: &str = "x-hub-caller";
pub const UNAUTHENTICATED: &str = "unauthenticated";
const MIN_SECRET_BYTES: usize = 32;
/// How often hub re-reads the secrets directory.
pub const RELOAD_EVERY: Duration = Duration::from_secs(30);
/// How long the caller's previous secret stays valid after a change: enough for ESO, kubelet and the caller's cache (30 s).
pub const GRACE: Duration = Duration::from_secs(15 * 60);

/// Caller name for the actor journal: the verified name or [`UNAUTHENTICATED`].
#[derive(Clone, Debug, PartialEq)]
pub struct Caller(pub String);

/// The list of callers with the hashes of their secrets, as it is in the directory at one moment.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CallerSet(Vec<(String, [u8; 32])>);

/// Caller name: lowercase Latin letters, digits and hyphens, up to 40 characters.
fn valid_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= 40
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

fn digest(secret: &[u8]) -> [u8; 32] {
    Sha256::digest(secret).into()
}

/// Constant-time comparison: hashes of equal length are compared, all bytes to the end.
fn same(a: &[u8; 32], b: &[u8; 32]) -> bool {
    std::hint::black_box(a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y))) == 0
}

impl CallerSet {
    pub fn new<N: Into<String>, S: AsRef<[u8]>>(
        list: impl IntoIterator<Item = (N, S)>,
    ) -> anyhow::Result<Self> {
        let mut out = Vec::new();
        for (name, secret) in list {
            let name = name.into();
            anyhow::ensure!(
                valid_name(&name),
                "caller name {name:?}: [a-z][a-z0-9-]{{0,39}}"
            );
            let secret = secret.as_ref();
            anyhow::ensure!(
                secret.len() >= MIN_SECRET_BYTES,
                "caller {name}: secret shorter than {MIN_SECRET_BYTES} bytes"
            );
            anyhow::ensure!(!out.iter().any(|(n, _)| *n == name), "caller {name} twice");
            out.push((name, digest(secret)));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(Self(out))
    }

    /// Secrets directory: one file per caller. No directory or empty files: the list is empty.
    /// Hidden entries (`..data` and other kubelet service links) are skipped.
    pub fn load(dir: &Path) -> anyhow::Result<Self> {
        use anyhow::Context;
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).with_context(|| format!("read {}", dir.display())),
        };
        let mut list = Vec::new();
        for entry in entries {
            let entry = entry.with_context(|| format!("read {}", dir.display()))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            // metadata follows the link: in a secret volume the files are links to ..data/<name>.
            if !std::fs::metadata(&path)
                .with_context(|| format!("stat {}", path.display()))?
                .is_file()
            {
                continue;
            }
            let secret =
                std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
            let secret = secret.trim_ascii();
            if !secret.is_empty() {
                list.push((name, secret.to_vec()));
            }
        }
        Self::new(list)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn names(&self) -> Vec<&str> {
        self.0.iter().map(|(n, _)| n.as_str()).collect()
    }
}

struct Entry {
    name: String,
    current: [u8; 32],
    /// Previous secret and the moment of the change: valid for [`GRACE`].
    previous: Option<([u8; 32], Instant)>,
}

/// What [`Callers::update`] changed; for the log.
#[derive(Debug, PartialEq)]
pub enum Update {
    Unchanged,
    Changed,
    /// There were secrets, but the directory became empty: the last known ones are kept.
    KeptLast,
}

/// The active callers; an object shared by the layer and the directory reload.
#[derive(Clone, Default)]
pub struct Callers(Arc<RwLock<Vec<Entry>>>);

impl Callers {
    pub fn from_set(set: CallerSet) -> Self {
        let callers = Self::default();
        callers.update(set, Instant::now());
        callers
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Vec<Entry>> {
        self.0.read().unwrap_or_else(|e| e.into_inner())
    }

    pub fn enforced(&self) -> bool {
        !self.read().is_empty()
    }

    pub fn names(&self) -> Vec<String> {
        self.read().iter().map(|e| e.name.clone()).collect()
    }

    /// A new directory snapshot. A changed secret is remembered as the previous one; a vanished caller is revoked at once.
    /// An empty snapshot after a non-empty one does not open hub.
    pub fn update(&self, set: CallerSet, now: Instant) -> Update {
        let mut entries = self.0.write().unwrap_or_else(|e| e.into_inner());
        if set.is_empty() && !entries.is_empty() {
            return Update::KeptLast;
        }
        let mut changed = set.0.len() != entries.len();
        let next = set
            .0
            .into_iter()
            .map(|(name, d)| match entries.iter().find(|e| e.name == name) {
                Some(old) if same(&old.current, &d) => Entry {
                    name,
                    current: d,
                    previous: old.previous,
                },
                Some(old) => {
                    changed = true;
                    Entry {
                        name,
                        current: d,
                        previous: Some((old.current, now)),
                    }
                }
                None => {
                    changed = true;
                    Entry {
                        name,
                        current: d,
                        previous: None,
                    }
                }
            })
            .collect();
        *entries = next;
        if changed {
            Update::Changed
        } else {
            Update::Unchanged
        }
    }

    /// Caller name, if the name is known and the secret is theirs: the current one or the previous one within [`GRACE`].
    fn check(&self, name: &str, secret: &str, now: Instant) -> Option<Caller> {
        let given = digest(secret.as_bytes());
        self.read()
            .iter()
            .find(|e| e.name == name)
            .filter(|e| {
                let current = same(&e.current, &given);
                let previous = e.previous.is_some_and(|(d, since)| {
                    same(&d, &given) && now.saturating_duration_since(since) < GRACE
                });
                current | previous
            })
            .map(|e| Caller(e.name.clone()))
    }

    /// Re-reads the directory every [`RELOAD_EVERY`]. A read error keeps the previous list.
    pub fn spawn_reload(&self, dir: PathBuf) {
        let callers = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(RELOAD_EVERY);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            tick.tick().await;
            loop {
                tick.tick().await;
                let dir = dir.clone();
                let set = match tokio::task::spawn_blocking(move || CallerSet::load(&dir)).await {
                    Ok(Ok(set)) => set,
                    Ok(Err(err)) => {
                        tracing::warn!(
                            error = format!("{err:#}"),
                            "hub callers reload failed, keeping the last list"
                        );
                        continue;
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "hub callers reload task failed");
                        continue;
                    }
                };
                match callers.update(set, Instant::now()) {
                    Update::Unchanged => {}
                    Update::Changed => {
                        tracing::info!(callers = ?callers.names(), "hub callers reloaded")
                    }
                    Update::KeptLast => tracing::warn!(
                        callers = ?callers.names(),
                        "hub caller secrets vanished from HUB_CALLERS_DIR, keeping the last known"
                    ),
                }
            }
        });
    }
}

/// Route table: a `docs-*` caller may only call document sync, and document sync is only for them.
pub fn route_allowed(caller: &str, method: &Method, path: &str) -> bool {
    let sync = path == "/api/documents/sync";
    if caller.starts_with(crate::documents::DOCS_CALLER_PREFIX) {
        sync && method == Method::POST
    } else {
        !sync
    }
}

fn header_str(req: &Request, name: impl header::AsHeaderName) -> Option<&str> {
    req.headers().get(name).and_then(|v| v.to_str().ok())
}

/// Layer on all `/api/*` routes: 401 without a valid name and secret.
pub async fn require(State(callers): State<Callers>, mut req: Request, next: Next) -> Response {
    let name = header_str(&req, CALLER_HEADER).unwrap_or("").trim();
    let span = tracing::Span::current();
    let caller = if callers.enforced() {
        let secret = header_str(&req, header::AUTHORIZATION)
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("")
            .trim();
        match callers.check(name, secret, Instant::now()) {
            Some(caller) if !route_allowed(&caller.0, req.method(), req.uri().path()) => {
                tracing::warn!(
                    caller = caller.0.as_str(),
                    path = req.uri().path(),
                    "hub caller not allowed on this route"
                );
                return (
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({"error": "caller_not_allowed"})),
                )
                    .into_response();
            }
            Some(caller) => caller,
            None => {
                // Only a valid name from the header goes to the journal: otherwise an arbitrary string would end up there.
                let shown = if valid_name(name) { name } else { "<invalid>" };
                tracing::warn!(caller = shown, "hub caller rejected");
                return (
                    StatusCode::UNAUTHORIZED,
                    [(header::WWW_AUTHENTICATE, "Bearer")],
                    "unknown caller or wrong secret",
                )
                    .into_response();
            }
        }
    } else {
        // Open mode: the header name is not trusted, the actor journal sees only `unauthenticated`.
        if valid_name(name) {
            span.record("hub.caller_claimed", name);
        }
        Caller(UNAUTHENTICATED.to_owned())
    };
    span.record("hub.caller", caller.0.as_str());
    req.extensions_mut().insert(caller);
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(list: &[(&str, &str)]) -> CallerSet {
        CallerSet::new(list.iter().copied()).unwrap()
    }

    #[test]
    fn names_and_secrets_are_checked() {
        assert!(CallerSet::new([("hub-web", "s".repeat(31))]).is_err());
        assert!(CallerSet::new([("Hub", "s".repeat(32))]).is_err());
        assert!(
            CallerSet::new([("hub-web", "a".repeat(32)), ("hub-web", "b".repeat(32))]).is_err()
        );
        let c = Callers::from_set(
            CallerSet::new([("hub-web", "a".repeat(32)), ("hub-ingest", "b".repeat(40))]).unwrap(),
        );
        let now = Instant::now();
        assert_eq!(c.names(), ["hub-ingest", "hub-web"]);
        assert_eq!(
            c.check("hub-web", &"a".repeat(32), now),
            Some(Caller("hub-web".into()))
        );
        assert_eq!(
            c.check("hub-ingest", &"a".repeat(32), now),
            None,
            "secret of another caller"
        );
        assert_eq!(c.check("hub-web", &"a".repeat(33), now), None);
        assert_eq!(c.check("nobody", &"a".repeat(32), now), None);
        assert_eq!(c.check("", "", now), None);
    }

    #[test]
    fn rotation_keeps_the_previous_secret_for_a_grace_period() {
        let (old, new, third) = ("o".repeat(32), "n".repeat(32), "t".repeat(32));
        let t0 = Instant::now();
        let c = Callers::from_set(set(&[("hub-web", &old)]));
        assert_eq!(c.update(set(&[("hub-web", &old)]), t0), Update::Unchanged);
        assert_eq!(c.update(set(&[("hub-web", &new)]), t0), Update::Changed);
        assert!(c.check("hub-web", &new, t0).is_some());
        assert!(
            c.check("hub-web", &old, t0 + GRACE / 2).is_some(),
            "previous in grace"
        );
        assert!(
            c.check("hub-web", &old, t0 + GRACE).is_none(),
            "previous after grace"
        );
        // A repeated snapshot with the same secret does not extend the previous one.
        assert_eq!(
            c.update(set(&[("hub-web", &new)]), t0 + GRACE / 2),
            Update::Unchanged
        );
        assert!(c.check("hub-web", &old, t0 + GRACE).is_none());
        // The next change: only the last two are valid.
        c.update(set(&[("hub-web", &third)]), t0);
        assert!(c.check("hub-web", &new, t0).is_some());
        assert!(c.check("hub-web", &old, t0).is_none());
    }

    #[test]
    fn reload_adds_and_revokes_callers_but_never_opens() {
        let (a, b) = ("a".repeat(32), "b".repeat(32));
        let t0 = Instant::now();
        let c = Callers::default();
        assert!(!c.enforced(), "no secrets yet: open");
        assert_eq!(c.update(set(&[("hub-web", &a)]), t0), Update::Changed);
        assert!(c.enforced(), "secret appeared: enforced without restart");
        c.update(set(&[("hub-web", &a), ("hub-ingest", &b)]), t0);
        assert!(c.check("hub-ingest", &b, t0).is_some());
        assert_eq!(c.update(set(&[("hub-web", &a)]), t0), Update::Changed);
        assert!(
            c.check("hub-ingest", &b, t0).is_none(),
            "removed caller revoked"
        );
        assert_eq!(c.update(CallerSet::default(), t0), Update::KeptLast);
        assert!(c.enforced(), "an empty directory does not open hub");
        assert!(c.check("hub-web", &a, t0).is_some());
    }

    async fn caller_seen_by_handler(callers: Callers, auth: Option<&str>) -> (StatusCode, String) {
        use axum::{Extension, Router, body::Body, routing::get};
        use tower::ServiceExt;
        let app = Router::new()
            .route(
                "/api/x",
                get(|Extension(c): Extension<Caller>| async move { c.0 }),
            )
            .route_layer(axum::middleware::from_fn_with_state(callers, require));
        let mut req = Request::get("/api/x").header(CALLER_HEADER, "hub-web");
        if let Some(a) = auth {
            req = req.header(header::AUTHORIZATION, a);
        }
        let res = app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
        let status = res.status();
        let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn open_mode_does_not_trust_the_claimed_name() {
        assert_eq!(
            caller_seen_by_handler(Callers::default(), None).await,
            (StatusCode::OK, UNAUTHENTICATED.to_owned())
        );
        let secret = "s".repeat(32);
        let guarded = Callers::from_set(set(&[("hub-web", &secret)]));
        assert_eq!(
            caller_seen_by_handler(guarded, Some(&format!("Bearer {secret}"))).await,
            (StatusCode::OK, "hub-web".to_owned())
        );
    }

    #[test]
    fn docs_callers_reach_only_the_document_sync() {
        let sync = "/api/documents/sync";
        assert!(route_allowed("docs-demo", &Method::POST, sync));
        assert!(!route_allowed("docs-demo", &Method::GET, sync));
        assert!(!route_allowed("docs-demo", &Method::GET, "/api/tickets"));
        assert!(!route_allowed(
            "docs-demo",
            &Method::GET,
            "/api/documents/docs/superpowers/specs/a.md"
        ));
        assert!(!route_allowed("hub-web", &Method::POST, sync));
        assert!(route_allowed("hub-web", &Method::GET, "/api/tickets"));
        assert!(route_allowed(
            "hub-web",
            &Method::GET,
            "/api/documents/docs/x.md"
        ));
    }

    #[test]
    fn directory_is_read_like_a_secret_volume() {
        let dir = std::env::temp_dir().join(format!("hub-callers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(CallerSet::load(&dir).unwrap().is_empty(), "no directory");
        std::fs::create_dir_all(dir.join("..data")).unwrap();
        assert!(CallerSet::load(&dir).unwrap().is_empty(), "empty directory");
        std::fs::write(dir.join("..data/hub-web"), format!("{}\n", "a".repeat(48))).unwrap();
        std::os::unix::fs::symlink("..data/hub-web", dir.join("hub-web")).unwrap();
        std::fs::write(dir.join("hub-ingest"), "").unwrap();
        let s = CallerSet::load(&dir).unwrap();
        assert_eq!(s.names(), ["hub-web"], "empty file is not a caller");
        assert!(
            Callers::from_set(s)
                .check("hub-web", &"a".repeat(48), Instant::now())
                .is_some(),
            "newline trimmed"
        );
        std::fs::write(dir.join("hub-ingest"), "short").unwrap();
        assert!(CallerSet::load(&dir).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
