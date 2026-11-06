//! An archive installation's update with the real executables of this build, on every operating
//! system: a synthetic release (a local HTTP double, never GitHub) carries this build's own
//! `personal-teams-assistant` and `pta` in the archive format of this computer's target (`.zip` on
//! Windows, `.tgz` elsewhere), and the updater downloads, checks, unpacks and runs them to read
//! their version, exactly as it would a published release. Nothing is installed.
use personal_teams_assistant::maintenance::{
    self, CLI, CRATE, Version,
    release::{self, Source},
};
use sha2::{Digest as _, Sha256};
use std::{io::Write as _, path::Path};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

const TARGET: &str = if cfg!(windows) {
    "x86_64-pc-windows-msvc"
} else if cfg!(target_os = "macos") {
    "aarch64-apple-darwin"
} else {
    "x86_64-unknown-linux-gnu"
};

fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// The release archive of this build, laid out as `binaries.yml` lays it out.
fn archive(version: &Version) -> Vec<u8> {
    let dir = release::directory_name(TARGET, version);
    let files = [
        (exe(CRATE), env!("CARGO_BIN_EXE_personal-teams-assistant")),
        (exe(CLI), env!("CARGO_BIN_EXE_pta")),
    ];
    if cfg!(windows) {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.add_directory(format!("{dir}/"), options).unwrap();
        for (name, from) in files {
            zip.start_file(format!("{dir}/{name}"), options).unwrap();
            zip.write_all(&std::fs::read(from).unwrap()).unwrap();
        }
        zip.start_file(format!("{dir}/LICENSE"), options).unwrap();
        zip.write_all(b"MIT").unwrap();
        zip.finish().unwrap().into_inner()
    } else {
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (name, from) in files {
            tar.append_path_with_name(from, format!("{dir}/{name}"))
                .unwrap();
        }
        let mut header = tar::Header::new_gnu();
        header.set_size(3);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, format!("{dir}/LICENSE"), &b"MIT"[..])
            .unwrap();
        tar.into_inner().unwrap().finish().unwrap()
    }
}

async fn serve(version: &Version, bytes: Vec<u8>, digest: String) -> MockServer {
    let server = MockServer::start().await;
    let name = release::archive_name(TARGET, version);
    let base = format!("/owner/repo/releases/download/v{version}");
    Mock::given(method("GET"))
        .and(path(format!("{base}/{name}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{base}/{name}.sha256")))
        .respond_with(ResponseTemplate::new(200).set_body_string(digest))
        .mount(&server)
        .await;
    server
}

fn source(server: &MockServer) -> Source {
    Source {
        base: format!("{}/owner/repo", server.uri()),
        hosts: vec!["127.0.0.1".into()],
        https_only: false,
    }
}

fn environment() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    maintenance::build_environment(
        std::env::vars_os().filter(|(name, _)| name == "PATH" || name == "SYSTEMROOT"),
    )
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|e| {
            e.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[tokio::test]
async fn this_builds_own_release_archive_stages_as_a_verified_pair() {
    let version = Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
    let bytes = archive(&version);
    let name = release::archive_name(TARGET, &version);
    let digest = format!("{:x}  {name}\n", Sha256::digest(&bytes));
    let server = serve(&version, bytes.clone(), digest).await;
    let dir = tempfile::tempdir().unwrap();
    let partial = dir.path().join(format!("{version}.partial"));
    let manifest = release::stage(
        &source(&server),
        &version,
        TARGET,
        &partial,
        &environment(),
        7,
        |_, _| {},
    )
    .await
    .unwrap();
    assert_eq!(manifest.version, version.to_string());
    assert_eq!(manifest.target.as_deref(), Some(TARGET));
    let archive = manifest.archive.unwrap();
    assert_eq!(archive.name, name);
    assert_eq!(archive.size, bytes.len() as u64);
    assert_eq!(archive.sha256, format!("{:x}", Sha256::digest(&bytes)));
    let mut wanted = vec![exe(CRATE), exe(CLI)];
    wanted.sort();
    assert_eq!(entries(&partial.join("root/bin")), wanted);
    assert_eq!(entries(&partial), ["root"], "the download was removed");
    // The unpacked executables are byte for byte the ones of this build.
    assert_eq!(
        std::fs::read(partial.join("root/bin").join(exe(CLI))).unwrap(),
        std::fs::read(env!("CARGO_BIN_EXE_pta")).unwrap()
    );
}

#[tokio::test]
async fn a_pair_that_reports_another_version_or_a_wrong_digest_is_not_staged() {
    // This build's archive offered as a newer release: the pair reports this build's version.
    let version = Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
    let newer = Version(version.0, version.1, version.2 + 1);
    let bytes = archive(&newer);
    let name = release::archive_name(TARGET, &newer);
    let server = serve(
        &newer,
        bytes.clone(),
        format!("{:x}  {name}\n", Sha256::digest(&bytes)),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let error = release::stage(
        &source(&server),
        &newer,
        TARGET,
        &dir.path().join("a"),
        &environment(),
        7,
        |_, _| {},
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("reports a different version"),
        "{error}"
    );
    // A digest of other bytes stops before anything is unpacked.
    let server = serve(
        &newer,
        bytes,
        format!("{:x}  {name}\n", Sha256::digest(b"other")),
    )
    .await;
    let partial = dir.path().join("b");
    let error = release::stage(
        &source(&server),
        &newer,
        TARGET,
        &partial,
        &environment(),
        7,
        |_, _| {},
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("does not match"), "{error}");
    assert!(entries(&partial.join("root/bin")).is_empty());
}
