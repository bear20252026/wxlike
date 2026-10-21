//! `cargo run -p wxlike-ffi --features bindgen --bin uniffi-bindgen -- generate …`
//!
//! 需要 `--features bindgen`：`uniffi` 的 `cli` 会把整个 bindgen（含 clap、
//! cargo-metadata）链进来，那是生成绑定时才要的东西，不该出现在手机的 .so 里。

fn main() {
    uniffi::uniffi_bindgen_main();
}
