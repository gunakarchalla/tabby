# Implementation Plan — `stochastic` Generation Style

This document is a phase-wise implementation guide for adding a fourth completion generation style, **`stochastic`**, to Tabby. The existing styles are `code` (default), `hint`, and `pseudocode`. `stochastic` produces a numbered English "Steps" list wrapped in a language-native comment.

Each phase is self-contained and lists exact file paths, line anchors, code snippets, and acceptance checks. Phases must be applied in order — later phases assume the changes from earlier ones.

---

## Specification (authoritative)

| Aspect | Line-comment language (marker `{m}`) | Block-comment language (markers `{s}` / `{e}`) |
|---|---|---|
| Prompt prefix injection | `\n{m} Steps (English):\n{m} 1:` | `\n{s} Steps (English):\n1:` |
| Prompt suffix injection | `""` (empty) | `{e}\n` |
| Display prefix (ghost text) | `\n{m} Steps (English):\n{m} 1:` | `\n{s} Steps (English):\n1:` |
| Display suffix (ghost text) | `None` | `{e}` |
| Stop condition | First `\n` whose successor is not a prefix of `{m}` | First occurrence of `{e}` |
| No-comment language fallback | Downgrade to `code` style (existing behavior) | — |
| Empty-output placeholder | `(no steps generated)` | Same |
| Cycle order in VS Code | `code → hint → pseudocode → stochastic → code` | — |

---

## Phase 1 — Server prompt injection

**File:** `crates/tabby/src/services/completion/completion_prompt.rs`

In `build_styled` (around lines 124–159), add two new arms to the `match (style, &comment_style)` block — place them after the existing `pseudocode + Block` arm and before the `_ =>` downgrade fallback:

```rust
("stochastic", CommentStyle::Line(m)) => (
    format!("\n{m} Steps (English):\n{m} 1:"),
    String::new(),
    Some(format!("\n{m} Steps (English):\n{m} 1:")),
    None,
),
("stochastic", CommentStyle::Block(s, e)) => (
    format!("\n{s} Steps (English):\n1:"),
    format!("{e}\n"),
    Some(format!("\n{s} Steps (English):\n1:")),
    Some(e.to_string()),
),
```

The existing `_ =>` no-comment branch already downgrades to `code` for `CommentStyle::None`; no change there.

**Acceptance:** `cargo build -p tabby` succeeds; existing tests in this file still pass.

---

## Phase 2 — Server stop conditions

**File:** `crates/tabby-inference/src/decoding.rs`

The trie-based stop logic only matches suffixes of the buffer. The line-comment `stochastic` rule ("stop at `\n` followed by non-`{m}`") cannot be expressed as a fixed stop word, so we extend `StopCondition` with a predicate.

### 2.1 Extend the `StopCondition` struct (lines 113–166)

Add two fields:

```rust
pub struct StopCondition<'a> {
    stop_trie: Option<CachedTrie<'a>>,
    extra_stop_trie: Option<Trie<u8>>,
    reversed_text: String,
    num_decoded: usize,
    // NEW:
    forward_text: String,
    stochastic_line_marker: Option<String>,
}
```

Initialize the new fields to `String::new()` and `None` in both existing constructors (`new`, `new_with_extra_trie`).

Add a third constructor:

```rust
pub fn new_with_stochastic_line(
    stop_trie: Option<CachedTrie<'a>>,
    marker: String,
    text: &str,
) -> Self {
    Self {
        stop_trie,
        extra_stop_trie: None,
        reversed_text: reverse(text),
        num_decoded: 0,
        forward_text: text.to_string(),
        stochastic_line_marker: Some(marker),
    }
}
```

### 2.2 Extend `should_stop`

After the existing `extra_stop_trie` block and before the final `(false, 0)` return, insert:

```rust
if let Some(marker) = &self.stochastic_line_marker {
    self.forward_text.push_str(new_text);
    if let Some(matched) = check_stochastic_line_end(&self.forward_text, marker) {
        return (true, matched);
    }
}
```

### 2.3 Add the predicate helper (file scope)

