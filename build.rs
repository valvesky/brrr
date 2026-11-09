//! Keep the native manifest out of non-Windows builds.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest = embed_manifest::new_manifest("Omapower.Omapower")
        .dpi_awareness(embed_manifest::manifest::DpiAwareness::PerMonitorV2Only);
    // new_manifest includes the Common Controls v6 dependency.
    embed_manifest::embed_manifest(manifest).expect("embed Windows application manifest");
    println!("cargo:rerun-if-changed=build.rs");
}
