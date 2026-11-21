// SPDX-License-Identifier: Apache-2.0
//! Give the shared library its versioned soname (libtransom-view-0.so.0).
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,libtransom-view-0.so.0");
    }
}
