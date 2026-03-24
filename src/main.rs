mod analysis;
mod cli;
mod config;
mod finding;
mod hydration;
mod linter;
mod merge;
mod output;
mod parser;

use clap::Parser;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = cli::Args::parse();
    match args.command {
        cli::Command::Review(opts) => {
            let exit_code = run_review(opts);
            std::process::exit(exit_code);
        }
        cli::Command::Version => {
            println!("quorum {}", env!("CARGO_PKG_VERSION"));
        }
    }
    Ok(())
}

fn run_review(opts: cli::ReviewOpts) -> i32 {
    if opts.files.is_empty() {
        eprintln!("Error: No files specified");
        return 3;
    }

    let style = output::Style::detect(opts.no_color);
    let use_json = opts.json || !std::io::IsTerminal::is_terminal(&std::io::stdout());
    let mut all_findings = Vec::new();
    let mut had_errors = false;

    for file_path in &opts.files {
        if !file_path.exists() {
            eprintln!("Error: File not found: {}", file_path.display());
            had_errors = true;
            continue;
        }

        let lang = match parser::Language::from_path(file_path) {
            Some(l) => l,
            None => {
                eprintln!("Warning: Unsupported file type, skipping: {}", file_path.display());
                continue;
            }
        };

        let source = match std::fs::read_to_string(file_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error: Could not read {}: {}", file_path.display(), e);
                had_errors = true;
                continue;
            }
        };

        let tree = match parser::parse(&source, lang) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("Error: Parse failed for {}: {}", file_path.display(), e);
                had_errors = true;
                continue;
            }
        };

        // Run local analysis
        let mut file_findings = Vec::new();
        file_findings.extend(analysis::analyze_complexity(&tree, &source, lang, 5));
        file_findings.extend(analysis::analyze_insecure_patterns(&tree, &source, lang));

        if use_json {
            all_findings.extend(file_findings);
        } else {
            let merged = merge::merge_findings(vec![file_findings], 0.8);
            let file_str = file_path.to_string_lossy();
            print!("{}", output::format_review(&file_str, &merged, &style));
            all_findings.extend(merged);
        }
    }

    // If all files had errors and no findings, exit with tool error
    if had_errors && all_findings.is_empty() {
        if use_json {
            println!("[]");
        }
        return 3;
    }

    if use_json {
        let merged = merge::merge_findings(vec![all_findings], 0.8);
        match output::format_json(&merged) {
            Ok(json) => println!("{}", json),
            Err(e) => {
                eprintln!("Error: JSON serialization failed: {}", e);
                return 3;
            }
        }
        return output::compute_exit_code(&merged);
    }

    output::compute_exit_code(&all_findings)
}
