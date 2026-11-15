fn main() {
    println!("cargo:rerun-if-changed=src/volumes/mounts.m");
    cc::Build::new()
        .file("src/volumes/mounts.m")
        .flag("-fobjc-arc")
        .compile("cardinal_mounts");
    println!("cargo:rustc-link-lib=framework=DiskArbitration");
    tauri_build::build()
}
