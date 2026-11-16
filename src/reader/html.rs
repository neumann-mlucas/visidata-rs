//! HTML pages (S-42 D): VisiData's list of a page's tables plus `links`;
//! `<CR>` opens one. Cells are text; a column whose cells hold links gets
//! `<name>_link<i>` columns after it.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use polars::prelude::*;
use scraper::{ElementRef, Html, Selector};

use super::{OpenError, unique_headers};

#[expect(clippy::expect_used, reason = "literal selectors")]
fn sel(s: &str) -> Selector {
    Selector::parse(s).expect("valid selector")
}

fn page(path: &Path) -> Result<Html, OpenError> {
    let bytes = std::fs::read(path)?;
    Ok(Html::parse_document(&String::from_utf8_lossy(&bytes)))
}

/// XPath `normalize-space`.
fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A table's name: its `id`, else `table_<i>` (i among all the page's tables).
fn table_name(e: &ElementRef, i: usize) -> String {
    e.value()
        .attr("id")
        .map_or_else(|| format!("table_{i}"), String::from)
}

/// `th` without a `scope`, or `scope="col"`.
fn is_header(cell: &ElementRef) -> bool {
    cell.value().name() == "th" && matches!(cell.value().attr("scope"), None | Some("" | "col"))
}

/// A cell: its text (each text piece trimmed, joined by a space) and links.
type Cell = (Option<String>, Vec<Option<String>>);

