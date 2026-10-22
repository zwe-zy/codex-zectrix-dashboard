use std::collections::HashMap;
use std::path::PathBuf;

use codex_zectrix_dashboard::status_board::{
    BoardLocalTime, BoardTaskState, StatusBoardClock, StatusBoardRenderError, StatusBoardState,
    format_reset_time, format_updated_time, render_status_board,
};
use image::GrayImage;
use sha2::{Digest, Sha256};

const NOW: i64 = 1_788_134_400;
const TODAY_RESET: i64 = 1_788_165_000;
const TOMORROW_RESET: i64 = 1_788_218_100;
const LATER_RESET: i64 = 1_788_401_100;
const UPLOADED: i64 = 1_788_133_280;

struct FixedClock {
    now: i64,
    local_times: HashMap<i64, BoardLocalTime>,
}

impl StatusBoardClock for FixedClock {
    fn now_epoch_seconds(&self) -> i64 {
        self.now
    }

    fn local_time(&self, epoch_seconds: i64) -> Option<BoardLocalTime> {
        self.local_times.get(&epoch_seconds).copied()
    }
}

fn fixed_clock() -> FixedClock {
    FixedClock {
        now: NOW,
        local_times: HashMap::from([
            (NOW, BoardLocalTime::new(2026, 8, 31, 10, 0).unwrap()),
            (
                TODAY_RESET,
                BoardLocalTime::new(2026, 8, 31, 18, 30).unwrap(),
            ),
            (
                TOMORROW_RESET,
                BoardLocalTime::new(2026, 9, 1, 9, 15).unwrap(),
            ),
            (LATER_RESET, BoardLocalTime::new(2026, 9, 3, 12, 5).unwrap()),
            (UPLOADED, BoardLocalTime::new(2026, 8, 31, 9, 58).unwrap()),
        ]),
    }
}

fn state(task_state: BoardTaskState) -> StatusBoardState {
    StatusBoardState {
        task_state,
        quota_remaining_percent: Some(73.0),
        quota_resets_at_epoch_seconds: Some(TODAY_RESET),
        quota_stale: false,
        task_stale: false,
        uploaded_at_epoch_seconds: Some(UPLOADED),
        ..Default::default()
    }
}

fn decode(png: &[u8]) -> GrayImage {
    image::load_from_memory(png).unwrap().to_luma8()
}
#[test]
fn single_integer_window_renders_its_actual_duration() {
    for name in [
        "pop-cut",
        "eva",
        "pixel-console",
        "space-gauge",
        "cyber-hud",
    ] {
        let mut board = state(BoardTaskState::Idle);
        board.theme = name.parse().unwrap();
        let mut images = std::collections::HashSet::new();
        for duration in [Some(300), Some(10080), None] {
            board.quota_window_duration_mins = duration;
            assert!(
                images.insert(render_status_board(&board, &fixed_clock()).unwrap()),
                "{name} must distinguish hourly, weekly and unknown duration"
            );
        }
    }
}

#[test]
fn new_theme_numbers_never_overwrite_reset_labels() {
    use codex_zectrix_dashboard::status_board::BoardQuotaWindow;
    for name in ["pixel-console", "space-gauge", "cyber-hud"] {
        for dual in [false, true] {
            let mut board = state(BoardTaskState::Running);
            board.theme = name.parse().unwrap();
            board.quota_window_duration_mins = Some(10080);
            if dual {
                board.quota_secondary = Some(BoardQuotaWindow {
                    remaining_percent: 40.0,
                    resets_at_epoch_seconds: Some(LATER_RESET),
                    window_duration_mins: Some(10080),
                    stale: false,
                });
            }
            let region = match (name, dual) {
                ("pixel-console", false) => Region {
                    x: 216,
                    y: 139,
                    width: 160,
                    height: 100,
                },
                ("pixel-console", true) => Region {
                    x: 26,
                    y: 209,
                    width: 350,
                    height: 40,
                },
                ("cyber-hud", false) => Region {
                    x: 220,
                    y: 145,
                    width: 150,
                    height: 90,
                },
                ("cyber-hud", true) => Region {
                    x: 26,
                    y: 216,
                    width: 349,
                    height: 50,
                },
                _ => Region {
                    x: 22,
                    y: 253,
                    width: 355,
                    height: 20,
                },
            };
            let before = decode(&render_status_board(&board, &fixed_clock()).unwrap());
            for value in [0.0, 100.0, 99.5, 0.1, 99.99] {
                board.quota_remaining_percent = Some(value);
                if let Some(w) = board.quota_secondary.as_mut() {
                    w.remaining_percent = value;
                }
                let after = decode(&render_status_board(&board, &fixed_clock()).unwrap());
                assert_eq!(
                    fingerprint(&before, region),
                    fingerprint(&after, region),
                    "{name} {dual} {value}: digits must not cover reset/status labels"
                );
            }
        }
    }
}

