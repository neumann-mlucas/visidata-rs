//! Seeded random keys through the real app. Fails on a panic or a broken
//! invariant; the assertion message gives the seed and the key tail to replay.

use super::scenario::{Harness, csv_text};

const KEYS: &[&str] = &[
    "j", "k", "h", "l", "G", "g g", "0", "$", "<C-d>", "<C-u>", "-", "g -", "!", "z !", "[", "]",
    "z o", "s", "t", "u", "g s", "g u", ",", "#", "%", "~", "@", "F", "S", "I", "W", "q", "<CR>",
    "g <CR>", "\"", "g \"", "U", "R", ".", "z s", "z c", "P", "n", "N", "z f", "1", "2", "5", "d",
    "g d", "x", "y", "z y", "z p", "g z p", "z x", "z d", "a", "p", "A", "z P",
];

/// Prompt openers paired with what to type; a tenth of the steps use these.
const PROMPTS: &[(&str, &str)] = &[
    ("/", "a"),
    ("z |", "^[ab]"),
    ("|", "1"),
    ("^", "renamed"),
    (":", "derive d2 = id * 2"),
    (":", "setcol 7"),
    (":", "sql SELECT * FROM self LIMIT 3"),
    (":", "filter grp a"),
    (":", "sort -id"),
    (":", "type currency"),
];

fn next(state: &mut u64) -> usize {
    // xorshift64: reproducible without a dependency
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state as usize
}

fn check_invariants(h: &Harness, seed: u64, log: &[&str]) {
    let s = h.app.sheets.active();
    let tail = &log[log.len().saturating_sub(12)..];
    let ctx = || format!("seed {seed}, last keys {tail:?}, sheet {:?}", s.name);
    if !s.is_loaded() {
        return; // a failed or dropped result: the next window restates this
    }
    let Some(df) = s.window() else { return };
    assert!(
        df.height() == 0 || s.view.row_cursor < df.height(),
        "row cursor {} past {} rows: {}",
        s.view.row_cursor,
        df.height(),
        ctx()
    );
    assert!(
        df.width() == 0 || s.view.col_cursor < df.width(),
        "col cursor {} past {} columns: {}",
        s.view.col_cursor,
        df.width(),
        ctx()
    );
    assert!(
        s.window_rids().is_empty() || s.window_rids().len() == df.height(),
        "{} row ids for {} rows: {}",
        s.window_rids().len(),
        df.height(),
        ctx()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn random_keys_keep_the_app_consistent() {
    for seed in 1..=8u64 {
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut h = Harness::new(40).await;
        let mut log: Vec<&str> = Vec::new();
        for _ in 0..250 {
            if next(&mut state).is_multiple_of(10) {
                let (open, text) = PROMPTS[next(&mut state) % PROMPTS.len()];
                log.extend([open, text]);
                h.keys(open).await;
                if h.app.input.is_some() {
                    h.type_(text).await;
                }
            } else {
                let k = KEYS[next(&mut state) % KEYS.len()];
                log.push(k);
                h.keys(k).await;
            }
            h.app.quit = false; // `q` on the last sheet
            check_invariants(&h, seed, &log);
        }
    }
}

/// Edits, then undo all of them: the view is what it was. Redo: the edits are back.
#[tokio::test(flavor = "multi_thread")]
async fn undo_then_redo_round_trips_the_view() {
    let mut h = Harness::new(30).await;
    let start = csv_text(h.app.sheets.active(), b',');
    let steps = [
        ("l", ""),
        ("-", ""),
        ("[", ""),
        ("z |", "1"),
        ("^", "renamed"),
        (":", "derive d2 = id * 2"),
        ("g s", ""),
        (":", "setcol 7"),
    ];
    for (keys, text) in steps {
        h.keys(keys).await;
        if h.app.input.is_some() {
            h.type_(text).await;
        }
    }
    let edited = csv_text(h.app.sheets.active(), b',');
    assert_ne!(start, edited);
    let depth = h.app.sheets.active().state.undo.len();
    for _ in 0..depth {
        h.keys("U").await;
    }
    assert_eq!(start, csv_text(h.app.sheets.active(), b','));
    for _ in 0..depth {
        h.keys("R").await;
    }
    assert_eq!(edited, csv_text(h.app.sheets.active(), b','));
}
