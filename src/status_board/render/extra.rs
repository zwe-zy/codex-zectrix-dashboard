//! Pocket, instrument and HUD compositions share data semantics, not decoration.
use super::*;
use crate::status_board::BoardQuotaWindow;

fn label(
    c: &mut Canvas,
    text: &str,
    x: i32,
    y: i32,
    inverted: bool,
) -> Result<(), StatusBoardRenderError> {
    draw_text(
        &FontRenderer::new::<u8g2_font_wqy13_t_gb2312>(),
        text,
        x,
        y,
        if inverted {
            BinaryColor::Off
        } else {
            BinaryColor::On
        },
        c,
    )
}
fn small(
    c: &mut Canvas,
    text: &str,
    x: i32,
    y: i32,
    inverted: bool,
) -> Result<(), StatusBoardRenderError> {
    draw_text(
        &FontRenderer::new::<u8g2_font_6x12_tf>(),
        text,
        x,
        y,
        if inverted {
            BinaryColor::Off
        } else {
            BinaryColor::On
        },
        c,
    )
}
fn status(
    c: &mut Canvas,
    s: &StatusBoardState,
    y: i32,
    inverted: bool,
) -> Result<(), StatusBoardRenderError> {
    draw_text(
        &FontRenderer::new::<u8g2_font_wqy16_t_gb2312>(),
        format!("任务 / {}", s.task_state.large_label()).as_str(),
        27,
        y,
        if inverted {
            BinaryColor::Off
        } else {
            BinaryColor::On
        },
        c,
    )?;
    small(c, s.task_state.small_code(), 310, y - 1, inverted)
}
fn flags(
    c: &mut Canvas,
    s: &StatusBoardState,
    y: i32,
    inverted: bool,
) -> Result<(), StatusBoardRenderError> {
    let mut flags = Vec::new();
    if s.is_sample {
        flags.push("测试画面");
    }
    if s.task_partial {
        flags.push("部分可见");
    }
    if s.task_stale {
        flags.push("任务延迟");
    }
    label(c, flags.join("  ").as_str(), 27, y, inverted)
}
fn footer(c: &mut Canvas, updated: &str, inverted: bool) -> Result<(), StatusBoardRenderError> {
    small(c, "GROUP: codex", 24, 295, inverted)?;
    label(
        c,
        format!("画面更新 {updated}").as_str(),
        273,
        295,
        inverted,
    )
}
fn reset(
    window: Option<&BoardQuotaWindow>,
    clock: &impl StatusBoardClock,
) -> Result<String, StatusBoardRenderError> {
    format_reset_time(window.and_then(|w| w.resets_at_epoch_seconds), clock)
}
fn quota_label(window: Option<&BoardQuotaWindow>) -> &'static str {
    if window.is_some_and(|w| w.stale) {
        "剩余 / 数据延迟"
    } else {
        "剩余额度"
    }
}
fn meter(c: &mut Canvas, x: i32, y: i32, width: i32, value: f64, inverted: bool) {
    let (ink, paper) = if inverted { (255, 0) } else { (0, 255) };
    let step = width / 10;
    for index in 0..10 {
        c.fill_rect(x + index * step, y, step - 3, 9, ink);
        if f64::from(index + 1) > (value / 10.0).ceil() {
            c.fill_rect(x + index * step + 1, y + 1, step - 5, 7, paper);
        }
    }
}

// Scale the actual glyph bounds so 100%, fractional values and --% all fit.
fn fitted_number(
    c: &mut Canvas,
    value: Option<f64>,
    rect: (i32, i32, i32, i32),
    inverted: bool,
) -> Result<(), StatusBoardRenderError> {
    let mut mask = Canvas::new();
    draw_text(
        &FontRenderer::new::<u8g2_font_logisoso42_tf>(),
        themed::percent(value).as_str(),
        0,
        60,
        BinaryColor::On,
        &mut mask,
    )?;
    let mut bounds = (240, 80, 0, 0);
    for y in 0..80 {
        for x in 0..240 {
            if mask.pixels[(y * WIDTH as i32 + x) as usize] == 0 {
                bounds.0 = bounds.0.min(x);
                bounds.1 = bounds.1.min(y);
                bounds.2 = bounds.2.max(x);
                bounds.3 = bounds.3.max(y);
            }
        }
    }
    let (x, y, width, height) = rect;
    let sw = bounds.2 - bounds.0 + 1;
    let sh = bounds.3 - bounds.1 + 1;
    let scale = (f64::from(width) / f64::from(sw)).min(f64::from(height) / f64::from(sh));
    let dw = (f64::from(sw) * scale).floor() as i32;
    let dh = (f64::from(sh) * scale).floor() as i32;
    for dy in 0..dh {
        for dx in 0..dw {
            let sx = bounds.0 + dx * sw / dw;
            let sy = bounds.1 + dy * sh / dh;
            if mask.pixels[(sy * WIDTH as i32 + sx) as usize] == 0 {
                c.set_pixel(
                    x + (width - dw) / 2 + dx,
                    y + (height - dh) / 2 + dy,
                    if inverted { 255 } else { 0 },
                );
            }
        }
    }
    Ok(())
}

