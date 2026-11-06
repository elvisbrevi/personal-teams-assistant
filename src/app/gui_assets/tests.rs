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

/// The headers every response of a listener carries, `no-store` included (the document, the
/// sign-in pages, errors).
pub(in crate::app) fn assert_secure_headers(response: &Response, what: &str) {
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "no-store",
        "{what}"
    );
    assert!(response.headers().get(header::ETAG).is_none(), "{what}");
    assert_policy_headers(response, what);
}

/// The headers of one GUI file from `source`: the document is `no-store`; every other file may be
/// kept by the browser only to revalidate it against its tag, a hash of the bytes served.
pub(in crate::app) fn assert_gui_file_headers(
    response: &Response,
    asset: &Asset,
    source: Option<&Path>,
    what: &str,
) {
    if asset.file == "index.html" {
        return assert_secure_headers(response, what);
    }
    let headers = response.headers();
    assert_eq!(headers[header::CACHE_CONTROL], CACHE_RULE, "{what}");
    assert_eq!(
        headers[header::ETAG],
        etag(&expected(source, asset)).as_str(),
        "{what}"
    );
    assert_policy_headers(response, what);
}

/// The tag a listener sends for `asset` from `source`.
pub(in crate::app) fn expected_etag(source: Option<&Path>, asset: &Asset) -> String {
    etag(&expected(source, asset))
}

fn assert_policy_headers(response: &Response, what: &str) {
    let headers = response.headers();
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
fn the_document_and_its_modules_need_a_session_and_nothing_else_does() {
    let gated: BTreeSet<&str> = ASSETS
        .iter()
        .filter(|a| a.session)
        .map(|a| a.file)
        .collect();
    let modules: BTreeSet<&str> = ASSETS
        .iter()
        .map(|a| a.file)
        .filter(|file| file.ends_with(".js"))
        .collect();
    // The translation and its catalog also translate the pages shown without a session.
    let expected: BTreeSet<&str> = modules
        .iter()
        .copied()
        .filter(|file| !["i18n.js", "catalog.js"].contains(file))
        .chain(["index.html"])
        .collect();
    assert_eq!(gated, expected);
    assert!(public().all(|asset| !asset.session));
    assert!(session_scripts().all(|asset| asset.session && asset.file.ends_with(".js")));
    assert_eq!(session_scripts().count(), gated.len() - 1);
    // A public module may import only public modules, or a page without a session breaks.
    for asset in public().filter(|asset| asset.file.ends_with(".js")) {
        for import in imports(asset) {
            assert!(!find(import).session, "{} imports {import}", asset.file);
        }
    }
}

/// The relative specifiers of a module's static `import … from './x.js'` and `export … from`.
fn imports(asset: &Asset) -> Vec<&'static str> {
    let code = std::str::from_utf8(asset.embedded).unwrap();
    let mut found = Vec::new();
    let mut statement = String::new();
    for line in code.lines() {
        if statement.is_empty() && !(line.starts_with("import ") || line.starts_with("export ")) {
            continue;
        }
        statement.push_str(line);
        statement.push('\n');
        if !line.trim_end().ends_with(';') {
            continue;
        }
        if let Some((_, from)) = statement.rsplit_once(" from '") {
            let specifier = from.split('\'').next().unwrap();
            let file = specifier
                .strip_prefix("./")
                .unwrap_or_else(|| panic!("{}: {specifier} is not ./<file>", asset.file));
            let served = ASSETS
                .iter()
                .find(|asset| asset.file == file)
                .unwrap_or_else(|| panic!("{} imports {file}, which is not served", asset.file));
            found.push(served.file);
        }
        statement.clear();
    }
    found
}

#[test]
fn every_module_is_javascript_and_every_import_is_served() {
    // A missing or misspelled module fails here, not in a page load.
    let modules: Vec<&Asset> = ASSETS
        .iter()
        .filter(|asset| asset.file.ends_with(".js"))
        .collect();
    assert!(modules.len() > 30, "only {} modules", modules.len());
    for asset in &modules {
        assert_eq!(
            asset.content_type, "text/javascript; charset=utf-8",
            "{}",
            asset.file
        );
        let code = std::str::from_utf8(asset.embedded).unwrap();
        // Dynamic imports, eval and string-built code are refused by the CSP and by this rule.
        for forbidden in [
            "import(",
            "eval(",
            "new Function(",
            ".innerHTML",
            "insertAdjacentHTML",
            "document.write(",
        ] {
            assert!(!code.contains(forbidden), "{} uses {forbidden}", asset.file);
        }
        for import in imports(asset) {
            assert!(import.ends_with(".js"), "{} imports {import}", asset.file);
        }
    }
}

