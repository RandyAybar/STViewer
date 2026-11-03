fn main() {
    // The Windows exe embeds icons/icon.ico at build time; re-run when the icons change.
    println!("cargo:rerun-if-changed=icons");
    tauri_build::build()
}
