---
name: skill-review-expert
description: Review a Claude Code skill file for correctness, clarity, and compliance with Claude prompting guidelines and skill authoring best practices. Use when editing, auditing, or creating skills.
---

# Skill Review Expert

Review a skill file against Claude's prompting guidelines and skill authoring best practices. Produces structured findings grouped by severity.

**Scope:** This review covers the skill file's text — structure, clarity, consistency, and compliance. It does not verify referenced tool behavior or runtime agent compliance.

## Workflow

1. Read the target skill file
2. Run the automated scans (see Tooling section)
3. Run each check category below, using scan results as evidence
4. Report findings grouped by severity (Critical > Important > Suggestion)
5. For each finding: state the problem, cite the violated check ID, show a concrete fix

**Authority hierarchy:** Claude platform docs > skill authoring best practices > reviewer opinion. When uncertain, check Context7 (`/websites/platform_claude_en`) for the canonical answer.

**Failure modes:**
- File has no frontmatter → treat as unstructured skill, flag under B2
- File is not a skill (no actionable instructions) → report and stop
- Context7 unreachable → proceed using cached knowledge, note in report

## Tooling (run first)

Automated scans provide evidence for checks A1, B5, D3, and D4. Run these before the manual review:

```bash
# A1: Negative framing scan — each hit is a candidate for positive reframing
grep -n "Don't\|Do not\|Do NOT\|Never\|Avoid\|don't" "$SKILL_FILE"

# B5: Backslash path scan
grep -n '\\\\' "$SKILL_FILE"

# D3: Cross-reference scan — find "above", "below", "see section" references
grep -n -i 'see \|above\|below\|described in' "$SKILL_FILE"

# D4: Terminology frequency — spot synonyms for key concepts
grep -o -i '\bfinding\b\|issue\b\|result\b\|problem\b\|check\b' "$SKILL_FILE" | sort | uniq -c | sort -rn
```

## Check Categories

### A. Prompting Compliance (Claude platform docs)

Source: https://platform.claude.com/docs/en/build-with-claude/prompt-engineering/claude-prompting-best-practices

| # | Check | What to look for |
|---|-------|-----------------|
| A1 | **Positive framing** | Instructions phrased as what TO do. Flag every "Don't", "Never", "Avoid" that can be reframed. Example: "Use `tp` for real bugs" instead of "Don't use `wontfix`". |
| A2 | **Clear and direct** | Imperative, unambiguous, specific. Flag hedging ("you might want to", "consider", "it may be useful"). Example: "Run `cargo test`" passes; "You could try running tests" fails. |
| A3 | **Concrete examples** | Every non-trivial instruction has a code block, command, or worked example. Flag abstract rules with no example. |
| A4 | **Structured with markup** | Headers, tables, and code blocks for scannability. Flag walls of prose (>5 sentences without a break), deeply nested bullets, or unstructured lists > 5 items. |
| A5 | **Consistent instructions** | Tables, decision trees, examples, and prose all agree. Flag any case where two sections give different guidance for the same scenario. |
| A6 | **Context before instructions** | Reference material (what things are) appears before procedural material (what to do with them). Flag instructions that reference concepts defined later. |

### B. Skill Authoring Best Practices

Source: https://platform.claude.com/docs/en/agents-and-tools/agent-skills/best-practices

| # | Check | What to look for |
|---|-------|-----------------|
| B1 | **Concise — skip what Claude knows** | Flag definitions of common concepts (what a PR is, what unit tests are). Only include context Claude lacks. Example of waste: "A pull request is a way to propose changes..." |
| B2 | **Clear trigger in frontmatter** | `description` field specifies when to activate. Flag vague descriptions ("useful for development"). Example of good: "Use when reviewing code with quorum or recording feedback." |
| B3 | **Delegate to deterministic tools** | Invoke linters/scripts/CLIs for verifiable checks; reserve LLM reasoning for judgment calls. Flag skills that use LLM reasoning for things `grep` or a linter could verify. |
| B4 | **Reference external truths** | Stable rules belong in referenced files (STYLE_GUIDE.md, .eslintrc), not duplicated inline. Flag large rule blocks (>20 lines) that restate a config file. |
| B5 | **Unix paths only** | All file paths use forward slashes. Flag backslashes. |
| B6 | **Actionable output** | Every instruction produces a concrete action (a command, a verdict, a file edit). Flag instructions ending with "handle appropriately" or "use good judgment". |

### C. LLM Anti-Pattern Guards

