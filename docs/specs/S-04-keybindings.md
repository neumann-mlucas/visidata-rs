# S-04 — Keybinding model

Status: **draft** (unapproved)
Phase: 0
Depends on: S-02 (`Action` enum, `App` state)
Research: [`../research/tui-patterns.md`](../research/tui-patterns.md) Q3
Related prep: extract VisiData default binding table (grunt, not spec)

## Problem

Three constraints, two of them non-standard:

1. **Must feel like VisiData from keystroke 1** (direction §9.2) — the binding *values* matter, not just the shape.
2. **Must be extensible** — user call. Config file + runtime Lua additions, both.
3. **Must support vim count prefix** (`3j`, `10gg`). No surveyed Rust TUI does this; we need it because VisiData does.

Plus the usual: chord/prefix maps (`g`, `z`, `gz`, `Alt-`), which-key help, `.` repeat.

## Decision

**Yazi's keymap shape + atuin's layered maps + count prefix we add ourselves.** Linear-vec of chords per layer, first-key match → fire or open which-key overlay. TOML config with Lua mutation API. Count prefix is one `Option<usize>` on `App`. `.` repeat captures the resolved `Action`, not keystrokes.

## Shape

### Config

```toml
# ~/.config/visidata-rs/keymap.toml (and defaults ship as a baked-in TOML string)

[[sheet.keymap]]
on = "j"
run = "cursor_down"
desc = "move cursor down"

[[sheet.keymap]]
on = ["g", "g"]
run = "goto_top"
desc = "jump to first row"

[[sheet.keymap]]
on = ["z", "<Left>"]
run = "freeze_column"
desc = "freeze current column"

[[sheet.keymap]]
on = "<C-s>"
run = "save_sheet"
desc = "save sheet to file"
```

- `on` → single `Key` string or `Vec<Key>` for chord.
- `run` → `Action` name (snake_case, matches `#[serde(rename_all)]` in S-02). Multi-action via `run = ["a", "b"]` fires in sequence.
- `desc` → which-key overlay text.
- `for = "linux"` optional platform guard (yazi feature; cheap to carry).
- `[[sheet.keymap]]`, `[[input.keymap]]`, `[[pick.keymap]]`, `[[confirm.keymap]]`, `[[help.keymap]]`, `[[which.keymap]]` → one layer per modal context. Layers are stacked by the active overlay (S-02 `Overlay` enum); only the top layer dispatches.

### Key type

```rust
pub struct Key {
    pub code: KeyCode,     // Char(c) | F(n) | Enter | Esc | ... (crossterm-shaped)
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl FromStr for Key {
    // Parses: "a", "A", "<C-a>", "<S-F1>", "<Esc>", "<CR>", "<Space>", "<Left>", "<A-x>"
}
```

Modifier normalization: `<S-a>` and `A` both map to `{Char('A'), shift:true}`. Printable shifted (`!`, `)`) also normalized to shift-less `{Char('!')}`. Mirrors yazi `key.rs`.

### Chord resolution

```rust
pub struct Keymap {
    layers: HashMap<Layer, Vec<Chord>>,       // insertion order preserved per layer
}

pub struct Chord {
    on: Vec<Key>,
    run: Vec<Action>,
    desc: String,
    r#for: Option<Platform>,
}

pub enum Dispatch {
    Fire(Vec<Action>),         // all keys matched, actions ready
    Pending(Vec<ChordRef>),    // prefix match, show which-key overlay
    Miss,                      // no chord starts with this
}

impl Keymap {
    fn resolve(&self, layer: Layer, pending: &[Key], next: Key) -> Dispatch { /* ... */ }
}
```

Linear scan per layer. Chord layers rarely exceed 200 entries — trie unnecessary. Equality on `on` alone (yazi trick): a user re-declaring `on = "j"` shadows the default. Order: last declaration wins.

### Count prefix

```rust
// in App:
pending_count: Option<usize>,

// handle_event flow:
// 1. If pending_prefix is non-empty, append key to prefix buffer.
// 2. Else if key is digit 0-9 AND no pending prefix AND digits aren't bound as leaf
//    (e.g. "0" is bound to goto_leftmost in VisiData — need lookup), accumulate into pending_count.
// 3. Else resolve chord; on Fire, dispatch Action with count arg if the action supports it.
```

`Action` variants that accept counts carry an `Option<usize>` (defaulted to 1):

```rust
Action::CursorDown { count: Option<usize> },
Action::ScrollPage { count: Option<usize> },
```

Non-counting actions ignore `pending_count` and clear it. Counting actions take `count.unwrap_or(1)`.

