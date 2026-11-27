//! The option registry (S-37 P0c): every `vd.option` with its type, default
//! and help. `vd.option`, `:set` and the `O` sheet check values through it.

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Bool,
    /// 0 or more.
    Count,
    Choice(&'static [&'static str]),
    /// A command line, not empty.
    Command,
    /// `%.Nf` / `{:.Nf}`, N up to 15.
    FloatFmt,
    /// Any text, kept as typed: blanks count (`value_joiner` is a space).
    Text,
}

#[derive(Debug)]
pub struct Opt {
    pub name: &'static str,
    pub kind: Kind,
    /// As `O` shows it; `""`: none (detected, or every digit).
    pub default: &'static str,
    /// `:set` takes effect now; else on the next start.
    pub live: bool,
    pub help: &'static str,
}

/// By name.
pub const OPTIONS: &[Opt] = &[
    Opt {
        name: "clipboard_copy_cmd",
        kind: Kind::Command,
        default: "",
        live: true,
        help: "program that copies to the system clipboard; default: detected, else OSC 52",
    },
    Opt {
        name: "clipboard_paste_cmd",
        kind: Kind::Command,
        default: "",
        live: true,
        help: "program that prints the system clipboard; default: detected",
    },
    Opt {
        name: "csv_skipinitialspace",
        kind: Kind::Bool,
        default: "true",
        live: true,
        help: "later .csv opens drop blanks at the start of each cell and name",
    },
    Opt {
        name: "disp_float_fmt",
        kind: Kind::FloatFmt,
        default: "",
        live: true,
        help: "decimals of float columns (%.2f); default: shortest exact text",
    },
    Opt {
        name: "disp_footer",
        kind: Kind::Bool,
        default: "false",
        live: true,
        help: "a totals row of + aggregators under the table (:footer)",
    },
    Opt {
        name: "disp_header_rule",
        kind: Kind::Bool,
        default: "false",
        live: true,
        help: "a ─ rule under the header row",
    },
    Opt {
        name: "duckdb_cmd",
        kind: Kind::Command,
        default: "duckdb",
        live: true,
        help: "the duckdb program, for .duckdb files and :exec-sql",
    },
    Opt {
        name: "header",
        kind: Kind::Count,
        default: "1",
        live: true,
        help: "rows read as column names by later opens (csv, tsv, xlsx, fixed, usv); 0: column_1, column_2, …",
    },
    Opt {
        name: "histogram_bins",
        kind: Kind::Count,
        default: "0",
        live: true,
        help: "ranges F counts with numeric_binning; 0: the square root of the rows",
    },
    Opt {
        name: "incr_base",
        kind: Kind::Count,
        default: "1",
        live: true,
        help: "first number of i, gi and :addcol-incr-key",
    },
    Opt {
        name: "keymap",
        kind: Kind::Choice(&["visidata", "vim"]),
        default: "visidata",
        live: false,
        help: "vim rebinds the keys where vim and VisiData disagree",
    },
    Opt {
        name: "mouse",
        kind: Kind::Bool,
        default: "false",
        live: false,
        help: "wheel and clicks move the cursor",
    },
    Opt {
        name: "name_joiner",
        kind: Kind::Text,
        default: "_",
        live: true,
        help: "text between column names that :join-cols joins",
    },
    Opt {
        name: "numeric_binning",
        kind: Kind::Bool,
        default: "false",
        live: true,
        help: "F on a number column counts ranges, not values",
    },
    Opt {
        name: "session_autosave",
        kind: Kind::Bool,
        default: "true",
        live: true,
        help: "save the session on quit",
    },
    Opt {
        name: "source_max_rows",
        kind: Kind::Count,
        default: "1000000",
        live: true,
        help: "a Lua source with poll keeps its newest rows up to this; 0: all",
    },
    Opt {
        name: "unfurl_empty",
        kind: Kind::Bool,
        default: "false",
        live: true,
        help: "zM keeps a row, key and value empty, for an empty list",
    },
    Opt {
        name: "validation",
        kind: Kind::Choice(&["refuse", "warn"]),
        default: "refuse",
        live: true,
        help: "an edit failing :validate: refused, or kept with a warning",
    },
    Opt {
        name: "value_joiner",
        kind: Kind::Text,
        default: " ",
        live: true,
        help: "text between the values that :join-cols joins",
    },
    Opt {
        name: "xlsx_meta_columns",
        kind: Kind::Bool,
        default: "false",
        live: true,
        help: "later xlsx opens add X_fontColor and X_fillColor (ARGB) after each column X",
    },
];

pub fn find(name: &str) -> Option<&'static Opt> {
    OPTIONS.iter().find(|o| o.name == name)
}

/// `value` for option `name`, as it's stored (`" 3"` → `"3"`); an error says
/// what it takes.
pub fn check(name: &str, value: &str) -> Result<String, String> {
    let Some(o) = find(name) else {
        let names: Vec<&str> = OPTIONS.iter().map(|o| o.name).collect();
        return Err(format!("unknown option {name} ({})", names.join(", ")));
    };
    let v = value.trim();
    match o.kind {
        Kind::Bool => match v {
            "true" | "false" => Ok(v.into()),
            _ => Err(format!("{name} is \"true\" or \"false\"")),
        },
        Kind::Count => v
            .parse::<usize>()
            .map(|n| n.to_string())
            .map_err(|_| format!("{name} is a count, 0 or more (got {value:?})")),
        Kind::Choice(cs) => match cs.contains(&v) {
            true => Ok(v.into()),
            false => {
                let cs: Vec<String> = cs.iter().map(|c| format!("{c:?}")).collect();
                Err(format!("{name} is {}", cs.join(" or ")))
            }
        },
        Kind::Command if v.is_empty() => Err(format!("{name} is empty")),
        Kind::Command => Ok(value.into()),
        Kind::Text => Ok(value.into()),
        Kind::FloatFmt => match crate::plugins::float_fmt_digits(v) {
            Some(_) => Ok(v.into()),
            None => Err(format!(
                "{name} is \"%.Nf\" or \"{{:.Nf}}\", N up to 15 (got {value:?})"
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_by_type() {
        assert_eq!(check("histogram_bins", " 7").unwrap(), "7");
        assert!(check("histogram_bins", "-1").is_err());
        assert!(
            check("mouse", "yes")
                .unwrap_err()
                .contains("\"true\" or \"false\"")
        );
        assert!(
            check("keymap", "emacs")
                .unwrap_err()
                .contains("\"visidata\" or \"vim\"")
        );
        assert!(check("duckdb_cmd", " ").is_err());
        assert_eq!(check("disp_float_fmt", "{:.3f}").unwrap(), "{:.3f}");
        assert!(check("disp_float_fmt", "%d").is_err());
        assert_eq!(check("value_joiner", " ").unwrap(), " ");
        assert!(
            check("nope", "x")
                .unwrap_err()
                .starts_with("unknown option nope (clipboard_copy_cmd,")
        );
        let mut names: Vec<&str> = OPTIONS.iter().map(|o| o.name).collect();
        names.sort();
        assert!(
            names.iter().zip(OPTIONS).all(|(n, o)| *n == o.name),
            "keep OPTIONS by name"
        );
        assert!(
            OPTIONS
                .iter()
                .all(|o| o.default.is_empty() || check(o.name, o.default).is_ok())
        );
    }
}