```rust
/// For stochastic + line-comment: stop the moment a `\n` is followed by any
/// content that is not a prefix of the comment marker.
///
/// Returns the number of bytes (from `\n` inclusive) to truncate from the end
/// of the generated text, or `None` if we should keep going.
fn check_stochastic_line_end(text: &str, marker: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let nl_pos = bytes.iter().rposition(|&b| b == b'\n')?;
    let after = &bytes[nl_pos + 1..];
    if after.is_empty() {
        return None; // newline just emitted; wait for the next char(s)
    }
    let m = marker.as_bytes();
    let n = after.len().min(m.len());
    if after[..n] != m[..n] {
        Some(text.len() - nl_pos)
    } else {
        // Full or partial prefix of the marker — keep waiting.
        None
    }
}
```

This handles multi-character markers (`--`, `//`, `<!--`) correctly: if the model has emitted `\n-` and `{m}` is `--`, we return `None` and wait; if the next token is `X` we stop; if it's `-` we continue waiting and ultimately don't stop.

### 2.4 Wire into `create_with_style` (lines 44–82)

Add new arms inside the `match (style, language.comment_style())`:

```rust
("stochastic", CommentStyle::Line(m)) => {
    let cached = self.get_trie(language);
    return StopCondition::new_with_stochastic_line(cached, m.to_string(), text);
}
("stochastic", CommentStyle::Block(_, e)) => vec![e.to_string()],
```

Place these alongside the existing `hint` / `pseudocode` arms. The `_ => vec![]` fallback at the end already covers `CommentStyle::None`.

**Acceptance:** `cargo build -p tabby-inference` succeeds; existing tests in this module still pass.

---

## Phase 3 — Output wrapping and request schema

**File:** `crates/tabby/src/services/completion.rs`

### 3.1 Update the `generation_style` doc comment (lines 69–74)

Add a fourth bullet: `- "stochastic": numbered English step list wrapped in a language-native comment.`

### 3.2 Update `is_non_code_style` (lines 122–124)

```rust
fn is_non_code_style(&self) -> bool {
    matches!(
        self.generation_style.as_str(),
        "hint" | "pseudocode" | "stochastic"
    )
}
```

### 3.3 Add empty-output placeholder in `wrap_with_style` (lines 573–578)

In the `match styled.effective_style.as_str()` block, add:

```rust
"stochastic" => "(no steps generated)",
```

**Acceptance:** `cargo build -p tabby` succeeds.

---

## Phase 4 — Server tests

### 4.1 `crates/tabby-inference/src/decoding.rs` — `tests` module

Add three new tests:

```rust
#[test]
fn test_create_with_style_stochastic_line_stops_on_non_comment() {
    use tabby_common::languages::get_language;
    let factory = StopConditionFactory::default();
    let lang = get_language("dockerfile"); // marker "#"
    let mut cond = factory.create_with_style("", Some(lang), "stochastic");
    let (should_stop, _) = cond.should_stop(" Step one");
    assert!(!should_stop);
    let (should_stop, _) = cond.should_stop("\n# 2: Step two");
    assert!(!should_stop);
    let (should_stop, n) = cond.should_stop("\nFROM alpine");
    assert!(should_stop);
    assert!(n >= "\nFROM alpine".len());
}

#[test]
fn test_create_with_style_stochastic_line_partial_marker_waits() {
    use tabby_common::languages::get_language;
    let factory = StopConditionFactory::default();
    let lang = get_language("sql"); // line marker "--"
    let mut cond = factory.create_with_style("", Some(lang), "stochastic");
    // After "\n-" we have a partial-prefix match of "--"; do not stop.
    let (should_stop, _) = cond.should_stop("\n-");
    assert!(!should_stop);
    // Completing the marker keeps us going.
    let (should_stop, _) = cond.should_stop("- 2: next step");
    assert!(!should_stop);
    // Now a divergence after a newline.
    let (should_stop, _) = cond.should_stop("\nSELECT");
    assert!(should_stop);
}

#[test]
fn test_create_with_style_stochastic_block_stops_on_close() {
    use tabby_common::languages::get_language;
    let factory = StopConditionFactory::default();
    let lang = get_language("python"); // Block ("'''", "'''")
    let mut cond = factory.create_with_style("", Some(lang), "stochastic");
    let (should_stop, _) = cond.should_stop(" Parse input\n2: Validate\n3: Compute");
    assert!(!should_stop);
    let (should_stop, _) = cond.should_stop("'''");
    assert!(should_stop);
}
```

> Note: `sql` resolves to `CommentStyle::Block("/*", "*/")` per the existing test in `languages.rs`. If the partial-marker test must use a `CommentStyle::Line` language with a multi-char marker, substitute a language whose `line_comment` is multi-character (e.g., Lua `--` if that resolves to `Line`). Verify with `get_language("lua").comment_style()` and adjust the test language accordingly.

