//! Measure how much `redact_secrets` alters real source before it reaches the
//! LLM (#530).
//!
//! Routing the skills path through the redaction chokepoint changes what the
//! model sees. If `redact_secrets` is aggressive it could mangle legitimate
//! code and degrade review quality, so this quantifies the change on a real
//! corpus -- this repository's own sources -- without spending anything on
//! LLM calls.
//!
//! Run: `cargo run --example redaction_impact -- src`

fn main() {
    let root = std::env::args().nth(1).unwrap_or_else(|| "src".to_string());

    let mut files = 0usize;
    let mut altered = 0usize;
    let mut total_bytes = 0usize;
    let mut changed_bytes = 0usize;
    let mut examples: Vec<(String, String, String)> = Vec::new();

    let mut stack = vec![std::path::PathBuf::from(&root)];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(&path) else {
                continue;
            };
            files += 1;
            total_bytes += src.len();
            let safe = quorum::redact::redact_secrets(&src);
            if safe != src {
                altered += 1;
                // Record the first differing line for eyeballing.
                for (a, b) in src.lines().zip(safe.lines()) {
                    if a != b {
                        changed_bytes += a.len();
                        if examples.len() < 12 {
                            examples.push((
                                path.display().to_string(),
                                a.trim().chars().take(88).collect(),
                                b.trim().chars().take(88).collect(),
                            ));
                        }
                        break;
                    }
                }
            }
        }
    }

    println!("files scanned:        {files}");
    println!("files altered:        {altered}");
    println!(
        "share altered:        {:.2}%",
        if files > 0 {
            altered as f64 * 100.0 / files as f64
        } else {
            0.0
        }
    );
    println!("total bytes:          {total_bytes}");
    println!("first-diff-line bytes:{changed_bytes}");
    println!();
    if examples.is_empty() {
        println!("No source file in this corpus is altered by redaction.");
    } else {
        println!("Sample alterations (first differing line per file):");
        for (path, before, after) in &examples {
            println!("  {path}");
            println!("    before: {before}");
            println!("    after:  {after}");
        }
    }
}