#[test]
fn dual_window_keeps_pop_collage_halftone() {
    let mut board = state(BoardTaskState::Running);
    board.quota_secondary = Some(codex_zectrix_dashboard::status_board::BoardQuotaWindow {
        remaining_percent: 40.0,
        resets_at_epoch_seconds: None,
        window_duration_mins: Some(10080),
        stale: false,
    });
    let img = decode(&render_status_board(&board, &fixed_clock()).unwrap());
    assert_eq!(
        img.get_pixel(390, 42)[0],
        0,
        "dual layout lost the collage dot background"
    );
    assert_eq!(img.get_pixel(386, 42)[0], 255);
}

#[test]
fn pop_dual_digits_do_not_overlap_remaining_or_reset_labels() {
    let mut board = state(BoardTaskState::Running);
    board.quota_secondary = Some(codex_zectrix_dashboard::status_board::BoardQuotaWindow {
        remaining_percent: 40.0,
        resets_at_epoch_seconds: None,
        window_duration_mins: Some(10080),
        stale: false,
    });
    let before = decode(&render_status_board(&board, &fixed_clock()).unwrap());
    board.quota_remaining_percent = Some(99.5);
    let after = decode(&render_status_board(&board, &fixed_clock()).unwrap());
    assert_change_is_confined_to(
        &before,
        &after,
        Region {
            x: 20,
            y: 155,
            width: 165,
            height: 51,
        },
    );
}

#[test]
fn pop_secondary_uses_the_opposite_slant_of_the_reset_card() {
    let mut board = state(BoardTaskState::Running);
    board.quota_secondary = Some(codex_zectrix_dashboard::status_board::BoardQuotaWindow {
        remaining_percent: 99.5,
        resets_at_epoch_seconds: Some(LATER_RESET),
        window_duration_mins: Some(10080),
        stale: false,
    });
    let before = decode(&render_status_board(&board, &fixed_clock()).unwrap());
    assert_eq!(
        before.get_pixel(393, 151)[0],
        0,
        "second card must use the descending RESET frame edge"
    );
    board.quota_secondary.as_mut().unwrap().remaining_percent = 100.0;
    let after = decode(&render_status_board(&board, &fixed_clock()).unwrap());
    assert_change_is_confined_to(
        &before,
        &after,
        Region {
            x: 235,
            y: 154,
            width: 145,
            height: 51,
        },
    );
}

