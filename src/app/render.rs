//! Drawing the screen: title bar, sheet, status line, sidebar, panes, which-key popup, pivot preview.

use super::*;
use crate::plugins::Bar;

impl App {
    pub(super) fn draw<B: ratatui::backend::Backend>(
        &mut self,
        term: &mut Terminal<B>,
    ) -> Result<()>
    where
        B::Error: Send + Sync + 'static,
    {
        let s = self.sheets.active();
        let working = !s.is_loaded() || s.is_counting() || !self.busy.is_empty();
        self.working_since = working.then(|| self.working_since.unwrap_or_else(Instant::now));
        self.drawn_spin = self.spin();
        self.refresh_sidebar_stats();
        // A `vd.statusline` that just failed: draw again with the built-in
        // bar and the reason on it.
        for _ in 0..2 {
            term.draw(|f| {
                self.render(f);
                crate::theme::downgrade(f.buffer_mut(), self.color, &self.theme);
            })?;
            match self.plugins.as_ref().and_then(|p| p.take_bar_error()) {
                Some(e) => self.toast = Some(e),
                None => break,
            }
        }
        Ok(())
    }

    /// Spinner frame once background work has lasted long enough to mention.
    pub(super) fn spin(&self) -> Option<usize> {
        let ms = self.working_since?.elapsed().as_millis();
        (ms >= SPIN_AFTER_MS).then_some((ms / 100) as usize % SPINNER.len())
    }