### 4.2 `crates/tabby/src/services/completion/completion_prompt.rs` — `tests` module

Add four new tests modeled on the existing `test_build_styled_*_pseudocode` tests:

```rust
#[test]
fn test_build_styled_dockerfile_stochastic() {
    let pb = create_prompt_builder(true);
    let seg = make_segment("FROM alpine\n".into(), Some("\n".into()));
    let styled = pb.build_styled("dockerfile", seg, &[], "stochastic");
    assert_eq!(styled.effective_style, "stochastic");
    assert!(styled.prompt.contains("\n# Steps (English):\n# 1:"));
    assert_eq!(
        styled.display_prefix.as_deref(),
        Some("\n# Steps (English):\n# 1:")
    );
    assert!(styled.display_suffix.is_none());
}

#[test]
fn test_build_styled_python_stochastic() {
    let pb = create_prompt_builder(true);
    let seg = make_segment("def fib(n):\n    ".into(), Some("\n".into()));
    let styled = pb.build_styled("python", seg, &[], "stochastic");
    assert_eq!(styled.effective_style, "stochastic");
    assert!(styled.prompt.contains("\n''' Steps (English):\n1:"));
    assert!(styled.prompt.contains("'''\n"));
    assert_eq!(
        styled.display_prefix.as_deref(),
        Some("\n''' Steps (English):\n1:")
    );
    assert_eq!(styled.display_suffix.as_deref(), Some("'''"));
}

#[test]
fn test_build_styled_html_stochastic() {
    let pb = create_prompt_builder(true);
    let seg = make_segment("<body>\n".into(), Some("</body>".into()));
    let styled = pb.build_styled("html", seg, &[], "stochastic");
    assert_eq!(styled.effective_style, "stochastic");
    assert!(styled.prompt.contains("\n<!-- Steps (English):\n1:"));
    assert_eq!(
        styled.display_prefix.as_deref(),
        Some("\n<!-- Steps (English):\n1:")
    );
    assert_eq!(styled.display_suffix.as_deref(), Some("-->"));
}

#[test]
fn test_build_styled_txt_stochastic_downgrades_to_code() {
    let pb = create_prompt_builder(true);
    let seg = make_segment("hello".into(), Some("\n".into()));
    let styled = pb.build_styled("txt", seg.clone(), &[], "stochastic");
    let plain = pb.build("txt", seg, &[]);
    assert_eq!(styled.effective_style, "code");
    assert_eq!(styled.prompt, plain);
    assert!(styled.display_prefix.is_none());
    assert!(styled.display_suffix.is_none());
}
```

**Acceptance:** `cargo test -p tabby-inference` and `cargo test -p tabby -- --skip golden services::completion` both pass.

---

## Phase 5 — LSP / agent surface

### 5.1 `clients/tabby-agent/src/protocol.ts` (line 220)

Extend the union:

```ts
style?: "code" | "hint" | "pseudocode" | "stochastic";
```

### 5.2 `clients/tabby-agent/src/config/type.d.ts`

- Lines 89–90: add `stochastic: string` to both the `replace` and `insert` shape definitions:
  ```ts
  replace: { code: string; hint: string; pseudocode: string; stochastic: string };
  insert: { code: string; hint: string; pseudocode: string; stochastic: string };
  ```
- Line 130: extend the `style` union to include `"stochastic"`.

### 5.3 `clients/tabby-agent/src/config/default.ts`

- Lines 9–10: add imports for the two new prompt MD files (created in Phase 6):
  ```ts
  import editCommandReplaceStochasticPrompt from "../chat/prompts/edit-command-replace-stochastic.md";
  import editCommandInsertStochasticPrompt from "../chat/prompts/edit-command-insert-stochastic.md";
  ```
- Lines 88–89: add the `stochastic` keys:
  ```ts
  replace: { code: ..., hint: ..., pseudocode: ..., stochastic: editCommandReplaceStochasticPrompt },
  insert: { code: ..., hint: ..., pseudocode: ..., stochastic: editCommandInsertStochasticPrompt },
  ```

### 5.4 `clients/tabby-agent/src/chat/inlineEdit.ts` (line 202)

Replace the variant chain:

```ts
const variant =
  style === "hint" ? "hint" :
  style === "pseudocode" ? "pseudocode" :
  style === "stochastic" ? "stochastic" :
  "code";
```

