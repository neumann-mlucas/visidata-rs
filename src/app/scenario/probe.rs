//! Panic probes: default chords on edge-case sheets, on normal and tiny
//! terminals. They are slow (about 2 minutes), so they run only on request:
//! `cargo nextest run --run-ignored only probe_`. `PROBE_SEED=<n>` picks
//! another set of random walks.
use super::*;

/// Edge-case CSVs: header only, wide Unicode, one cell, blank cells.
pub(in crate::app) const FILES: &[(&str, &str)] = &[
    ("header", "a,b\n"),
    ("wide", "名前,emo\n日本語テキスト,👍🏽\né,x\n,\n"),
    ("one", "x\n1\n"),
    ("nulls", "a,b\n,\n,\n"),
];

/// Default sheet chords as key strings, with their descriptions.
pub(in crate::app) fn chords() -> Vec<(String, String)> {
    Keymap::defaults()
        .chords(crate::keymap::Layer::Sheet)
        .iter()
        .map(|c| {
            let ks: Vec<String> = c.on.iter().map(|k| k.to_string()).collect();
            (ks.join(" "), c.desc.to_string())
        })
        .collect()
}

/// A chord that may write a file in the working directory or quit.
pub(in crate::app) fn unsafe_chord(desc: &str) -> bool {
    desc.contains("save") || desc.contains("write") || desc.contains("quit")
}

/// Press `keys` on a fresh sheet of `body`, on a `w`×`h` terminal. Runs on
/// its own thread and runtime, so the caller sees a panic as `false`.
fn survives(body: &str, keys: &str, w: u16, h: u16) -> bool {
    let (body, keys) = (body.to_string(), keys.to_string());
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
            write!(tmp, "{body}").unwrap();
            tmp.flush().unwrap();
            let loaded = crate::reader::open(tmp.path().as_os_str(), None).unwrap();
            let mut hn = Harness::with_source(loaded, tmp).await;
            hn.term = Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            hn.keys(&keys).await;
            hn.keys("<Esc>").await;
        });
    })
    .join()
    .is_ok()
}

const SIZES: [(u16, u16); 2] = [(100, 12), (12, 3)];

/// Each chord alone, then nothing, Enter, or `j` Enter (an empty or
/// moved-to prompt answer).
#[test]
#[ignore = "slow; run on request"]
fn probe_every_chord() {
    let mut fails = Vec::new();
    for (name, body) in FILES {
        for (chord, desc) in chords() {
            for tail in ["", "<CR>", "j <CR>"] {
                if unsafe_chord(&desc) && !tail.is_empty() {
                    continue;
                }
                for (w, h) in SIZES {
                    if !survives(body, &format!("{chord} {tail}"), w, h) {
                        fails.push(format!("{name} {w}x{h}: {chord:?} then {tail:?} ({desc})"));
                    }
                }
            }
        }
    }
    assert!(fails.is_empty(), "{}", fails.join("\n"));
}

/// 60 random walks of 25 chords, each followed by a random prompt answer.
#[test]
#[ignore = "slow; run on request"]
fn probe_random_walks() {
    let chords: Vec<String> = chords()
        .into_iter()
        .filter(|(_, d)| !unsafe_chord(d))
        .map(|(k, _)| k)
        .collect();
    let tails = ["", "<CR>", "j <CR>", "<Esc>", "a <CR>"];
    let mut seed: u64 = std::env::var("PROBE_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    // Knuth's MMIX LCG: no rand dependency for a test.
    let mut next = move || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (seed >> 33) as usize
    };
    let mut fails = Vec::new();
    for walk in 0..60 {
        let (name, body) = FILES[walk % FILES.len()];
        let mut keys = Vec::new();
        for _ in 0..25 {
            keys.push(chords[next() % chords.len()].as_str());
            keys.push(tails[next() % tails.len()]);
        }
        let keys = keys.join(" ");
        let (w, h) = SIZES[walk % 2];
        if !survives(body, &keys, w, h) {
            fails.push(format!("{name} {w}x{h}: {keys}"));
        }
    }
    assert!(fails.is_empty(), "{}", fails.join("\n\n"));
}