    /// `&mut` only to clamp the view to the sheet area measured here.
    pub(super) fn render(&mut self, f: &mut ratatui::Frame<'_>) {
        let area = f.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(area);
        let main_area = chunks[1];
        let preview = self
            .input
            .is_some()
            .then_some(())
            .and(self.pivot_preview.as_ref());
        let (sheet_area, plan_area) = if self.sidebar.is_none() && preview.is_none() {
            (main_area, None)
        } else {
            let halves = Layout::default()
                .direction(Direction::Horizontal)
                .constraints(if preview.is_some() {
                    [Constraint::Percentage(50), Constraint::Percentage(50)]
                } else {
                    [Constraint::Percentage(65), Constraint::Percentage(35)]
                })
                .split(main_area);
            (halves[0], Some(halves[1]))
        };
        // `Z` (S-23 B): two panes, each a title row over its table; the
        // focused one shows the active sheet.
        if self
            .split
            .as_ref()
            .is_some_and(|sp| !self.sheets.iter().any(|s| s.id == sp.other))
        {
            self.split = None; // its sheet was closed
        }
        let (title_area, sheet_area, other) = match &self.split {
            None => (chunks[0], sheet_area, None),
            Some(sp) => {
                let region = Rect {
                    y: chunks[0].y,
                    height: sheet_area.height + 1,
                    ..sheet_area
                };
                let halves = Layout::default()
                    .direction(match sp.vertical {
                        true => Direction::Horizontal,
                        false => Direction::Vertical,
                    })
                    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(region);
                let (mine, theirs) = match sp.focus_first {
                    true => (halves[0], halves[1]),
                    false => (halves[1], halves[0]),
                };
                let rows = |r: Rect| {
                    let t = Rect { height: 1, ..r };
                    let b = Rect {
                        y: r.y + 1,
                        height: r.height.saturating_sub(1),
                        ..r
                    };
                    (t, b)
                };
                let (t, b) = rows(mine);
                (t, b, Some((rows(theirs), sp.other)))
            }
        };
        // `:footer` (S-36 C): the table's last row holds the totals
        let (sheet_area, footer_area) = match self.footer_key().is_some() && sheet_area.height > 2 {
            true => (
                Rect {
                    height: sheet_area.height - 1,
                    ..sheet_area
                },
                Some(Rect {
                    y: sheet_area.y + sheet_area.height - 1,
                    height: 1,
                    ..sheet_area
                }),
            ),
            false => (sheet_area, None),
        };
        self.viewport_rows = (sheet_area.height as usize).saturating_sub(1).max(1);
        self.viewport_cols = sheet_area.width;
        self.sheet_area = sheet_area;
        // ponytail: the notes file is read here on first sight, on the UI
        // thread; it's small. Move to a job if notes files grow.
        let _ = self.notes_mut();
        let lua_render = self.plugins.as_ref().filter(|p| p.has_renderers());
        let render_cell =
            |c: &str, d: &DataType, v: &AnyValue| lua_render.and_then(|p| p.render(c, d, v));
        let render = lua_render.map(|_| &render_cell as crate::widget::CellRenderer);
        let s = self.sheets.active_mut();
        refresh_widths(s, render, &self.theme);
        // A stale window may be shorter than the view being loaded (rows
        // just added): clamping to it would lose the cursor.
        if let Some(df) = s.window().filter(|_| s.is_loaded()).cloned() {
            clamp_view(&mut s.view, &df, sheet_area, &s.state.widths);
        }

        if let Some(((title, table), id)) = other
            && let Some(s) = self.sheets.find_by_id_mut(id)
        {
            refresh_widths(s, render, &self.theme);
            if let Some(df) = s.window().filter(|_| s.is_loaded()).cloned() {
                clamp_view(&mut s.view, &df, table, &s.state.widths);
            }
            let Some(s) = self.sheets.iter().find(|s| s.id == id) else {
                return;
            };
            self.render_titlebar(f, title, s, 2);
            self.draw_sheet(f, table, s, render, None);
            // the unfocused pane is dim
            f.buffer_mut().set_style(
                Rect {
                    height: table.height + 1,
                    ..title
                },
                Style::default().add_modifier(Modifier::DIM),
            );
        }
        self.render_titlebar(f, title_area, self.sheets.active(), 1);
        let s = self.sheets.active();
        let notes = self
            .notes_path()
            .and_then(|p| self.notes.get(&p))
            .filter(|n| !n.notes.is_empty());
        // a row's key text once per frame, not once per cell
        let keys_of = std::cell::RefCell::new(std::collections::HashMap::new());
        let noted = |ri: usize, column: &str| {
            notes.is_some_and(|n| {
                let rid = s.window_rids().get(ri).copied();
                let mut cache = keys_of.borrow_mut();
                let keys = cache.entry(ri).or_insert_with(|| {
                    s.window()
                        .filter(|_| !s.view.keys.is_empty())
                        .map(|df| crate::sheet::key_text(df, &s.view.keys, ri))
                });
                n.get(rid, keys.as_deref(), column).is_some()
            })
        };
        let noted = notes.map(|_| &noted as crate::widget::Noted);
        self.draw_sheet(f, sheet_area, s, render, noted);
        if let (Some(area), Some(df)) = (footer_area, s.window()) {
            // last totals while new ones are on their way, dim
            let fresh = self.current_footer();
            let mine = format!("{}|", s.id);
            let stale = self.footer.as_ref().filter(|(k, _)| k.starts_with(&mine));
            let shown = fresh.or(stale.map(|(_, f)| f));
            let (rows, totals) = shown.map_or((0, &[][..]), |(n, t)| (*n, t.as_slice()));
            crate::widget::paint_footer(
                f.buffer_mut(),
                area,
                &s.view,
                df,
                &s.state.widths,
                &self.theme,
                (rows, totals),
            );
            if fresh.is_none() {
                f.buffer_mut()
                    .set_style(area, Style::default().add_modifier(Modifier::DIM));
            }
        }
        // Rows from the old window, while a slow new one is on its way.
        let s = self.sheets.active();
        if !s.is_loaded() && self.spin().is_some() {
            f.buffer_mut()
                .set_style(sheet_area, Style::default().add_modifier(Modifier::DIM));
        }
        if let Some(area) = plan_area {
            match self.pivot_preview.as_ref().filter(|_| self.input.is_some()) {
                Some(p) => self.render_pivot_preview(f, area, p),
                None => self.render_sidebar(f, area),
            }
        }

        self.render_statusbar(f, chunks[2]);
        if !self.pending_candidates.is_empty() {
            self.render_which_key(f, main_area);
        }
    }