### 5.5 `clients/tabby-agent/src/codeCompletion/index.ts`

No code change needed — the two `generation_style: config.generation?.style ?? "code"` sites at lines 593 and 658 already pass through any string value.

**Acceptance:** `cd clients/tabby-agent && pnpm build` succeeds with no TS errors.

---

## Phase 6 — New chat prompt MD files

Create both files. Mirror the structure of the existing `edit-command-{insert,replace}-pseudocode.md` files; replace the pseudocode-specific instructions with numbered-step instructions.

### 6.1 `clients/tabby-agent/src/chat/prompts/edit-command-insert-stochastic.md`

The prompt should:

1. Explain that the output must be a numbered English step list, not real code.
2. Specify language-aware comment formatting:
   - Line-comment languages: every line begins with `{m} `; format is `{m} Steps (English):` then `{m} 1: …`, `{m} 2: …`, etc.
   - Block-comment languages: open the comment block once with `{s}`, write `Steps (English):` on the first line, then `1: …`, `2: …`, then close with `{e}`.
3. Limit output to 3–10 steps.
4. Forbid emitting executable code or explanations outside the comment.
5. Keep the existing `{{filepath}}`, `{{document}}`, `{{languageId}}`, `{{userCommand}}` template placeholders so the existing rendering pipeline works unchanged. Open one of the existing pseudocode prompts to copy the placeholder block exactly.

### 6.2 `clients/tabby-agent/src/chat/prompts/edit-command-replace-stochastic.md`

Same content as 6.1 but framed as "rewrite the selected code as a numbered step list." Mirror the structure of `edit-command-replace-pseudocode.md`.

**Acceptance:** files exist; `pnpm build` in `clients/tabby-agent` succeeds (the build picks up MD files via the existing import mechanism).

---

## Phase 7 — Chat-panel RPC

**Files:** `clients/tabby-chat-panel/src/client.ts` and `clients/tabby-chat-panel/src/server.ts`

- `client.ts` line 138: extend `getGenerationStyle?: () => Promise<'code' | 'hint' | 'pseudocode' | 'stochastic'>`. Update the JSDoc on line 136.
- `server.ts` line 92: extend `updateGenerationStyle: (style: 'code' | 'hint' | 'pseudocode' | 'stochastic') => Promise<void>`. Update the JSDoc on line 89.

**Acceptance:** `cd clients/tabby-chat-panel && pnpm build` succeeds.

---

## Phase 8 — Web UI chat (`ee/tabby-ui`)

### 8.1 Type unions

Extend every occurrence of `'code' | 'hint' | 'pseudocode'` to include `'stochastic'`:

| File | Line(s) |
|---|---|
| `app/chat/page.tsx` | 65, 180 |
| `components/chat/types.ts` | 60 |
| `components/chat/chat-context.ts` | 64 |
| `components/chat/chat.tsx` | 75 (signature of `buildStyleInstruction`) |

### 8.2 `buildStyleInstruction` (lines 74–98 of `components/chat/chat.tsx`)

Add a third branch before the final `return null`:

```ts
if (style === 'stochastic') {
  return (
    'When showing code, output ONLY a numbered English STEP LIST (1:, 2:, 3:, ...) ' +
    'describing what the code would do. No real code. ' +
    "Detect the target language's comment style:\n" +
    '- Line-comment languages: prefix every line (including the "Steps (English):" header) with the marker.\n' +
    '- Block-comment languages: open the comment on its own line, then "Steps (English):", then numbered items, then close the comment.\n' +
    'Limit to 3–10 steps. Do not emit executable code. Do not explain outside the comments.'
  )
}
```

### 8.3 `chat-panel.tsx` lines 266–271

No code change required — the existing label `Style: {generationStyle}` will render `Style: stochastic` correctly.

**Acceptance:** `cd ee/tabby-ui && pnpm build` succeeds with no TS errors.

---

## Phase 9 — VS Code extension

### 9.1 `clients/vscode/package.json` (lines 411–425)

Extend the `tabby.generationStyle` setting:

```json
"tabby.generationStyle": {
  "type": "string",
  "enum": ["code", "hint", "pseudocode", "stochastic"],
  "default": "code",
  "description": "Output style for Tabby code suggestions.",
  "enumDescriptions": [
    "Real code (default)",
    "Short natural-language hint wrapped in a language-native comment",
    "BEGIN..END pseudocode wrapped in a language-native comment",
    "Numbered natural-language steps wrapped in a language-native comment"
  ]
}
```