**Digit-vs-command collision.** VisiData binds `0` to "move to first column." Our resolution rule: a digit is a count only if (a) no chord starts with that exact digit as a single-key leaf in the current layer, or (b) there's already a non-zero `pending_count` accumulating (so `30j` works even if `3` were bound). This gives us `3j = CursorDown{count:3}` and `0 = goto_leftmost` on an empty count buffer. Rules are a bit fiddly; S-04 locks the rules, the test suite guards them.

### `.` repeat

```rust
// in App:
last_action: Option<Vec<Action>>,  // the fully resolved sequence from the previous Fire

// Binding:
// on = "." run = "repeat_last_action"
```

Capture is post-chord, post-count — if the user types `3j`, `last_action = [CursorDown{count:Some(3)}]`. `.` re-fires it. Keymap changes don't invalidate `.`; the resolved action is immune to rebinding of `j`. Deliberate.

Not every action is repeatable — `Quit`, `PushSheet`, `Redraw` are excluded. `impl Action { fn repeatable(&self) -> bool }`, defaults to `true`, named excludes.

### Runtime extension (Phase 3 hook)

```lua
-- init.lua
vd.bind("sheet", "<C-f>", "find", { desc = "find in column" })
vd.bind("sheet", { "g", "f" }, "find_global", { desc = "find across sheet" })
vd.unbind("sheet", "x")
```

mlua function pushes a `Chord` into the right layer vec. Order rule means late Lua binds shadow defaults, matching Neovim's `:map` ergonomics. API lives in S-10; S-04 reserves the shape.

### Which-key overlay

When `resolve()` returns `Pending`, open an `Overlay::Which` with the candidate chords. Timeout: 1000 ms (`timeout_len`, config-overridable like Neovim) — on timeout, dispatch the longest matching prefix as a `Miss` (standard vim behavior).

## Why not <alternative>

- **Flat hashmap per mode (television, bottom).** No chords. Breaks VisiData's `g`/`z` prefix contract immediately.
- **Fixed struct of fields (gitui).** 89 named fields that live in Rust forever. Zero config dynamism, zero Lua extensibility — kills constraint #2.
- **Nested hashmap by mode (zellij).** Clean but needs mode switches modeled as explicit `SwitchToMode(Prefix)` actions. More machinery than yazi's linear vec for the same expressiveness.
- **Trie over keys.** Correct, overkill. Keymap size doesn't justify. Revisit if a layer exceeds 500 entries.
- **Count prefix as a separate "normal mode" (vim-strict).** Clean but means two state machines (counts, chords). One `pending_count` + resolution rule is simpler and covers the use case.
- **Capturing `.` repeat as keystrokes.** Fragile under rebinding. Action-resolved is correct.

## Open sub-decisions

- Timeout length (which-key wait before dispatch-prefix): default 1000 ms, config-overridable `timeout_len`.
- Prefix match showing a bare `Which` overlay vs inline hint: start with full overlay (yazi), hint-mode later.
- `0` as both "first column" and "count prefix component": documented rule above; needs a test case in the keymap test suite.
- VisiData default binding extraction: a prep task before Phase 0 ships — grep `addCommand(` across `../visidata/visidata/`, build a TOML table. Not in-spec; tracked in `tasks.md`.
- Multiple-action binding (`run = ["cursor_down", "cursor_down"]`): supported syntactically; `pending_count` applies to the whole sequence or just the first? Lock: applies only to the first action, matching Neovim `3dd`.

## Verification before leaving this spec

- Can a Lua plugin add a chord binding at runtime? Yes — `vd.bind` appends to the layer vec; late declarations shadow earlier.
- Can a user rebind a chord without recompiling? Yes — TOML config in `~/.config/visidata-rs/keymap.toml`.
- Does `.` survive a `:nmap` rebind of the bound key? Yes — captures the resolved `Action`.
- Does `3j` move the cursor down 3 rows? Yes — `pending_count` accumulates, `Action::CursorDown` reads it.
- Is `g0g0g0` ambiguous? No — `0` after `g` is part of the chord, not a count. Rule: digits only accumulate into count at the start of a chord buffer.

Prep tasks before any code lands:

- [ ] Dump VisiData default bindings from `../visidata/visidata/**/*.py` (`addCommand\(` grep) into a canonical TOML. ~400-500 entries expected. Phase 0 only ships the subset reachable from sheet navigation + Phase 0 features.
- [ ] Lock the "digit-vs-command" test matrix: at minimum `0`, `30j`, `3`, `3<Esc>`, `g0`, `03j`.