    /// A sheet's body: a table, or a chart for plot and bar sheets (S-23 C).
    fn draw_sheet(
        &self,
        f: &mut ratatui::Frame<'_>,
        area: Rect,
        sheet: &Sheet,
        render: Option<crate::widget::CellRenderer>,
        noted: Option<crate::widget::Noted>,
    ) {
        let theme = &self.theme;
        match sheet.origin {
            Origin::Plot { .. } => {
                f.render_widget(crate::widget::PlotWidget { sheet, theme }, area)
            }
            Origin::Bars { .. } => {
                f.render_widget(crate::widget::BarsWidget { sheet, theme }, area)
            }
            _ => f.render_widget(
                SheetWidget {
                    sheet,
                    theme,
                    render,
                    noted,
                    no_color: self.color == crate::theme::ColorDepth::None,
                    visual: self.visual_on(sheet),
                },
                area,
            ),
        }
    }

    pub(super) fn render_pivot_preview(
        &self,
        f: &mut ratatui::Frame<'_>,
        area: Rect,
        p: &PivotPreview,
    ) {
        let title = match &p.body {
            Ok(_) => format!(" pivot preview: first {PIVOT_PREVIEW_ROWS} rows (Enter = all) "),
            Err(_) => " pivot ".to_string(),
        };
        let block = ratatui::widgets::Block::default()
            .title(title)
            .borders(ratatui::widgets::Borders::ALL)
            .style(Style::default().fg(self.theme.header_fg));
        let inner = block.inner(area);
        f.render_widget(block, area);
        match &p.body {
            Ok(sheet) => f.render_widget(
                SheetWidget {
                    sheet,
                    theme: &self.theme,
                    render: None,
                    noted: None,
                    no_color: self.color == crate::theme::ColorDepth::None,
                    visual: None,
                },
                inner,
            ),
            Err(hint) => f.render_widget(
                Paragraph::new(hint.as_str()).wrap(ratatui::widgets::Wrap { trim: false }),
                inner,
            ),
        }
    }

    pub(super) fn render_sidebar(&self, f: &mut ratatui::Frame<'_>, area: Rect) {
        let Some(page) = &self.sidebar else { return };
        let s = self.sheets.active();
        let column = self.current_column_name().unwrap_or_default();
        let err = |e: PolarsError| format!("<error>: {e}");
        let cell = || match s.cursor_value(&column) {
            Some(v) if !v.is_null() => crate::sheet::any_text(&v),
            Some(_) => "null".into(),
            None => String::new(),
        };
        let (title, text): (String, ratatui::text::Text<'static>) = match page {
            SidebarPage::Plan => (
                "plan".into(),
                s.window_plan().describe_plan().unwrap_or_else(err).into(),
            ),
            SidebarPage::PlanOpt => (
                "plan (opt)".into(),
                s.window_plan()
                    .describe_optimized_plan()
                    .unwrap_or_else(err)
                    .into(),
            ),
            SidebarPage::Column => (format!("column: {column}"), self.column_page().into()),
            SidebarPage::Lua(name) => {
                let lines = self.plugins.as_ref().map(|p| {
                    p.page(name, &self.bar_ctx(s, 1), &cell())
                        .unwrap_or_else(|e| vec![Line::from(e)])
                });
                (name.clone(), lines.unwrap_or_default().into())
            }
            SidebarPage::Cell => {
                let text = cell();
                let text = match self.cursor_note() {
                    Some(n) => format!("{text}\n\nnote: {}", n.text),
                    None => text,
                };
                (format!("cell: {column}"), text.into())
            }
            SidebarPage::Help => {
                let lines: Vec<String> = self
                    .keymap
                    .chords(Layer::Sheet)
                    .iter()
                    .map(|c| {
                        let keys: Vec<String> = c.on.iter().map(|k| k.to_string()).collect();
                        format!("{:>6}  {}", keys.join(" "), c.desc)
                    })
                    .collect();
                ("help".into(), lines.join("\n").into())
            }
        };
        let block = ratatui::widgets::Block::default()
            .title(format!(" {title} "))
            .borders(ratatui::widgets::Borders::ALL)
            .style(Style::default().fg(self.theme.plan_border_fg));
        let p = Paragraph::new(text)
            .block(block)
            .wrap(ratatui::widgets::Wrap { trim: false });
        f.render_widget(p, area);
    }

