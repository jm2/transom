// SPDX-License-Identifier: GPL-3.0-or-later
//! Generate FreeRDP 3 bindings with bindgen against the system library (ADR 0001).

use std::env;
use std::path::PathBuf;

const MIN_FREERDP: &str = "3.10";

fn main() {
    println!("cargo:rerun-if-changed=src/wrapper.h");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");

    let mut include_paths = Vec::new();
    for lib in ["freerdp-client3", "freerdp3", "winpr3"] {
        let found = pkg_config::Config::new()
            .atleast_version(MIN_FREERDP)
            .probe(lib)
            .unwrap_or_else(|e| {
                panic!(
                    "FreeRDP >= {MIN_FREERDP} development files not found ({lib}): {e}\n\
                     Fedora: sudo dnf install freerdp-devel libwinpr-devel\n\
                     Debian/Ubuntu: sudo apt install freerdp3-dev libwinpr3-dev\n\
                     macOS: brew install freerdp\n\
                     Windows: vcpkg install freerdp"
                )
            });
        include_paths.extend(found.include_paths);
    }

    let mut builder = bindgen::Builder::default()
        .header("src/wrapper.h")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .default_enum_style(bindgen::EnumVariation::Consts)
        .prepend_enum_name(false)
        .derive_default(true)
        .layout_tests(false)
        .generate_comments(false)
        .allowlist_function("freerdp_.*")
        .allowlist_function("gdi_.*")
        .allowlist_function("graphics_register_pointer")
        .allowlist_function("PubSub_Subscribe.*")
        .allowlist_function("PubSub_Unsubscribe.*")
        .allowlist_function("CreateEventA")
        .allowlist_function("SetEvent")
        .allowlist_function("ResetEvent")
        .allowlist_function("CloseHandle")
        .allowlist_function("WaitForMultipleObjects")
        .allowlist_type("rdpClientContext")
        .allowlist_type("RDP_CLIENT_ENTRY_POINTS")
        .allowlist_type("rdpGdi")
        .allowlist_type("rdpPointer")
        .allowlist_type("DispClientContext")
        .allowlist_type("RdpgfxClientContext")
        .allowlist_type("DISPLAY_CONTROL_MONITOR_LAYOUT")
        .allowlist_type("ChannelConnectedEventArgs")
        .allowlist_type("ChannelDisconnectedEventArgs")
        .allowlist_type("FreeRDP_Settings_Keys_.*")
        .allowlist_type("rdp_auth_reason")
        .allowlist_var("FreeRDP_.*")
        .allowlist_var("VERIFY_CERT_FLAG_.*")
        .allowlist_var("FREERDP_VERSION_.*")
        .allowlist_var("DISPLAY_CONTROL_.*")
        .allowlist_var("ORIENTATION_.*")
        .allowlist_var("PTR_FLAGS_.*")
        .allowlist_var("PTR_XFLAGS_.*")
        .allowlist_var("KBD_FLAGS_.*")
        .allowlist_var("WAIT_.*")
        .allowlist_var("INFINITE")
        .allowlist_var("MAXIMUM_WAIT_OBJECTS")
        .allowlist_var("OSMAJORTYPE_.*")
        .allowlist_var("OSMINORTYPE_.*")
        .allowlist_var("DISP_DVC_CHANNEL_NAME")
        .allowlist_var("RDPGFX_DVC_CHANNEL_NAME")
        .allowlist_var("AUTH_.*")
        .allowlist_var("GW_AUTH_.*")
        .allowlist_var("FREERDP_ERROR_.*")
        .allowlist_var("ERRINFO_.*");

    for p in &include_paths {
        builder = builder.clang_arg(format!("-I{}", p.display()));
    }

    let bindings = builder
        .generate()
        .expect("bindgen failed to generate FreeRDP bindings");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("freerdp_bindings.rs");
    bindings.write_to_file(&out).expect("writing bindings");
}
