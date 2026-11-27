//! Number `fmtstr`s (S-36 B): the part of Python's `str.format` and `%`
//! that VisiData's `numericFormatter` hands them to, so replayed formats read
//! the same. `{:,.2f}`, `{:_d}`, `{:.1%}`, `${:,.2f}`, `{:.2f} kg`, `{:e}`;
//! C style `%.2f`, `%d`, `%e`. One field, optional `,`/`_` grouping, `.N`.

use polars::prelude::*;

#[derive(Debug, Clone, PartialEq)]
pub struct NumFmt {
    prefix: String,
    suffix: String,
    c_style: bool,
    group: Option<char>,
    digits: Option<u8>,
    kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Fixed,
    Int,
    Exp,
    Percent,
}

impl NumFmt {
    pub fn parse(s: &str) -> Result<Self, String> {
        let bad = |why: &str| format!("fmtstr: {s:?} {why}");
        let (prefix, spec, suffix, c_style) = if let Some(rest) = s.strip_prefix('%') {
            let end = rest
                .find(|c: char| c.is_ascii_alphabetic())
                .ok_or_else(|| bad("has no conversion (%d, %.2f, %e)"))?;
            let (spec, suffix) = (&rest[..=end], &rest[end + 1..]);
            if suffix.contains('%') {
                return Err(bad("has more than one % field"));
            }
            ("", spec, suffix, true)
        } else {
            let open = s.find('{').ok_or_else(|| bad("has no {} field"))?;
            let close = open + s[open..].find('}').ok_or_else(|| bad("has no closing }"))?;
            let (prefix, suffix) = (&s[..open], &s[close + 1..]);
            if (prefix.chars().chain(suffix.chars())).any(|c| c == '{' || c == '}') {
                return Err(bad("has more than one {} field"));
            }
            let field = &s[open + 1..close];
            let spec = match field.strip_prefix('0').unwrap_or(field) {
                "" => "",
                f => f.strip_prefix(':').ok_or_else(|| bad("names a field"))?,
            };
            (prefix, spec, suffix, false)
        };
        let mut rest = spec;
        let group = match rest.chars().next() {
            Some(c @ (',' | '_')) if !c_style => {
                rest = &rest[1..];
                Some(c)
            }
            _ => None,
        };
        let digits = match rest.strip_prefix('.') {
            Some(r) => {
                let n = r.find(|c: char| !c.is_ascii_digit()).unwrap_or(r.len());
                let d = r[..n].parse::<u8>().map_err(|_| bad("has a bad .N"))?;
                rest = &r[n..];
                Some(d.min(15))
            }
            None => None,
        };
        let kind = match (rest, c_style) {
            ("f", _) => Kind::Fixed,
            ("e", _) => Kind::Exp,
            ("d" | "i", true) | ("d", false) => Kind::Int,
            ("%", false) => Kind::Percent,
            ("", false) if digits.is_none() => Kind::Int, // `{:,}`, `{}`: ints only
            _ => {
                return Err(bad(
                    "isn't supported (one field: optional , or _, .N, type f d e %)",
                ));
            }
        };
        if kind == Kind::Int && digits.is_some() {
            return Err(bad("has decimals on an integer type"));
        }
        Ok(NumFmt {
            prefix: prefix.into(),
            suffix: suffix.into(),
            c_style,
            group,
            digits,
            kind,
        })
    }

    /// `%.Nf` / `{:.Nf}` alone: a float's precision (S-32), not a format.
    pub fn fixed(digits: u8) -> Self {
        NumFmt {
            prefix: String::new(),
            suffix: String::new(),
            c_style: true,
            group: None,
            digits: Some(digits),
            kind: Kind::Fixed,
        }
    }

    /// Python fails `{:d}` on a float (C-style `%d` truncates it).
    pub fn fits_float(&self) -> bool {
        self.c_style || self.kind != Kind::Int
    }

    /// Decimals shown (Python's default 6), or None for an integer type.
    pub fn digits(&self) -> Option<u8> {
        (self.kind != Kind::Int).then(|| self.digits.unwrap_or(6))
    }

    /// The same format at `d` decimals (`Alt++` / `Alt+-`, K2).
    pub fn with_digits(&self, d: u8) -> Self {
        NumFmt {
            digits: Some(d.min(15)),
            ..self.clone()
        }
    }