    /// The column page: one stat per line, as `I` names them.
    fn column_page(&self) -> String {
        match &self.sidebar_stats {
            None => String::new(),
            Some((_, Err(e))) => e.clone(),
            Some((_, Ok(df))) => {
                let mut out = Vec::new();
                for c in df.columns() {
                    if c.name() == "sheet" || c.name() == "column" {
                        continue;
                    }
                    let v = match c.get(0) {
                        Ok(v) if !v.is_null() => crate::sheet::any_text(&v),
                        _ => "·".into(),
                    };
                    out.push(format!("{:<9}{v}", c.name().as_str()));
                }
                out.join("\n")
            }
        }
    }

    pub(super) fn render_which_key(&self, f: &mut ratatui::Frame<'_>, area: Rect) {
        let pending_len = self.pending.len();
        let lines: Vec<Line> = self
            .pending_candidates
            .iter()
            .filter_map(|c| c.on.get(pending_len).map(|k| (k, c.desc.as_ref())))
            .map(|(k, desc)| {
                Line::from(vec![
                    Span::styled(
                        format!(" {k:>6} "),
                        Style::default()
                            .fg(self.theme.which_key_fg)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("→ "),
                    Span::raw(desc.to_string()),
                ])
            })
            .collect();
        let h = (lines.len() as u16 + 2).min(area.height);
        let w = 44.min(area.width);
        let x = area.x + area.width.saturating_sub(w);
        let y = area.y + area.height.saturating_sub(h);
        let rect = Rect {
            x,
            y,
            width: w,
            height: h,
        };
        let block = ratatui::widgets::Block::default()
            .title(format!(
                " {} ",
                self.pending
                    .iter()
                    .map(|k| k.to_string())
                    .collect::<Vec<_>>()
                    .join("")
            ))
            .borders(ratatui::widgets::Borders::ALL)
            .style(
                Style::default()
                    .bg(self.theme.overlay_bg)
                    .fg(self.theme.overlay_fg),
            );
        let p = Paragraph::new(lines).block(block);
        f.render_widget(ratatui::widgets::Clear, rect);
        f.render_widget(p, rect);
    }

    /// `crumbs | what shapes the view …| column | row`. The position on the
    /// right always shows; the crumbs give up their head, the chips their tail.
    pub(super) fn render_titlebar(
        &self,
        f: &mut ratatui::Frame<'_>,
        area: Rect,
        s: &Sheet,
        pane: u8,
    ) {
        let View {
            row_cursor,
            col_cursor,
            window_offset,
            ..
        } = s.view;
        let (rows, cols) = s
            .window()
            .map(|df| (df.height(), df.width()))
            .unwrap_or((0, 0));
        let abs_row = window_offset + row_cursor as u64 + u64::from(rows > 0); // 1-based
        let col = col_cursor + usize::from(cols > 0); // 1-based
        let total = s.row_count().map_or_else(|| "?".to_string(), grouped);
        let position = format!(" row {}/{total} ", grouped(abs_row));
        let col_name = s
            .window()
            .and_then(|df| df.get_column_names().get(col_cursor).map(|n| n.to_string()))
            .unwrap_or_default();
        let column = format!(" {} {col}/{cols} ", truncate_head(&col_name, 24));

        let mut chips: Vec<String> = self
            .spin()
            .map(|i| SPINNER[i].to_string())
            .into_iter()
            .collect();
        if let Some(fl) = &s.view.filter {
            chips.push(format!("filter {}~{}", fl.column, fl.regex));
        }
        if let Some(p) = &s.view.filter_expr {
            chips.push(format!("filter {p}"));
        }
        // One key shows as the header's arrow.
        if s.view.sort.len() > 1 {
            let keys: Vec<String> = s
                .view
                .sort
                .iter()
                .map(|k| {
                    let arrow = if k.order == SortOrder::Asc {
                        '▲'
                    } else {
                        '▼'
                    };
                    format!("{}{arrow}", k.column)
                })
                .collect();
            chips.push(format!("sort {}", keys.join(" ")));
        }
        if !s.view.selected.is_empty() {
            chips.push(format!("sel {}", grouped(s.view.selected.len() as u64)));
        }
        if !s.view.hidden.is_empty() {
            chips.push(format!("hidden {}", s.view.hidden.len()));
        }
        if s.state.linked {
            let behind = self
                .busy
                .iter()
                .any(|j| j.label == "refresh" && j.sheet_id == s.id);
            chips.push(if behind { "⟳!" } else { "⟳" }.into());
        }
        let chips = match chips.is_empty() {
            true => String::new(),
            false => format!(" {} ", chips.join(" · ")),
        };

        let room = (area.width as usize).saturating_sub(position.width() + column.width());
        // Chips get at most a third of the room from a long name.
        let name_room = room.saturating_sub(chips.width().min(room / 3) + 2);
        let name = format!(" {} ", truncate_head(&crumbs(&self.sheets, s), name_room));
        let chips = fit(&chips, room.saturating_sub(name.width()));
        let th = &self.theme;
        let left = [
            Span::styled(name, th.titlebar_left()),
            Span::styled(chips, th.titlebar_mid()),
        ];
        let right = [
            Span::styled(column, th.titlebar_mid().add_modifier(Modifier::BOLD)),
            Span::styled(position, th.titlebar_right()),
        ];
        let line = match self.lua_bar(Bar::Title, s, pane, (&left, &right), area.width) {
            Some(line) => line,
            None => Line::from_iter(left.into_iter().chain(right)),
        };
        f.render_widget(Paragraph::new(line).style(th.titlebar_mid()), area);
    }

    /// The bar a `vd.statusline` / `vd.titlebar` function draws, if any.
    fn lua_bar(
        &self,
        bar: Bar,
        s: &Sheet,
        pane: u8,
        builtin: (&[Span<'_>], &[Span<'_>]),
        width: u16,
    ) -> Option<Line<'static>> {
        let p = self.plugins.as_ref().filter(|p| p.has_bar(bar))?;
        let (left, right) = p.bar(bar, &self.bar_ctx(s, pane), builtin)?;
        Some(crate::plugins::compose(left, right, width))
    }

