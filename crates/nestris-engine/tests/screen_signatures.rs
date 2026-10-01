//! Screen classification (signature mode) on frames from real station
//! captures (`testdata/screens/frames`: MS2109 card, PAL console, event ROM).
//! `canon_*` are rectified 256×240 frames, `raw_*` downscaled raw frames
//! seen before any geometry lock.

use std::path::PathBuf;

use nestris_engine::enums::GameState;
use nestris_engine::state::screen::ScreenClassifier;
use nestris_vision::Image;

fn frames_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/screens/frames")
}

/// Load an RGB/RGBA PNG as a BGR image.
fn load(name: &str) -> Image {
    let path = frames_dir().join(name);
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let mut reader = decoder.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).unwrap();
    let step = match info.color_type {
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        other => panic!("{name}: unexpected {other:?}"),
    };
    let (w, h) = (info.width as usize, info.height as usize);
    let mut data = Vec::with_capacity(w * h * 3);
    for px in buf[..info.buffer_size()].chunks_exact(step) {
        data.extend_from_slice(&[px[2], px[1], px[0]]);
    }
    Image::from_vec(data, w, h, 3)
}

fn classify_canon(name: &str, prev: Option<GameState>) -> GameState {
    let canon = load(name);
    let mut c = ScreenClassifier::new();
    c.classify_frame(&canon, Some(&canon), prev, None, None)
        .state
}

fn classify_raw(name: &str, prev: Option<GameState>) -> GameState {
    let raw = load(name);
    let mut c = ScreenClassifier::new();
    c.classify_frame(&raw, None, prev, None, None).state
}

#[test]
fn locked_menus_are_recognized() {
    for (name, want) in [
        ("canon_title.png", GameState::Title),
        ("canon_type_select.png", GameState::TypeSelect),
        ("canon_level_select.png", GameState::LevelSelect),
        ("canon_highscore_entry.png", GameState::HighscoreEntry),
        ("canon_ending.png", GameState::GameOver),
    ] {
        assert_eq!(classify_canon(name, None), want, "{name}");
        // Coming from a menu the state changes at once.
        assert_eq!(classify_canon(name, Some(GameState::Title)), want, "{name}");
    }
}

#[test]
fn boot_screens_are_not_menus() {
    for name in ["canon_boot_copyright.png", "canon_boot_flashcart.png"] {
        assert_eq!(classify_canon(name, None), GameState::Unknown, "{name}");
    }
}

#[test]
fn gameplay_pause_and_curtain() {
    assert_eq!(
        classify_canon("canon_in_game.png", Some(GameState::InGame)),
        GameState::InGame
    );
    // The blanked pause ("PAUSE" on black) is a pause, not a lost signal.
    assert_eq!(
        classify_canon("canon_pause.png", Some(GameState::InGame)),
        GameState::Paused
    );
    assert_eq!(
        classify_canon("canon_pause.png", Some(GameState::Paused)),
        GameState::Paused
    );
    // Dark purple curtain (level 7 palette): full striped rows from the top.
    assert_eq!(
        classify_canon("canon_curtain.png", Some(GameState::InGame)),
        GameState::GameOver
    );
}

#[test]
fn leaving_a_game_needs_consecutive_menu_frames() {
    let canon = load("canon_level_select.png");
    let mut c = ScreenClassifier::new();
    let states: Vec<GameState> = (0..6)
        .map(|_| {
            c.classify_frame(&canon, Some(&canon), Some(GameState::InGame), None, None)
                .state
        })
        .collect();
    assert_eq!(states[0], GameState::Unknown);
    assert_eq!(*states.last().unwrap(), GameState::LevelSelect);
}

#[test]
fn menus_without_geometry_use_the_frame_box() {
    for (name, want) in [
        ("raw_title.png", GameState::Title),
        ("raw_type_select.png", GameState::TypeSelect),
        ("raw_level_select.png", GameState::LevelSelect),
    ] {
        assert_eq!(classify_raw(name, None), want, "{name}");
    }
}

#[test]
fn black_and_uniform_frames_have_no_signal() {
    for name in ["raw_black.png", "raw_gray.png"] {
        assert_eq!(classify_raw(name, None), GameState::NoSignal, "{name}");
    }
}
