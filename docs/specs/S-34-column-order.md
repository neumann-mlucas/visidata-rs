# S-34 — Column order: `H` `L` `gH` `gL`, sorting the columns sheet

Status: **shipped 2026-11-12** (branch `feature/column-order`); replay `aggregators-set` passes. Depends on: S-25 P3 (columns
sheet), S-31 C (copies keep column attributes), S-29 (sessions).

Today a sheet shows its columns in plan order, with the key columns first. Nothing moves a column,
so the only way to rearrange a wide sheet is `:sql SELECT …`, which makes a read-only copy.

## 1. VisiData reference

`visidata/features/slide.py`:

| Keys | Longname | Does |
|---|---|---|
| `H` / `L` | `slide-left` / `slide-right` | move the current column one position; a key column moves among the key columns |
| `gH` / `gL` | `slide-leftmost` / `slide-rightmost` | to the first / last position of its section (keys, or the rest) |
| `zH` / `zL` | `slide-left-n` / `slide-right-n` | N positions, asked for (`slide col left n=`) |
| `J` `K` `gJ` `gK` `zJ` `zK` | `slide-down` … | the same for rows |

Moves are undoable and mark the sheet modified. The columns sheet's rows *are* the source's column
list, so sorting it (`[`, `]`) or sliding its rows reorders the source's columns. On a plot
(`TextCanvas`/`Canvas`) `H` `J` `K` `L` have their own meanings, as they do in vdrs today.

Corpus: `aggregators-set` (`[` on a freq sheet's columns sheet) is the only replay case this
unblocks. The other slide users already pass (`append`) or are blocked by something else. The gain
is daily use, not the replay count.

## 2. Design

Two ways to store the order:

- **A. A plan step** (`PlanStep::Select` with the new order). It would be exported and undone
  like other steps, but every reorder would deepen the plan (S-24's stack depth), and it would
  shuffle the positions that `insert_edit` uses to place edits after their column's last writer.
- **B. A view attribute**, like `hidden` and `keys`: `View.order: Vec<String>`, the non-key
  columns in display order.

**Chosen: B.** Order is how the sheet looks, not what it computes, as with hidden and key columns.
`view_plan` already projects (keys first, then the rest, dropping hidden). It applies `order`
there, in one place, so everything downstream follows with no further change:

- rendering and the cursor,
- `:export` (`visible_columns` → `.select([...])` / `SELECT …`),
- `vd.sheet().column_names`,
- copies (S-31 `inherit_attrs`, filtered to the kept columns).

Projection rule: keys first, in `keys` order. Then the columns named in `order`, in that order.
Then every other column (new ones from `=`, `i`, `za`, a join) in plan order, which keeps S-31's "new
columns go last". A hidden column keeps its place in `order` and comes back there when unhidden.

## 3. Behaviour

- **A1 — `H` `L` `gH` `gL` `zH` `zL`** as in VisiData, on every sheet that isn't a plot.
  - On a key column they reorder `keys`.
  - On any other column they rewrite `order`, starting from the current display order.
  - The cursor stays on the moved column.
  - Each move is one undo step (the view is in the snapshot already).
  - Moving past an edge is a no-op with no toast, as in VisiData.
- **A2 — plots keep `H` `J` `K` `L`.** Today these keys are bound to the plot's box resize (a
  no-op elsewhere). They become slide actions, and the dispatch turns them back into box resizes
  on a plot sheet, as VisiData's canvas does.
- **A3 — rows don't slide.** `J` `K` `gJ` `gK` stay unbound on data sheets: vdrs rows follow the
  plan's order (sort, file), and a manual row order would need its own plan step and export
  support. Out of scope; revisit if someone asks.
- **A4 — the columns sheet.**
  - `[` / `]` sort it and then write its row order back to the source as the source's `order`
    (key columns keep their place), as one undo step on the source.
  - Sorting by `type` groups the columns by type; by `name` sorts them A–Z.
  - `H`/`L` on the columns sheet move the current *column of the columns sheet*, as on any sheet.
- **A5 — the vim profile.** `H` `L` stay `screen_top` / `screen_bottom`. The slides are there as
  ex commands (`:slide-left`, …, every action name is one) and `vd.bind`.

## 4. Sessions and export

- **`:export`**: no change beyond `visible_columns` returning the list when `order` is set.
- **Sessions** need an absolute form, because replaying relative slides breaks once columns are
  added. **Proposed: a new ex command `:order col…`**. It sets the order of the columns it names;
  the others follow in their current order. `:mksession` writes it after the steps.
  - This is a new public command, so the name needs your OK. The alternative is to extend `:sort`;
    that's rejected because `:sort` already means sorting rows.

## 5. Renames, drops, copies

- `rename_col` renames the entry in `order` (as it does `precision`, S-32).
- A column that leaves the plan drops out of the projection silently. `order` keeps the name,
  harmless, as `hidden` does today.
- Copies (`"`, `g"`, dedupe, freq dive) inherit `order` for the columns they keep.

## 6. Test plan

- **Scenario tests:**
  - `H L gH gL` on plain and key columns: the screen and the cursor follow the move, `U` undoes it.
  - `zH 2`.
  - A plot still resizes its box with `H`.
  - `[` on `C` reorders the source; `U` on the source undoes it.
- **Golden case:** a slide, then `:export`. The Polars and DuckDB scripts give the reordered columns
  (`ordered` not needed: column order is checked by the header).
- **Session round trip:** a reordered sheet saved with `:mksession` and replayed gives the same
  order. The existing every-case round-trip test covers it once a golden case has a slide.
- **Replay:** translate `slide-left` / `slide-right` / `-leftmost` / `-rightmost` longnames;
  `aggregators-set` comes out of quarantine; `MIN_PASS` +1.
- **Guide:** the `H`/`L` keys rows (the guide test enforces every default key), plus a sentence in
  "Columns & types" and the columns-sheet paragraph.

## 7. Size

About 1.5 days:

- `View.order` and the projection: 0.25 d.
- The keys, the vim and plot interplay: 0.25 d.
- Columns-sheet write-back: 0.25 d.
- `:order` and sessions: 0.25 d.
- Tests and replay: 0.5 d.

## 8. Open questions

1. The `:order` name (§4).
2. Should `gH`/`gL` on a non-key column cross into the key section? VisiData: no (stays in its
   section). Proposed: same.
