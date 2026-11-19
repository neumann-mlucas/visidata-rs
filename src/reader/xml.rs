//! `.xml` as VisiData's XmlSheet (S-53): a row per element in document order,
//! its `tag` (without a namespace prefix), `children` (child elements) and
//! `text` (before its first child, lxml's `text`), then a column per
//! attribute, first seen first.

use std::path::Path;

use polars::prelude::*;
use quick_xml::XmlVersion;
use quick_xml::events::Event;

use super::{OpenError, unique_headers};

struct Element {
    tag: String,
    children: i64,
    text: Option<String>,
    attrs: Vec<(String, String)>,
}

fn bad(e: impl std::fmt::Display) -> OpenError {
    OpenError::Polars(polars_err!(ComputeError: "xml: {e}"))
}

pub(super) fn elements(path: &Path) -> Result<DataFrame, OpenError> {
    let text = std::fs::read_to_string(path)?;
    let mut reader = quick_xml::Reader::from_str(&text);
    let mut rows: Vec<Element> = Vec::new();
    // open elements: the row, and whether its `text` still takes text (no child yet)
    let mut open: Vec<(usize, bool)> = Vec::new();
    loop {
        let event = reader.read_event().map_err(bad)?;
        let text = match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                if let Some((parent, takes)) = open.last_mut() {
                    rows[*parent].children += 1;
                    *takes = false;
                }
                let mut attrs = Vec::new();
                for a in e.attributes() {
                    let a = a.map_err(bad)?;
                    let v = a.normalized_value(XmlVersion::Implicit1_0).map_err(bad)?;
                    attrs.push((a.key.local_name().into_inner().to_string(), v.into_owned()));
                }
                rows.push(Element {
                    tag: e.local_name().into_inner().to_string(),
                    children: 0,
                    text: None,
                    attrs,
                });
                if matches!(event, Event::Start(_)) {
                    open.push((rows.len() - 1, true));
                }
                continue;
            }
            Event::End(_) => {
                open.pop();
                continue;
            }
            Event::Text(t) => t.xml10_content().into_owned(),
            Event::CData(t) => t.xml10_content().into_owned(),
            Event::GeneralRef(r) => match r.resolve_char_ref().map_err(bad)? {
                Some(c) => c.to_string(),
                None => {
                    let name = r.into_inner();
                    quick_xml::escape::resolve_predefined_entity(&name)
                        .ok_or_else(|| bad(format!("unknown entity &{name};")))?
                        .to_string()
                }
            },
            Event::Eof => break,
            _ => continue,
        };
        if let Some(&(row, true)) = open.last() {
            rows[row].text.get_or_insert_default().push_str(&text);
        }
    }
    let mut names: Vec<String> = Vec::new();
    for (k, _) in rows.iter().flat_map(|r| &r.attrs) {
        if !names.contains(k) {
            names.push(k.clone());
        }
    }
    let headers = unique_headers(
        ["tag", "children", "text"]
            .map(String::from)
            .into_iter()
            .chain(names.clone()),
    );
    let mut cols = vec![
        Column::new(
            headers[0].as_str().into(),
            rows.iter().map(|r| r.tag.as_str()).collect::<Vec<_>>(),
        ),
        Column::new(
            headers[1].as_str().into(),
            rows.iter().map(|r| r.children).collect::<Vec<_>>(),
        ),
        Column::new(
            headers[2].as_str().into(),
            rows.iter().map(|r| r.text.as_deref()).collect::<Vec<_>>(),
        ),
    ];
    for (k, h) in names.iter().zip(&headers[3..]) {
        let v: Vec<Option<&str>> = (rows.iter())
            .map(|r| {
                r.attrs
                    .iter()
                    .find(|(n, _)| n == k)
                    .map(|(_, v)| v.as_str())
            })
            .collect();
        cols.push(Column::new(h.as_str().into(), v));
    }
    DataFrame::new_infer_height(cols).map_err(OpenError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elements_in_document_order_with_text_and_attributes() {
        let f = tempfile::Builder::new().suffix(".xml").tempfile().unwrap();
        let xml = "<?xml version='1.0'?>\n<data a=\"1\">\n  <row id=\"x &amp; y\"><i>0</i><n>Beef &amp; Salmon&#33;</n><e/></row>\n</data>\n";
        std::fs::write(f.path(), xml).unwrap();
        let df = elements(f.path()).unwrap();
        let text = |c: &str| -> Vec<Option<String>> {
            let c = df.column(c).unwrap().cast(&DataType::String).unwrap();
            c.str()
                .unwrap()
                .iter()
                .map(|v| v.map(str::to_string))
                .collect()
        };
        let s = |v: &str| Some(v.to_string());
        assert_eq!(
            df.get_column_names(),
            ["tag", "children", "text", "a", "id"]
        );
        assert_eq!(text("tag"), [s("data"), s("row"), s("i"), s("n"), s("e")]);
        assert_eq!(text("children"), [s("1"), s("3"), s("0"), s("0"), s("0")]);
        assert_eq!(
            text("text"),
            [s("\n  "), None, s("0"), s("Beef & Salmon!"), None]
        );
        assert_eq!(text("a"), [s("1"), None, None, None, None]);
        assert_eq!(text("id"), [None, s("x & y"), None, None, None]);
    }
}
