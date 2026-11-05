fn main() {
    // The Core links at its own address. Absolute path, so it resolves regardless of the directory the linker runs in.
    println!("cargo:rustc-link-arg=-T{}/linker-arm64.ld", env!("CARGO_MANIFEST_DIR"));
    println!("cargo:rustc-link-arg=-no-pie");
}
