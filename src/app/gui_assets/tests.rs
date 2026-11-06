use super::*;
use std::collections::BTreeSet;

/// Serves this thread's requests from `dir` until the guard drops.
pub(in crate::app) struct DevelopmentDir(());
impl DevelopmentDir {
    pub(in crate::app) fn set(dir: Option<&Path>) -> Self {
        TEST_SOURCE.with(|source| *source.borrow_mut() = Some(dir.map(Path::to_path_buf)));
        DevelopmentDir(())
    }
}
impl Drop for DevelopmentDir {
    fn drop(&mut self) {
        TEST_SOURCE.with(|source| *source.borrow_mut() = None);
    }
}

/// A development copy of `desktop/ui` in which every file differs from the embedded one.
pub(in crate::app) fn edited_copy() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for asset in ASSETS {
        let mut bytes = asset.embedded.to_vec();
        bytes.extend_from_slice(b"\n/* edited on disk */\n");
        std::fs::write(dir.path().join(asset.file), bytes).unwrap();
    }
    dir
}

/// What a listener must serve for `asset` from `source`.
pub(in crate::app) fn expected(source: Option<&Path>, asset: &Asset) -> Vec<u8> {
    bytes_from(source, asset.file).unwrap().into_owned()
}

/// The headers every GUI response carries, from the binary or from disk alike.
pub(in crate::app) fn assert_secure_headers(response: &Response, what: &str) {
    let headers = response.headers();
    assert_eq!(headers[header::CACHE_CONTROL], "no-store", "{what}");
    assert_eq!(
        headers[header::CONTENT_SECURITY_POLICY],
        "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
        "{what}"
    );
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff", "{what}");
    assert_eq!(headers[header::REFERRER_POLICY], "no-referrer", "{what}");
    assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY", "{what}");
}

fn ui_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("desktop/ui")
}

#[test]
fn every_file_under_desktop_ui_is_served_and_every_served_file_exists() {
    let on_disk: BTreeSet<String> = std::fs::read_dir(ui_dir())
        .unwrap()
        .map(|entry| entry.unwrap())
        .inspect(|entry| {
            assert!(
                entry.file_type().unwrap().is_file(),
                "desktop/ui/{} is not a file: the GUI is flat",
                entry.file_name().to_string_lossy()
            )
        })
        .map(|entry| entry.file_name().into_string().unwrap())
        .collect();
    let served: BTreeSet<String> = ASSETS.iter().map(|a| a.file.to_owned()).collect();
    assert_eq!(served.len(), ASSETS.len(), "a file is listed twice");
    assert_eq!(
        on_disk, served,
        "desktop/ui and gui_assets::ASSETS must name the same files"
    );
    for asset in ASSETS {
        assert_eq!(
            asset.embedded,
            std::fs::read(ui_dir().join(asset.file)).unwrap(),
            "{}",
            asset.file
        );
    }
}

#[test]
fn the_document_and_its_scripts_need_a_session_and_nothing_else_does() {
    let gated: BTreeSet<&str> = ASSETS
        .iter()
        .filter(|a| a.session)
        .map(|a| a.file)
        .collect();
    assert_eq!(
        gated,
        BTreeSet::from(["index.html", "app.js", "transport.js"])
    );
    assert!(public().all(|asset| !asset.session));
}

#[test]
fn a_release_build_ignores_the_development_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.html"), "changed").unwrap();
    std::fs::write(dir.path().join("app.js"), "changed").unwrap();
    let value = dir.path().as_os_str();
    assert_eq!(development_dir(false, Some(value)), None);
    assert_eq!(development_dir(false, None), None);
    // What a release build therefore serves: the embedded copy, never the directory's.
    let source = development_dir(false, Some(value));
    assert_eq!(
        bytes_from(source.as_deref(), "app.js").unwrap().as_ref(),
        find("app.js").embedded
    );
    // A debug build with the same value serves the directory.
    let source = development_dir(true, Some(value)).unwrap();
    assert_eq!(source, dir.path().canonicalize().unwrap());
    assert_eq!(
        bytes_from(Some(&source), "app.js").unwrap().as_ref(),
        b"changed"
    );
}

#[test]
fn the_release_binary_does_not_read_the_variable() {
    // The variable is read in one place, under `cfg(debug_assertions)`, so a release build
    // ignores it whatever the environment holds.
    let code = include_str!("../gui_assets.rs");
    let lines: Vec<&str> = code.lines().map(str::trim).collect();
    let reads: Vec<usize> = (0..lines.len())
        .filter(|&i| lines[i].contains("std::env::var_os(DEV_DIR_VARIABLE)"))
        .collect();
    assert_eq!(reads.len(), 1);
    assert_eq!(lines[reads[0] - 1], "#[cfg(debug_assertions)]");
    assert_eq!(code.matches("var_os(").count(), 1);
}

#[test]
fn only_an_absolute_existing_gui_directory_is_a_development_directory() {
    let dir = tempfile::tempdir().unwrap();
    // No document: not a GUI directory.
    assert_eq!(development_dir(true, Some(dir.path().as_os_str())), None);
    std::fs::write(dir.path().join("index.html"), "x").unwrap();
    assert!(development_dir(true, Some(dir.path().as_os_str())).is_some());
    assert_eq!(development_dir(true, Some("desktop/ui".as_ref())), None);
    assert_eq!(
        development_dir(true, Some(dir.path().join("missing").as_os_str())),
        None
    );
    assert_eq!(
        development_dir(true, Some(dir.path().join("index.html").as_os_str())),
        None
    );
}

#[test]
fn reads_from_the_development_directory_never_leave_it() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("ui");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("app.js"), "inside").unwrap();
    std::fs::write(root.path().join("secret.txt"), "outside").unwrap();
    let dir = dir.canonicalize().unwrap();
    assert_eq!(read_from(&dir, "app.js").unwrap(), b"inside");
    for escape in [
        "../secret.txt",
        "..",
        ".",
        "",
        "/etc/passwd",
        "sub/app.js",
        "..\\secret.txt",
        "C:secret.txt",
    ] {
        assert_eq!(read_from(&dir, escape), None, "{escape}");
    }
    // A symbolic link inside the directory that points outside it is refused too.
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.path().join("secret.txt"), dir.join("style.css")).unwrap();
        assert_eq!(read_from(&dir, "style.css"), None);
        assert!(
            serve_from(Some(&dir), "style.css").status() == StatusCode::NOT_FOUND,
            "a link that leaves the directory is not served"
        );
    }
    // A file missing from the directory is not found; the embedded copy is not mixed in.
    assert_eq!(
        serve_from(Some(&dir), "i18n.js").status(),
        StatusCode::NOT_FOUND
    );
}

#[test]
fn the_variable_is_on_no_allowlist_a_child_process_inherits() {
    use std::ffi::OsString;
    let name = DEV_DIR_VARIABLE;
    assert!(
        name.starts_with("PTA_"),
        "reserved for the host (consumers)"
    );
    assert!(!crate::llm::agent::PASSED_ENV.contains(&name));
    assert!(!crate::app::web::profiles::WORKER_ENV.contains(&name));
    let kept =
        crate::maintenance::build_environment([(OsString::from(name), OsString::from("/tmp/ui"))]);
    assert!(kept.iter().all(|(key, _)| key != name));
}
