//! The GUI's files (`desktop/ui`), served by both listeners: the portal (`web.rs`) and the owner's
//! local page (`page.rs`). This is the one list of what is served; each listener decides only
//! which paths need its session, and its middleware adds the same security headers (CSP,
//! `nosniff`…) whatever the source of the bytes.
//!
//! Caching: the document and every other response of the listeners are `no-store`. The GUI's
//! other files (its modules, stylesheet, icons and manifest) are the same public code for
//! everybody and hold no account data, so the browser may keep them privately but must ask before
//! each use ([`CACHE_RULE`]): the listener answers `304 Not Modified` while the file still has the
//! [`etag`] the browser holds (a hash of its bytes), through the same session check as the file
//! itself. A new version, or an edited file on the development path, has a new tag, so a cached
//! copy is never used once it changed. Splitting the GUI into modules made this worth it: a
//! repeat visit downloads none of them (docs/ui-design-conventions.md §2).
//!
//! Release builds serve the copies compiled into the binary, always. A debug build started with
//! [`DEV_DIR_VARIABLE`] set to a directory with the same files reads each one from that directory
//! on every request instead, so editing a file and refreshing the browser is the whole loop
//! (`scripts/dev-host.sh`). The variable is read only in debug builds, only once, and is on no
//! allowlist of the environment a child process inherits.
use axum::{
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    sync::OnceLock,
};

/// Development builds only: serve `desktop/ui` from this absolute directory, uncached.
pub const DEV_DIR_VARIABLE: &str = "PTA_DEV_UI_DIR";

pub(super) struct Asset {
    /// The file's name directly under `desktop/ui` (the GUI is flat); it is served at `/<file>`.
    pub(super) file: &'static str,
    pub(super) content_type: &'static str,
    /// The document and its modules: each listener serves them only with its own session.
    pub(super) session: bool,
    embedded: &'static [u8],
}

macro_rules! asset {
    ($file:literal, $content_type:literal, $session:literal) => {
        Asset {
            file: $file,
            content_type: $content_type,
            session: $session,
            embedded: include_bytes!(concat!("../../desktop/ui/", $file)),
        }
    };
}

/// Every file of `desktop/ui`, and only those (`gui_assets::tests` checks both directions). The
/// GUI is native ES modules: `app.js` is the entry the document loads, the others are what it
/// imports (`tests` checks that every import is served and that the document preloads the graph).
/// Every module needs the session except the translation, which also translates the pages shown
/// without one and holds no account data.
pub(super) const ASSETS: &[Asset] = &[
    asset!("index.html", "text/html; charset=utf-8", true),
    asset!("app.js", "text/javascript; charset=utf-8", true),
    asset!("transport.js", "text/javascript; charset=utf-8", true),
    asset!("control.js", "text/javascript; charset=utf-8", true),
    asset!("state.js", "text/javascript; charset=utf-8", true),
    asset!("dom.js", "text/javascript; charset=utf-8", true),
    asset!("appearance.js", "text/javascript; charset=utf-8", true),
    asset!("notices.js", "text/javascript; charset=utf-8", true),
    asset!("nav.js", "text/javascript; charset=utf-8", true),
    asset!("markdown.js", "text/javascript; charset=utf-8", true),
    asset!("dialogs.js", "text/javascript; charset=utf-8", true),
    asset!("directory-sheet.js", "text/javascript; charset=utf-8", true),
    asset!("home.js", "text/javascript; charset=utf-8", true),
    asset!("profile.js", "text/javascript; charset=utf-8", true),
    asset!("todo.js", "text/javascript; charset=utf-8", true),
    asset!("todo-request.js", "text/javascript; charset=utf-8", true),
    asset!("sessions.js", "text/javascript; charset=utf-8", true),
    asset!("implementation.js", "text/javascript; charset=utf-8", true),
    asset!("guides.js", "text/javascript; charset=utf-8", true),
    asset!("messages.js", "text/javascript; charset=utf-8", true),
    asset!("activities.js", "text/javascript; charset=utf-8", true),
    asset!("knowledge.js", "text/javascript; charset=utf-8", true),
    asset!("knowledge-map.js", "text/javascript; charset=utf-8", true),
    asset!("chat.js", "text/javascript; charset=utf-8", true),
    asset!("workflows.js", "text/javascript; charset=utf-8", true),
    asset!("workflow-blocks.js", "text/javascript; charset=utf-8", true),
    asset!("run-states.js", "text/javascript; charset=utf-8", true),
    asset!("workflow-runs.js", "text/javascript; charset=utf-8", true),
    asset!(
        "workflow-supervisor.js",
        "text/javascript; charset=utf-8",
        true
    ),
    asset!("workflow-pause.js", "text/javascript; charset=utf-8", true),
    asset!(
        "settings-general.js",
        "text/javascript; charset=utf-8",
        true
    ),
    asset!(
        "settings-account.js",
        "text/javascript; charset=utf-8",
        true
    ),
    asset!(
        "settings-machines.js",
        "text/javascript; charset=utf-8",
        true
    ),
    asset!("settings-models.js", "text/javascript; charset=utf-8", true),
    asset!("settings-daily.js", "text/javascript; charset=utf-8", true),
    asset!(
        "settings-variables.js",
        "text/javascript; charset=utf-8",
        true
    ),
    asset!(
        "settings-migration.js",
        "text/javascript; charset=utf-8",
        true
    ),
    asset!(
        "settings-updates.js",
        "text/javascript; charset=utf-8",
        true
    ),
    asset!(
        "settings-service.js",
        "text/javascript; charset=utf-8",
        true
    ),
    asset!("settings-cloud.js", "text/javascript; charset=utf-8", true),
    asset!(
        "settings-workflows.js",
        "text/javascript; charset=utf-8",
        true
    ),
    asset!("i18n.js", "text/javascript; charset=utf-8", false),
    asset!("catalog.js", "text/javascript; charset=utf-8", false),
    asset!("style.css", "text/css; charset=utf-8", false),
    asset!("manifest.webmanifest", "application/manifest+json", false),
    asset!("favicon.png", "image/png", false),
    asset!("apple-touch-icon.png", "image/png", false),
    asset!("icon-192.png", "image/png", false),
    asset!("icon-512.png", "image/png", false),
];