    /// The fmtstr, normalized (`{0:…}` → `{:…}`).
    pub fn text(&self) -> String {
        let d = self.digits.map_or(String::new(), |d| format!(".{d}"));
        let ty = match (self.kind, self.c_style) {
            (Kind::Fixed, _) => "f",
            (Kind::Exp, _) => "e",
            (Kind::Int, _) => "d",
            (Kind::Percent, _) => "%",
        };
        match self.c_style {
            true => format!("%{d}{ty}{}", self.suffix),
            false => {
                let g = self.group.map_or(String::new(), String::from);
                format!("{}{{:{g}{d}{ty}}}{}", self.prefix, self.suffix)
            }
        }
    }

    pub fn float(&self, v: f64) -> String {
        let body = if !v.is_finite() {
            let t = match () {
                _ if v.is_nan() => "nan",
                _ if v > 0.0 => "inf",
                _ => "-inf",
            };
            match self.kind {
                Kind::Percent => format!("{t}%"),
                _ => t.to_string(),
            }
        } else {
            let d = self.digits.unwrap_or(6) as usize;
            match self.kind {
                Kind::Fixed => self.grouped(format!("{v:.d$}")),
                Kind::Percent => self.grouped(format!("{:.d$}", v * 100.0)) + "%",
                Kind::Exp => exp_text(&format!("{v:.d$e}")),
                // no i64: `%d` of 1e20 writes every digit, as Python; `+ 0.0` drops -0
                Kind::Int => self.grouped(format!("{:.0}", v.trunc() + 0.0)),
            }
        };
        format!("{}{body}{}", self.prefix, self.suffix)
    }

    pub fn int(&self, v: i64) -> String {
        match self.kind {
            Kind::Int => format!(
                "{}{}{}",
                self.prefix,
                self.grouped(v.to_string()),
                self.suffix
            ),
            _ => self.float(v as f64),
        }
    }

    /// `,` / `_` between each 3 digits of the integer part.
    fn grouped(&self, t: String) -> String {
        let Some(sep) = self.group else { return t };
        let (sign, t) = t.split_at(usize::from(t.starts_with('-')));
        let (int, frac) = t.split_at(t.find('.').unwrap_or(t.len()));
        let mut out = String::with_capacity(t.len() + int.len() / 3);
        for (i, c) in int.chars().enumerate() {
            if i > 0 && (int.len() - i) % 3 == 0 {
                out.push(sep);
            }
            out.push(c);
        }
        format!("{sign}{out}{frac}")
    }

    /// `e` as the text of each value (a null stays null): what freq groups
    /// by and its dive compares (K3).
    pub fn text_expr(&self, e: Expr) -> Expr {
        let f = self.clone();
        e.map(
            move |c| {
                let s = c.as_materialized_series();
                let out: StringChunked = match s.dtype() {
                    d if d.is_integer() => {
                        let s = s.cast(&DataType::Int64)?;
                        s.i64()?.iter().map(|v| v.map(|v| f.int(v))).collect()
                    }
                    _ => {
                        let s = s.cast(&DataType::Float64)?;
                        s.f64()?.iter().map(|v| v.map(|v| f.float(v))).collect()
                    }
                };
                Ok(out.with_name(s.name().clone()).into_column())
            },
            |_, field| Ok(Field::new(field.name().clone(), DataType::String)),
        )
    }

    /// The Polars Python expression `text_expr` exports to.
    pub fn py(&self, c: &str) -> String {
        let f = self.text().replace('\\', "\\\\").replace('\'', "\\'");
        let call = match self.c_style {
            true => format!("'{f}' % v"),
            false => format!("'{f}'.format(v)"),
        };
        format!("pl.col({c}).map_elements(lambda v: {call}, return_dtype=pl.String)")
    }