fn pixel_number(
    c: &mut Canvas,
    value: Option<f64>,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> Result<(), StatusBoardRenderError> {
    let formatted = themed::percent(value);
    let digits = formatted.trim_end_matches('%');
    let columns: i32 = digits
        .chars()
        .map(|ch| if ch == '.' { 2 } else { 4 })
        .sum::<i32>()
        - 1;
    let cell = ((width - 22) / columns).min(height / 5);
    let left = x + (width - 22 - columns * cell) / 2;
    let top = y + (height - cell * 5) / 2;
    let mut offset = 0;
    for ch in digits.chars() {
        let rows = match ch {
            '0' => [7, 5, 5, 5, 7],
            '1' => [2, 6, 2, 2, 7],
            '2' => [7, 1, 7, 4, 7],
            '3' => [7, 1, 7, 1, 7],
            '4' => [5, 5, 7, 1, 1],
            '5' => [7, 4, 7, 1, 7],
            '6' => [7, 4, 7, 5, 7],
            '7' => [7, 1, 1, 1, 1],
            '8' => [7, 5, 7, 5, 7],
            '9' => [7, 5, 7, 1, 7],
            '.' => [0, 0, 0, 0, 4],
            _ => [0, 0, 7, 0, 0],
        };
        for (row, bits) in rows.into_iter().enumerate() {
            for col in 0..3 {
                if bits & (4 >> col) != 0 {
                    c.fill_rect(
                        left + (offset + col) * cell,
                        top + row as i32 * cell,
                        cell - 1,
                        cell - 1,
                        0,
                    );
                }
            }
        }
        offset += if ch == '.' { 2 } else { 4 };
    }
    draw_text(
        &FontRenderer::new::<u8g2_font_helvB14_tf>(),
        "%",
        x + width - 19,
        top + cell * 5,
        BinaryColor::On,
        c,
    )
}

pub(super) fn draw_pocket(
    c: &mut Canvas,
    s: &StatusBoardState,
    clock: &impl StatusBoardClock,
    updated: &str,
) -> Result<(), StatusBoardRenderError> {
    let values = themed::windows(s);
    let dual = values.len() == 2;
    c.stroke_polygon(
        &[
            (23, 11),
            (377, 11),
            (377, 21),
            (387, 21),
            (387, 269),
            (377, 269),
            (377, 279),
            (23, 279),
            (23, 269),
            (13, 269),
            (13, 21),
            (23, 21),
        ],
        3,
        0,
    );
    c.fill_rect(24, 24, 352, 28, 0);
    small(c, "CODEX // POCKET", 34, 43, true)?;
    small(c, "C-01", 340, 43, true)?;
    status(c, s, 77, false)?;
    flags(c, s, 98, false)?;
    c.line(25, 106, 374, 106, 2, 0);
    if dual {
        c.line(200, 115, 200, 263, 1, 0);
        for (index, window) in values.iter().enumerate() {
            let x = 28 + index as i32 * 184;
            label(
                c,
                window_label(window.window_duration_mins).as_str(),
                x,
                126,
                false,
            )?;
            pixel_number(c, Some(window.remaining_percent), x, 140, 160, 60)?;
            label(c, quota_label(Some(window)), x, 223, false)?;
            label(
                c,
                format!("重置 {}", reset(Some(window), clock)?).as_str(),
                x,
                243,
                false,
            )?;
            meter(c, x, 254, 160, window.remaining_percent, false);
        }
    } else {
        let w = values.first();
        label(
            c,
            window_label(w.and_then(|w| w.window_duration_mins)).as_str(),
            28,
            126,
            false,
        )?;
        pixel_number(c, w.map(|w| w.remaining_percent), 29, 145, 177, 85)?;
        small(c, "RESET", 229, 154, false)?;
        label(c, reset(w, clock)?.as_str(), 229, 178, false)?;
        label(c, quota_label(w), 229, 216, false)?;
        if let Some(w) = w {
            meter(c, 28, 254, 346, w.remaining_percent, false);
        } else {
            small(c, "QUOTA UNAVAILABLE", 28, 263, false)?;
        }
    }
    footer(c, updated, false)
}

fn dial(c: &mut Canvas, cx: i32, cy: i32, radius: i32, value: Option<f64>) {
    let point = |angle: f64, r: i32| {
        (
            cx + (angle.cos() * f64::from(r)).round() as i32,
            cy + (angle.sin() * f64::from(r)).round() as i32,
        )
    };
    for index in 0..=20 {
        let angle = std::f64::consts::PI * (1.0 + f64::from(index) / 20.0);
        let (x1, y1) = point(angle, radius);
        let (x2, y2) = point(angle, radius - if index % 5 == 0 { 12 } else { 6 });
        c.line(x1, y1, x2, y2, if index % 5 == 0 { 3 } else { 1 }, 0);
    }
    if let Some(value) = value {
        let (x, y) = point(std::f64::consts::PI * (1.0 + value / 100.0), radius - 18);
        c.line(cx, cy, x, y, 3, 0);
        c.circle(cx, cy, 5, 0, true);
        c.circle(cx, cy, 2, 255, true);
    }
}

pub(super) fn draw_gauge(
    c: &mut Canvas,
    s: &StatusBoardState,
    clock: &impl StatusBoardClock,
    updated: &str,
) -> Result<(), StatusBoardRenderError> {
    c.fill_rect(0, 0, 400, 36, 0);
    small(c, "MISSION / CODEX", 24, 24, true)?;
    small(c, "C-01", 350, 24, true)?;
    status(c, s, 63, false)?;
    flags(c, s, 85, false)?;
    c.line(20, 94, 380, 94, 2, 0);
    let values = themed::windows(s);
    if values.len() == 2 {
        c.line(200, 104, 200, 270, 1, 0);
        for (index, w) in values.iter().enumerate() {
            let x = 24 + index as i32 * 190;
            label(
                c,
                window_label(w.window_duration_mins).as_str(),
                x,
                114,
                false,
            )?;
            if w.stale {
                label(c, "额度延迟", x + 105, 114, false)?;
            }
            dial(c, x + 81, 191, 66, Some(w.remaining_percent));
            fitted_number(c, Some(w.remaining_percent), (x + 1, 205, 160, 38), false)?;
            label(
                c,
                format!("重置 {}", reset(Some(w), clock)?).as_str(),
                x,
                265,
                false,
            )?;
        }
    } else {
        let w = values.first();
        label(
            c,
            window_label(w.and_then(|w| w.window_duration_mins)).as_str(),
            25,
            115,
            false,
        )?;
        if w.is_some_and(|w| w.stale) {
            label(c, "额度延迟", 320, 115, false)?;
        }
        dial(c, 200, 191, 83, w.map(|w| w.remaining_percent));
        fitted_number(
            c,
            w.map(|w| w.remaining_percent),
            (112, 204, 176, 42),
            false,
        )?;
        label(
            c,
            format!("重置 {}", reset(w, clock)?).as_str(),
            131,
            267,
            false,
        )?;
    }
    small(c, "REMAINING / %", 24, 282, false)?;
    footer(c, updated, false)
}

fn hud_panel(c: &mut Canvas, x: i32, width: i32) {
    c.stroke_polygon(
        &[
            (x, 110),
            (x + width - 14, 110),
            (x + width, 124),
            (x + width, 270),
            (x + 12, 270),
            (x, 258),
        ],
        1,
        255,
    );
    c.fill_rect(x + 1, 110, 38, 3, 255);
}
pub(super) fn draw_hud(
    c: &mut Canvas,
    s: &StatusBoardState,
    clock: &impl StatusBoardClock,
    updated: &str,
) -> Result<(), StatusBoardRenderError> {
    c.fill_rect(0, 0, 400, 300, 0);
    c.stroke_polygon(
        &[
            (22, 9),
            (378, 9),
            (391, 22),
            (391, 278),
            (378, 291),
            (22, 291),
            (9, 278),
            (9, 22),
        ],
        1,
        255,
    );
    small(c, "CODEX / NEURAL LINK", 25, 33, true)?;
    small(c, "C-01", 350, 33, true)?;
    c.line(24, 45, 375, 45, 1, 255);
    status(c, s, 76, true)?;
    flags(c, s, 98, true)?;
    let values = themed::windows(s);
    if values.len() == 2 {
        for (index, w) in values.iter().enumerate() {
            let x = 24 + index as i32 * 183;
            hud_panel(c, x, 169);
            label(
                c,
                window_label(w.window_duration_mins).as_str(),
                x + 12,
                132,
                true,
            )?;
            fitted_number(c, Some(w.remaining_percent), (x + 12, 151, 145, 57), true)?;
            label(c, quota_label(Some(w)), x + 12, 230, true)?;
            label(
                c,
                format!("重置 {}", reset(Some(w), clock)?).as_str(),
                x + 12,
                255,
                true,
            )?;
        }
    } else {
        let w = values.first();
        hud_panel(c, 24, 352);
        label(
            c,
            window_label(w.and_then(|w| w.window_duration_mins)).as_str(),
            36,
            132,
            true,
        )?;
        fitted_number(c, w.map(|w| w.remaining_percent), (36, 152, 168, 78), true)?;
        small(c, "RESET", 226, 164, true)?;
        label(c, reset(w, clock)?.as_str(), 226, 188, true)?;
        label(c, quota_label(w), 226, 221, true)?;
        if let Some(w) = w {
            meter(c, 36, 247, 328, w.remaining_percent, true);
        } else {
            small(c, "QUOTA UNAVAILABLE", 36, 256, true)?;
        }
    }
    // Keep the timestamp clear of the outer cut-corner frame.
    c.fill_rect(17, 282, 366, 18, 0);
    footer(c, updated, true)
}
