//! Zip archives (S-42 D): the list of members, VisiData's columns; `<CR>`
//! opens one through the reader for its extension.

use std::io::Write;
use std::path::Path;

use polars::prelude::*;

use super::{LoadedSource, OpenError};

fn bad(e: impl std::fmt::Display) -> OpenError {
    OpenError::Polars(polars_err!(ComputeError: "zip: {e}"))
}

fn archive(path: &Path) -> Result<::zip::ZipArchive<std::fs::File>, OpenError> {
    ::zip::ZipArchive::new(std::fs::File::open(path)?).map_err(bad)
}

/// `a/b.csv` → (`a/`, `b.csv`); a top-level member's directory is `.`,
/// as VisiData's.
fn split(name: &str) -> (String, String) {
    let name = name.trim_end_matches('/');
    match name.rsplit_once('/') {
        Some((dir, file)) => (format!("{dir}/"), file.to_string()),
        None => (".".into(), name.to_string()),
    }
}

/// One row per member: `directory filename ext size compressed_size date_time`.
pub(super) fn index(path: &Path) -> Result<DataFrame, OpenError> {
    let mut zip = archive(path)?;
    let n = zip.len();
    let (mut dirs, mut files, mut exts) = (Vec::new(), Vec::new(), Vec::new());
    let (mut sizes, mut packed, mut times) = (Vec::new(), Vec::new(), Vec::new());
    for i in 0..n {
        let f = zip.by_index(i).map_err(bad)?;
        let name = f.name().map_err(bad)?.to_string();
        let (dir, file) = split(&name);
        let ext = match f.is_dir() {
            true => "/".to_string(),
            false => Path::new(&file)
                .extension()
                .map_or_else(String::new, |e| e.to_string_lossy().into_owned()),
        };
        dirs.push(dir);
        files.push(file);
        exts.push(ext);
        sizes.push(f.size() as i64);
        packed.push(f.compressed_size() as i64);
        times.push(f.last_modified().map(|t| {
            format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                t.year(),
                t.month(),
                t.day(),
                t.hour(),
                t.minute(),
                t.second()
            )
        }));
    }
    DataFrame::new_infer_height(vec![
        Column::new("directory".into(), dirs),
        Column::new("filename".into(), files),
        Column::new("ext".into(), exts),
        Column::new("size".into(), sizes),
        Column::new("compressed_size".into(), packed),
        Column::new("date_time".into(), times),
    ])
    .map_err(OpenError::from)
}

/// The member at index row (`directory`, `filename`), opened by its
/// extension from a tempfile the source keeps alive.
pub(super) fn open_member(path: &Path, member: &str) -> Result<LoadedSource, OpenError> {
    let mut zip = archive(path)?;
    let mut f = zip.by_name(member).map_err(bad)?;
    // only the extension of the member's name reaches the filesystem
    let ext = Path::new(member)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let mut tmp = tempfile::Builder::new().suffix(&ext).tempfile()?;
    std::io::copy(&mut f, &mut tmp)?;
    tmp.flush()?;
    let kind = super::detect(Path::new(member))?;
    let mut loaded = super::open_path(tmp.path(), kind)?;
    loaded.display_name = format!("{}:{member}", path.display());
    loaded.path = Some(path.to_owned());
    // a .gz member or a JSONL with comments already copied it to its own tempfile
    if loaded._tempfile.is_none() {
        loaded._tempfile = Some(tmp);
    }
    Ok(loaded)
}

/// The member a list row names: `directory` + `filename` (`.` is the top).
pub fn member_name(directory: &str, filename: &str) -> String {
    match directory {
        "." | "" => filename.to_string(),
        d => format!("{d}{filename}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn members_list_and_open() {
        let mut t = tempfile::Builder::new().suffix(".zip").tempfile().unwrap();
        {
            let mut z = ::zip::ZipWriter::new(&mut t);
            z.start_file("d/a.csv", ::zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(b"x,y\n1,2\n").unwrap();
            z.start_file("top.csv", ::zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(b"z\n3\n").unwrap();
            z.finish().unwrap();
        }
        let df = index(t.path()).unwrap();
        let col = |c: &str| -> Vec<String> {
            let c = df.column(c).unwrap();
            c.str()
                .unwrap()
                .iter()
                .map(|v| v.unwrap_or("").to_string())
                .collect()
        };
        assert_eq!(col("directory"), ["d/", "."]);
        assert_eq!(col("filename"), ["a.csv", "top.csv"]);
        assert_eq!(member_name("d/", "a.csv"), "d/a.csv");
        let m = open_member(t.path(), "d/a.csv").unwrap();
        assert_eq!(m.lazy.collect().unwrap().shape(), (1, 2));
    }
}
