use super::*;
use crate::status_board::BoardQuotaWindow;

pub(super) fn windows(state: &StatusBoardState) -> Vec<BoardQuotaWindow> {
    let primary = state
        .quota_remaining_percent
        .map(|remaining_percent| BoardQuotaWindow {
            remaining_percent,
            resets_at_epoch_seconds: state.quota_resets_at_epoch_seconds,
            window_duration_mins: state.quota_window_duration_mins,
            stale: state.quota_stale,
        });
    primary
        .into_iter()
        .chain(state.quota_secondary.clone())
        .collect()
}
pub(super) fn percent(value: Option<f64>) -> String {
    match value {
        Some(value) if value.fract() == 0.0 => format!("{value:.0}%"),
        Some(value) => format!("{value:.1}%"),
        None => "--%".into(),
    }
}
fn reset_parts(value: &str) -> (&str, &str) {
    value.split_once(' ').unwrap_or(("重置", value))
}

pub(super) fn draw_pop_dual(
    canvas: &mut Canvas,
    state: &StatusBoardState,
    clock: &impl StatusBoardClock,
    updated: &str,
) -> Result<(), StatusBoardRenderError> {
    let title = FontRenderer::new::<u8g2_font_helvB14_tf>();
    let chinese = FontRenderer::new::<u8g2_font_wqy13_t_gb2312>();
    let small = FontRenderer::new::<u8g2_font_6x12_tf>();
    let number = FontRenderer::new::<u8g2_font_logisoso42_tf>();
    draw_halftone(canvas);
    canvas.fill_polygon(&[(0, 0), (241, 0), (231, 46), (0, 62)], 0);
    draw_text(&title, "CODEX // NOW", 15, 29, BinaryColor::Off, canvas)?;
    canvas.fill_rect(250, 2, 146, 32, 255);
    draw_text(&small, "GROUP: codex", 257, 15, BinaryColor::On, canvas)?;
    draw_text(&small, "DUAL / QUOTA", 257, 29, BinaryColor::On, canvas)?;
    canvas.fill_polygon(&[(10, 69), (289, 56), (300, 111), (19, 120)], 0);
    draw_text(&small, "TASK STATUS", 25, 78, BinaryColor::Off, canvas)?;
    let (label, focus) = pop_status_label(state);
    for (index, glyph) in label.chars().enumerate() {
        let x = 27 + index as i32 * 28;
        let y = 83 + index as i32 % 2;
        if index == focus {
            canvas.fill_polygon(
                &[
                    (x - 1, y - 1),
                    (x + 25, y - 2),
                    (x + 27, y + 25),
                    (x - 1, y + 26),
                ],
                255,
            );
        }
        draw_scaled_bold_text(
            &chinese,
            glyph.to_string().as_str(),
            x,
            y - 10,
            2,
            if index == focus { 0 } else { 255 },
            canvas,
        )?;
    }
    canvas.fill_rect(302, 57, 96, 58, 255);
    if state.is_sample {
        draw_text(&chinese, "测试画面", 306, 72, BinaryColor::On, canvas)?;
    }
    if state.task_partial {
        draw_text(&chinese, "部分可见", 306, 91, BinaryColor::On, canvas)?;
    }
    if state.task_stale {
        draw_text(&chinese, "任务延迟", 306, 110, BinaryColor::On, canvas)?;
    }
    for (index, window) in windows(state).iter().enumerate() {
        if index == 1 {
            draw_pop_secondary(canvas, window, clock)?;
            continue;
        }
        let x = 7 + index as i32 * 198;
        let panel = [(x, 132), (x + 177, 121), (x + 186, 256), (x + 8, 266)];
        canvas.fill_polygon(&panel, 255);
        canvas.stroke_polygon(&panel, 2, 0);
        canvas.fill_polygon(
            &[(x + 10, 127), (x + 105, 122), (x + 108, 149), (x + 11, 154)],
            0,
        );
        draw_text(
            &chinese,
            window_label(window.window_duration_mins).as_str(),
            x + 18,
            144,
            BinaryColor::Off,
            canvas,
        )?;
        draw_sheared_text(
            &number,
            percent(Some(window.remaining_percent)).as_str(),
            x + 15,
            135,
            0,
            canvas,
        )?;
        draw_bold_text(&chinese, "剩余额度", x + 17, 220, BinaryColor::On, canvas)?;
        if window.stale {
            draw_text(&chinese, "延迟", x + 140, 220, BinaryColor::On, canvas)?;
        }
        let reset = format_reset_time(window.resets_at_epoch_seconds, clock)?;
        draw_text(
            &chinese,
            format!("重置 {reset}").as_str(),
            x + 14,
            247,
            BinaryColor::On,
            canvas,
        )?;
    }
    canvas.fill_polygon(&[(161, 278), (400, 266), (400, 300), (156, 300)], 0);
    draw_text(&small, "UPDATED", 250, 291, BinaryColor::Off, canvas)?;
    draw_text(&small, updated, 341, 291, BinaryColor::Off, canvas)?;
    Ok(())
}

