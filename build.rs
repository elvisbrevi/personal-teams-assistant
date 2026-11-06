fn main() {
    // The Windows host can exhaust MSVC's default 1 MiB main-thread stack at startup.
    // Reserve 8 MiB (commit stays at its default); keep the CLI and other targets unchanged.
    // Cargo's target variables also cover cross-compilation; cfg! here describes the build host.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        println!("cargo:rustc-link-arg-bin=personal-teams-assistant=/STACK:8388608");
    }
}
