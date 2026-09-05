use embed_manifest::{embed_manifest, manifest::DpiAwareness, new_manifest};

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let manifest = new_manifest("BareEye").dpi_awareness(DpiAwareness::PerMonitorV2);

        embed_manifest(manifest).expect("failed to embed BareEye Windows manifest");
    }

    println!("cargo:rerun-if-changed=build.rs");
}