/// The files served without a session, for the listeners' routers.
pub(super) fn public() -> impl Iterator<Item = &'static Asset> {
    ASSETS.iter().filter(|asset| !asset.session)
}

/// The modules served only with a listener's session (the document has its own route).
pub(super) fn session_scripts() -> impl Iterator<Item = &'static Asset> {
    ASSETS
        .iter()
        .filter(|asset| asset.session && asset.file != "index.html")
}

fn find(file: &str) -> &'static Asset {
    ASSETS
        .iter()
        .find(|asset| asset.file == file)
        .unwrap_or_else(|| panic!("{file} is not a GUI asset"))
}

#[cfg(test)]
thread_local! {
    /// A test's development directory for this thread (`Some(None)` forces the embedded copy).
    pub(super) static TEST_SOURCE: std::cell::RefCell<Option<Option<PathBuf>>> =
        const { std::cell::RefCell::new(None) };
}

/// Where this process reads the GUI from: `None` is the embedded copy.
fn source() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(source) = TEST_SOURCE.with(|source| source.borrow().clone()) {
        return source;
    }
    static SOURCE: OnceLock<Option<PathBuf>> = OnceLock::new();
    SOURCE
        .get_or_init(|| {
            #[cfg(debug_assertions)]
            let value = std::env::var_os(DEV_DIR_VARIABLE);
            #[cfg(not(debug_assertions))]
            let value: Option<std::ffi::OsString> = None;
            let dir = development_dir(cfg!(debug_assertions), value.as_deref());
            match &dir {
                Some(dir) => tracing::warn!(event = "gui_from_disk", dir = %dir.display()),
                None if value.is_some() => tracing::warn!(event = "gui_dir_ignored"),
                None => {}
            }
            dir
        })
        .clone()
}

/// Decides the source once, at a listener's start, so the log says where the GUI comes from.
pub(super) fn resolve() {
    let _ = source();
}

/// The directory a development build serves from: only in a debug build, only an absolute,
/// existing directory that holds the GUI's document. Anything else serves the embedded copy.
pub(super) fn development_dir(
    debug_build: bool,
    value: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    if !debug_build {
        return None;
    }
    let dir = PathBuf::from(value?);
    if !dir.is_absolute() {
        return None;
    }
    let dir = dir.canonicalize().ok()?;
    (dir.is_dir() && dir.join("index.html").is_file()).then_some(dir)
}