#[test]
fn all_five_themes_preserve_window_shape_and_render_status_extremes() {
    use codex_zectrix_dashboard::status_board::{BoardQuotaWindow, BoardTheme};
    let mut signatures = std::collections::HashSet::new();
    let directory = std::env::var_os("CODEX_ZECTRIX_THEME_PREVIEW_DIR").map(PathBuf::from);
    for name in [
        "pop-cut",
        "eva",
        "pixel-console",
        "space-gauge",
        "cyber-hud",
    ] {
        let theme: BoardTheme = name.parse().expect("registered render theme");
        for dual in [false, true] {
            for task_state in [
                BoardTaskState::Running,
                BoardTaskState::Idle,
                BoardTaskState::Completed,
                BoardTaskState::Failed,
                BoardTaskState::Interrupted,
                BoardTaskState::Empty,
                BoardTaskState::Unavailable,
            ] {
                let mut board = state(task_state);
                board.theme = theme;
                board.quota_window_duration_mins = Some(if dual { 300 } else { 10080 });
                if !dual {
                    board.quota_resets_at_epoch_seconds = Some(LATER_RESET);
                }
                board.is_sample = true;
                if dual {
                    board.quota_secondary = Some(BoardQuotaWindow {
                        remaining_percent: 40.0,
                        resets_at_epoch_seconds: Some(LATER_RESET),
                        window_duration_mins: Some(10080),
                        stale: false,
                    });
                }
                let png = render_status_board(&board, &fixed_clock()).unwrap();
                assert_eq!(ihdr(&png), (400, 300, 1, 0));
                let img = decode(&png);
                assert!(img.pixels().all(|p| matches!(p[0], 0 | 255)));
                assert!(
                    signatures.insert(format!("{:x}", Sha256::digest(&png))),
                    "theme/window/status lost in render"
                );
                if theme == BoardTheme::Eva {
                    // The hazard stripe and rail must survive both quota layouts.
                    assert_eq!(img.get_pixel(8, 8)[0], 0);
                    assert_eq!(img.get_pixel(24, 8)[0], 255);
                    assert_eq!(img.get_pixel(8, 180)[0], 0);
                }
                if task_state == BoardTaskState::Running {
                    if let Some(ref directory) = directory {
                        std::fs::create_dir_all(directory).unwrap();
                        std::fs::write(
                            directory.join(format!(
                                "{}-{}.png",
                                theme.as_str(),
                                if dual { "dual" } else { "single" }
                            )),
                            &png,
                        )
                        .unwrap();
                    }
                    let normal = png;
                    board.task_partial = true;
                    board.task_stale = true;
                    board.quota_stale = true;
                    if let Some(ref mut second) = board.quota_secondary {
                        second.stale = true;
                    }
                    board.quota_remaining_percent = Some(100.0);
                    assert_ne!(render_status_board(&board, &fixed_clock()).unwrap(), normal);
                    if let Some(ref directory) = directory {
                        std::fs::write(
                            directory.join(format!(
                                "{}-{}-full-stale.png",
                                theme.as_str(),
                                if dual { "dual" } else { "single" }
                            )),
                            render_status_board(&board, &fixed_clock()).unwrap(),
                        )
                        .unwrap();
                    }
                    board.quota_remaining_percent = Some(99.5);
                    assert_eq!(
                        ihdr(&render_status_board(&board, &fixed_clock()).unwrap()),
                        (400, 300, 1, 0)
                    );
                    if let Some(ref directory) = directory {
                        std::fs::write(
                            directory.join(format!(
                                "{}-{}-fractional-stale.png",
                                theme.as_str(),
                                if dual { "dual" } else { "single" }
                            )),
                            render_status_board(&board, &fixed_clock()).unwrap(),
                        )
                        .unwrap();
                    }
                    let fractional = render_status_board(&board, &fixed_clock()).unwrap();
                    board.quota_remaining_percent = Some(0.0);
                    let zero = render_status_board(&board, &fixed_clock()).unwrap();
                    assert_ne!(fractional, zero);
                    board.quota_remaining_percent = Some(99.5);
                    // A secondary-only observation must use the same single-window layout.
                    let mut secondary_only = board.clone();
                    secondary_only.quota_remaining_percent = None;
                    secondary_only.quota_secondary = Some(BoardQuotaWindow {
                        remaining_percent: 99.5,
                        resets_at_epoch_seconds: board.quota_resets_at_epoch_seconds,
                        window_duration_mins: board.quota_window_duration_mins,
                        stale: board.quota_stale,
                    });
                    board.quota_secondary = None;
                    assert_eq!(
                        render_status_board(&secondary_only, &fixed_clock()).unwrap(),
                        render_status_board(&board, &fixed_clock()).unwrap()
                    );
                    board.quota_remaining_percent = None;
                    board.quota_resets_at_epoch_seconds = None;
                    board.quota_window_duration_mins = None;
                    let missing = render_status_board(&board, &fixed_clock()).unwrap();
                    if let Some(ref directory) = directory {
                        std::fs::write(
                            directory.join(format!("{}-missing.png", theme.as_str())),
                            &missing,
                        )
                        .unwrap();
                    }
                    assert_ne!(missing, zero);
                    assert_eq!(ihdr(&missing), (400, 300, 1, 0));
                }
            }
        }
    }
}
#[test]
fn single_fractional_window_does_not_draw_a_second_window() {
    let mut board = state(BoardTaskState::Idle);
    board.quota_remaining_percent = Some(75.5);
    let image = decode(&render_status_board(&board, &fixed_clock()).unwrap());
    assert_eq!(
        image.get_pixel(206, 190)[0],
        255,
        "no divider for a nonexistent second window"
    );
}

