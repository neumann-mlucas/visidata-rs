//! `:plot`, `.`, `g.` (S-23 C1): a plot is a sheet whose rows are the parent's
//! view binned by x — per bin the mean x and each y's min and max, so spikes
//! survive and the frame stays O(screen) however many rows the view has.

use super::*;

/// What a plot shows (S-23 C3): the visible x and y ranges, and the cursor
/// box (`x`, `y` of its lower left corner, width, height), in data units.
#[derive(Clone)]
pub struct PlotView {
    pub x: [f64; 2],
    pub y: [f64; 2],
    pub cursor: [f64; 4],
    /// The parent as it was: binning again (zoom) works after it's closed.
    pub parent: Box<Sheet>,
}

impl PlotView {
    /// The box moved by `dx`, `dy` box sizes; the view pans to keep it in.
    pub fn step(&mut self, dx: f64, dy: f64) {
        let c = &mut self.cursor;
        c[0] += dx * c[2];
        c[1] += dy * c[3];
        let pan = |lo: &mut f64, hi: &mut f64, a: f64, b: f64| {
            let d = if a < *lo {
                a - *lo
            } else if b > *hi {
                b - *hi
            } else {
                0.0
            };
            *lo += d;
            *hi += d;
        };
        let [x0, y0, w, h] = *c;
        let [mut a, mut b] = self.x;
        pan(&mut a, &mut b, x0, x0 + w);
        self.x = [a, b];
        let [mut a, mut b] = self.y;
        pan(&mut a, &mut b, y0, y0 + h);
        self.y = [a, b];
    }

    /// Zoom by `f` (< 1 in) around the cursor box's center.
    pub fn zoom(&mut self, f: f64) {
        let [x0, y0, w, h] = self.cursor;
        let (cx, cy) = (x0 + w / 2.0, y0 + h / 2.0);
        let z = |[lo, hi]: [f64; 2], c: f64| [c - (c - lo) * f, c + (hi - c) * f];
        self.x = z(self.x, cx);
        self.y = z(self.y, cy);
        self.cursor = [cx - w * f / 2.0, cy - h * f / 2.0, w * f, h * f];
    }

    /// The box as the view: `zz`.
    pub fn zoom_to_cursor(&mut self) {
        let [x0, y0, w, h] = self.cursor;
        self.x = [x0, x0 + w];
        self.y = [y0, y0 + h];
        self.cursor = [x0 + w * 0.375, y0 + h * 0.375, w / 4.0, h / 4.0];
    }
}

/// The x column of a binned frame is first, then `{y}_min`, `{y}_max` per y.
pub fn plot_columns(y: &str) -> (String, String) {
    (format!("{y}_min"), format!("{y}_max"))
}