/// A table as text columns, VisiData's `HtmlTableSheet.iterload`.
fn table_frame(table: &ElementRef) -> Result<DataFrame, OpenError> {
    let a = sel("a");
    let mut headers: Vec<Vec<Option<String>>> = Vec::new();
    let mut maxlinks: HashMap<usize, usize> = HashMap::new();
    let mut ncols = 0;
    let mut spans: BTreeMap<usize, (usize, Cell)> = BTreeMap::new();
    let mut rows: Vec<Vec<Cell>> = Vec::new();
    for (rownum, tr) in table.select(&sel("tr")).enumerate() {
        let mut row: BTreeMap<usize, Cell> = BTreeMap::new();
        for (c, (left, cell)) in std::mem::take(&mut spans) {
            *maxlinks.entry(c).or_default() =
                maxlinks.get(&c).copied().unwrap_or(0).max(cell.1.len());
            row.insert(c, cell.clone());
            if left > 1 {
                spans.insert(c, (left - 1, cell));
            }
        }
        let mut colnum = 0;
        if let Some(h) = headers.get(rownum) {
            while colnum < h.len() && h[colnum].is_some() {
                colnum += 1;
            }
        }
        let cells: Vec<ElementRef> = tr.children().filter_map(ElementRef::wrap).collect();
        let has_data = cells.iter().any(|c| !is_header(c));
        let mut is_data_row = false;
        for cell in &cells {
            let num = |k: &str| {
                cell.value()
                    .attr(k)
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(1)
                    .max(1)
            };
            let (colspan, rowspan) = (num("colspan"), num("rowspan"));
            let mut text: String = cell.text().map(str::trim).collect::<Vec<_>>().join(" ");
            let links: Vec<Option<String>> = cell
                .select(&a)
                .map(|x| x.value().attr("href").map(String::from))
                .collect();
            if is_header(cell) && !has_data {
                let m = maxlinks.entry(colnum).or_default();
                *m = (*m).max(links.len());
                for k in rownum..rownum + rowspan {
                    if headers.len() <= k {
                        headers.resize(k + 1, Vec::new());
                    }
                    for j in colnum..colnum + colspan {
                        if headers[k].len() <= j {
                            headers[k].resize(j + 1, None);
                        }
                        headers[k][j] = Some(text.clone());
                    }
                    text = String::new(); // later rows of a rowspan: empty, not None
                }
            } else {
                is_data_row = true;
                while row.contains_key(&colnum) {
                    colnum += 1;
                }
                let m = maxlinks.entry(colnum).or_default();
                *m = (*m).max(links.len());
                let cell = (Some(text), links);
                if rowspan > 1 {
                    for j in colnum..colnum + colspan {
                        spans.insert(j, (rowspan - 1, cell.clone()));
                    }
                }
                row.insert(colnum, cell);
            }
            colnum += colspan;
        }
        if is_data_row && let Some((&last, _)) = row.last_key_value() {
            ncols = ncols.max(last + 1);
            let mut list: Vec<Cell> = vec![(None, Vec::new()); ncols];
            for (i, v) in row {
                list[i] = v;
            }
            rows.push(list);
        }
    }
    // column names: the header rows joined by `_`, else the first row
    let names: Vec<String> = if !headers.is_empty() {
        let width = headers.iter().map(Vec::len).max().unwrap_or(0);
        (0..width)
            .map(|j| {
                let parts: Vec<&str> = headers
                    .iter()
                    .filter_map(|h| h.get(j).and_then(|v| v.as_deref()))
                    .filter(|v| !v.is_empty())
                    .collect();
                parts.join("_")
            })
            .collect()
    } else if rows.is_empty() {
        Vec::new()
    } else {
        let first = rows.remove(0);
        (0..ncols)
            .map(|i| match first.get(i) {
                Some((text, links)) => {
                    let mut parts: Vec<String> =
                        text.iter().filter(|t| !t.is_empty()).cloned().collect();
                    if !links.is_empty() {
                        let l: Vec<String> = links
                            .iter()
                            .map(|l| l.as_deref().map_or("None".into(), |l| format!("'{l}'")))
                            .collect();
                        parts.push(format!("[{}]", l.join(", ")));
                    }
                    parts.join("_")
                }
                None => String::new(),
            })
            .collect()
    };
    let mut cols: Vec<(String, Vec<Option<String>>)> = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let values = rows
            .iter()
            .map(|r| r.get(i).and_then(|c| c.0.clone()))
            .collect();
        cols.push((name.clone(), values));
        for j in 0..maxlinks.get(&i).copied().unwrap_or(0) {
            let values = rows
                .iter()
                .map(|r| r.get(i).and_then(|c| c.1.get(j).cloned().flatten()))
                .collect();
            cols.push((format!("{name}_link{j}"), values));
        }
    }
    // VisiData strips a column's name
    let unique = unique_headers(cols.iter().map(|(n, _)| n.trim().to_string()));
    let columns: Vec<Column> = unique
        .into_iter()
        .zip(cols)
        .map(|(n, (_, v))| Column::new(n.into(), v))
        .collect();
    if columns.is_empty() {
        return Ok(DataFrame::empty());
    }
    DataFrame::new_infer_height(columns).map_err(OpenError::from)
}

/// Elements with a link attribute: `tag attribute text link`.
fn links_frame(doc: &Html) -> Result<DataFrame, OpenError> {
    let (mut tags, mut attrs, mut texts, mut links) = (vec![], vec![], vec![], vec![]);
    for e in doc.root_element().descendent_elements() {
        for attr in ["href", "src", "action"] {
            if let Some(v) = e.value().attr(attr) {
                tags.push(e.value().name().to_string());
                attrs.push(attr);
                let text = e
                    .children()
                    .next()
                    .and_then(|n| n.value().as_text().map(|t| t.to_string()));
                texts.push(text);
                links.push(v.to_string());
            }
        }
    }
    DataFrame::new_infer_height(vec![
        Column::new("tag".into(), tags),
        Column::new("attribute".into(), attrs),
        Column::new("text".into(), texts),
        Column::new("link".into(), links),
    ])
    .map_err(OpenError::from)
}