#[test]
fn dual_window_layout_handles_fractional_missing_and_stale_values() {
    let mut board = state(BoardTaskState::Idle);
    board.quota_remaining_percent = Some(75.5);
    board.quota_window_duration_mins = Some(300);
    board.quota_secondary = Some(codex_zectrix_dashboard::status_board::BoardQuotaWindow {
        remaining_percent: 70.0,
        resets_at_epoch_seconds: None,
        window_duration_mins: Some(10080),
        stale: true,
    });
    let png = render_status_board(&board, &fixed_clock()).unwrap();
    let img = decode(&png);
    assert_eq!(img.dimensions(), (400, 300));
    assert!(img.pixels().all(|pixel| matches!(pixel[0], 0 | 255)));
    if let Some(directory) = std::env::var_os("CODEX_ZECTRIX_PREVIEW_DIR") {
        let directory = PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("dual-windows.png"), png).unwrap();
    }
}

fn ihdr(png: &[u8]) -> (u32, u32, u8, u8) {
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(&png[12..16], b"IHDR");
    (
        u32::from_be_bytes(png[16..20].try_into().unwrap()),
        u32::from_be_bytes(png[20..24].try_into().unwrap()),
        png[24],
        png[25],
    )
}

fn black_ratio(image: &GrayImage) -> f64 {
    let black = image.pixels().filter(|pixel| pixel[0] == 0).count();
    black as f64 / f64::from(image.width() * image.height())
}

