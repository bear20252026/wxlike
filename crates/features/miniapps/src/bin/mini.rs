//! `mini` — pack, sign, inspect and dev-serve mini-app packages.
//!
//! Every command prints one JSON object on stdout; human errors go to stderr.
//! Signing keys are read from `--key` or `MINI_SIGNING_KEY` as a 32-byte hex
//! seed and are never echoed.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use wxlike_miniapps::packages::MiniError;
use wxlike_miniapps::toolchain::{self, DevServer, DEFAULT_HOST_VERSION};

const USAGE: &str = "\
mini pack <dir> [--manifest <file>] [--out <blob>] [--host <version>]
mini sign <blob> [--key <hex-seed>|--key-file <file>] [--key-id <id>]
                 [--out <blob>] [--sig <file>] [--host <version>]
mini inspect <blob> [--sig <file>] [--trust <trust.json>] [--public-key <hex>]
                    [--host <version>]
mini dev-serve <dir> [--manifest <file>] [--port <n>] [--once] [--host <version>]";
const EXIT_USAGE: u8 = 2;
const KEY_ENV: &str = "MINI_SIGNING_KEY";

enum CliError {
    Usage(String),
    Failed(String),
}

impl From<MiniError> for CliError {
    fn from(err: MiniError) -> Self {
        CliError::Failed(err.to_string())
    }
}

impl From<std::io::Error> for CliError {
    fn from(err: std::io::Error) -> Self {
        CliError::Failed(err.to_string())
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(CliError::Usage(message)) => {
            eprintln!("mini: {message}\n\n{USAGE}");
            ExitCode::from(EXIT_USAGE)
        }
        Err(CliError::Failed(message)) => {
            eprintln!("mini: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), CliError> {
    match args.first().map(String::as_str) {
        Some("pack") => cmd_pack(&args[1..]),
        Some("sign") => cmd_sign(&args[1..]),
        Some("inspect") => cmd_inspect(&args[1..]),
        Some("dev-serve") => cmd_dev_serve(&args[1..]),
        Some("-h" | "--help" | "help") | None => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(CliError::Usage(format!("unknown command {other:?}"))),
    }
}

fn cmd_pack(args: &[String]) -> Result<(), CliError> {
    let dir = positional(args, &["--manifest", "--out", "--host"])
        .into_iter()
        .next()
        .ok_or_else(|| usage("pack needs a source directory"))?;
    let manifest = path_flag(args, "--manifest");
    let out = path_flag(args, "--out");
    let report = toolchain::pack(
        Path::new(&dir),
        manifest.as_deref(),
        out.as_deref(),
        &host_version(args),
    )?;
    print_json(&report)
}

fn cmd_sign(args: &[String]) -> Result<(), CliError> {
    let blob = positional(
        args,
        &[
            "--key",
            "--key-file",
            "--key-id",
            "--out",
            "--sig",
            "--host",
        ],
    )
    .into_iter()
    .next()
    .ok_or_else(|| usage("sign needs a package blob"))?;
    let seed = read_seed(args)?;
    let key = toolchain::signing_key(&seed)?;
    let key_id = flag(args, "--key-id");
    let out = path_flag(args, "--out");
    let sig = path_flag(args, "--sig");
    let report = toolchain::sign(
        Path::new(&blob),
        &key,
        key_id.as_deref(),
        out.as_deref(),
        sig.as_deref(),
        &host_version(args),
    )?;
    print_json(&report)
}

fn cmd_inspect(args: &[String]) -> Result<(), CliError> {
    let blob = positional(args, &["--sig", "--trust", "--public-key", "--host"])
        .into_iter()
        .next()
        .ok_or_else(|| usage("inspect needs a package blob"))?;
    let sig = path_flag(args, "--sig");
    let trust = match path_flag(args, "--trust") {
        Some(path) => Some(toolchain::load_trust(&path)?),
        None => None,
    };
    let public_key = flag(args, "--public-key");
    let report = toolchain::inspect(
        Path::new(&blob),
        sig.as_deref(),
        trust.as_ref(),
        public_key.as_deref(),
        &host_version(args),
    )?;
    print_json(&report)
}

fn cmd_dev_serve(args: &[String]) -> Result<(), CliError> {
    let dir = positional(args, &["--manifest", "--port", "--host"])
        .into_iter()
        .next()
        .ok_or_else(|| usage("dev-serve needs a source directory"))?;
    let manifest = path_flag(args, "--manifest");
    let port = match flag(args, "--port") {
        Some(raw) => raw.parse().map_err(|_| usage("--port must be 0..65535"))?,
        None => 0,
    };
    let server = DevServer::bind(
        Path::new(&dir),
        manifest.as_deref(),
        &host_version(args),
        port,
    )?;
    let info = serde_json::json!({
        "url": server.url(),
        "entry": server.manifest().entry,
        "app_id": server.manifest().app_id,
        "version": server.manifest().version,
        "channel": server.manifest().channel.as_str(),
    });
    // One line: callers (CI, tests) read stdout line-by-line.
    println!(
        "{}",
        serde_json::to_string(&info).map_err(|e| CliError::Failed(e.to_string()))?
    );
    std::io::stdout().flush()?;
    if bool_flag(args, "--once") {
        server.serve_once()?;
    } else {
        eprintln!("mini: serving {} (Ctrl-C stops)", server.url());
        server.serve()?;
    }
    Ok(())
}

fn read_seed(args: &[String]) -> Result<String, CliError> {
    if let Some(seed) = flag(args, "--key") {
        return Ok(seed);
    }
    if let Some(path) = path_flag(args, "--key-file") {
        return Ok(std::fs::read_to_string(path)?.trim().to_string());
    }
    std::env::var(KEY_ENV).map_err(|_| usage("sign needs --key, --key-file or MINI_SIGNING_KEY"))
}

fn host_version(args: &[String]) -> String {
    flag(args, "--host").unwrap_or_else(|| DEFAULT_HOST_VERSION.to_string())
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let index = args.iter().position(|arg| arg == name)?;
    args.get(index + 1).cloned()
}

fn bool_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|arg| arg == name)
}

fn path_flag(args: &[String], name: &str) -> Option<PathBuf> {
    flag(args, name).map(PathBuf::from)
}

fn positional(args: &[String], value_flags: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg.starts_with("--") {
            index += if value_flags.contains(&arg.as_str()) {
                2
            } else {
                1
            };
            continue;
        }
        out.push(arg.clone());
        index += 1;
    }
    out
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<(), CliError> {
    let text = serde_json::to_string_pretty(value).map_err(|e| CliError::Failed(e.to_string()))?;
    println!("{text}");
    Ok(())
}

fn usage(message: &str) -> CliError {
    CliError::Usage(message.to_string())
}
