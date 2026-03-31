# YAML / HA Pattern Detection Research

Research into patterns detectable via `tree-sitter-yaml` AST for Home Assistant config review.

## tree-sitter-yaml Node Types (Key Ones)

```
stream                          # root node
  document                      # YAML document (can have multiples in one file)
    block_node                  # block-style content
      block_mapping             # key: value pairs
        block_mapping_pair      # single key: value (fields: key, value)
      block_sequence            # - list items
        block_sequence_item     # single list item
      block_scalar              # | or > multiline strings
    flow_node                   # inline content
      flow_mapping              # { key: value }
        flow_pair               # single k: v in flow (fields: key, value)
      flow_sequence             # [ item, item ]
      plain_scalar              # unquoted value
      double_quote_scalar       # "quoted value"
      single_quote_scalar       # 'quoted value'
      tag                       # !secret, !include, !env_var, custom tags
      anchor                    # &anchor_name
      alias                     # *alias_name
```

**Key insight**: `block_mapping_pair` has `key` and `value` fields -- this is our primary traversal target. We walk the tree looking at key names and their values/children.

## Detectable Patterns

### Tier 1: General YAML (any YAML file)

| # | Pattern | Severity | Node Types Used | Description |
|---|---------|----------|-----------------|-------------|
| 1 | **Duplicate keys** | High/bug | `block_mapping` -> children `block_mapping_pair` | Walk children of a `block_mapping`, collect key text, flag duplicates. HA docs explicitly warn: "the last value for a key is used" -- silent data loss. |
| 2 | **Hardcoded secrets** | High/security | `block_mapping_pair` key+value | Key name matches secret patterns (`password`, `api_key`, `token`, etc.), value is a `plain_scalar`/`double_quote_scalar` (not a `tag` like `!secret`). |
| 3 | **Tab indentation** | Medium/style | Check source text for `\t` in leading whitespace | HA docs: "Tabs are not allowed to be used for indentation." tree-sitter-yaml may also produce parse errors on tabs. |
| 4 | **Trailing whitespace in values** | Low/style | `plain_scalar` with trailing spaces | Can cause subtle string comparison bugs in entity_id matching. |

### Tier 2: Home Assistant Automation Patterns

These require detecting we're inside an `automation:` block (walk up from `block_mapping_pair` looking for parent key = "automation").

| # | Pattern | Severity | Detection Strategy |
|---|---------|----------|--------------------|
| 5 | **Automation missing `id`** | Medium/quality | Automation list item (`block_sequence_item` under `automation:`) without an `id:` key. Without `id`, the UI can't manage the automation, and debug traces are disabled. |
| 6 | **Automation missing `alias`** | Low/quality | Automation without `alias:` key -- hard to identify in logs and UI. |
| 7 | **Automation missing `mode`** | Info/quality | Automation without `mode:` key. Defaults to `single`, which issues warnings on re-trigger. Many automations should be `restart` or `queued`. Not a bug, but worth flagging for awareness. |
| 8 | **Deprecated `trigger:`/`action:`/`condition:` (singular)** | Medium/quality | HA has migrated from `trigger:` to `triggers:`, `action:` to `actions:`, `condition:` to `conditions:` (plural forms). Singular still works but is deprecated syntax. Detect `block_mapping_pair` with key `trigger`, `action`, or `condition` under automation items. |
| 9 | **Empty action list** | High/bug | Automation with `actions:` key whose value is empty (null, `[]`, or no children). An automation that does nothing is almost certainly a mistake. |
| 10 | **Empty trigger list** | High/bug | Same for `triggers:` -- automation with no triggers never fires. |

### Tier 3: Entity/Service Patterns

| # | Pattern | Severity | Detection Strategy |
|---|---------|----------|--------------------|
| 11 | **entity_id without domain prefix** | High/bug | Value of `entity_id:` key that doesn't contain a `.` (e.g., `motion_sensor` instead of `binary_sensor.motion_sensor`). Walk `block_mapping_pair` where key = `entity_id`, check value text contains `.`. |
| 12 | **service call without domain** | Medium/bug | Value of `service:` key that doesn't contain a `.` (e.g., `turn_on` instead of `light.turn_on`). Same check as entity_id. |
| 13 | **Hardcoded entity_id in templates** | Info/quality | `double_quote_scalar` or `plain_scalar` values containing Jinja2 `{{ }}` with hardcoded entity IDs. Suggests the value should use `target:` instead. |