#[derive(Clone, Copy)]
struct Region {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl Region {
    fn contains(self, x: u32, y: u32) -> bool {
        (self.x..self.x + self.width).contains(&x) && (self.y..self.y + self.height).contains(&y)
    }
}

fn region_pixels(image: &GrayImage, region: Region) -> Vec<u8> {
    (region.y..region.y + region.height)
        .flat_map(|y| (region.x..region.x + region.width).map(move |x| image.get_pixel(x, y)[0]))
        .collect()
}

fn region_black_ratio(image: &GrayImage, region: Region) -> f64 {
    let pixels = region_pixels(image, region);
    let black = pixels.iter().filter(|pixel| **pixel == 0).count();
    black as f64 / pixels.len() as f64
}

fn fingerprint(image: &GrayImage, region: Region) -> String {
    format!("{:x}", Sha256::digest(region_pixels(image, region)))
}

fn assert_change_is_confined_to(before: &GrayImage, after: &GrayImage, region: Region) {
    let mut changed = 0;
    for y in 0..before.height() {
        for x in 0..before.width() {
            if before.get_pixel(x, y) != after.get_pixel(x, y) {
                changed += 1;
                assert!(
                    region.contains(x, y),
                    "unexpected changed pixel at ({x}, {y})"
                );
            }
        }
    }
    assert!(changed > 0, "expected a visible change inside the region");
}

#[test]
fn approved_p_composition_keeps_the_collage_geometry() {
    let image =
        decode(&render_status_board(&state(BoardTaskState::Running), &fixed_clock()).unwrap());

    let top_wedge = Region {
        x: 0,
        y: 0,
        width: 210,
        height: 48,
    };
    let dotted_top_right = Region {
        x: 260,
        y: 0,
        width: 130,
        height: 70,
    };
    let status_card = Region {
        x: 30,
        y: 82,
        width: 165,
        height: 52,
    };
    let footer_left = Region {
        x: 0,
        y: 270,
        width: 150,
        height: 30,
    };
    let footer_right = Region {
        x: 300,
        y: 270,
        width: 100,
        height: 30,
    };

    assert!(region_black_ratio(&image, top_wedge) > 0.82);
    assert!((0.08..0.30).contains(&region_black_ratio(&image, dotted_top_right)));
    assert!(region_black_ratio(&image, status_card) > 0.62);
    assert!(region_black_ratio(&image, footer_left) < 0.35);
    assert!(region_black_ratio(&image, footer_right) > 0.62);
}

#[test]
fn stacked_labels_use_the_approved_reverse_contrast_pattern() {
    let image =
        decode(&render_status_board(&state(BoardTaskState::Running), &fixed_clock()).unwrap());

    let quota_tiles = [
        Region {
            x: 31,
            y: 235,
            width: 21,
            height: 23,
        },
        Region {
            x: 54,
            y: 233,
            width: 21,
            height: 23,
        },
        Region {
            x: 77,
            y: 236,
            width: 21,
            height: 23,
        },
        Region {
            x: 100,
            y: 232,
            width: 21,
            height: 23,
        },
    ];
    assert!(region_black_ratio(&image, quota_tiles[0]) < 0.45);
    assert!(region_black_ratio(&image, quota_tiles[1]) < 0.45);
    assert!(region_black_ratio(&image, quota_tiles[2]) > 0.55);
    assert!(region_black_ratio(&image, quota_tiles[3]) < 0.45);

    let reset_s_tile = Region {
        x: 253,
        y: 104,
        width: 15,
        height: 25,
    };
    let reset_r_tile = Region {
        x: 227,
        y: 104,
        width: 13,
        height: 25,
    };
    assert!(region_black_ratio(&image, reset_s_tile) < 0.50);
    assert!(region_black_ratio(&image, reset_r_tile) > 0.50);
}

#[test]
fn each_task_state_uses_only_its_approved_white_focus_tile() {
    let cases = [
        (
            BoardTaskState::Running,
            Region {
                x: 103,
                y: 108,
                width: 26,
                height: 28,
            },
            Region {
                x: 35,
                y: 107,
                width: 26,
                height: 28,
            },
        ),
        (
            BoardTaskState::Completed,
            Region {
                x: 103,
                y: 108,
                width: 26,
                height: 28,
            },
            Region {
                x: 35,
                y: 107,
                width: 26,
                height: 28,
            },
        ),
        (
            BoardTaskState::Failed,
            Region {
                x: 120,
                y: 108,
                width: 26,
                height: 28,
            },
            Region {
                x: 52,
                y: 107,
                width: 26,
                height: 28,
            },
        ),
        (
            BoardTaskState::Interrupted,
            Region {
                x: 120,
                y: 108,
                width: 26,
                height: 28,
            },
            Region {
                x: 52,
                y: 107,
                width: 26,
                height: 28,
            },
        ),
        (
            BoardTaskState::Empty,
            Region {
                x: 52,
                y: 107,
                width: 26,
                height: 28,
            },
            Region {
                x: 86,
                y: 104,
                width: 26,
                height: 28,
            },
        ),
        (
            BoardTaskState::Unavailable,
            Region {
                x: 120,
                y: 108,
                width: 26,
                height: 28,
            },
            Region {
                x: 52,
                y: 107,
                width: 26,
                height: 28,
            },
        ),
    ];

    for (task_state, white_focus, black_peer) in cases {
        let image = decode(&render_status_board(&state(task_state), &fixed_clock()).unwrap());
        assert!(
            region_black_ratio(&image, white_focus) < 0.48,
            "{task_state:?} focus tile should be white"
        );
        assert!(
            region_black_ratio(&image, black_peer) > 0.52,
            "{task_state:?} non-focus tile should be black"
        );
    }
}

#[test]
fn black_status_tiles_have_no_white_outline() {
    let image =
        decode(&render_status_board(&state(BoardTaskState::Running), &fixed_clock()).unwrap());

    for (x, y) in [(32, 104), (66, 101), (134, 102), (168, 106)] {
        assert_eq!(
            image.get_pixel(x, y)[0],
            0,
            "black status tile unexpectedly has a white edge at ({x}, {y})"
        );
    }
}

#[test]
fn quota_value_stays_inside_the_inset_area_of_its_white_frame() {
    let clock = fixed_clock();
    let mut full = state(BoardTaskState::Running);
    full.quota_remaining_percent = Some(100.0);
    let full = decode(&render_status_board(&full, &clock).unwrap());
    let mut missing = state(BoardTaskState::Running);
    missing.quota_remaining_percent = None;
    let missing = decode(&render_status_board(&missing, &clock).unwrap());

    assert_change_is_confined_to(
        &full,
        &missing,
        Region {
            x: 20,
            y: 158,
            width: 190,
            height: 69,
        },
    );
}

#[test]
fn quota_digits_have_a_visible_but_small_slant() {
    let mut board = state(BoardTaskState::Running);
    board.quota_remaining_percent = Some(11.0);
    let image = decode(&render_status_board(&board, &fixed_clock()).unwrap());
    let mut left_edges = Vec::new();

    for y in 158..227 {
        if let Some(x) = (30..105).find(|x| image.get_pixel(*x, y)[0] == 0) {
            left_edges.push(x as i32);
        }
    }

    assert!(left_edges.len() >= 16, "expected visible quota digit rows");
    let top = left_edges.iter().take(8).sum::<i32>() / 8;
    let bottom = left_edges.iter().rev().take(8).sum::<i32>() / 8;
    assert!(
        bottom - top >= 8,
        "quota slant is too subtle: top edge {top}, bottom edge {bottom}"
    );
}

#[test]
fn every_task_state_renders_a_distinct_binary_400_by_300_board() {
    let states = [
        BoardTaskState::Running,
        BoardTaskState::Completed,
        BoardTaskState::Failed,
        BoardTaskState::Interrupted,
        BoardTaskState::Empty,
        BoardTaskState::Unavailable,
    ];
    let mut rendered = Vec::new();

    for task_state in states {
        let png = render_status_board(&state(task_state), &fixed_clock()).unwrap();
        let image = decode(&png);
        assert_eq!(image.dimensions(), (400, 300));
        assert!(image.pixels().all(|pixel| matches!(pixel[0], 0 | 255)));
        assert!((0.18..0.62).contains(&black_ratio(&image)));

        rendered.push(png);
    }

    for left in 0..rendered.len() {
        for right in left + 1..rendered.len() {
            assert_ne!(rendered[left], rendered[right]);
        }
    }
}

#[test]
fn every_state_has_a_distinct_label_inside_the_p_status_card() {
    let label = Region {
        x: 30,
        y: 98,
        width: 180,
        height: 47,
    };
    let states = [
        BoardTaskState::Running,
        BoardTaskState::Completed,
        BoardTaskState::Failed,
        BoardTaskState::Interrupted,
        BoardTaskState::Empty,
        BoardTaskState::Unavailable,
    ];
    let fingerprints: Vec<_> = states
        .into_iter()
        .map(|task_state| {
            let image = decode(&render_status_board(&state(task_state), &fixed_clock()).unwrap());
            fingerprint(&image, label)
        })
        .collect();

    for left in 0..fingerprints.len() {
        for right in left + 1..fingerprints.len() {
            assert_ne!(fingerprints[left], fingerprints[right]);
        }
    }
}

#[test]
fn quota_values_have_distinct_glyphs_inside_the_white_burst() {
    let clock = fixed_clock();
    let quota_region = Region {
        x: 20,
        y: 140,
        width: 190,
        height: 112,
    };
    let fingerprints: Vec<_> = [Some(0.0), Some(73.0), Some(100.0), None]
        .into_iter()
        .map(|quota| {
            let mut board = state(BoardTaskState::Running);
            board.quota_remaining_percent = quota;
            let image = decode(&render_status_board(&board, &clock).unwrap());
            fingerprint(&image, quota_region)
        })
        .collect();

    for left in 0..fingerprints.len() {
        for right in left + 1..fingerprints.len() {
            assert_ne!(fingerprints[left], fingerprints[right]);
        }
    }
}

#[test]
fn missing_quota_changes_only_the_quota_value_region() {
    let clock = fixed_clock();
    let available = decode(&render_status_board(&state(BoardTaskState::Running), &clock).unwrap());
    let mut missing = state(BoardTaskState::Running);
    missing.quota_remaining_percent = None;
    let missing = decode(&render_status_board(&missing, &clock).unwrap());

    assert_change_is_confined_to(
        &available,
        &missing,
        Region {
            x: 20,
            y: 140,
            width: 198,
            height: 112,
        },
    );
}

#[test]
fn reset_text_and_missing_placeholder_change_only_the_reset_text_region() {
    let clock = fixed_clock();
    let available = decode(&render_status_board(&state(BoardTaskState::Running), &clock).unwrap());
    let mut missing = state(BoardTaskState::Running);
    missing.quota_resets_at_epoch_seconds = None;
    let missing = decode(&render_status_board(&missing, &clock).unwrap());
    let reset_text = Region {
        x: 240,
        y: 128,
        width: 142,
        height: 78,
    };

    assert_change_is_confined_to(&available, &missing, reset_text);
    assert_ne!(
        fingerprint(&available, reset_text),
        fingerprint(&missing, reset_text)
    );
}

#[test]
fn updated_time_and_missing_placeholder_change_only_the_updated_time_region() {
    let clock = fixed_clock();
    let available = decode(&render_status_board(&state(BoardTaskState::Running), &clock).unwrap());
    let mut missing = state(BoardTaskState::Running);
    missing.uploaded_at_epoch_seconds = None;
    let missing = decode(&render_status_board(&missing, &clock).unwrap());
    let updated_time = Region {
        x: 344,
        y: 274,
        width: 48,
        height: 16,
    };

    assert_change_is_confined_to(&available, &missing, updated_time);
    assert_ne!(
        fingerprint(&available, updated_time),
        fingerprint(&missing, updated_time)
    );
}

#[test]
fn each_stale_flag_changes_only_its_single_marker_region() {
    let clock = fixed_clock();
    let fresh = decode(&render_status_board(&state(BoardTaskState::Running), &clock).unwrap());
    let mut task_stale = state(BoardTaskState::Running);
    task_stale.task_stale = true;
    let task_stale = decode(&render_status_board(&task_stale, &clock).unwrap());
    let task_marker = Region {
        x: 200,
        y: 274,
        width: 76,
        height: 20,
    };
    assert_change_is_confined_to(&fresh, &task_stale, task_marker);
    assert_ne!(
        fingerprint(&fresh, task_marker),
        fingerprint(&task_stale, task_marker)
    );

    let mut quota_stale = state(BoardTaskState::Running);
    quota_stale.quota_stale = true;
    let quota_stale = decode(&render_status_board(&quota_stale, &clock).unwrap());
    let quota_marker = Region {
        x: 140,
        y: 228,
        width: 66,
        height: 22,
    };
    assert_change_is_confined_to(&fresh, &quota_stale, quota_marker);
    assert_ne!(
        fingerprint(&fresh, quota_marker),
        fingerprint(&quota_stale, quota_marker)
    );
}

#[test]
fn png_ihdr_is_true_one_bit_grayscale_and_decodes_to_binary_pixels() {
    let png = render_status_board(&state(BoardTaskState::Running), &fixed_clock()).unwrap();

    assert_eq!(ihdr(&png), (400, 300, 1, 0));
    let image = decode(&png);
    assert_eq!(image.dimensions(), (400, 300));
    assert!(image.pixels().all(|pixel| matches!(pixel[0], 0 | 255)));
}

#[test]
fn stale_flags_and_quota_boundaries_each_change_the_visible_frame() {
    let clock = fixed_clock();
    let base = render_status_board(&state(BoardTaskState::Running), &clock).unwrap();

    let mut task_stale = state(BoardTaskState::Running);
    task_stale.task_stale = true;
    assert_ne!(render_status_board(&task_stale, &clock).unwrap(), base);

    let mut quota_stale = state(BoardTaskState::Running);
    quota_stale.quota_stale = true;
    assert_ne!(render_status_board(&quota_stale, &clock).unwrap(), base);

    let mut zero = state(BoardTaskState::Running);
    zero.quota_remaining_percent = Some(0.0);
    let mut full = state(BoardTaskState::Running);
    full.quota_remaining_percent = Some(100.0);
    assert_ne!(
        render_status_board(&zero, &clock).unwrap(),
        render_status_board(&full, &clock).unwrap()
    );
}

#[test]
fn renderer_rejects_a_directly_constructed_out_of_range_quota() {
    let mut invalid = state(BoardTaskState::Running);
    invalid.quota_remaining_percent = Some(101.0);

    assert!(matches!(
        render_status_board(&invalid, &fixed_clock()),
        Err(StatusBoardRenderError::QuotaOutOfRange)
    ));
}

#[test]
fn reset_and_updated_labels_use_only_explicit_timestamps() {
    let clock = fixed_clock();
    assert_eq!(format_reset_time(None, &clock).unwrap(), "--:--");
    assert_eq!(
        format_reset_time(Some(TODAY_RESET), &clock).unwrap(),
        "今天 18:30"
    );
    assert_eq!(
        format_reset_time(Some(TOMORROW_RESET), &clock).unwrap(),
        "明天 09:15"
    );
    assert_eq!(
        format_reset_time(Some(LATER_RESET), &clock).unwrap(),
        "09-03 12:05"
    );
    assert_eq!(format_updated_time(None, &clock).unwrap(), "--:--");
    assert_eq!(
        format_updated_time(Some(UPLOADED), &clock).unwrap(),
        "09:58"
    );
}

#[test]
fn rendering_is_deterministic_for_the_same_state_and_clock() {
    let board = state(BoardTaskState::Completed);
    let clock = fixed_clock();

    assert_eq!(
        render_status_board(&board, &clock).unwrap(),
        render_status_board(&board, &clock).unwrap()
    );
}

#[test]
fn write_all_seven_previews_when_explicitly_requested() {
    if std::env::var_os("UPDATE_PREVIEWS").as_deref() != Some(std::ffi::OsStr::new("1")) {
        return;
    }

    let output_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("p-dashboard-previews");
    std::fs::create_dir_all(&output_dir).unwrap();
    let clock = fixed_clock();
    let mut fractional = state(BoardTaskState::Running);
    fractional.quota_remaining_percent = Some(75.5);
    fractional.quota_window_duration_mins = Some(300);
    fractional.task_partial = true;
    std::fs::write(
        output_dir.join("single-fractional-partial.png"),
        render_status_board(&fractional, &clock).unwrap(),
    )
    .unwrap();
    let mut secondary = fractional.clone();
    secondary.quota_remaining_percent = None;
    secondary.quota_secondary = Some(codex_zectrix_dashboard::status_board::BoardQuotaWindow {
        remaining_percent: 99.5,
        window_duration_mins: Some(10080),
        resets_at_epoch_seconds: Some(TOMORROW_RESET),
        stale: false,
    });
    std::fs::write(
        output_dir.join("secondary-only-partial.png"),
        render_status_board(&secondary, &clock).unwrap(),
    )
    .unwrap();
    let cases = [
        ("running.png", BoardTaskState::Running),
        ("completed.png", BoardTaskState::Completed),
        ("failed.png", BoardTaskState::Failed),
        ("interrupted.png", BoardTaskState::Interrupted),
        ("empty.png", BoardTaskState::Empty),
        ("unavailable.png", BoardTaskState::Unavailable),
    ];
    for (filename, task_state) in cases {
        std::fs::write(
            output_dir.join(filename),
            render_status_board(&state(task_state), &clock).unwrap(),
        )
        .unwrap();
    }

    let mut stale = state(BoardTaskState::Running);
    stale.task_stale = true;
    stale.quota_stale = true;
    std::fs::write(
        output_dir.join("stale.png"),
        render_status_board(&stale, &clock).unwrap(),
    )
    .unwrap();
}
