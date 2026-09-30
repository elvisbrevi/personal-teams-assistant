use std::{env, fs, path::Path};

fn copy_tree(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let destination = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &destination);
        } else {
            fs::copy(entry.path(), destination).unwrap();
        }
    }
}
fn main() {
    // Tauri emits editor schemas relative to cwd. Keep generated files in OUT_DIR
    // so Cargo's package verification sees an immutable source tree.
    let source = env::current_dir().unwrap();
    let staging = std::path::PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("tauri-source");
    fs::create_dir_all(&staging).unwrap();
    for name in ["Cargo.toml", "tauri.conf.json", "Info.plist"] {
        println!("cargo:rerun-if-changed={}", source.join(name).display());
        fs::copy(source.join(name), staging.join(name)).unwrap();
    }
    for name in ["capabilities", "icons", "ui"] {
        println!("cargo:rerun-if-changed={}", source.join(name).display());
        copy_tree(&source.join(name), &staging.join(name));
    }
    env::set_current_dir(staging).unwrap();
    tauri_build::build();
}