impl Sheet {
    /// Bin `parent`'s view into at most `bins` x ranges; `x` numeric or a
    /// date, `ys` numeric. Lazy until the binned rows.
    pub fn plot_from(
        parent: &Sheet,
        id: SheetId,
        x: &str,
        ys: &[String],
        bins: u32,
        range: Option<[f64; 2]>,
    ) -> Result<Self, String> {
        let e = |e: PolarsError| e.to_string();
        let lf = parent.view_plan();
        let schema = lf.clone().collect_schema().map_err(e)?;
        let dtype = |c: &str| {
            schema
                .get(c)
                .cloned()
                .ok_or_else(|| format!("plot: no column {c}"))
        };
        let xt = dtype(x)?;
        if !(xt.is_primitive_numeric() || matches!(xt, DataType::Date | DataType::Datetime(..))) {
            return Err(format!(
                "plot: x column {x} is {xt}; needs numbers or dates"
            ));
        }
        if ys.is_empty() {
            return Err("plot: no numeric column to plot".into());
        }
        for y in ys {
            let t = dtype(y)?;
            if !t.is_primitive_numeric() {
                return Err(format!("plot: {y} is {t}; needs numbers"));
            }
        }
        let f = |c: &str| col(c).to_physical().cast(DataType::Float64);
        let mut cols = vec![f(x).alias(x)];
        cols.extend(ys.iter().map(|y| f(y).alias(y.as_str())));
        let mut base = lf.select(cols).filter(col(x).is_not_null());
        let (lo, hi) = match range {
            // zoomed: bin only what's in view
            Some([lo, hi]) => {
                base = base.filter(col(x).gt_eq(lit(lo)).and(col(x).lt_eq(lit(hi))));
                (lo, hi)
            }
            None => {
                let r = base
                    .clone()
                    .select([col(x).min().alias("lo"), col(x).max().alias("hi")])
                    .collect()
                    .map_err(e)?;
                let get = |n: &str| r.column(n).ok()?.f64().ok()?.get(0);
                let (Some(lo), Some(hi)) = (get("lo"), get("hi")) else {
                    return Err(format!("plot: {x} has no values"));
                };
                (lo, hi)
            }
        };
        let span = (hi - lo).max(f64::MIN_POSITIVE);
        let last = f64::from(bins.max(1) - 1);
        let bin = ((col(x) - lit(lo)) / lit(span) * lit(last))
            .floor()
            .cast(DataType::Int64)
            .alias("__bin");
        let mut aggs = vec![col(x).mean()];
        for y in ys {
            let (lo, hi) = plot_columns(y);
            aggs.push(col(y.as_str()).min().alias(lo));
            aggs.push(col(y.as_str()).max().alias(hi));
        }
        // x back to its type, so the axis reads as dates
        let back = match &xt {
            DataType::Date => col(x).cast(DataType::Int32).cast(DataType::Date),
            DataType::Datetime(..) => col(x).cast(DataType::Int64).cast(xt.clone()),
            _ => col(x),
        };
        let df = base
            .with_column(bin)
            .group_by([col("__bin")])
            .agg(aggs)
            .sort(["__bin"], Default::default())
            .drop(Selector::ByName {
                names: Arc::new(["__bin".into()]),
                strict: true,
            })
            .with_column(back)
            .collect()
            .map_err(e)?;
        let name = format!("plot({x}→{}): {}", ys.join(","), parent.name);
        let pts = plot_points(&df, x, ys);
        let all = || pts.iter().flatten();
        let ylo = all().map(|p| p.1).fold(f64::INFINITY, f64::min);
        let yhi = all().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
        let pad = |lo: f64, hi: f64| match (lo.is_finite(), lo < hi) {
            (false, _) => [0.0, 1.0],
            (true, false) => [lo - 0.5, hi + 0.5], // one value: center it
            (true, true) => [lo, hi],
        };
        let (xr, yr) = (pad(lo, hi), pad(ylo, yhi));
        let (w, h) = ((xr[1] - xr[0]) / 8.0, (yr[1] - yr[0]) / 8.0);
        let origin = Origin::Plot {
            parent: parent.id,
            x: x.into(),
            ys: ys.to_vec(),
        };
        let mut s = Self::meta(id, name, df, origin);
        s.state.plot = Some(PlotView {
            x: xr,
            y: yr,
            cursor: [xr[0] + 3.5 * w, yr[0] + 3.5 * h, w, h],
            parent: Box::new(parent.clone()),
        });
        Ok(s)
    }

    /// `Enter` on a plot (S-23 C4): `parent`'s rows `rids` (in its box).
    pub fn box_from(parent: &Sheet, id: SheetId, rids: Vec<u64>) -> Result<Self, String> {
        let df = parent
            .view_plan_rid(true)
            .collect()
            .map_err(|e| e.to_string())?;
        let keep = rids.into_iter().collect();
        let name = format!("plot box: {}", parent.name);
        Self::rows_from(parent, id, df, &keep, name)
    }

    /// Row ids of `parent`'s view whose x and some y fall in `cursor`.
    pub fn rids_in_box(
        parent: &Sheet,
        x: &str,
        ys: &[String],
        cursor: [f64; 4],
    ) -> Result<Vec<u64>, String> {
        let [x0, y0, w, h] = cursor;
        let f = |c: &str| col(c).to_physical().cast(DataType::Float64);
        let within = |c: &str, lo: f64, hi: f64| f(c).gt_eq(lit(lo)).and(f(c).lt_eq(lit(hi)));
        let any_y = ys
            .iter()
            .map(|y| within(y, y0, y0 + h))
            .reduce(Expr::or)
            .unwrap_or(lit(false));
        let df = parent
            .view_plan_rid(true)
            .filter(within(x, x0, x0 + w).and(any_y))
            .select([col(ROW_ID)])
            .collect()
            .map_err(|e| e.to_string())?;
        rid_values(&df).map_err(|e| e.to_string())
    }
}

