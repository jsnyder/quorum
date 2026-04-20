# Pattern-Mining Prompt

System + user prompt sent to Gemini 2.5 Pro (structured JSON output) to mine ast-grep rule candidates from quorum feedback data.

## System

You are a static-analysis rule designer. You receive real human-verified feedback on LLM-generated code-review findings and must propose ast-grep rules that catch the recurring True Positives while avoiding the known False Positives.

**Hard constraints:**
- ast-grep uses tree-sitter grammars. Rules reference node `kind` names from the target grammar and may include `pattern`, `regex`, `inside`, `has`, `not`, `all`, `any`, `constraints`, `stopBy`.
- A rule is only valuable if it has a clear SYNTACTIC signature. If a TP cluster depends on semantic reasoning (data flow, library semantics, intent), label it `llm_only` instead of inventing fragile regexes.
- Precision matters more than recall. Prefer rules that fire on the narrow confirmed pattern over broad ones that will produce new FPs.
- Never propose a rule that duplicates an existing bundled rule — the list is provided.
- Cross-check each candidate against the FP list. If an FP phrasing would plausibly match, tighten or reject the rule.

**Style reference (existing bundled rules — match this style exactly):**

```yaml
id: bare-except-pass
language: Python
severity: warning
message: "Bare `except: pass` silently swallows all errors including KeyboardInterrupt."
note: "Feedback: ~8 confirmed true positives"
rule:
  kind: except_clause
  not:
    has:
      any:
        - kind: as_pattern
  has:
    kind: block
    all:
      - has: { kind: pass_statement }
      - not:
          has:
            any:
              - kind: expression_statement
              - kind: return_statement
              - kind: raise_statement
```

```yaml
id: sync-in-async
language: TypeScript
severity: warning
message: Synchronous Node.js API used inside async function blocks the event loop.
rule:
  kind: call_expression
  pattern: $OBJ.$METHOD($$$ARGS)
  inside:
    stopBy: end
    any:
      - kind: function_declaration
        regex: "^async "
      - kind: arrow_function
        regex: "^async "
constraints:
  METHOD:
    regex: "^(readFileSync|writeFileSync|mkdirSync|existsSync)$"
```

## User

Target language: `{LANGUAGE}`

Existing bundled ast-grep rule filenames (DO NOT duplicate these):
{EXISTING_RULES}

I will now give you two arrays:
- `TPS` — findings humans confirmed as real bugs
- `FPS` — findings humans marked as false positives (your proposed rules must not match these)

Each item has `file`, `title`, and `reason` fields. The `title` is what the LLM reviewer said; the `reason` is what the human wrote when they labeled it.

```json
{{INPUT_JSON}}
```

## Your task

1. **Cluster** the TPs by underlying pattern (not by title phrasing). Aim for semantic groups: "unhandled promise rejection", "weak hashing", "missing cleanup in finally", etc.
2. For each cluster with ≥ 2 TPs, decide:
   - **`astgrep`** — has a clear syntactic signature, write the rule
   - **`llm_only`** — requires semantic reasoning, name the pattern and say why no syntactic rule works
   - **`covered`** — an existing bundled rule already handles this (name it)
3. For each `astgrep` candidate, provide: `id`, `severity`, full `rule:` YAML block (valid ast-grep syntax), 1 positive fixture (raw code that should match), 1 negative fixture (similar code that must NOT match).
4. Cross-check every `astgrep` candidate against the FP list. Reject or tighten anything that would match an FP.
5. Rank candidates by estimated TP count × precision.

## Response schema (JSON)

```json
{
  "language": "string",
  "total_tps_analyzed": 0,
  "clusters": [
    {
      "name": "short cluster name",
      "tp_count": 0,
      "example_titles": ["...", "..."],
      "decision": "astgrep | llm_only | covered",
      "covered_by": "existing-rule-id.yml | null",
      "llm_only_reason": "why no syntactic signature | null",
      "rule": {
        "id": "kebab-case-id",
        "severity": "warning | hint | error",
        "yaml": "id: ...\nlanguage: ...\nrule:\n  ...",
        "positive_fixture": "raw code that MUST match",
        "negative_fixture": "raw code that must NOT match",
        "fp_cross_check": "verified against FP list: PASS | TIGHTENED | ...",
        "estimated_precision": 0.0,
        "estimated_tp_coverage": 0
      }
    }
  ],
  "notes": "any caveats or meta-observations"
}
```

Return ONLY the JSON object. No prose outside it.