/// Reads `file` from `dir`, refusing anything that would leave it: separators, `..`, or a
/// symbolic link that resolves outside.
pub(super) fn read_from(dir: &Path, file: &str) -> Option<Vec<u8>> {
    let plain =
        !file.is_empty() && file != "." && file != ".." && !file.contains(['/', '\\', ':', '\0']);
    if !plain {
        return None;
    }
    let target = dir.join(file).canonicalize().ok()?;
    if target.parent() != Some(dir) || !target.is_file() {
        return None;
    }
    std::fs::read(target).ok()
}

/// The bytes of one GUI file from `source`, or `None` when a development directory lacks it.
pub(super) fn bytes_from(source: Option<&Path>, file: &str) -> Option<Cow<'static, [u8]>> {
    let asset = find(file);
    match source {
        None => Some(Cow::Borrowed(asset.embedded)),
        Some(dir) => read_from(dir, asset.file).map(Cow::Owned),
    }
}

/// The text of one GUI file (the document, which a listener adapts before serving it).
pub(super) fn text(file: &str) -> Option<Cow<'static, str>> {
    match bytes_from(source().as_deref(), file)? {
        Cow::Borrowed(bytes) => std::str::from_utf8(bytes).ok().map(Cow::Borrowed),
        Cow::Owned(bytes) => String::from_utf8(bytes).ok().map(Cow::Owned),
    }
}

/// How the browser may cache the GUI's files other than the document: only in its own cache, and
/// never without asking the listener first (the listener's middleware makes everything else
/// `no-store`).
pub(super) const CACHE_RULE: &str = "private, no-cache";

/// A strong validator for a file's bytes: a hash of them.
pub(super) fn etag(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let hex: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
    format!("\"{hex}\"")
}

/// The tags of the embedded copies, hashed once.
fn embedded_etag(asset: &'static Asset) -> &'static str {
    static TAGS: OnceLock<Vec<String>> = OnceLock::new();
    let tags = TAGS.get_or_init(|| ASSETS.iter().map(|asset| etag(asset.embedded)).collect());
    let index = ASSETS
        .iter()
        .position(|candidate| std::ptr::eq(candidate, asset))
        .unwrap();
    &tags[index]
}

/// Whether `If-None-Match` names `tag`. The comparison is weak (RFC 9110): a proxy that compresses
/// the file (Cloudflare) marks the tag weak, and the bytes it stands for are still the same.
fn not_modified(request: &HeaderMap, tag: &str) -> bool {
    request
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .any(|candidate| candidate == "*" || candidate.trim_start_matches("W/") == tag)
}

/// One GUI file as a response, with its content type and, but for the document, its validator and
/// caching rule; the listener's middleware adds the rest. `request` is the request's headers.
pub(super) fn serve(file: &str, request: &HeaderMap) -> Response {
    serve_from(source().as_deref(), file, request)
}

pub(super) fn serve_from(source: Option<&Path>, file: &str, request: &HeaderMap) -> Response {
    let asset = find(file);
    let Some(body) = bytes_from(source, file) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if asset.file == "index.html" {
        // The entry point always comes fresh from the listener (the middleware's `no-store`).
        return ([(header::CONTENT_TYPE, asset.content_type)], body).into_response();
    }
    let tag = match &body {
        Cow::Borrowed(_) => embedded_etag(asset).to_owned(),
        Cow::Owned(bytes) => etag(bytes),
    };
    if not_modified(request, &tag) {
        return (
            StatusCode::NOT_MODIFIED,
            [
                (header::ETAG, tag),
                (header::CACHE_CONTROL, CACHE_RULE.into()),
            ],
        )
            .into_response();
    }
    (
        [
            (header::CONTENT_TYPE, asset.content_type.to_owned()),
            (header::ETAG, tag),
            (header::CACHE_CONTROL, CACHE_RULE.into()),
        ],
        body,
    )
        .into_response()
}

/// An adapted document, with the document's content type.
pub(super) fn document(body: String) -> Response {
    (
        [(header::CONTENT_TYPE, find("index.html").content_type)],
        body,
    )
        .into_response()
}

#[cfg(test)]
pub(super) mod tests;