### Tier 4: Security & Configuration Patterns

| # | Pattern | Severity | Detection Strategy |
|---|---------|----------|--------------------|
| 14 | **IP addresses / URLs with credentials** | High/security | Scalar values matching patterns like `http://user:pass@`, `ftp://`, or containing embedded credentials in URLs. |
| 15 | **`!include` with absolute paths** | Medium/quality | `tag` node with text `!include` followed by value starting with `/`. Absolute paths break portability. |
| 16 | **Exposed ports (0.0.0.0)** | Medium/security | Scalar values containing `0.0.0.0` in `host:` or `server:` keys. Same pattern as Python analysis but for YAML configs. |

### Tier 5: ESPHome-Specific (bonus, same YAML grammar)

| # | Pattern | Severity | Detection Strategy |
|---|---------|----------|--------------------|
| 17 | **ESPHome missing `ota:` password** | Medium/security | ESPHome config (detect `esphome:` top-level key) with `ota:` section but no `password:` key. |
| 18 | **ESPHome `wifi:` without `ap:` fallback** | Low/quality | WiFi config without AP fallback mode -- device becomes unreachable if WiFi is down. |
| 19 | **ESPHome `api:` without encryption** | Medium/security | `api:` section without `encryption:` key. API traffic is unencrypted by default. |

### Tier 6: Jinja2 Template Patterns (text-level, not AST)

These operate on scalar values that contain Jinja2 syntax -- detected by regex on the value text, not by tree-sitter nodes.

| # | Pattern | Severity | Detection Strategy |
|---|---------|----------|--------------------|
| 20 | **`states('sensor.xxx')` without availability check** | Medium/bug | Template using `states()` without checking for `'unavailable'` or `'unknown'`. Common cause of template errors in HA. |
| 21 | **`states.sensor.xxx.state` (dot notation)** | Medium/quality | Deprecated dot-notation for state access. Should use `states('sensor.xxx')`. |
| 22 | **Float comparison without `float` filter** | Low/bug | Template comparing `states(...)` to a number without `| float` or `| int` filter. States are always strings in HA. |

## Implementation Priority

**Phase 1 (this PR):** Patterns 1-4 -- general YAML, no HA-specific knowledge needed
**Phase 2:** Patterns 5-10 -- HA automation structure, requires context-aware key walking
**Phase 3:** Patterns 11-16 -- entity/service/security, requires HA domain knowledge
**Future:** Patterns 17-22 -- ESPHome, Jinja2, need deeper specialization

## AST Traversal Strategy

The key helper we need is a "context-aware key walker" that, given a `block_mapping_pair` node, can answer:

1. **What is the key name?** -- `source[key_node.byte_range()]`
2. **What is the value type?** -- Check `value` child's kind: `plain_scalar`, `tag`, `block_sequence`, `block_mapping`, etc.
3. **What is the parent context?** -- Walk up through `block_mapping` -> `block_sequence_item` -> `block_sequence` -> `block_mapping_pair` (value) -> get that pair's key name. This gives us "this key is inside automation -> triggers -> trigger"
4. **Is the value a HA tag?** -- Value child is `flow_node` containing a `tag` node with `!secret`, `!include`, etc.

Example tree for an automation:
```yaml
automation:           # block_mapping_pair (key="automation")
  - alias: Test       #   block_sequence_item -> block_mapping -> block_mapping_pair
    triggers:          #   block_mapping_pair (key="triggers")
      - trigger: state #     block_sequence_item -> block_mapping -> block_mapping_pair
        entity_id: binary_sensor.motion
    actions:
      - service: light.turn_on
        target:
          entity_id: light.living_room
```

This parent-context walking is the core of all HA-specific detections.
