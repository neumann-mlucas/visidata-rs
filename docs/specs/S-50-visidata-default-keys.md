# S-50 VisiData default keys

Status: shipped 2026-11-16.

VisiData default keys vdrs left unbound (`docs/visidata-parity.md` §2.1), each
checked against `../visidata` `addCommand`.

| Key | VisiData | vdrs |
|---|---|---|
| `o` | open-file | ex line holding `open ` |
| `Ctrl+S` | save-sheet | ex line holding `save ` (VisiData prefills the sheet name) |
| `&` | join-sheets-top2 | ex line holding `join <sheet below> `; keys and type typed (VisiData asks only the type) |
| `M` | melt | `Unpivot` of the visible non-key columns, keys kept |
| `{` `}` | go-prev/next-selected | `GotoSelected`, view order (`rids_to_selected`), background job |
| `<` `>` | go-prev/next-value | `ValueChange` (was vim-profile only) |
| `z<` `z>` | go-prev/next-null | `NullChange`: loaded rows, as `ValueChange` |
| `c` | go-col-regex | prompt; next visible column after the cursor whose name matches, wrapping |
| `g[` `g]` | sort-keys-asc/desc | `SortKeys`: the key columns replace the sort |

- A lone `<` parses as a key now (it started a `<name>`).
- The vim profile keeps its `M`, `{` `}`, `c`; its comments name the defaults.
- Not here: `T` transpose, `gF` freq-keys (freq over several columns), `zEnter`
  open-cell, `g&` join-all. Each is a feature, not a binding.
- Replay: the corpus runner already handles melt and sort-keys; motions don't
  change output, so `MIN_PASS` stays.

## Tests

- Scenario `visidata_motion_sort_and_prompt_keys`: every key on the real key path.
- Guide sync tests (key rows, action names).
