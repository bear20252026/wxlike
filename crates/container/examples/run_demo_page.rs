//! End-to-end demo: pack a page directory -> load in container -> run lifecycle.

use std::path::Path;
use wxlike_container::Container;
use wxlike_dpage::pack_dir;

fn main() -> anyhow::Result<()> {
    let tmp = std::env::temp_dir().join("wxlike-demo-page");
    let page_dir = tmp.join("hello_page");
    let _ = std::fs::remove_dir_all(&page_dir);
    std::fs::create_dir_all(&page_dir)?;
    std::fs::write(
        page_dir.join("manifest.json"),
        r#"{"name":"hello","main_js":"main.js","title":"Hello Page"}"#,
    )?;
    std::fs::write(
        page_dir.join("main.js"),
        r#"
        wxhost.log('page loaded at', wxhost.nowMs());
        var n = wxhost.storeGet('visits') || '0';
        n = String(Number(n) + 1);
        wxhost.storeSet('visits', n);
        wxhost.log('visits =', n);
        wxhost.setTitle('Hello #' + n);
        globalThis.onLoad = function (payload) { wxhost.log('onLoad payload =', payload); };
        globalThis.onShow = function () { wxhost.log('onShow'); };
        "#,
    )?;
    std::fs::write(page_dir.join("style.css"), b"body { color: teal; }")?;

    println!("1) packing {}", page_dir.display());
    let blob = pack_dir(Path::new(&page_dir))?;
    let out = tmp.join("hello.dpage");
    std::fs::write(&out, &blob)?;
    println!("   -> {} ({} bytes, stub + zstd payload)", out.display(), blob.len());

    println!("2) unpacking into container");
    let page = wxlike_dpage::unpack(&blob)?;
    println!("   page '{}' id={:08x}, {} files", page.manifest.name, page.id, page.files.len());

    println!("3) starting container lifecycle");
    let mut container = Container::load(page)?;
    let title = container.start(r#"{"from":"rust-host"}"#)?;
    println!("   title set by page: {title}");
    for line in container.logs() {
        println!("   [page] {line}");
    }
    Ok(())
}
