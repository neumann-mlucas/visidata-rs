//! S-57 §3: the layout functions keep columns and the cursor on screen, for
//! any widths (up to `u16::MAX`) and any area.

use proptest::prelude::*;
use unicode_width::UnicodeWidthStr;

use super::*;
use crate::sheet::View;

const PAD: u16 = SheetWidget::CELL_PAD;

/// Column widths: narrow, wide, and at the `u16` edge (`_` on a huge cell).
fn widths() -> impl Strategy<Value = Vec<u16>> {
    let w = prop_oneof![0..12u16, 0..300u16, (u16::MAX - 3)..=u16::MAX];
    proptest::collection::vec(w, 0..30)
}

/// An area from 1×1 up, at an offset that keeps its right edge in `u16`.
fn area() -> impl Strategy<Value = Rect> {
    (0..300u16, 0..40u16, 1..=400u16, 1..=100u16).prop_map(|(x, y, width, height)| Rect {
        x,
        y,
        width,
        height,
    })
}

/// Cursor, scroll and frozen count, some past the end of the sheet.
fn view(cols: usize, rows: usize) -> impl Strategy<Value = View> {
    (
        0..cols + 3,
        0..cols + 3,
        0..cols + 3,
        0..rows + 3,
        0..rows + 3,
    )
        .prop_map(
            |(frozen_left, col_offset, col_cursor, row_cursor, row_offset)| {
                let mut v = View::default();
                v.frozen_left = frozen_left;
                v.col_offset = col_offset;
                v.col_cursor = col_cursor;
                v.row_cursor = row_cursor;
                v.row_offset = row_offset;
                v
            },
        )
}

/// Widths, an area and a view. Half the areas end exactly where the
/// columns from the view's first scrolled one do, so the edge cases of
/// "fits" against "passes the edge" come up.
fn layout() -> impl Strategy<Value = (Vec<u16>, Rect, View)> {
    (widths(), area())
        .prop_flat_map(|(w, a)| {
            let n = w.len();
            (Just(w), Just(a), view(n, 0), 0..n + 1, any::<bool>())
        })
        .prop_map(|(w, mut a, v, k, exact)| {
            let start = (v.frozen_left.min(w.len()) + v.col_offset).min(w.len());
            let span: usize = (0..v.frozen_left.min(w.len()))
                .chain(start..k.max(start))
                .map(|i| usize::from(w[i]) + usize::from(PAD))
                .sum();
            if exact && (1..=400).contains(&span) {
                a.width = span as u16;
            }
            (w, a, v)
        })
}

/// A frame of `cols` integer columns and `rows` rows.
fn frame(cols: usize, rows: usize) -> DataFrame {
    let columns: Vec<Column> = (0..cols)
        .map(|i| Column::new(format!("c{i}").into(), vec![0i64; rows]))
        .collect();
    DataFrame::new(rows, columns).unwrap()
}

/// Cell text: empty, wide, combining, control, very long.
fn cell() -> impl Strategy<Value = String> {
    prop_oneof![
        proptest::sample::select(
            &[
                "",
                "x",
                "日本語テキスト",
                "👍🏽",
                "e\u{301}",
                "a\tb",
                "line\nbreak",
                "\u{200b}"
            ][..]
        )
        .prop_map(str::to_string),
        "\\PC{0,20}",
        Just("w".repeat(70_000)),
    ]
}