Source: observed failure modes in agent workflows

| # | Check | What to look for |
|---|-------|-----------------|
| C1 | **Inline until repeated** | Flag instructions that push extraction or DRY before a pattern repeats 3+ times. Example: "Extract a helper for this" when the pattern appears once. |
| C2 | **Trust the type system** | Flag instructions to add null checks, error handling, or validation for scenarios guaranteed by types or framework contracts. Example: null-checking a non-optional field. |
| C3 | **Delegate style to linters** | Style/whitespace rules belong in linter config. Flag instructions about indentation, line length, or brace style. Example: "Use 2-space indentation" belongs in .editorconfig. |
| C4 | **Remove dead interfaces cleanly** | Flag instructions to maintain old exports, re-export removed types, or add deprecation wrappers when all call sites are controlled. Example: keeping a `_legacyFoo` re-export. |

### D. Internal Consistency

| # | Check | What to look for |
|---|-------|-----------------|
| D1 | **Decision tree completeness** | Every scenario has exactly one matching path. Flag gaps (no matching row) and overlaps (multiple matching rows). Example: a finding that is both "wrong" and "outside diff" should have a clear resolution order. |
| D2 | **Example-prose agreement** | Code examples match surrounding text. Flag examples using different flags or patterns than the prose specifies. Example: prose says `--verbose` but the code block uses `-v`. |
| D3 | **Cross-reference integrity** | "See section X" or "described above" points to existing content. Use the grep scan to find all references, verify each. |
| D4 | **Terminology consistency** | Same concept uses the same term throughout. Use the frequency scan to spot synonyms. Example: "finding" and "issue" used interchangeably for the same concept. |

### E. Information Architecture

| # | Check | What to look for |
|---|-------|-----------------|
| E1 | **Happy path first** | Most common workflow appears before edge cases and advanced options. Flag skills that lead with configuration. Example: "Configuration" section before "Quick Start" is backwards. |
| E2 | **Progressive disclosure** | Simple usage before advanced flags. Quick reference before deep explanation. Flag skills that front-load complexity or version history. |
| E3 | **Scannable for quick decisions** | An agent mid-task can find the answer in <10 seconds. Flag decision logic buried in paragraphs — decision points belong in tables or numbered steps. |
| E4 | **Section ordering matches usage flow** | Sections follow the order encountered during a task. Flag reference material interleaved with workflow steps. |

### F. Agent Compliance Prediction

| # | Check | What to look for |
|---|-------|-----------------|
| F1 | **Unambiguous verdicts** | For every decision point, the agent can determine the right action without guessing. Flag cases where the answer depends on unstated context. Example: "use the appropriate model" — appropriate how? |
| F2 | **Required vs optional is explicit** | Flag ambiguous optionality ("you can also...", "optionally...") for steps that are actually required. Example: "You can run tests" when tests are mandatory. |
| F3 | **Failure modes addressed** | What happens when a tool fails, a file is missing, or output is unexpected? Flag workflows with no error path. Example: "Run `quorum review`" with no guidance for when quorum is not installed. |
| F4 | **Scope boundaries clear** | The skill states what it covers and what it delegates. Flag skills that could cause scope creep. Example: a review skill that starts refactoring code. |

## Output Format

```
## Skill Review: <skill-name>

### Critical (blocks correct agent behavior)
- **[A5]** Conflicting instructions: "Key principles" says use `tp` for out-of-diff bugs,
  but "Workflow step 3" says use `fp --fp-kind out-of-scope`.
  Fix: Align both to `tp --in-diff false`, which matches calibrator behavior.

### Important (degrades quality or compliance)
- **[A1]** Negative framing: "Don't use wontfix" (line 42)
  Fix: "Always choose `tp` or `fp` — `wontfix` is inert."

### Suggestions (polish)
- **[E2]** Configuration section appears before basic usage.
  Fix: Move configuration after the workflow section.

### Passing
A2, A3, A4, B1, B2, B5, C1-C4, D1-D4, E1, E3, F1-F4
```

## Evaluation-Driven Iteration

After reviewing, the skill author should:

1. Fix all Critical findings — these cause wrong agent behavior
2. Fix Important findings — these degrade quality measurably
3. Consider Suggestions — these improve polish but may not be worth the churn
4. Re-run this review on the updated skill to verify fixes and catch regressions

For calibrating this reviewer: run it on a skill, observe the agent using that skill on a real task, note where the agent makes mistakes. Add a check for each observed failure mode.
