use super::*;

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
        term.draw(|f| self.render(f))?;
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
        self.viewport_rows = (sheet_area.height as usize).saturating_sub(1).max(1);
        let lua_render = self.plugins.as_ref().filter(|p| p.has_renderers());
        let render_cell =
            |c: &str, d: &DataType, v: &AnyValue| lua_render.and_then(|p| p.render(c, d, v));
        let render = lua_render.map(|_| &render_cell as crate::widget::CellRenderer);
        let s = self.sheets.active_mut();
        refresh_widths(s, render);
        // A stale window may be shorter than the view being loaded (rows
        // just added): clamping to it would lose the cursor.
        if let Some(df) = s.window().filter(|_| s.is_loaded()).cloned() {
            clamp_view(&mut s.view, &df, sheet_area, &s.state.widths);
        }

        self.render_titlebar(f, chunks[0]);
        f.render_widget(
            SheetWidget {
                sheet: self.sheets.active(),
                theme: &self.theme,
                render,
            },
            sheet_area,
        );
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
        let Some(page) = self.sidebar else { return };
        let s = self.sheets.active();
        let column = self.current_column_name().unwrap_or_default();
        let err = |e: PolarsError| format!("<error>: {e}");
        let (title, text) = match page {
            SidebarPage::Plan => (
                "plan".into(),
                s.window_plan().describe_plan().unwrap_or_else(err),
            ),
            SidebarPage::PlanOpt => (
                "plan (opt)".into(),
                s.window_plan()
                    .describe_optimized_plan()
                    .unwrap_or_else(err),
            ),
            SidebarPage::Column => (format!("column: {column}"), self.column_page()),
            SidebarPage::Cell => {
                let v = s
                    .window()
                    .and_then(|df| df.column(&column).ok()?.get(s.view.row_cursor).ok());
                let text = match v {
                    Some(v) if !v.is_null() => crate::sheet::any_text(&v),
                    Some(_) => "null".into(),
                    None => String::new(),
                };
                (format!("cell: {column}"), text)
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
                ("help".into(), lines.join("\n"))
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

    /// `name | kind · what shapes the view …| column | row`. The position
    /// on the right always shows; the name gives up its head, the chips their tail.
    pub(super) fn render_titlebar(&self, f: &mut ratatui::Frame<'_>, area: Rect) {
        let s = self.sheets.active();
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
        let total = s.row_count();
        let percent = total
            .filter(|t| *t > 0)
            .map_or_else(String::new, |t| format!(" · {}%", abs_row * 100 / t));
        let total = total.map_or_else(|| "?".to_string(), grouped);
        let position = format!(" row {}/{total}{percent} ", grouped(abs_row));
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
        chips.push(format!("{:?}", s.kind).to_lowercase());
        if self.sheets.len() > 1 {
            chips.push(format!("sheets {}", self.sheets.len()));
        }
        if let Some(fl) = &s.view.filter {
            chips.push(format!("filter {}~{}", fl.column, fl.regex));
        }
        if let Some(p) = &s.view.filter_expr {
            chips.push(format!("filter {p}"));
        }
        if !s.view.sort.is_empty() {
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
        let chips = format!(" {} ", chips.join(" · "));

        let room = (area.width as usize).saturating_sub(position.width() + column.width());
        // Chips get at most a third of the room from a long name.
        let name_room = room.saturating_sub(chips.width().min(room / 3) + 2);
        let name = format!(" {} ", truncate_head(&s.name, name_room));
        let chips = fit(&chips, room.saturating_sub(name.width()));
        let th = &self.theme;
        let line = Line::from(vec![
            Span::styled(name, th.titlebar_left()),
            Span::styled(chips, th.titlebar_mid()),
            Span::styled(column, th.titlebar_mid().add_modifier(Modifier::BOLD)),
            Span::styled(position, th.titlebar_right()),
        ]);
        f.render_widget(Paragraph::new(line), area);
    }

    pub(super) fn render_statusbar(&self, f: &mut ratatui::Frame<'_>, area: Rect) {
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
            _ => Line::from(
                " hjkl · gg/G · / search · [ ] sort · gz| filter · s sel · F freq · I describe · U undo · : ex · q ",
            ),
        };
        let mut text = text;
        if self.recording.is_some() && self.input.is_none() {
            let rec = Span::styled("● rec ", self.theme.toast().add_modifier(Modifier::BOLD));
            text.spans.insert(0, rec);
        }
        f.render_widget(Paragraph::new(text).style(self.theme.statusbar()), area);
    }
}
