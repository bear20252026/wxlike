//! dpage CLI: pack / unpack / inspect dynamic page packages.
//!
//! Usage:
//! ```text
//! dpage pack <dir> <out.dpage>
//! dpage unpack <in.dpage> <dir>
//! dpage info <in.dpage>
//! ```

use std::path::Path;
use wxlike_dpage::{pack_dir, peek, safe_rel_path, unpack};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("{}", USAGE);
        std::process::exit(2);
    }
    if let Err(err) = run(&args) {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}

const USAGE: &str = "usage: dpage <pack|unpack|info> ...";

fn run(args: &[String]) -> anyhow::Result<()> {
    match args[1].as_str() {
        "pack" if args.len() == 4 => pack_command(&args[2], &args[3]),
        "unpack" if args.len() == 4 => unpack_command(&args[2], &args[3]),
        "info" if args.len() == 3 => info_command(&args[2]),
        _ => usage_exit(),
    }
}

fn usage_exit() -> anyhow::Result<()> {
    eprintln!("{USAGE}");
    std::process::exit(2);
}

fn pack_command(src: &str, out: &str) -> anyhow::Result<()> {
    let blob = pack_dir(Path::new(src))?;
    std::fs::write(out, &blob)?;
    println!("packed {src} -> {out} ({} bytes)", blob.len());
    Ok(())
}

fn unpack_command(input: &str, out_dir: &str) -> anyhow::Result<()> {
    let blob = std::fs::read(input)?;
    let page = unpack(&blob)?;
    let dir = Path::new(out_dir);
    std::fs::create_dir_all(dir)?;
    for (path, data) in &page.files {
        write_page_file(dir, path, data)?;
    }
    println!(
        "unpacked page '{}' (id {:08x}), {} files -> {out_dir}",
        page.manifest.name,
        page.id,
        page.files.len()
    );
    Ok(())
}

/// 写单个页面文件。`unpack` 已校验路径，这里再挡一次，防止将来有人绕过
/// `unpack` 直接喂 `files`。
fn write_page_file(dir: &Path, path: &str, data: &[u8]) -> anyhow::Result<()> {
    anyhow::ensure!(safe_rel_path(path), "unsafe path: {path:?}");
    let out = dir.join(path);
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(out, data)?;
    Ok(())
}

fn info_command(input: &str) -> anyhow::Result<()> {
    let blob = std::fs::read(input)?;
    let (version, id) = peek(&blob)?;
    let page = unpack(&blob)?;
    println!("version: {version}");
    println!("id:      {id:08x}");
    println!("name:    {}", page.manifest.name);
    println!("title:   {}", page.manifest.title);
    println!("entry:   {}", page.manifest.main_js);
    println!("files:   {}", page.files.len());
    for name in page.files.keys() {
        println!("  - {name}");
    }
    Ok(())
}
