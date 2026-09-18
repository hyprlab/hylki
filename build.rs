//! Compile the bundled icon set (resources/hylki.gresource.xml) into a
//! `.gresource` blob that is embedded in the binary and registered at startup
//! (see `main.rs`). This lets every symbolic icon Hylki draws render identically
//! on any distribution, regardless of the host icon theme — the icons are
//! prefixed with the app ID so no system theme can override them.

fn main() {
    println!("cargo:rerun-if-changed=resources/hylki.gresource.xml");
    println!("cargo:rerun-if-changed=resources/icons");
    glib_build_tools::compile_resources(
        &["resources"],
        "resources/hylki.gresource.xml",
        "hylki.gresource",
    );
    // The bundled sender logos (data/logos/, listed by tools/fetch-logos.py).
    println!("cargo:rerun-if-changed=resources/logos.gresource.xml");
    println!("cargo:rerun-if-changed=data/logos");
    glib_build_tools::compile_resources(
        &["resources"],
        "resources/logos.gresource.xml",
        "logos.gresource",
    );
}