    /// The DuckDB expression: `printf` for C style, else fmtlib's `format`
    /// (no `%` type there: scaled, with the sign added).
    pub fn sql(&self, c: &str) -> String {
        let q = |s: &str| format!("'{}'", s.replace('\'', "''"));
        // fmtlib takes no decimals on an integer, nor `%d` a double
        let v = match self.kind {
            Kind::Int => format!("CAST(trunc({c}) AS BIGINT)"),
            _ => format!("CAST({c} AS DOUBLE)"),
        };
        match (self.c_style, self.kind) {
            (true, _) => format!("printf({}, {v})", q(&self.text())),
            (false, Kind::Percent) => {
                let inner = NumFmt {
                    kind: Kind::Fixed,
                    suffix: format!("%{}", self.suffix),
                    ..self.clone()
                };
                format!("format({}, {v} * 100)", q(&inner.text()))
            }
            (false, _) => format!("format({}, {v})", q(&self.text())),
        }
    }
}

/// Rust's `1.5e6` as Python writes it: `1.5e+06`.
fn exp_text(t: &str) -> String {
    let (m, e) = t.split_once('e').unwrap_or((t, "0"));
    let e: i32 = e.parse().unwrap_or(0);
    let sign = if e < 0 { '-' } else { '+' };
    format!("{m}e{sign}{:02}", e.abs())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Expected texts from Python 3 (`fmt.format(v)` / `fmt % v`).
    #[test]
    fn formats_as_python_does() {
        let f = |fmt: &str, v: f64| NumFmt::parse(fmt).unwrap().float(v);
        let i = |fmt: &str, v: i64| NumFmt::parse(fmt).unwrap().int(v);
        assert_eq!(f("{:,.2f}", 1234567.891), "1,234,567.89");
        assert_eq!(f("{:,.2f}", -0.005), "-0.01");
        assert_eq!(f("{:,.2f}", 0.125), "0.12");
        assert_eq!(f("{:,.2f}", 2.675), "2.67");
        assert_eq!(f("{:.1%}", 0.1234), "12.3%");
        assert_eq!(i("{:_d}", 1234567), "1_234_567");
        assert_eq!(f("${:,.2f}", -1234.5), "$-1,234.50");
        assert_eq!(f("{:.2f} kg", 3.256), "3.26 kg");
        assert_eq!(f("{:e}", 1234567.0), "1.234567e+06");
        assert_eq!(f("{:.2e}", 0.000123), "1.23e-04");
        assert_eq!(f("{:,.0f}", 1e15), "1,000,000,000,000,000");
        assert_eq!(f("%.3f", 2.0005), "2.001");
        assert_eq!(f("%d", -1.7), "-1");
        assert_eq!(f("%d", 1e20), "100000000000000000000");
        assert_eq!(f("%d", -0.5), "0");
        assert_eq!(f("%e", 12345.678), "1.234568e+04");
        assert_eq!(i("{:,d}", -1000), "-1,000");
        assert_eq!(f("{:.0f}", 0.5), "0");
        assert_eq!(f("{:.0f}", 1.5), "2");
        assert_eq!(f("{:.1f}", f64::NAN), "nan");
        assert_eq!(f("{:.1%}", f64::INFINITY), "inf%");
        assert_eq!(i("{:,}", 1234), "1,234");
        assert_eq!(i("{:.2f}", 1234), "1234.00");
        assert_eq!(i("{:.3%}", 1), "100.000%");
        assert_eq!(i("{0:,d}", 12), "12");
    }

    #[test]
    fn refuses_what_it_cant_write() {
        for bad in [
            "{:>10.2f}",
            "{:+.2f}",
            "{} {}",
            "{x:.2f}",
            "{:.2}",
            "%5.2f",
            "%s",
            "%.2f%%",
            "no field",
            "{:.2d}",
            "{:x}",
        ] {
            assert!(NumFmt::parse(bad).is_err(), "{bad}");
        }
        assert!(!NumFmt::parse("{:,d}").unwrap().fits_float());
        assert!(NumFmt::parse("%d").unwrap().fits_float());
    }

    #[test]
    fn digits_change_and_text_round_trips() {
        let f = NumFmt::parse("${:,.2f} kg").unwrap();
        assert_eq!(f.digits(), Some(2));
        assert_eq!(f.with_digits(3).text(), "${:,.3f} kg");
        assert_eq!(NumFmt::parse("{:e}").unwrap().digits(), Some(6));
        assert_eq!(NumFmt::parse("{:,d}").unwrap().digits(), None);
        for t in ["{:_.1%}", "%.4e", "%d units", "{:,d}"] {
            assert_eq!(NumFmt::parse(t).unwrap().text(), t);
        }
    }
}