/// The entry's static module graph, in evaluation order.
fn graph(entry: &'static str) -> Vec<&'static str> {
    fn visit(file: &'static str, seen: &mut Vec<&'static str>) {
        if seen.contains(&file) {
            return;
        }
        seen.push(file);
        for import in imports(find(file)) {
            visit(import, seen);
        }
    }
    let mut seen = Vec::new();
    visit(entry, &mut seen);
    seen
}

#[test]
fn the_document_loads_one_entry_and_preloads_its_whole_graph() {
    let document = std::str::from_utf8(find("index.html").embedded).unwrap();
    let scripts: Vec<&str> = document
        .match_indices("<script")
        .map(|(at, _)| &document[at..])
        .collect();
    assert_eq!(scripts.len(), 1, "one script: the entry module");
    // The entry and its graph download after the first paint, as deferred scripts did: at low
    // priority they do not compete with the stylesheet (docs/ui-design-conventions.md §2).
    assert!(
        scripts[0]
            .starts_with("<script type=\"module\" src=\"app.js\" fetchpriority=\"low\"></script>")
    );
    for hint in document.split("<link rel=\"modulepreload\"").skip(1) {
        let tag = hint.split('>').next().unwrap();
        assert!(tag.ends_with(" fetchpriority=\"low\""), "{tag}");
    }
    let preloaded: BTreeSet<&str> = document
        .split("<link rel=\"modulepreload\" href=\"")
        .skip(1)
        .map(|rest| rest.split('"').next().unwrap())
        .collect();
    let graph: BTreeSet<&str> = graph("app.js")
        .into_iter()
        .filter(|file| *file != "app.js")
        .collect();
    assert_eq!(
        preloaded, graph,
        "index.html must preload exactly the entry's imports"
    );
    // Every module is part of the page: nothing served is left out of the graph.
    let modules: BTreeSet<&str> = ASSETS
        .iter()
        .map(|a| a.file)
        .filter(|file| file.ends_with(".js") && *file != "app.js")
        .collect();
    assert_eq!(graph, modules, "a module the entry never imports");
}

#[test]
fn no_file_exceeds_its_agreed_size() {
    // A module is one view or one shared piece; the catalog is the one place for translations.
    const KIB: usize = 1024;
    for asset in ASSETS {
        let limit = match asset.file {
            "catalog.js" => 320 * KIB,
            "index.html" | "style.css" => 128 * KIB,
            file if file.ends_with(".js") => 40 * KIB,
            _ => 512 * KIB,
        };
        assert!(
            asset.embedded.len() <= limit,
            "{} is {} bytes, over its {limit}",
            asset.file,
            asset.embedded.len()
        );
    }
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
            serve_from(Some(&dir), "style.css", &HeaderMap::new()).status()
                == StatusCode::NOT_FOUND,
            "a link that leaves the directory is not served"
        );
    }
    // A file missing from the directory is not found; the embedded copy is not mixed in.
    assert_eq!(
        serve_from(Some(&dir), "i18n.js", &HeaderMap::new()).status(),
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

#[test]
fn a_gui_file_is_revalidated_against_a_hash_of_its_bytes() {
    let none = HeaderMap::new();
    for asset in ASSETS.iter().filter(|asset| asset.file != "index.html") {
        let response = serve_from(None, asset.file, &none);
        assert_eq!(response.status(), StatusCode::OK, "{}", asset.file);
        assert_eq!(response.headers()[header::CACHE_CONTROL], CACHE_RULE);
        let tag = response.headers()[header::ETAG]
            .to_str()
            .unwrap()
            .to_owned();
        assert_eq!(tag, etag(asset.embedded));
        // The tag the browser holds, strong or as a compressing proxy weakened it, in a list or not.
        for held in [
            tag.clone(),
            format!("W/{tag}"),
            format!("\"other\", {tag}"),
            "*".to_owned(),
        ] {
            let mut request = HeaderMap::new();
            request.insert(header::IF_NONE_MATCH, held.parse().unwrap());
            let response = serve_from(None, asset.file, &request);
            assert_eq!(response.status(), StatusCode::NOT_MODIFIED, "{held}");
            assert_eq!(response.headers()[header::ETAG], tag.as_str());
            assert_eq!(response.headers()[header::CACHE_CONTROL], CACHE_RULE);
            assert!(response.headers().get(header::CONTENT_TYPE).is_none());
        }
        let mut request = HeaderMap::new();
        request.insert(header::IF_NONE_MATCH, "\"0123\"".parse().unwrap());
        assert_eq!(
            serve_from(None, asset.file, &request).status(),
            StatusCode::OK
        );
    }
    // The document is never cached: it names the modules of the version being served.
    let mut request = HeaderMap::new();
    request.insert(header::IF_NONE_MATCH, "*".parse().unwrap());
    let document = serve_from(None, "index.html", &request);
    assert_eq!(document.status(), StatusCode::OK);
    assert!(document.headers().get(header::ETAG).is_none());
    assert!(document.headers().get(header::CACHE_CONTROL).is_none());
}

#[test]
fn an_edited_file_on_the_development_path_gets_a_new_tag() {
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path().canonicalize().unwrap();
    std::fs::write(dir.join("app.js"), "first").unwrap();
    let tag = |dir: &Path| {
        serve_from(Some(dir), "app.js", &HeaderMap::new()).headers()[header::ETAG]
            .to_str()
            .unwrap()
            .to_owned()
    };
    let first = tag(&dir);
    assert_eq!(first, etag(b"first"));
    let mut request = HeaderMap::new();
    request.insert(header::IF_NONE_MATCH, first.parse().unwrap());
    assert_eq!(
        serve_from(Some(&dir), "app.js", &request).status(),
        StatusCode::NOT_MODIFIED
    );
    std::fs::write(dir.join("app.js"), "second").unwrap();
    assert_ne!(tag(&dir), first);
    assert_eq!(
        serve_from(Some(&dir), "app.js", &request).status(),
        StatusCode::OK
    );
}
