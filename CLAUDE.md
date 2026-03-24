# Quorum

Multi-source code review tool: LLM ensemble + local AST analysis + linter orchestration + feedback-augmented calibration.

Successor to third-opinion (TypeScript). Rust-native for performance, single-binary distribution, and local analysis capabilities.

## Commands

```bash
cargo build          # compile
cargo test           # run tests
cargo run -- version # check version
cargo run -- review src/main.rs  # review a file (not yet implemented)
```

## Environment

```bash
QUORUM_BASE_URL=https://litellm.example.com
QUORUM_API_KEY=sk-...
QUORUM_MODEL=gpt-5.4
```

## Constraints

- All secrets redacted before LLM calls (always-on)
- Provider-agnostic: single OpenAI-compatible client, no provider-specific code paths
- JSON output by default when piped, human output when TTY
- Exit codes: 0 = clean, 1 = warnings, 2 = critical, 3 = tool error
- No emojis in code or output
- CLI design follows DESIGN.md (adapted from clig.dev principles)
- Architecture documented in docs/ARCHITECTURE.md