proptest! {
    /// §3 (1): the kept characters are a prefix that fits, and the next
    /// character would not fit.
    #[test]
    fn take_width_keeps_the_longest_prefix_that_fits(s in "\\PC{0,30}", max in 0..40usize) {
        let kept: String = take_width(s.chars(), max).collect();
        prop_assert!(s.starts_with(&kept));
        prop_assert!(kept.width() <= max);
        if let Some(next) = s[kept.len()..].chars().next() {
            prop_assert!(kept.width() + next.width().unwrap_or(0) > max);
        }
    }

    /// §3 (2): frozen columns, then scrolled ones from `col_offset`, each
    /// where the one before ends; only the last scrolled one passes the edge.
    #[test]
    fn visible_columns_follow_each_other((w, a, v) in layout()) {
        let vis = visible_columns(&v, &w, a);
        let n = w.len();
        let frozen = v.frozen_left.min(n);
        let order: Vec<usize> = (0..frozen).chain((frozen + v.col_offset).min(n)..n).collect();
        let got: Vec<usize> = vis.iter().map(|(i, _)| *i).collect();
        prop_assert!(order.starts_with(&got), "{got:?} not a prefix of {order:?}");
        let edge = a.x + a.width;
        let end = |(i, x): (usize, u16)| x.saturating_add(w[i]).saturating_add(PAD);
        let mut at = a.x;
        for (k, &(i, x)) in vis.iter().enumerate() {
            prop_assert_eq!(x, at, "column {} starts off its slot", i);
            at = end((i, x));
            if i >= frozen && k + 1 < vis.len() {
                prop_assert!(at < edge, "scrolled column {} ends past the edge but more follow", i);
            }
        }
        if got.len() < order.len() && got.last().is_some_and(|i| *i >= frozen) {
            prop_assert!(at >= edge, "stopped at column {:?} with room left", got.last());
        }
    }

    /// §3 (3): a click at a column's start picks that column.
    #[test]
    fn column_at_finds_each_visible_column((w, a, v) in layout()) {
        for (i, x) in visible_columns(&v, &w, a) {
            if x < a.x + a.width {
                prop_assert_eq!(column_at(&v, &w, x - a.x), Some(i), "x {}", x - a.x);
            }
        }
    }

    /// §3 (4): after `clamp_view` the cursor is in range and on screen, and a
    /// second clamp changes nothing.
    #[test]
    fn clamp_view_keeps_the_cursor_on_screen(
        (w, a, mut v, rows) in (widths(), area(), 0..60usize).prop_flat_map(|(w, a, rows)| {
            let n = w.len();
            (Just(w), Just(a), view(n, rows), Just(rows))
        })
    ) {
        let df = frame(w.len(), rows);
        clamp_view(&mut v, &df, a, &w);
        let data_rows = (a.height as usize).saturating_sub(1).max(1);
        if rows > 0 {
            prop_assert!(v.row_cursor < rows);
            prop_assert!(v.row_offset <= v.row_cursor && v.row_cursor < v.row_offset + data_rows);
        }
        if !w.is_empty() {
            prop_assert!(v.col_cursor < w.len());
            let vis = visible_columns(&v, &w, a);
            let at = vis.iter().find(|(i, _)| *i == v.col_cursor);
            prop_assert!(at.is_some(), "cursor {} not in {:?}", v.col_cursor, vis);
            let frozen = v.frozen_left.min(w.len());
            let span = |ws: &[u16]| ws.iter().map(|w| usize::from(*w) + usize::from(PAD)).sum::<usize>();
            let need = span(&w[..frozen])
                + if v.col_cursor >= frozen { span(&w[v.col_cursor..=v.col_cursor]) } else { 0 };
            if need <= a.width as usize {
                let (i, x) = *at.unwrap();
                let end = usize::from(x) + usize::from(w[i]) + usize::from(PAD);
                prop_assert!(end <= usize::from(a.x + a.width), "cursor column ends at {end}");
            }
        }
        let once = (v.row_cursor, v.row_offset, v.col_cursor, v.col_offset);
        clamp_view(&mut v, &df, a, &w);
        prop_assert_eq!(once, (v.row_cursor, v.row_offset, v.col_cursor, v.col_offset));
    }

    /// §3 (5): drawing any cells in any area, as the app does after a clamp,
    /// never panics.
    #[test]
    fn sheet_widget_draws_any_cells(
        cells in proptest::collection::vec(proptest::collection::vec(cell(), 1..5), 0..6),
        a in area(),
        set_widths in proptest::option::of(widths()),
        (frozen_left, col_cursor, row_cursor) in (0..6usize, 0..6usize, 0..8usize),
    ) {
        let cols = cells.first().map_or(1, Vec::len);
        let columns: Vec<Column> = (0..cols)
            .map(|c| {
                let v: Vec<String> = cells.iter().map(|r| r.get(c).cloned().unwrap_or_default()).collect();
                Column::new(format!("c{c}").into(), v)
            })
            .collect();
        let df = DataFrame::new(cells.len(), columns).unwrap();
        let mut sheet = Sheet::from_source(0, csv_source());
        sheet.set_window(df.clone());
        let theme = Theme::dark();
        refresh_widths(&mut sheet, None, &theme);
        if let Some(w) = set_widths.filter(|w| w.len() == cols) {
            sheet.state.widths = w;
        }
        sheet.view.frozen_left = frozen_left;
        sheet.view.col_cursor = col_cursor;
        sheet.view.row_cursor = row_cursor;
        let widths = sheet.state.widths.clone();
        clamp_view(&mut sheet.view, &df, a, &widths);
        let mut buf = Buffer::empty(a);
        SheetWidget {
            sheet: &sheet,
            theme: &theme,
            render: None,
            noted: None,
            no_color: false,
            visual: None,
        }
        .render(a, &mut buf);
    }
}

/// A one-cell CSV source to build a sheet on; tests then set its window.
fn csv_source() -> crate::reader::LoadedSource {
    use std::io::Write;
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    writeln!(tmp, "c0\n1").unwrap();
    tmp.flush().unwrap();
    crate::reader::open(tmp.path().as_os_str(), Some(crate::reader::SourceKind::Csv)).unwrap()
}
