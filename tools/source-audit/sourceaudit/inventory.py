"""产品依赖锁的共享扫描面；独立安全测试工程不随产品分发。"""

CARGO_LOCKS = (
    ("Cargo.lock", "rust-root"),
    ("ui/src-tauri/Cargo.lock", "desktop"),
)
NPM_LOCKS = (
    ("ui/package-lock.json", "web"),
    ("apps/desktop-web/package-lock.json", "desktop-web"),
    ("design/ui-v2/prototype/package-lock.json", "design"),
    ("design/ui-v2/package-lock.json", "design-legacy"),
)

SURFACE_ECOSYSTEM = {
    **{surface: "crates.io" for _, surface in CARGO_LOCKS},
    **{surface: "npm" for _, surface in NPM_LOCKS},
    "mobile": "Pub",
}
