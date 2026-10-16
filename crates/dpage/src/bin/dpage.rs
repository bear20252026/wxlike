//! dpage CLI: pack / unpack / inspect dynamic page packages.
//!
//! Usage:
//! ```text
//! dpage pack <dir> <out.dpage>
//! dpage unpack <in.dpage> <dir>
//! dpage info <in.dpage>
//! ```

use std::path::Path;
use wxlike_dpage::{pack_dir, peek, unpack};

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
        "pack" if args.len() == 4 => {
            let blob = pack_dir(Path::new(&args[2]))?;
            std::fs::write(&args[3], &blob)?;
            println!("packed {} -> {} ({} bytes)", args[2], args[3], blob.len());
        }
        "unpack" if args.len() == 4 => {
            let blob = std::fs::read(&args[2])?;
            let page = unpack(&blob)?;
            let dir = Path::new(&args[3]);
            std::fs::create_dir_all(dir)?;
            for (path, data) in &page.files {
                let out = dir.join(path);
                if let Some(parent) = out.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(out, data)?;
            }
            println!(
                "unpacked page '{}' (id {:08x}), {} files -> {}",
                page.manifest.name,
                page.id,
                page.files.len(),
                args[3]
            );
        }
        "info" if args.len() == 3 => {
            let blob = std::fs::read(&args[2])?;
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
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
    Ok(())
}
