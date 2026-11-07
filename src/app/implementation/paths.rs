//! How execution machines name directories: the filesystem roots the directory sheet starts from and
//! the spelling of paths shown to people and stored as allowed roots.
//!
//! On Windows the canonical form of a path carries the verbatim prefix (`\\?\C:\…`,
//! `\\?\UNC\server\share\…`). Checks keep canonicalizing, so they accept either spelling; what is
//! shown or stored is the plain form (`C:\…`), and roots saved with the prefix read back plain.
use std::path::{Path, PathBuf};

/// The plain spelling of a path: a Windows verbatim disk or UNC prefix removed, anything else unchanged.
/// Text-based so a coordinator on any operating system can normalize a Windows machine's roots.
pub(super) fn plain(path: &Path) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path.to_path_buf();
    };
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        let bytes = rest.as_bytes();
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            // `\\?\C:` alone would read as a drive-relative path; keep the root separator.
            return PathBuf::from(if rest.len() == 2 {
                format!(r"{rest}\")
            } else {
                rest.to_string()
            });
        }
        if let Some(unc) = rest.strip_prefix(r"UNC\").filter(|unc| !unc.is_empty()) {
            return PathBuf::from(format!(r"\\{unc}"));
        }
    }
    path.to_path_buf()
}

pub(super) fn plain_all(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut result: Vec<PathBuf> = Vec::new();
    for path in paths.iter().map(|p| plain(p)) {
        if !result.contains(&path) {
            result.push(path);
        }
    }
    result
}

/// `GetDriveTypeW` result for a fixed local disk.
#[cfg_attr(not(windows), allow(dead_code))]
const DRIVE_FIXED: u32 = 3;

/// Root directories of the fixed drives in a `GetLogicalDrives` mask (bit 0 is `A:`), in letter
/// order; removable, network, CD and RAM drives are left out.
#[cfg_attr(not(windows), allow(dead_code))]
fn fixed_drives(mask: u32, kind: impl Fn(&str) -> u32) -> Vec<PathBuf> {
    (0..26u8)
        .filter(|bit| mask & (1 << bit) != 0)
        .map(|bit| format!(r"{}:\", char::from(b'A' + bit)))
        .filter(|root| kind(root) == DRIVE_FIXED)
        .map(PathBuf::from)
        .collect()
}

#[cfg(windows)]
fn local_fixed_drives() -> Vec<PathBuf> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLogicalDrives() -> u32;
        fn GetDriveTypeW(root: *const u16) -> u32;
    }
    let mask = unsafe { GetLogicalDrives() };
    fixed_drives(mask, |root| {
        let wide: Vec<u16> = std::ffi::OsStr::new(root)
            .encode_wide()
            .chain(Some(0))
            .collect();
        unsafe { GetDriveTypeW(wide.as_ptr()) }
    })
}

