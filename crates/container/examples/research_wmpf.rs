//! Local-reuse research loader: attempt to run a recovered WeChat runtime JS
//! (e.g. source_recovered/wmpf_js/WAWebview.js) inside the container.
//!
//! Honest expectations: the recovered runtimes expect a full browser/DOM
//! environment (window/document/XMLHttpRequest/WebSocket/...). This loader
//! installs a minimal shim, tries, and reports exactly how far it gets.
//! LOCAL EXPERIMENT ONLY — the recovered files are Tencent code; never ship them.

use wxlike_container::BoaEngine;

fn main() -> anyhow::Result<()> {
    // Boa's recursive parser needs a big stack for megabyte-scale minified bundles.
    let child = std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(run)
        .expect("spawn research thread");
    child.join().expect("research thread panicked")
}

fn run() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| r"D:\mimo\test\source_recovered\wmpf_js\WAWebview.js".to_string());
    let code = std::fs::read_to_string(&path)?;
    println!("[research] loading {} ({} KB)", path, code.len() / 1024);

    let mut eng = BoaEngine::new();
    println!("[research] installing minimal DOM shim");
    let shim = r#"
        var window = globalThis;
        var self = globalThis;
        var document = {
            createElement: function () { return { style: {}, children: [], appendChild: function (c) { this.children.push(c); } }; },
            createTextNode: function (t) { return { text: t }; },
            getElementById: function () { return null; },
            querySelector: function () { return null; },
            addEventListener: function () {},
            body: { appendChild: function () {}, style: {} }
        };
        var navigator = { userAgent: 'wxlike-research/0.1' };
        var location = { href: 'file://research/local', protocol: 'file:' };
        globalThis.addEventListener = function () {};
        globalThis.setTimeout = function (fn) { return 0; };
        globalThis.clearTimeout = function () {};
        globalThis.setInterval = function () { return 0; };
        globalThis.clearInterval = function () {};
        globalThis.XMLHttpRequest = function () { this.open = function(){}; this.send = function(){}; this.setRequestHeader = function(){}; };
        globalThis.WebSocket = function () { this.send = function(){}; this.close = function(){}; };
        globalThis.__wxConfig = { envVersion: 'research' };
    "#;
    match eng.eval(shim) {
        Ok(_) => println!("[research] shim ok"),
        Err(e) => return Err(e.context("shim failed (container bug)")),
    }

    println!("[research] eval runtime (this may take a while or fail — that is data, not a bug)");
    match eng.eval(&code) {
        Ok(summary) => {
            println!("[research] EVAL OK, result: {}", truncate(&summary, 120));
            println!(
                "[research] logs so far: {:?}",
                eng.logs().iter().take(5).collect::<Vec<_>>()
            );
            println!("[research] NOTE: eval succeeding does not mean the runtime works; exercise APIs to learn more.");
        }
        Err(e) => {
            println!("[research] EVAL FAILED: {e:#}");
            println!(
                "[research] this identifies the next missing shim — iterate: add global, retry."
            );
        }
    }
    Ok(())
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}
