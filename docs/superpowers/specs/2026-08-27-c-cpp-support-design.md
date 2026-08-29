# C/C++ Support — Phase 1 Design

Issue: #482. Related: #483 (MCP catalog under-reports languages).

## Goal

Recognize C and C++ files so they route through the LLM review path instead of
falling to the `Other` bucket. Measure that path against a real ESP32 corpus
before deciding whether ast-grep rules or clang-tidy earn their keep.

## Scope

Phase 1 only. Explicitly **not** in this branch:

- ast-grep rules for C/C++
- clang-tidy integration
- separate `Language::C`
- the eval corpus fixtures (landing separately, authored by the ESPresense session)

## Design

One enum variant covering both languages:

```text
from_extension:       c h cpp cc cxx hpp hh hxx  ->  Language::Cpp
tree_sitter_language:                            ->  tree_sitter_cpp::LANGUAGE
function_node_kinds:                             ->  ["function_definition"]
analysis dispatch:                               ->  no-op (like Yaml/Terraform)
```

`tree-sitter-cpp` parses C as a subset of C++. A separate `Language::C` would
double the match arms for no behavioural difference at this stage, and `.h` is
genuinely ambiguous between the two — a split forces a guess on the single most
common extension. Revisit if and when a rule needs to fire on one and not the other.

The analysis dispatch is a deliberate no-op: no AST checks ship in Phase 1. The
point of the phase is to measure the LLM-only path in isolation, so adding rules
now would confound the measurement it exists to produce.

## Touch points

11 exhaustive match sites across 5 files. The compiler enforces completeness —
a missed arm is a build error, not a silent fallthrough:

- `src/parser.rs` (2) — `from_extension`, `tree_sitter_language`
- `src/analysis.rs` (2) — dispatch, function-kind lookup
- `src/mcp/handler.rs` (2) — catalog, language routing
- `src/hydration.rs` (4)
- `src/pipeline.rs` (1)

## MCP catalog

Derive the catalog from the `Language` enum rather than appending a seventh line
to the hardcoded string at `src/mcp/handler.rs:273`. The string currently claims
six languages while the enum has nine — YAML, Terraform and Go are all supported
but unadvertised (#483).

Deriving it fixes the drift and makes recurrence impossible: a new variant cannot
be added without appearing in the catalog. Appending a line fixes today's symptom
and leaves the next language to drift again.

## Testing

- extension mapping across all eight extensions, including case variants
- parse of a real C++ translation unit yields a non-error root node
- function discovery finds definitions in both C and C++ source
- catalog derivation covers every enum variant (regression guard for #483)

## Validation

Live run against the ESPresense ESP32 firmware repo, which is what motivated the
issue. Success is the tool producing findings on real C++ at all — Phase 1 has no
recall target, since establishing the baseline is the deliverable.

## Risks

`tree-sitter-cpp` 0.23.4 against `tree-sitter` 0.26. The go and typescript grammars
are both 0.23 and work today, so the ABI generation is already proven in this tree.
The vendored Dockerfile grammar exists because tree-sitter-dockerfile pinned 0.20 and
conflicted; if cpp conflicts the same way, the same vendoring path is available.