/// Where the machine-wide directory sheet starts: every fixed local drive on Windows (the home
/// drive when none can be read), the single root `/` elsewhere.
pub(super) fn filesystem_roots(home: &Path) -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        let drives = local_fixed_drives();
        if !drives.is_empty() {
            return drives;
        }
    }
    home.ancestors()
        .last()
        .map(Path::to_path_buf)
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_prefixes_are_removed_and_other_paths_are_unchanged() {
        for (input, expected) in [
            (r"\\?\C:\Users\Synthetic\code", r"C:\Users\Synthetic\code"),
            (r"\\?\D:\", r"D:\"),
            (r"\\?\D:", r"D:\"),
            (r"\\?\UNC\server\share\repo", r"\\server\share\repo"),
            (r"C:\Users\Synthetic", r"C:\Users\Synthetic"),
            (r"\\server\share", r"\\server\share"),
            (r"\\?\Volume{0000}\dir", r"\\?\Volume{0000}\dir"),
            (r"\\?\UNC\", r"\\?\UNC\"),
            ("/home/synthetic/code", "/home/synthetic/code"),
        ] {
            assert_eq!(plain(Path::new(input)), PathBuf::from(expected), "{input}");
        }
        assert_eq!(
            plain_all(&[
                PathBuf::from(r"\\?\C:\work"),
                PathBuf::from(r"C:\work"),
                PathBuf::from(r"\\?\D:\code\sag"),
            ]),
            vec![PathBuf::from(r"C:\work"), PathBuf::from(r"D:\code\sag")]
        );
    }

    #[test]
    fn roots_saved_with_the_verbatim_prefix_read_back_plain() {
        let profile = tempfile::tempdir().unwrap();
        let state = super::super::State::new(profile.path()).unwrap();
        let file = profile.path().join("implementation").join("registry.json");
        let mut saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        saved["settings"]["roots"] = serde_json::json!([r"\\?\D:\code\sag", r"D:\code\sag"]);
        std::fs::write(&file, saved.to_string()).unwrap();
        drop(state);
        let state = super::super::State::new(profile.path()).unwrap();
        assert_eq!(
            state.registry.try_lock().unwrap().settings.roots,
            vec![PathBuf::from(r"D:\code\sag")]
        );
        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(
            stored["settings"]["roots"],
            serde_json::json!([r"D:\code\sag"])
        );
    }

    #[test]
    fn only_fixed_drives_are_offered_in_letter_order() {
        // A: removable (2), C: fixed, D: fixed, E: CD (5), Z: network (4), Q: absent from the mask.
        let kinds = |root: &str| match root {
            r"A:\" => 2,
            r"C:\" | r"D:\" | r"Q:\" => DRIVE_FIXED,
            r"E:\" => 5,
            r"Z:\" => 4,
            _ => 0,
        };
        let mask = 1 | 1 << 2 | 1 << 3 | 1 << 4 | 1 << 25;
        assert_eq!(
            fixed_drives(mask, kinds),
            vec![PathBuf::from(r"C:\"), PathBuf::from(r"D:\")]
        );
        assert!(fixed_drives(0, kinds).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn unix_keeps_the_single_root() {
        assert_eq!(
            filesystem_roots(Path::new("/home/synthetic")),
            vec![PathBuf::from("/")]
        );
    }

    /// A directory junction, the link Windows users make without elevation.
    #[cfg(windows)]
    fn junction(alias: &Path, target: &Path) {
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(alias)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_browse_and_roots_use_every_fixed_drive_in_plain_spelling() {
        use serde_json::json;
        let root = tempfile::tempdir().unwrap();
        let host = super::super::tests::host(root.path());
        let own = host.state.implementation.registry.lock().await.id.clone();
        let call = |method: &'static str, args: serde_json::Value| {
            let host = host.clone();
            async move { super::super::operation(&host, method, &args).await }
        };
        let listing = call("machine_browse", json!({})).await.unwrap();
        assert_eq!(listing["roots"], true);
        let drives: Vec<PathBuf> = serde_json::from_value(listing["directories"].clone()).unwrap();
        assert_eq!(drives, filesystem_roots(Path::new(r"C:\Users\Synthetic")));

        let work = root.path().join("work");
        std::fs::create_dir_all(work.join("child")).unwrap();
        let verbatim = work.canonicalize().unwrap();
        assert!(verbatim.to_str().unwrap().starts_with(r"\\?\"));
        let shown = plain(&verbatim);
        for path in [&verbatim, &shown] {
            let listing = call("machine_browse", json!({"path":path})).await.unwrap();
            let text = listing.to_string();
            assert!(!text.contains(r"\\\\?\\"), "{text}");
            assert_eq!(listing["path"], json!(shown));
            assert_eq!(listing["directories"], json!([shown.join("child")]));
        }

        // Either spelling saves; the plain one is stored, shown and confines implementation browsing.
        let saved = call(
            "machine_update",
            json!({"machine":own,"roots":[verbatim, shown]}),
        )
        .await
        .unwrap();
        assert_eq!(saved["saved"], true);
        let detail = call("machine_detail", json!({"machine":own}))
            .await
            .unwrap();
        assert_eq!(detail["roots"], json!([shown]));
        let stored = super::super::roots(&host).await.unwrap();
        assert_eq!(stored, vec![shown.clone()]);
        let inside =
            super::super::browse_directories(Some(verbatim.to_str().unwrap()), &stored).unwrap();
        assert_eq!(inside["path"], json!(shown));
        assert!(super::super::browse_directories(Some(r"C:\"), &stored).is_err());
        assert!(
            super::super::directories(&host, std::slice::from_ref(&shown))
                .await
                .is_ok()
        );

        // Junction aliases stay refused, also when they point to another fixed drive.
        let alias = root.path().join("alias");
        junction(&alias, &work);
        assert!(
            call("machine_update", json!({"machine":own,"roots":[alias]}))
                .await
                .is_err()
        );
        let home = plain(&std::env::temp_dir().canonicalize().unwrap());
        let home = home.ancestors().last().unwrap().to_path_buf();
        if let Some(other) = drives.iter().find(|d| **d != home)
            && let Ok(target) = tempfile::tempdir_in(other)
        {
            let across = root.path().join("across");
            junction(&across, target.path());
            assert!(
                call("machine_update", json!({"machine":own,"roots":[across]}))
                    .await
                    .is_err()
            );
            let direct = call(
                "machine_update",
                json!({"machine":own,"roots":[target.path()]}),
            )
            .await
            .unwrap();
            assert_eq!(direct["saved"], true);
            let detail = call("machine_detail", json!({"machine":own}))
                .await
                .unwrap();
            assert_eq!(
                detail["roots"],
                json!([plain(&target.path().canonicalize().unwrap())])
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_offers_the_fixed_drive_that_holds_the_temporary_directory() {
        let temp = std::env::temp_dir().canonicalize().unwrap();
        let drive = plain(&temp).ancestors().last().unwrap().to_path_buf();
        let roots = filesystem_roots(Path::new(r"C:\Users\Synthetic"));
        assert!(roots.contains(&drive), "{roots:?} lacks {drive:?}");
        assert!(roots.iter().all(|r| r.to_str().unwrap().len() == 3));
    }
}