fn draw_pop_secondary(
    canvas: &mut Canvas,
    window: &BoardQuotaWindow,
    clock: &impl StatusBoardClock,
) -> Result<(), StatusBoardRenderError> {
    let chinese = FontRenderer::new::<u8g2_font_wqy13_t_gb2312>();
    let number = FontRenderer::new::<u8g2_font_logisoso42_tf>();
    // The secondary window borrows the single-window RESET card's descending frame
    // and upright digits, giving the collage two deliberately different silhouettes.
    let panel = [(226, 138), (393, 151), (381, 260), (214, 246)];
    canvas.fill_polygon(&panel, 255);
    canvas.stroke_polygon(&panel, 3, 0);
    canvas.fill_polygon(&[(217, 124), (318, 122), (322, 148), (213, 151)], 0);
    let label = window_label(window.window_duration_mins);
    draw_text(&chinese, label.as_str(), 224, 143, BinaryColor::Off, canvas)?;
    if let Some(first) = label.chars().next() {
        let width = if first.is_ascii() { 8 } else { 14 };
        canvas.fill_polygon(
            &[
                (222, 126),
                (222 + width, 125),
                (224 + width, 145),
                (222, 147),
            ],
            255,
        );
        draw_text(
            &chinese,
            first.to_string().as_str(),
            224,
            143,
            BinaryColor::On,
            canvas,
        )?;
    }
    draw_text(
        &number,
        percent(Some(window.remaining_percent)).as_str(),
        238,
        201,
        BinaryColor::On,
        canvas,
    )?;
    draw_text(&chinese, "剩余额度", 236, 220, BinaryColor::On, canvas)?;
    if window.stale {
        draw_text(&chinese, "延迟", 336, 220, BinaryColor::On, canvas)?;
    }
    let reset = format_reset_time(window.resets_at_epoch_seconds, clock)?;
    draw_text(
        &chinese,
        format!("重置 {reset}").as_str(),
        230,
        240,
        BinaryColor::On,
        canvas,
    )?;
    Ok(())
}