    pub(super) fn render_statusbar(&self, f: &mut ratatui::Frame<'_>, area: Rect) {
        // Prompts and messages own the line; a `vd.statusline` draws the rest.
        let owned = self.input.is_some() || self.confirm.is_some() || self.toast.is_some();
        let text = match (
            &self.input,
            self.confirm
                .as_ref()
                .map(|(_, q)| q)
                .or(self.toast.as_ref()),
            self.pending_count,
            !self.pending.is_empty(),
        ) {
            (Some((prompt, line)), _, _, _) => {
                // Cursor = reversed cell (a space at the end of the line).
                let (before, after) = line.split();
                let mut rest = after.chars();
                let at = rest.next().map_or(" ".to_string(), String::from);
                Line::from(vec![
                    Span::styled(prompt.label(), Style::default().fg(self.theme.which_key_fg)),
                    Span::raw(before.to_string()),
                    Span::styled(at, Style::default().add_modifier(Modifier::REVERSED)),
                    Span::raw(rest.as_str().to_string()),
                    Span::styled(
                        if line.is_normal() {
                            "  -- NORMAL --"
                        } else {
                            ""
                        },
                        Style::default().fg(self.theme.which_key_fg),
                    ),
                    // Tab-completion candidates, after the text.
                    Span::styled(
                        self.toast
                            .as_ref()
                            .map_or(String::new(), |t| format!("   {t}")),
                        Style::default().add_modifier(Modifier::DIM),
                    ),
                ])
            }
            // Polars errors span lines (plan dumps); the bar shows the first.
            (_, Some(t), _, _) => Line::from(Span::styled(
                t.lines().next().unwrap_or_default().to_string(),
                self.theme.toast(),
            )),
            (_, _, Some(c), _) => Line::from(format!("count: {c}")),
            _ if !self.busy.is_empty() => Line::from(format!(
                "working: {}…",
                self.busy
                    .iter()
                    .map(|j| j.label)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            (_, _, _, true) => Line::from(format!(
                "pending: {}",
                self.pending
                    .iter()
                    .map(|k| k.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            )),
            _ if let Some(v) = self
                .live_visual()
                .or(self.visual.filter(|v| v.sheet != self.sheets.active().id)) =>
            {
                let (a, b) = v.rows();
                let what = if v.block { "block" } else { "rows" };
                Line::from(format!(
                    " -- VISUAL -- {what} {}–{} · s u select · y copy · d delete · e set · \" sheet · : ex · Esc",
                    a + 1,
                    b + 1
                ))
            }
            _ if self.sheets.active().state.plot.is_some() => {
                let v = self.sheets.active().state.plot.as_ref();
                let [x0, y0, w, h] = v.map_or([0.0; 4], |v| v.cursor);
                let n = crate::sheet::stat_num;
                Line::from(format!(
                    " box x {}–{} · y {}–{} · hjkl move · HJKL size · + - zoom · _ fit · s u select · Enter rows",
                    n(x0),
                    n(x0 + w),
                    n(y0),
                    n(y0 + h)
                ))
            }
            _ => match self.current_sel_stats() {
                Some(stats) => Line::from(
                    stats
                        .iter()
                        .map(|(k, v)| format!("{k} {v}"))
                        .collect::<Vec<_>>()
                        .join(" · "),
                ),
                None => Line::default(),
            },
        };
        // VisiData's right side: the last command's keys and name.
        let right = match self.input.is_some() || self.confirm.is_some() {
            true => vec![],
            false => vec![Span::styled(
                format!(" {} ", self.last_cmd),
                self.theme.statusbar().add_modifier(Modifier::DIM),
            )],
        };
        let rec = self.recording.is_some() && self.input.is_none();
        let rec =
            rec.then(|| Span::styled("● rec ", self.theme.toast().add_modifier(Modifier::BOLD)));
        let width = area
            .width
            .saturating_sub(rec.as_ref().map_or(0, |r| r.width() as u16));
        let mut text = match owned {
            true => crate::plugins::compose(text.spans, right, width),
            false => self
                .lua_bar(
                    Bar::Status,
                    self.sheets.active(),
                    1,
                    (&text.spans, &right),
                    width,
                )
                .unwrap_or_else(|| crate::plugins::compose(text.spans, right, width)),
        };
        if let Some(rec) = rec {
            text.spans.insert(0, rec);
        }
        f.render_widget(Paragraph::new(text).style(self.theme.statusbar()), area);
    }
}

/// `sales.csv › freq(region) › region=north`: the stack up to `s`, each
/// name without the `: parent` its command appended.
fn crumbs(sheets: &crate::sheet::Sheets, s: &Sheet) -> String {
    let mut names: Vec<&str> = Vec::new();
    for sh in sheets.iter() {
        let short = sh
            .origin
            .parent()
            .and_then(|id| sheets.iter().find(|p| p.id == id))
            .and_then(|p| sh.name.strip_suffix(p.name.as_str())?.strip_suffix(": "))
            .unwrap_or(&sh.name);
        names.push(short);
        if sh.id == s.id {
            break;
        }
    }
    names.join(" › ")
}