/// The page's tables and `links`, VisiData's `HtmlTablesSheet` columns:
/// `name rows cols id classes title aria_label caption summary heading`.
pub(super) fn index(path: &Path) -> Result<DataFrame, OpenError> {
    let doc = page(path)?;
    let headings = ["h1", "h2", "h3", "h4", "h5", "h6"];
    let mut out: Vec<[Option<String>; 10]> = Vec::new();
    let attr = |e: &ElementRef, k: &str| e.value().attr(k).map(String::from);
    for (i, t) in doc.select(&sel("table")).enumerate() {
        let df = table_frame(&t)?;
        let caption = t
            .children()
            .filter_map(ElementRef::wrap)
            .find(|c| c.value().name() == "caption")
            .map(|c| normalize(&c.text().collect::<String>()));
        let heading = t
            .prev_siblings()
            .filter_map(ElementRef::wrap)
            .find(|e| headings.contains(&e.value().name()))
            .map(|h| normalize(&h.text().collect::<String>()))
            .filter(|h| !h.is_empty());
        out.push([
            Some(table_name(&t, i)),
            Some(df.height().to_string()),
            Some(df.width().to_string()),
            attr(&t, "id"),
            attr(&t, "class"),
            attr(&t, "title"),
            attr(&t, "aria-label"),
            caption,
            attr(&t, "summary"),
            heading,
        ]);
    }
    let root = doc.root_element();
    // VisiData counts the links sheet's hidden `element` column too
    let links = links_frame(&doc)?.height().to_string();
    out.push([
        Some("links".into()),
        Some(links),
        Some("5".into()),
        attr(&root, "id"),
        attr(&root, "class"),
        attr(&root, "title"),
        attr(&root, "aria-label"),
        None,
        attr(&root, "summary"),
        None,
    ]);
    let names = [
        "name",
        "rows",
        "cols",
        "id",
        "classes",
        "title",
        "aria_label",
        "caption",
        "summary",
        "heading",
    ];
    let mut cols: Vec<Column> = Vec::new();
    for (k, n) in names.iter().enumerate() {
        let v: Vec<Option<String>> = out.iter().map(|r| r[k].clone()).collect();
        let c = Column::new((*n).into(), v);
        cols.push(match *n {
            "rows" | "cols" => c.cast(&DataType::Int64)?,
            _ => c,
        });
    }
    DataFrame::new_infer_height(cols).map_err(OpenError::from)
}

/// One part of the page: a table by name, or `links`.
pub(super) fn part(path: &Path, name: &str) -> Result<DataFrame, OpenError> {
    let doc = page(path)?;
    if name == "links" {
        return links_frame(&doc);
    }
    doc.select(&sel("table"))
        .enumerate()
        .find(|(i, t)| table_name(t, *i) == name)
        .map_or_else(
            || {
                Err(OpenError::Polars(
                    polars_err!(ComputeError: "html: no table {name}"),
                ))
            },
            |(_, t)| table_frame(&t),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(html: &str) -> DataFrame {
        let doc = Html::parse_document(html);
        let t = doc.select(&sel("table")).next().unwrap();
        table_frame(&t).unwrap()
    }

    fn texts(df: &DataFrame, c: &str) -> Vec<String> {
        let c = df.column(c).unwrap();
        c.str()
            .unwrap()
            .iter()
            .map(|v| v.unwrap_or("-").to_string())
            .collect()
    }

    #[test]
    fn tables_read_as_visidata() {
        // header rows join with `_`; a rowspan repeats down; links get columns
        let df = frame(
            "<table><tr><th colspan=2>top</th></tr><tr><th>a</th><th>b</th></tr>\
             <tr><td rowspan=2>x</td><td><a href='u'>y</a></td></tr><tr><td>z</td></tr></table>",
        );
        let names: Vec<String> = df
            .get_column_names()
            .iter()
            .map(|n| n.to_string())
            .collect();
        assert_eq!(names, ["top_a", "top_b", "top_b_link0"]);
        assert_eq!(texts(&df, "top_a"), ["x", "x"]);
        assert_eq!(texts(&df, "top_b"), ["y", "z"]);
        assert_eq!(texts(&df, "top_b_link0"), ["u", "-"]);
        // no header row: the first row names the columns
        let df = frame("<table><tr><td>n</td><td>v</td></tr><tr><td>1</td><td>2</td></tr></table>");
        assert_eq!(texts(&df, "v"), ["2"]);
    }
}