pub(super) fn draw_eva(
    canvas: &mut Canvas,
    state: &StatusBoardState,
    clock: &impl StatusBoardClock,
    updated: &str,
) -> Result<(), StatusBoardRenderError> {
    let chinese = FontRenderer::new::<u8g2_font_wqy13_t_gb2312>();
    let status = FontRenderer::new::<u8g2_font_wqy16_t_gb2312>();
    let small = FontRenderer::new::<u8g2_font_6x12_tf>();
    let number = FontRenderer::new::<u8g2_font_logisoso42_tf>();
    let values = windows(state);
    let dual = values.len() == 2;
    // Fixed striped header and equipment rail are present in both window layouts.
    canvas.fill_rect(0, 0, 400, 22, 0);
    for x in (0..420).step_by(32) {
        canvas.fill_polygon(&[(x + 16, 0), (x + 29, 0), (x + 16, 22), (x + 3, 22)], 255);
    }
    canvas.fill_rect(0, 22, 22, 278, 0);
    for (index, character) in "CODEX SYSTEM".chars().filter(|c| *c != ' ').enumerate() {
        draw_text(
            &small,
            character.to_string().as_str(),
            8,
            77 + index as i32 * 18,
            BinaryColor::Off,
            canvas,
        )?;
    }
    draw_text(
        &small,
        "CORE C-01 // STATUS MONITOR",
        33,
        39,
        BinaryColor::On,
        canvas,
    )?;
    canvas.fill_rect(31, 48, 246, 56, 0);
    draw_text(&small, "TASK / SIGNAL", 41, 63, BinaryColor::Off, canvas)?;
    draw_text(
        &status,
        state.task_state.large_label(),
        42,
        91,
        BinaryColor::Off,
        canvas,
    )?;
    canvas.fill_rect(286, 48, 105, 56, 0);
    canvas.fill_rect(288, 50, 101, 52, 255);
    draw_text(
        &small,
        state.task_state.small_code(),
        298,
        70,
        BinaryColor::On,
        canvas,
    )?;
    draw_text(&small, "UNIT / 01", 298, 92, BinaryColor::On, canvas)?;
    let mut labels = Vec::new();
    if state.is_sample {
        labels.push("测试画面");
    }
    if state.task_partial {
        labels.push("部分可见");
    }
    if state.task_stale {
        labels.push("任务延迟");
    }
    draw_text(
        &chinese,
        labels.join("  ").as_str(),
        33,
        121,
        BinaryColor::On,
        canvas,
    )?;
    if dual {
        for (index, window) in values.iter().enumerate() {
            let x = 33 + index as i32 * 184;
            canvas.fill_rect(x, 128, 173, 2, 0);
            draw_text(
                &chinese,
                window_label(window.window_duration_mins).as_str(),
                x + 3,
                148,
                BinaryColor::On,
                canvas,
            )?;
            draw_text(
                &number,
                percent(Some(window.remaining_percent)).as_str(),
                x + 3,
                204,
                BinaryColor::On,
                canvas,
            )?;
            draw_text(
                &chinese,
                if window.stale {
                    "剩余 / 数据延迟"
                } else {
                    "剩余额度"
                },
                x + 3,
                226,
                BinaryColor::On,
                canvas,
            )?;
            let reset = format_reset_time(window.resets_at_epoch_seconds, clock)?;
            draw_text(
                &chinese,
                format!("重置 {reset}").as_str(),
                x + 3,
                248,
                BinaryColor::On,
                canvas,
            )?;
            draw_meter(canvas, x + 3, 259, 166, window.remaining_percent);
        }
    } else {
        let window = values.first();
        let remaining = window.map(|w| w.remaining_percent);
        canvas.fill_rect(33, 128, 357, 2, 0);
        draw_text(
            &chinese,
            window_label(window.and_then(|w| w.window_duration_mins)).as_str(),
            37,
            148,
            BinaryColor::On,
            canvas,
        )?;
        let big = FontRenderer::new::<u8g2_font_logisoso78_tn>();
        if let Some(value) = remaining.filter(|v| v.fract() == 0.0) {
            // The 78-point digits provide a large quota anchor without crowding RESET.
            draw_text(
                &big,
                format!("{value:.0}").as_str(),
                36,
                236,
                BinaryColor::On,
                canvas,
            )?;
            draw_text(&number, "%", 181, 237, BinaryColor::On, canvas)?;
        } else {
            draw_text(
                &number,
                percent(remaining).as_str(),
                37,
                214,
                BinaryColor::On,
                canvas,
            )?;
        }
        let reset = format_reset_time(window.and_then(|w| w.resets_at_epoch_seconds), clock)?;
        let (context, time) = reset_parts(&reset);
        draw_text(&small, "RESET TIMER", 245, 149, BinaryColor::On, canvas)?;
        let reset_font = FontRenderer::new::<u8g2_font_helvB14_tf>();
        draw_text(&reset_font, time, 245, 179, BinaryColor::On, canvas)?;
        draw_text(&chinese, context, 245, 201, BinaryColor::On, canvas)?;
        draw_text(
            &chinese,
            if window.is_some_and(|w| w.stale) {
                "额度延迟"
            } else {
                "剩余额度"
            },
            245,
            232,
            BinaryColor::On,
            canvas,
        )?;
        if let Some(value) = remaining {
            draw_meter(canvas, 36, 259, 350, value);
        } else {
            draw_text(
                &small,
                "QUOTA UNAVAILABLE",
                36,
                268,
                BinaryColor::On,
                canvas,
            )?;
        }
    }
    canvas.fill_rect(31, 279, 361, 1, 0);
    draw_text(&small, "GROUP: codex", 34, 295, BinaryColor::On, canvas)?;
    draw_text(
        &small,
        format!("UPDATED {updated}").as_str(),
        278,
        295,
        BinaryColor::On,
        canvas,
    )?;
    Ok(())
}

fn draw_meter(canvas: &mut Canvas, x: i32, y: i32, width: i32, remaining: f64) {
    let count = 12;
    let step = width / count;
    for index in 0..count {
        canvas.fill_rect(x + index * step, y, step - 2, 12, 0);
        if f64::from(index + 1) > (remaining / 100.0 * f64::from(count)).ceil() {
            canvas.fill_rect(x + index * step + 1, y + 1, step - 4, 10, 255);
        }
    }
}