### 9.2 `clients/vscode/src/Config.ts` (line 85)

Extend the type union on the `generationStyle` getter:

```ts
get generationStyle(): "code" | "hint" | "pseudocode" | "stochastic" {
  return this.workspace.get("generationStyle", "code");
}
```

Also update the `updateGenerationStyle` signature on line ~89 to accept the wider union.

### 9.3 `clients/vscode/src/commands/index.ts` (lines 59–65)

Update the cycle:

```ts
cycleGenerationStyle: async () => {
  const current = this.config.generationStyle;
  const next =
    current === "code" ? "hint" :
    current === "hint" ? "pseudocode" :
    current === "pseudocode" ? "stochastic" :
    "code";
  await this.config.updateGenerationStyle(next);
  window.showInformationMessage(`Tabby generation style: ${next}`);
},
```

### 9.4 `clients/vscode/src/StatusBarItem.ts` (line 33)

Update the tooltip:

```ts
this.styleItem.tooltip = "Click to cycle Tabby generation style (code / hint / pseudocode / stochastic)";
```

### 9.5 `clients/vscode/src/commands/commandPalette.ts`

No code change — the line 108 label `Generation Style: ${this.config.generationStyle}` interpolates the value correctly.

**Acceptance:** `cd clients/vscode && pnpm build` succeeds; cycling through the status bar in a debug session shows all four labels.

---

## Phase 10 — Validation

Run in order from the repo root:

1. `make fix` — formats and lints all touched Rust.
2. `cargo test -p tabby-inference` — Phase 2 + 4.1 tests.
3. `cargo test -p tabby -- --skip golden` — Phase 4.2 tests + full server suite.
4. `pnpm install && pnpm build` — full TS graph (`tabby-agent`, `tabby-chat-panel`, `tabby-ui`, `vscode`).
5. `pnpm lint` — turbo lint across packages.
6. `pnpm test` — Mocha + Vitest across packages.
7. `ast-grep scan` — confirm no rule violations.
8. **Manual smoke test:**
   - Start the server: `cargo run serve --model TabbyML/StarCoder-1B`.
   - Launch the extension: `pnpm vscode:dev`.
   - In the debug VS Code, open a `.py` file (block-comment Python), trigger a completion with `stochastic` style — confirm ghost text begins `\n''' Steps (English):\n1:` and the response stops at `'''`.
   - Open a `Dockerfile` (line-comment), repeat — confirm ghost text begins `\n# Steps (English):\n# 1:` and stops the moment the model produces a non-`#` line.
   - Open a `.txt` file — confirm `stochastic` downgrades silently to `code`.
   - Cycle through styles using the status-bar button: confirm the order is `code → hint → pseudocode → stochastic → code`.
   - Open the chat side panel and confirm it reflects the new style without errors.

---

## Non-changes (intentional)

- No GraphQL schema change → `make update-graphql-schema` not required.
- No DB schema change → `make update-db-schema` not required.
- `crates/tabby/src/services/completion.rs` `wrap_with_style` reconstruction logic (other than the placeholder) is unchanged: it already reads `display_prefix` / `display_suffix` from the `StyledPrompt`, so no per-style branching is needed there.
- `clients/tabby-agent/src/codeCompletion/index.ts` already forwards the raw style string; no per-style code path.

---

## Risk register

| Risk | Mitigation |
|---|---|
| `forward_text` accumulator grows unbounded for long completions. | It's only populated when `stochastic_line_marker` is `Some`; max length is bounded by completion `max_tokens`. Acceptable. |
| Multi-character line markers (e.g., Lua `--`) interact subtly with the partial-prefix logic. | Phase 2.3 explicitly handles the `after.len() < marker.len()` partial-match case. Phase 4.1 includes a regression test. |
| Model emits a leading `\n` token before any comment text. | `check_stochastic_line_end` returns `None` when `after.is_empty()`; we wait for the next char. The default trie's `\n\n` stop word still covers the consecutive-newline degenerate case. |
| Existing styles regress because of `StopCondition` field additions. | Default values for the new fields preserve existing behavior; existing tests in `decoding.rs` must continue to pass without modification. |
| MD prompt files not bundled into the agent binary. | The default config imports them by relative path; the existing build pipeline (matching how `edit-command-insert-pseudocode.md` is bundled) will pick them up. Verify `pnpm build` succeeds in `clients/tabby-agent`. |