impl Sheet {
    /// `:hist [bins]` (S-23 C2): counts of `column` in `bins` equal ranges,
    /// right-closed as Polars' `hist` (`(lo, hi]`, the first takes the minimum).
    pub fn hist_from(parent: &Sheet, id: SheetId, column: &str, bins: u32) -> Result<Self, String> {
        let e = |e: PolarsError| e.to_string();
        let lf = parent.view_plan();
        let t = lf
            .clone()
            .collect_schema()
            .map_err(e)?
            .get(column)
            .cloned()
            .ok_or_else(|| format!("hist: no column {column}"))?;
        if !t.is_primitive_numeric() {
            return Err(format!("hist: {column} is {t}; needs numbers"));
        }
        let x = lf
            .select([col(column).cast(DataType::Float64).alias("x")])
            .filter(col("x").is_not_null());
        let range = x
            .clone()
            .select([col("x").min().alias("lo"), col("x").max().alias("hi")])
            .collect()
            .map_err(e)?;
        let get = |n: &str| range.column(n).ok()?.f64().ok()?.get(0);
        let (Some(lo), Some(hi)) = (get("lo"), get("hi")) else {
            return Err(format!("hist: {column} has no values"));
        };
        let bins = if hi > lo { bins.max(1) } else { 1 };
        let width = (hi - lo) / f64::from(bins);
        let last = i64::from(bins) - 1;
        let idx = match width > 0.0 {
            true => {
                ((col("x") - lit(lo)) / lit(width))
                    .ceil()
                    .cast(DataType::Int64)
                    - lit(1i64)
            }
            false => lit(0i64),
        };
        let counts = x
            .select([idx
                .clip(lit(0i64), lit(last))
                .cast(DataType::Int64)
                .alias("bin")])
            .group_by([col("bin")])
            .agg([len().alias("count")])
            .collect()
            .map_err(e)?;
        let mut n = vec![0u64; bins as usize];
        let (b, c) = (
            counts.column("bin").map_err(e)?,
            counts.column("count").map_err(e)?,
        );
        for (b, c) in b.i64().map_err(e)?.iter().zip(c.u32().map_err(e)?.iter()) {
            if let (Some(b), Some(c)) = (b, c) {
                n[b as usize] = u64::from(c);
            }
        }
        let edge = |i: u32| lo + width * f64::from(i);
        let label: Vec<String> = (0..bins)
            .map(|i| format!("{}–{}", stat_num(edge(i)), stat_num(edge(i + 1))))
            .collect();
        let df = DataFrame::new_infer_height(vec![
            Column::new("bin".into(), label),
            Column::new("count".into(), n),
            Column::new("lo".into(), (0..bins).map(edge).collect::<Vec<_>>()),
            Column::new(
                "hi".into(),
                (0..bins).map(|i| edge(i + 1)).collect::<Vec<_>>(),
            ),
        ])
        .map_err(e)?;
        let origin = Origin::Bars {
            parent: parent.id,
            label: "bin".into(),
            value: "count".into(),
            horizontal: false,
        };
        Ok(Self::meta(
            id,
            format!("hist({column}): {}", parent.name),
            df,
            origin,
        ))
    }

    /// `:bar` on a freq sheet: its rows (most common first) as bars.
    // ponytail: the first 1000 values; scroll the bars if someone needs more
    pub fn bars_from(parent: &Sheet, id: SheetId) -> Result<Self, String> {
        let Origin::Freq { column, .. } = &parent.origin else {
            return Err("bar: needs a frequency table (F)".into());
        };
        let e = |e: PolarsError| e.to_string();
        let lf = parent.view_plan();
        let names: Vec<String> = lf
            .clone()
            .collect_schema()
            .map_err(e)?
            .iter_names()
            .map(|n| n.to_string())
            .collect();
        // the count column follows the value column
        let value = names
            .iter()
            .position(|n| n == column)
            .and_then(|i| names.get(i + 1))
            .ok_or("bar: no count column")?
            .clone();
        let df = lf
            .select([
                col(column.as_str()).cast(DataType::String),
                col(value.as_str()),
            ])
            .limit(1000)
            .collect()
            .map_err(e)?;
        let origin = Origin::Bars {
            parent: parent.id,
            label: column.clone(),
            value,
            horizontal: true,
        };
        Ok(Self::meta(id, format!("bar: {}", parent.name), df, origin))
    }
}

