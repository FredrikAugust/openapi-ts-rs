//! openapi-ts-rs: generates TypeScript types from OpenAPI 3 specs in openapi-typescript 6.7.0's shape.
//!
//! Usage: openapi-ts-rs <spec.yml> [-o <out.d.ts>] [--strict]

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod emit;
mod load;
mod ts;
mod yaml;

use anyhow::{Result, bail};
use std::path::PathBuf;

const USAGE: &str = "usage: openapi-ts-rs <spec.yml> [-o <out.d.ts>] [--strict]\n\n  --strict  fail when a $ref does not resolve (default: warn)";

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let (mut input, mut output, mut strict) = (None, None, false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-o" | "--output" => output = args.next().map(PathBuf::from),
            "--strict" => strict = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            "-V" | "--version" => {
                println!("openapi-ts-rs {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            _ if input.is_none() => input = Some(PathBuf::from(a)),
            _ => bail!("unexpected argument {a}"),
        }
    }
    let Some(input) = input else { bail!("{USAGE}") };
    let t = std::time::Instant::now();
    let spec = load::Spec::load(&input)?;
    let t1 = t.elapsed();
    let g = emit::Gen::new(&spec);
    let ts = g.generate();
    if std::env::var_os("OPENAPI_TS_RS_TIMING").is_some() {
        eprintln!("load {t1:?}, emit {:?}", t.elapsed() - t1);
    }
    let errors = g.errors.take();
    if !errors.is_empty() {
        let msg = format!("{} unresolved $ref(s):\n  {}", errors.len(), errors.join("\n  "));
        if strict {
            bail!(msg);
        }
        eprintln!("warning: {msg}");
    }
    match output {
        Some(p) => {
            if let Some(dir) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(p, ts)?;
        }
        None => print!("{ts}"),
    }
    // Freeing every parsed document only delays the exit.
    std::mem::forget(g);
    std::mem::forget(spec);
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
