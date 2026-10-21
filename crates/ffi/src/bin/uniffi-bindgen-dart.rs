//! Dart 后端：`cargo run -p wxlike-ffi --features bindgen,dart --bin uniffi-bindgen-dart -- …`
//!
//! 为什么不直接用 `uniffi` 自带的：官方后端只有 Kotlin / Swift / Python / Ruby，
//! 而 ADR-11 选的 UI 是 Flutter。参数与官方 bindgen 一致（`generate <cdylib> --out-dir`）。

fn main() {
    if let Err(err) = uniffi_bindgen_dart::run(std::env::args_os()) {
        eprintln!("{err:#}");
        std::process::exit(1);
    }
}
