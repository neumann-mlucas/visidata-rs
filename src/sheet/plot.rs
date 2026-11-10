//! `:plot`, `.`, `g.` (S-23 C1): a plot is a sheet whose rows are the parent's
//! view binned by x — per bin the mean x and each y's min and max, so spikes
//! survive and the frame stays O(screen) however many rows the view has.

use super::*;

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
        let base = lf.select(cols).filter(col(x).is_not_null());
        let range = base
            .clone()
            .select([col(x).min().alias("lo"), col(x).max().alias("hi")])
            .collect()
            .map_err(e)?;
        let get = |n: &str| range.column(n).ok()?.f64().ok()?.get(0);
        let (Some(lo), Some(hi)) = (get("lo"), get("hi")) else {
            return Err(format!("plot: {x} has no values"));
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
        let origin = Origin::Plot {
            parent: parent.id,
            x: x.into(),
            ys: ys.to_vec(),
        };
        Ok(Self::meta(id, name, df, origin))
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
        let p = Sheet::plot_from(&sheet(df), 1, "x", &["y".into()], 10).unwrap();
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
        let err = |x: &str, y: &str| Sheet::plot_from(&s, 1, x, &[y.into()], 4).err().unwrap();
        assert_eq!(
            err("s", "v"),
            "plot: x column s is str; needs numbers or dates"
        );
        assert_eq!(err("d", "s"), "plot: s is str; needs numbers");
        let p = Sheet::plot_from(&s, 1, "d", &["v".into()], 4).unwrap();
        assert_eq!(
            p.window().unwrap().column("d").unwrap().dtype(),
            &DataType::Date
        );
    }
}
