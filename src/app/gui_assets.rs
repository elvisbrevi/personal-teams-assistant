//! The GUI's files (`desktop/ui`), served by both listeners: the portal (`web.rs`) and the owner's
//! local page (`page.rs`). This is the one list of what is served; each listener decides only
//! which paths need its session, and its middleware adds the same security headers (CSP,
//! `no-store`, `nosniff`…) whatever the source of the bytes.
//!
//! Release builds serve the copies compiled into the binary, always. A debug build started with
//! [`DEV_DIR_VARIABLE`] set to a directory with the same files reads each one from that directory
//! on every request instead, so editing a file and refreshing the browser is the whole loop
//! (`scripts/dev-host.sh`). The variable is read only in debug builds, only once, and is on no
//! allowlist of the environment a child process inherits.
use axum::{
    http::{StatusCode, header},
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
    /// The file's name under `desktop/ui`; it is served at `/<file>`.
    pub(super) file: &'static str,
    pub(super) content_type: &'static str,
    /// The document and its scripts: each listener serves them only with its own session.
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

/// Every file of `desktop/ui`, and only those (`gui_assets::tests` checks both directions).
pub(super) const ASSETS: &[Asset] = &[
    asset!("index.html", "text/html; charset=utf-8", true),
    asset!("app.js", "text/javascript; charset=utf-8", true),
    asset!("transport.js", "text/javascript; charset=utf-8", true),
    asset!("i18n.js", "text/javascript; charset=utf-8", false),
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

/// One GUI file as a response, with its content type; the listener's middleware adds the rest.
pub(super) fn serve(file: &str) -> Response {
    serve_from(source().as_deref(), file)
}

pub(super) fn serve_from(source: Option<&Path>, file: &str) -> Response {
    let asset = find(file);
    match bytes_from(source, file) {
        Some(body) => ([(header::CONTENT_TYPE, asset.content_type)], body).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
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
