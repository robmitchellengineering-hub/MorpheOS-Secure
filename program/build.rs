fn main() {
    // The loadable program links at 0x40200000 (the Core's free RAM), NOT the Core's own 0x40080000. The absolute
    // path is deliberate: the workspace root's rustflags also pass -Tcore/linker-arm64.ld, and only an absolute
    // -T here can be resolved from whatever directory the linker runs in.
    println!("cargo:rustc-link-arg=-T{}/linker.ld", env!("CARGO_MANIFEST_DIR"));
    println!("cargo:rustc-link-arg=-no-pie");
}
