//! Times each stage of one compile, for a perf check on a large file:
//!
//!     cargo run --release --example prof -- path/to/big.aly
//!
//! A stage that grows faster than the file is the one to look at; the
//! line helpers were quadratic once, at ten seconds for 9000 lines.

use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).expect("path");
    let src = std::fs::read_to_string(&path).unwrap();
    let t = Instant::now();
    let parsed = alloy_syntax::parse_lenient(&src, Default::default())
        .ok()
        .unwrap();
    println!(
        "parse            {:>7.1} ms  ({} tokens, {} stmts)",
        t.elapsed().as_secs_f64() * 1e3,
        parsed.lexed.toks.len(),
        parsed.chunk.block.stmts.len()
    );
    let options = alloy::EmitOptions::default();
    let t = Instant::now();
    let r = alloy::desugar::render(&src, &parsed.lexed.toks, &parsed.chunk, &options);
    println!(
        "render ship      {:>7.1} ms  ({} bytes)",
        t.elapsed().as_secs_f64() * 1e3,
        r.text.len()
    );
    let check = alloy::EmitOptions {
        check: true,
        ..options.clone()
    };
    let t = Instant::now();
    let _ = alloy::desugar::render(&src, &parsed.lexed.toks, &parsed.chunk, &check);
    println!(
        "render check     {:>7.1} ms",
        t.elapsed().as_secs_f64() * 1e3
    );
    let t = Instant::now();
    let st = alloy::fmt_structure::structure(&src, &parsed.lexed.toks);
    println!(
        "structure        {:>7.1} ms",
        t.elapsed().as_secs_f64() * 1e3
    );
    let _ = st;
    let t = Instant::now();
    let lints = alloy::lint::run(
        &src,
        &parsed.lexed.toks,
        &parsed.chunk,
        false,
        &Default::default(),
    );
    println!(
        "lint::run        {:>7.1} ms  ({} lints)",
        t.elapsed().as_secs_f64() * 1e3,
        lints.len()
    );
    let t = Instant::now();
    let _ = alloy::extensions::collect(&src);
    println!(
        "extensions       {:>7.1} ms",
        t.elapsed().as_secs_f64() * 1e3
    );
    let t = Instant::now();
    let _ = alloy::directives::scan(&src);
    println!(
        "directives       {:>7.1} ms",
        t.elapsed().as_secs_f64() * 1e3
    );
    let t = Instant::now();
    let _ = alloy::compile(&src).unwrap();
    println!(
        "compile total    {:>7.1} ms",
        t.elapsed().as_secs_f64() * 1e3
    );
}
