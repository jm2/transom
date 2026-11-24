// SPDX-License-Identifier: Apache-2.0
//! Generate FreeRDP 3 bindings with bindgen against the system library (ADR 0001).

use std::env;
use std::path::PathBuf;

const MIN_FREERDP: &str = "3.15";

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
                     Debian 13 / Ubuntu 25.10: sudo apt install freerdp3-dev libwinpr3-dev\n\
                     Arch: sudo pacman -S freerdp\n\
                     macOS: brew install freerdp\n\
                     Windows (MSYS2): pacman -S mingw-w64-clang-x86_64-freerdp \
                     (mingw-w64-clang-aarch64-freerdp on arm64)\n\
                     The scripts/build-* helpers check every dependency before building."
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
        .allowlist_function("h264_context_new")
        .allowlist_function("h264_context_free")
        .allowlist_function("avc420_decompress")
        .allowlist_function("region16_.*")
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
        .allowlist_type("CliprdrClientContext")
        .allowlist_type("CLIPRDR_.*")
        .allowlist_type("RdpgfxClientContext")
        .allowlist_type("gdiGfxSurface")
        .allowlist_type("gdiGfxCacheEntry")
        .allowlist_type("RDPGFX_.*")
        .allowlist_type("DISPLAY_CONTROL_MONITOR_LAYOUT")
        .allowlist_type("rdpMonitor")
        .allowlist_type("ChannelConnectedEventArgs")
        .allowlist_type("ChannelDisconnectedEventArgs")
        .allowlist_type("FreeRDP_Settings_Keys_.*")
        .allowlist_type("rdp_auth_reason")
        .allowlist_var("FreeRDP_.*")
        .allowlist_var("VERIFY_CERT_FLAG_.*")
        .allowlist_var("FREERDP_VERSION_.*")
        .allowlist_var("FREERDP_ADDIN_.*")
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
        .allowlist_var("CLIPRDR_SVC_CHANNEL_NAME")
        .allowlist_var("CB_.*")
        .allowlist_var("RDPGFX_.*")
        .allowlist_var("CHANNEL_RC_.*")
        .allowlist_var("AUTH_.*")
        .allowlist_var("GW_AUTH_.*")
        .allowlist_var("FREERDP_ERROR_.*")
        .allowlist_var("ERRINFO_.*");

    for p in &include_paths {
        builder = builder.clang_arg(format!("-I{}", p.display()));
    }
    builder = builder.clang_args(target_clang_args());

    let bindings = builder
        .generate()
        .expect("bindgen failed to generate FreeRDP bindings");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("freerdp_bindings.rs");
    bindings.write_to_file(&out).expect("writing bindings");

    hwdec_bindings();
}

/// Zero-copy H.264 (ADR 0006): libavcodec + VA-API, Linux only. Optional at build
/// time: without the FFmpeg development files (or with TRANSOM_NO_HWDEC=1) the
/// `transom_hwdec` cfg stays off and H.264 is decoded by FreeRDP in software.
fn hwdec_bindings() {
    println!("cargo::rustc-check-cfg=cfg(transom_hwdec)");
    println!("cargo:rerun-if-changed=src/av_wrapper.h");
    println!("cargo:rerun-if-env-changed=TRANSOM_NO_HWDEC");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux")
        || env::var_os("TRANSOM_NO_HWDEC").is_some()
    {
        return;
    }
    let mut include_paths = Vec::new();
    for lib in ["libavcodec", "libavutil"] {
        match pkg_config::Config::new().atleast_version("59").probe(lib) {
            Ok(found) => include_paths.extend(found.include_paths),
            Err(e) => {
                println!(
                    "cargo:warning=hardware H.264 decoding disabled ({lib} not found: {e}); \
                     Fedora: sudo dnf install libavcodec-free-devel"
                );
                return;
            }
        }
    }
    let mut builder = bindgen::Builder::default()
        .header("src/av_wrapper.h")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .default_enum_style(bindgen::EnumVariation::Consts)
        .prepend_enum_name(false)
        .derive_default(true)
        .layout_tests(false)
        .generate_comments(false)
        .allowlist_function("avcodec_.*")
        .allowlist_function("av_packet_.*")
        .allowlist_function("av_frame_.*")
        .allowlist_function("av_buffer_ref")
        .allowlist_function("av_buffer_unref")
        .allowlist_function("av_hwdevice_ctx_create")
        .allowlist_function("av_hwframe_map")
        .allowlist_function("av_hwframe_transfer_data")
        .allowlist_function("av_log_set_level")
        .allowlist_type("AVCodecContext")
        .allowlist_type("AVDRMFrameDescriptor")
        .allowlist_type("AVPixelFormat")
        .allowlist_type("AVHWDeviceType")
        .allowlist_var("AV_PIX_FMT_.*")
        .allowlist_var("AV_HWDEVICE_TYPE_.*")
        .allowlist_var("AV_HWFRAME_MAP_.*")
        .allowlist_var("AV_CODEC_HW_CONFIG_METHOD_.*")
        .allowlist_var("AV_CODEC_FLAG_.*")
        .allowlist_var("AV_INPUT_BUFFER_PADDING_SIZE")
        .allowlist_var("AV_LOG_.*")
        .allowlist_var("LIBAVCODEC_VERSION_MAJOR");
    for p in &include_paths {
        builder = builder.clang_arg(format!("-I{}", p.display()));
    }
    builder = builder.clang_args(target_clang_args());
    let bindings = builder
        .generate()
        .expect("bindgen failed to generate libavcodec bindings");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("av_bindings.rs");
    bindings.write_to_file(&out).expect("writing bindings");
    println!("cargo:rustc-cfg=transom_hwdec");
}

/// Clang arguments for the target. Rust's `*-pc-windows-gnullvm` triples have an
/// environment clang doesn't know, so libclang wouldn't use the MinGW toolchain
/// (and its sysroot include directory, e.g. MSYS2's `clang64/include`): give it the
/// equivalent MinGW triple.
fn target_clang_args() -> Vec<String> {
    let target = env::var("TARGET").unwrap_or_default();
    match target.as_str() {
        t if t.ends_with("-pc-windows-gnullvm") || t.ends_with("-pc-windows-gnu") => {
            let arch = t.split('-').next().unwrap_or("x86_64");
            // mingw-w64 has no C11 <threads.h>, which WinPR includes unless the
            // compiler says threads are unavailable (it then uses its _WIN32 path).
            vec![
                format!("--target={arch}-w64-windows-gnu"),
                "-D__STDC_NO_THREADS__=1".into(),
            ]
        }
        _ => Vec::new(),
    }
}