/// Points per y of a binned frame: each bin's (x, min) and (x, max).
pub fn plot_points(df: &DataFrame, x: &str, ys: &[String]) -> Vec<Vec<(f64, f64)>> {
    let f = |c: &str| -> Vec<Option<f64>> {
        df.column(c)
            .ok()
            .and_then(|c| c.to_physical_repr().cast(&DataType::Float64).ok())
            .and_then(|c| c.f64().ok().map(|a| a.iter().collect()))
            .unwrap_or_default()
    };
    let xs = f(x);
    ys.iter()
        .map(|y| {
            let (lo, hi) = plot_columns(y);
            let (lo, hi) = (f(&lo), f(&hi));
            xs.iter()
                .zip(lo.iter().zip(&hi))
                .filter_map(|(x, (a, b))| Some([(x.as_ref()?, a), (x.as_ref()?, b)]))
                .flatten()
                .filter_map(|(x, y)| Some((*x, (*y)?)))
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet(df: DataFrame) -> Sheet {
        Sheet::meta(0, "t".into(), df, Origin::File)
    }

    #[test]
    fn bins_keep_each_ranges_min_and_max() {
        let n = 1000i64;
        let x: Vec<i64> = (0..n).collect();
        // a slow wave with one spike up and one down
        let y: Vec<f64> = (0..n)
            .map(|i| match i {
                333 => 1e6,
                777 => -1e6,
                i => (i as f64 / 50.0).sin(),
            })
            .collect();
        let df = df!["x" => &x, "y" => &y].unwrap();
        let p = Sheet::plot_from(&sheet(df), 1, "x", &["y".into()], 10, None).unwrap();
        let w = p.window().unwrap();
        assert!(w.height() <= 10, "{}", w.height());
        let mins = w.column("y_min").unwrap().f64().unwrap();
        let maxs = w.column("y_max").unwrap().f64().unwrap();
        assert_eq!(maxs.max(), Some(1e6));
        assert_eq!(mins.min(), Some(-1e6));
        // every bin's min/max is the brute-force one over its x range
        for b in 0..w.height() {
            let lo = (b as f64 * (n - 1) as f64 / 9.0).ceil() as usize;
            let hi = ((b + 1) as f64 * (n - 1) as f64 / 9.0).ceil() as usize;
            let part = &y[lo..hi.min(n as usize).max(lo + 1)];
            let want_max = part.iter().copied().fold(f64::MIN, f64::max);
            assert_eq!(maxs.get(b), Some(want_max), "bin {b}");
        }
        let pts = plot_points(w, "x", &["y".into()]);
        assert_eq!(pts[0].len(), 2 * w.height());
    }

    #[test]
    fn plot_refuses_text_axes_and_keeps_date_x() {
        let mut df = df!["d" => [19723i32, 19725], "s" => ["a", "b"], "v" => [1.0, 2.0]].unwrap();
        df.apply("d", |c| c.cast(&DataType::Date).unwrap()).unwrap();
        let s = sheet(df);
        let err = |x: &str, y: &str| {
            Sheet::plot_from(&s, 1, x, &[y.into()], 4, None)
                .err()
                .unwrap()
        };
        assert_eq!(
            err("s", "v"),
            "plot: x column s is str; needs numbers or dates"
        );
        assert_eq!(err("d", "s"), "plot: s is str; needs numbers");
        let p = Sheet::plot_from(&s, 1, "d", &["v".into()], 4, None).unwrap();
        assert_eq!(
            p.window().unwrap().column("d").unwrap().dtype(),
            &DataType::Date
        );
    }
}

#[cfg(test)]
mod hist_tests {
    use super::*;

    #[test]
    fn hist_bins_are_right_closed_and_count_every_value() {
        let v: Vec<f64> = vec![0.0, 1.0, 2.5, 2.5, 5.0, 7.5, 7.5, 10.0, 3.0, 9.99];
        let df = df!["v" => &v].unwrap();
        let s = Sheet::meta(0, "t".into(), df, Origin::File);
        let h = Sheet::hist_from(&s, 1, "v", 4).unwrap();
        let w = h.window().unwrap();
        let counts: Vec<u64> = w
            .column("count")
            .unwrap()
            .u64()
            .unwrap()
            .into_no_null_iter()
            .collect();
        // (0, 2.5] takes 0 too; (2.5, 5]; (5, 7.5]; (7.5, 10]
        let want = |lo: f64, hi: f64, first: bool| {
            v.iter()
                .filter(|x| (**x > lo || (first && **x == lo)) && **x <= hi)
                .count() as u64
        };
        assert_eq!(
            counts,
            [
                want(0.0, 2.5, true),
                want(2.5, 5.0, false),
                want(5.0, 7.5, false),
                want(7.5, 10.0, false)
            ]
        );
        assert_eq!(counts.iter().sum::<u64>(), v.len() as u64);
        assert_eq!(
            w.column("bin").unwrap().str().unwrap().get(0),
            Some("0–2.5")
        );
        let one = Sheet::meta(0, "t".into(), df!["v" => [3.0, 3.0]].unwrap(), Origin::File);
        let h = Sheet::hist_from(&one, 1, "v", 4).unwrap();
        assert_eq!(h.window().unwrap().height(), 1, "one value: one bin");
        let txt = Sheet::meta(0, "t".into(), df!["s" => ["a"]].unwrap(), Origin::File);
        assert_eq!(
            Sheet::hist_from(&txt, 1, "s", 4).err().unwrap(),
            "hist: s is str; needs numbers"
        );
    }
}
