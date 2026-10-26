fn main() {
    tauri_build::build();

    // 集成测试（core_bridge_e2e 等）链进 tauri::test 的 mock runtime，
    // 间接引入 comctl32/user32 等 GUI 导入。测试 exe 没有 tauri-build 给
    // bin 嵌的资源 manifest，Windows 会把它解析到 WinSxS 的 Common-Controls
    // 5.82（旧版缺入口点），进程起不来（0xc0000139）。给测试目标补嵌
    // v6 依赖声明；bin 不动（tauri-build 已嵌，重复会 CVT1100）。
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-arg-tests=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-tests=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' publicKeyToken='6595b64144ccf1df' language='*' processorArchitecture='*'"
        );
    }
}
