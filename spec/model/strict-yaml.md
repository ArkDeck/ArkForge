# Strict YAML subset
status: draft
source: crates/arkforge-core/src/yaml.rs
applies-to: profiles/*.yaml, transcripts/*.yaml, every YAML file under spec/

ArkForge does not implement YAML. It implements a small block-structured grammar
that happens to be valid YAML, chosen so a document can be reviewed by eye and
hashed without ambiguity. A port MUST accept exactly this subset and MUST refuse
everything else; accepting more is a conformance failure because two readers
would then disagree about what a profile says.

## Accepted

| construct | form |
|---|---|
| document start | an optional first line `---` |
| comment | `#` to end of line, when `#` is at line start or preceded by a space, and not inside quotes |
| block mapping | `key: value` or `key:` followed by a more-indented block |
| block sequence | `- item` / `-` followed by a more-indented block; an item may itself be a mapping (`- key: value` starts an inline mapping) |
| flow sequence | `[a, b, c]` of plain or quoted scalars on one line; `[]` is the empty sequence; nested `[`/`{` inside it and empty items (`[a,,b]`) are rejected |
| plain scalar | any text without leading quote; whitespace trimmed |
| quoted scalar | `'…'` or `"…"`: the matching outer quotes are stripped and **no escape sequences are processed** (`'it''s'` yields `it''s`; a backslash is literal) |
| null scalar | the plain scalars `null` and `~` yield `null` |
| explicit empty value | `key:` with nothing after it on the line and no indented block → `null` |

Indentation is by spaces. Sibling keys at one level MUST share one indentation;
a child block MUST be more indented than its parent key.

## Rejected (typed error, never a best-effort parse)

- tabs anywhere in indentation;
- anchors (`&`), aliases (`*`) and tags (`!`) at any unquoted token boundary,
  including inside a mapping value or flow sequence. The same bytes inside a
  quoted scalar or an ordinary word such as `a&b` are literal text;
- flow mappings (`{a: 1}`);
- multi-line scalars (`|`, `>`, and continuation lines);
- duplicate keys within one mapping;
- inconsistent indentation;
- a document that is not a mapping at the root (profiles) — the loader
  requires `schemaVersion` and named blocks.

## Typing

The parser yields only **strings**, sequences, mappings and `null`. All typing
is done by the consumer (the profile loader):

| consumer expects | accepted text | notes |
|---|---|---|
| unsigned integer | decimal digits; `0x`/`0X` hex; `_` digit separators | `0xCC` → 204, `0x2207` → 8711 |
| optional unsigned | as above, or the literal `unknown` → absent | an omitted key is an error, not unknown |
| boolean | exactly `true` or `false` | `yes`/`no`/`on` are errors |
| version | `major.minor.patch` decimal | |
| identifier | OpaqueId grammar | |
| device mode | `[a-z0-9-]{1,64}` | |
| enum | exact spelling from `vocabularies.yaml` | |

## Conformance examples (to be generated as a `yaml` suite)

| input | result |
|---|---|
| `a: 1\nb: [x, 'y', "z"]` | mapping a→"1", b→["x","y","z"] |
| `a: 1\na: 2` | reject: duplicate key |
| `a:\n\t- x` | reject: tab |
| `a: &x 1` | reject: anchor |
| `a: *x` | reject: alias |
| `a: !!str 1` | reject: tag |
| `a: {b: 1}` | reject: flow mapping |
| `a: |\n  text` | reject: multi-line scalar |
| `a:` | mapping a→null |
| `a: 'it''s'` | mapping a→"it''s" (no escape processing) |
| `a: ~` | mapping a→null |
| `a: [x, [y]]` | reject: nested flow collection |
| `a: x # comment` | mapping a→"x" |
| `a: "x # not a comment"` | mapping a→"x # not a comment" |
| `a: 1\n  b: 2` | reject: inconsistent indentation |

## Digest implications

A profile's digest is over its canonical CBOR model (AF-PROF-001), not over the
YAML bytes, so comments, key order and quoting style do not affect it — but two
readers that disagree about the *value* of a scalar (for example whether
`0x2207` is a number) would. That is why this subset is closed.

## Requirements

### AF-YAML-001 — block structure
status: normative
tests: [AF-CONF-YAML-001..003, AF-CONF-YAML-008, AF-CONF-YAML-014..016]

Block mappings, block sequences, sequence items that are mappings, nested
blocks and the `---` document start MUST parse to the value trees the fixtures
record.

### AF-YAML-002 — flow sequences
status: normative
tests: [AF-CONF-YAML-004, AF-CONF-YAML-005]

### AF-YAML-003 — null
status: normative
tests: [AF-CONF-YAML-006, AF-CONF-YAML-007]

An empty value, `null` and `~` are `null`.

### AF-YAML-004 — comments
status: normative
tests: [AF-CONF-YAML-009..011]

`#` starts a comment at line start or after a space, never inside quotes.

### AF-YAML-005 — quoting strips, never escapes
status: normative
tests: [AF-CONF-YAML-012]

### AF-YAML-006 — scalars are text
status: normative
tests: [AF-CONF-YAML-013]

The reader yields text; numbers (decimal, `0x` hex, `_` separators) are typed
by the consumer.

### AF-YAML-010 — duplicate keys are refused
status: normative
tests: [AF-CONF-YAML-019, AF-CONF-YAML-020]

### AF-YAML-011 — tabs are refused
status: normative
tests: [AF-CONF-YAML-021]

### AF-YAML-012 — anchors, aliases and tags are refused
status: normative
tests: [AF-CONF-YAML-017, AF-CONF-YAML-018, AF-CONF-YAML-022..024]

At every unquoted token boundary they are refused. Markers inside quoted text
or inside a longer ordinary word are literal and are never resolved.

### AF-YAML-013 — flow mappings and nested/ill-formed flow sequences are refused
status: normative
tests: [AF-CONF-YAML-025, AF-CONF-YAML-028..030]

### AF-YAML-014 — multi-line scalars are refused
status: normative
tests: [AF-CONF-YAML-026, AF-CONF-YAML-027]

### AF-YAML-015 — inconsistent indentation is refused
status: normative
tests: [AF-CONF-YAML-031..033]
