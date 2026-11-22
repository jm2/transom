// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows: the icon and version resource on transom.exe (as Tributary and Balun).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(target_os = "windows")]
    {
        let manifest = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
        let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
        let icon = manifest.join("../../data/transom.ico");
        println!("cargo:rerun-if-changed={}", icon.display());
        let rc = out.join("transom-resource.rc");
        let mut res = winresource::WindowsResource::new();
        res.set_icon(icon.to_str().expect("icon path is not UTF-8"));
        res.set("ProductName", "Transom");
        res.set("FileDescription", "Transom remote desktop client");
        res.set("LegalCopyright", "Copyright © 2026 Transom contributors");
        res.write_resource_file(&rc)
            .expect("writing the Windows resource");
        embed_resource::compile_for(&rc, ["transom"], embed_resource::NONE)
            .manifest_required()
            .expect("compiling the Windows resource for transom.exe");
    }
}
