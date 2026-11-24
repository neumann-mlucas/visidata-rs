//! The random-walk panic probe (`scenario/probe.rs`) as a property: walks
//! of default chords and prompt answers on generated tables and the edge-case
//! files, on a normal and a tiny terminal. Proptest shrinks a failing walk
//! to its shortest form.

use std::sync::LazyLock;

use super::super::scenario::probe::{FILES, chords, unsafe_chord};
use super::*;

/// The chords a walk may press: none that saves, writes or quits.
static SAFE: LazyLock<Vec<String>> = LazyLock::new(|| {
    (chords().into_iter())
        .filter(|(_, d)| !unsafe_chord(d))
        .map(|(k, _)| k)
        .collect()
});

/// What follows a chord: nothing, or an answer to the prompt it may open.
const TAILS: &[&str] = &["", "<CR>", "j <CR>", "<Esc>", "a <CR>"];

fn press() -> impl Strategy<Value = String> {
    (0..SAFE.len(), proptest::sample::select(TAILS)).prop_map(|(c, t)| format!("{} {t}", SAFE[c]))
}

fn body() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => table().prop_map(|t| t.csv()),
        1 => proptest::sample::select(FILES).prop_map(|(_, b)| b.to_string()),
    ]
}

/// Press `keys` on a sheet of `body`; `Err` if the app is unhealthy after.
async fn walk(body: &str, keys: &str, (w, h): (u16, u16)) -> Result<(), String> {
    keymap_and_theme(None);
    let mut tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
    tmp.write_all(body.as_bytes()).unwrap();
    tmp.flush().unwrap();
    let loaded = crate::reader::open(tmp.path().as_os_str(), None).unwrap();
    let mut hn = Harness::with_source(loaded, tmp).await;
    hn.term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
    hn.keys(keys).await;
    hn.keys("<Esc>").await;
    match health(&hn) {
        None => Ok(()),
        Some(why) => Err(format!("{why}\n--- input.csv\n{body}--- keys\n{keys}")),
    }
}

proptest! {
    #![proptest_config(config(16))]

    /// No walk of safe chords panics or leaves the app unhealthy.
    #[test]
    fn random_walks_keep_the_app_healthy(
        body in body(),
        presses in proptest::collection::vec(press(), 1..=25),
        size in proptest::sample::select(&[(100u16, 12u16), (12, 3)][..]),
    ) {
        let keys = presses.join(" ");
        let r = RT.block_on(walk(&body, &keys, size));
        prop_assert!(r.is_ok(), "{}", r.unwrap_err());
    }
}
