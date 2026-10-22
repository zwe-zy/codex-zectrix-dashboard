use std::convert::Infallible;

use embedded_graphics::geometry::{OriginDimensions, Size};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::{DrawTarget, Pixel, Point};
use thiserror::Error;
use time::{Date, Month};
use u8g2_fonts::FontRenderer;
use u8g2_fonts::fonts::{
    u8g2_font_6x12_tf, u8g2_font_helvB14_tf, u8g2_font_logisoso42_tf, u8g2_font_logisoso78_tn,
    u8g2_font_wqy13_t_gb2312, u8g2_font_wqy16_t_gb2312,
};
use u8g2_fonts::types::{FontColor, VerticalPosition};

use super::StatusBoardState;
mod extra;
mod themed;

const WIDTH: u32 = 400;
const HEIGHT: u32 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoardLocalTime {
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
}

impl BoardLocalTime {
    pub fn new(
        year: i32,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
    ) -> Result<Self, StatusBoardRenderError> {
        let month_value =
            Month::try_from(month).map_err(|_| StatusBoardRenderError::InvalidTime)?;
        Date::from_calendar_date(year, month_value, day)
            .map_err(|_| StatusBoardRenderError::InvalidTime)?;
        if hour > 23 || minute > 59 {
            return Err(StatusBoardRenderError::InvalidTime);
        }
        Ok(Self {
            year,
            month,
            day,
            hour,
            minute,
        })
    }

    fn date(self) -> Result<Date, StatusBoardRenderError> {
        let month = Month::try_from(self.month).map_err(|_| StatusBoardRenderError::InvalidTime)?;
        Date::from_calendar_date(self.year, month, self.day)
            .map_err(|_| StatusBoardRenderError::InvalidTime)
    }
}

pub trait StatusBoardClock {
    fn now_epoch_seconds(&self) -> i64;
    fn local_time(&self, epoch_seconds: i64) -> Option<BoardLocalTime>;
}

#[derive(Debug, Error)]
pub enum StatusBoardRenderError {
    #[error("status board local time unavailable")]
    TimeUnavailable,
    #[error("status board local time invalid")]
    InvalidTime,
    #[error("status board quota percentage outside display range")]
    QuotaOutOfRange,
    #[error("failed to render status board text: {0}")]
    Font(String),
    #[error("failed to encode status board PNG: {0}")]
    Png(#[from] png::EncodingError),
}

pub fn format_reset_time(
    resets_at_epoch_seconds: Option<i64>,
    clock: &impl StatusBoardClock,
) -> Result<String, StatusBoardRenderError> {
    let Some(reset_epoch_seconds) = resets_at_epoch_seconds else {
        return Ok("--:--".into());
    };
    let now = clock
        .local_time(clock.now_epoch_seconds())
        .ok_or(StatusBoardRenderError::TimeUnavailable)?;
    let reset = clock
        .local_time(reset_epoch_seconds)
        .ok_or(StatusBoardRenderError::TimeUnavailable)?;
    let now_date = now.date()?;
    let reset_date = reset.date()?;
    let prefix = if reset_date == now_date {
        "今天".into()
    } else if now_date.next_day() == Some(reset_date) {
        "明天".into()
    } else {
        format!("{:02}-{:02}", reset.month, reset.day)
    };
    Ok(format!("{prefix} {:02}:{:02}", reset.hour, reset.minute))
}

pub fn format_updated_time(
    uploaded_at_epoch_seconds: Option<i64>,
    clock: &impl StatusBoardClock,
) -> Result<String, StatusBoardRenderError> {
    let Some(uploaded_at_epoch_seconds) = uploaded_at_epoch_seconds else {
        return Ok("--:--".into());
    };
    let uploaded = clock
        .local_time(uploaded_at_epoch_seconds)
        .ok_or(StatusBoardRenderError::TimeUnavailable)?;
    Ok(format!("{:02}:{:02}", uploaded.hour, uploaded.minute))
}

pub fn render_status_board(
    state: &StatusBoardState,
    clock: &impl StatusBoardClock,
) -> Result<Vec<u8>, StatusBoardRenderError> {
    if state
        .quota_remaining_percent
        .is_some_and(|percent| !percent.is_finite() || !(0.0..=100.0).contains(&percent))
        || state.quota_secondary.as_ref().is_some_and(|window| {
            !window.remaining_percent.is_finite()
                || !(0.0..=100.0).contains(&window.remaining_percent)
        })
    {
        return Err(StatusBoardRenderError::QuotaOutOfRange);
    }
    let reset_time = format_reset_time(state.quota_resets_at_epoch_seconds, clock)?;
    let updated_time = format_updated_time(state.uploaded_at_epoch_seconds, clock)?;
    let mut canvas = Canvas::new();
    if state.theme != super::BoardTheme::PopCut {
        match state.theme {
            super::BoardTheme::Eva => themed::draw_eva(&mut canvas, state, clock, &updated_time)?,
            super::BoardTheme::PixelConsole => {
                extra::draw_pocket(&mut canvas, state, clock, &updated_time)?
            }
            super::BoardTheme::SpaceGauge => {
                extra::draw_gauge(&mut canvas, state, clock, &updated_time)?
            }
            super::BoardTheme::CyberHud => {
                extra::draw_hud(&mut canvas, state, clock, &updated_time)?
            }
            super::BoardTheme::PopCut => unreachable!(),
        }
        return canvas.png_bytes();
    }
    if state.quota_secondary.is_some() && state.quota_remaining_percent.is_some() {
        themed::draw_pop_dual(&mut canvas, state, clock, &updated_time)?;
        return canvas.png_bytes();
    }
    if state.quota_remaining_percent.is_none()
        && let Some(secondary) = &state.quota_secondary
    {
        let mut single = state.clone();
        single.quota_remaining_percent = Some(secondary.remaining_percent);
        single.quota_window_duration_mins = secondary.window_duration_mins;
        single.quota_resets_at_epoch_seconds = secondary.resets_at_epoch_seconds;
        single.quota_stale = secondary.stale;
        single.quota_secondary = None;
        let reset = format_reset_time(single.quota_resets_at_epoch_seconds, clock)?;
        draw_composition(&mut canvas, &single, &reset, &updated_time)?;
        return canvas.png_bytes();
    }
    draw_composition(&mut canvas, state, &reset_time, &updated_time)?;
    canvas.png_bytes()
}

fn draw_composition(
    canvas: &mut Canvas,
    state: &StatusBoardState,
    reset_time: &str,
    updated_time: &str,
) -> Result<(), StatusBoardRenderError> {
    let small_font = FontRenderer::new::<u8g2_font_6x12_tf>();
    let display_font = FontRenderer::new::<u8g2_font_helvB14_tf>();
    let chinese_font = FontRenderer::new::<u8g2_font_wqy13_t_gb2312>();
    let status_font = FontRenderer::new::<u8g2_font_wqy16_t_gb2312>();
    let quota_font = FontRenderer::new::<u8g2_font_logisoso78_tn>();
    let accent_font = FontRenderer::new::<u8g2_font_logisoso42_tf>();

    draw_halftone(canvas);

    let top_wedge = [(0, 0), (226, 0), (224, 48), (0, 79)];
    canvas.fill_polygon(&top_wedge, 0);
    draw_text(
        &display_font,
        "CODEX // NOW",
        18,
        31,
        BinaryColor::Off,
        canvas,
    )?;

    let slash = [(219, 59), (390, 32), (392, 39), (220, 68)];
    canvas.fill_polygon(&slash, 0);
    canvas.fill_rect(236, 0, 164, 30, 255);
    draw_text(
        &small_font,
        "GROUP: codex",
        242,
        12,
        BinaryColor::On,
        canvas,
    )?;
    draw_text(
        &chinese_font,
        window_label(state.quota_window_duration_mins).as_str(),
        242,
        27,
        BinaryColor::On,
        canvas,
    )?;
    if state.task_partial {
        canvas.fill_rect(236, 74, 158, 22, 255);
        draw_text(
            &chinese_font,
            "部分任务可见",
            242,
            90,
            BinaryColor::On,
            canvas,
        )?;
    }
    if state.is_sample {
        canvas.fill_rect(236, 45, 158, 22, 255);
        draw_text(&chinese_font, "测试画面", 242, 61, BinaryColor::On, canvas)?;
    }

    let quota_burst = [(8, 160), (209, 151), (194, 241), (27, 233), (10, 211)];
    canvas.fill_polygon(&quota_burst, 255);
    canvas.stroke_polygon(&quota_burst, 3, 0);

    let status_slab = [(17, 76), (196, 65), (214, 137), (47, 157), (17, 132)];
    canvas.fill_polygon(&status_slab, 0);
    draw_text(&small_font, "TASK STATUS", 35, 96, BinaryColor::Off, canvas)?;
    let (status_label, focus_index) = pop_status_label(state);
    draw_status_tiles(&status_font, status_label, focus_index, canvas)?;

    let quota = state
        .quota_remaining_percent
        .map(|percent| {
            if percent.fract() == 0.0 {
                format!("{percent:.0}")
            } else {
                format!("{percent:.1}")
            }
        })
        .unwrap_or_else(|| "--".into());
    let fractional = quota.contains('.');
    draw_sheared_text(
        if fractional {
            &accent_font
        } else {
            &quota_font
        },
        quota.as_str(),
        24,
        158,
        0,
        canvas,
    )?;
    let percent_x = if fractional {
        140
    } else {
        match quota.len() {
            3 => 148,
            2 => 108,
            _ => 68,
        }
    };
    draw_sheared_text(&accent_font, "%", percent_x, 158, 0, canvas)?;
    let quota_label_frame = [(27, 233), (126, 229), (130, 258), (25, 262)];
    canvas.fill_polygon(&quota_label_frame, 0);
    let quota_white_tiles = [
        [(31, 235), (52, 234), (53, 257), (30, 258)],
        [(54, 233), (75, 234), (76, 256), (53, 256)],
        [(100, 232), (121, 231), (122, 255), (99, 256)],
    ];
    for tile in quota_white_tiles {
        canvas.fill_polygon(&tile, 255);
    }
    let quota_black_tile = [(77, 236), (98, 234), (99, 258), (76, 259)];
    canvas.fill_polygon(&quota_black_tile, 0);
    canvas.stroke_polygon(&quota_black_tile, 1, 255);
    draw_bold_text(&chinese_font, "剩", 35, 253, BinaryColor::On, canvas)?;
    draw_bold_text(&chinese_font, "余", 58, 251, BinaryColor::On, canvas)?;
    draw_bold_text(&chinese_font, "额", 81, 254, BinaryColor::Off, canvas)?;
    draw_bold_text(&chinese_font, "度", 104, 250, BinaryColor::On, canvas)?;
    if state.quota_stale {
        canvas.fill_rect(144, 229, 58, 18, 0);
        draw_text(
            &chinese_font,
            "数据延迟",
            148,
            244,
            BinaryColor::Off,
            canvas,
        )?;
    }

    let reset_panel = [(238, 119), (393, 130), (380, 223), (226, 211)];
    canvas.fill_polygon(&reset_panel, 255);
    canvas.stroke_polygon(&reset_panel, 3, 0);
    let reset_tag = [(228, 105), (296, 103), (300, 129), (224, 132)];
    canvas.fill_polygon(&reset_tag, 0);
    let reset_s_tile = [(253, 105), (267, 104), (269, 128), (252, 130)];
    canvas.fill_polygon(&reset_s_tile, 255);
    draw_text(&display_font, "R", 229, 124, BinaryColor::Off, canvas)?;
    draw_text(&display_font, "E", 242, 126, BinaryColor::Off, canvas)?;
    draw_text(&display_font, "S", 255, 124, BinaryColor::On, canvas)?;
    draw_text(&display_font, "E", 269, 125, BinaryColor::Off, canvas)?;
    draw_text(&display_font, "T", 283, 123, BinaryColor::Off, canvas)?;
    let (reset_context, reset_clock) = if let Some((context, clock)) = reset_time.split_once(' ') {
        let context = match context {
            "今天" => "今天重置",
            "明天" => "明天重置",
            date => date,
        };
        (context, clock)
    } else {
        ("暂不可用", reset_time)
    };
    draw_text(&accent_font, reset_clock, 247, 176, BinaryColor::On, canvas)?;
    draw_text(
        &chinese_font,
        reset_context,
        246,
        199,
        BinaryColor::On,
        canvas,
    )?;

    let footer = [(179, 272), (400, 245), (400, 300), (189, 300)];
    canvas.fill_polygon(&footer, 0);
    draw_text(&small_font, "UPDATED", 292, 286, BinaryColor::Off, canvas)?;
    draw_text(
        &small_font,
        updated_time,
        346,
        286,
        BinaryColor::Off,
        canvas,
    )?;
    if state.task_stale {
        draw_text(
            &chinese_font,
            "数据延迟",
            204,
            289,
            BinaryColor::Off,
            canvas,
        )?;
    }
    Ok(())
}

fn window_label(minutes: Option<u32>) -> String {
    match minutes {
        Some(300) => "5 小时".into(),
        Some(10080) => "7 天".into(),
        Some(value) if value % 60 == 0 => format!("{} 小时", value / 60),
        Some(value) => format!("{value} 分钟"),
        None => "时长未知".into(),
    }
}
fn pop_status_label(state: &StatusBoardState) -> (&'static str, usize) {
    match state.task_state {
        super::BoardTaskState::Idle => ("当前空闲", 1),
        super::BoardTaskState::Running => ("任务运行中", 2),
        super::BoardTaskState::Completed => ("本轮已完成", 2),
        super::BoardTaskState::Failed => ("任务失败", 2),
        super::BoardTaskState::Interrupted => ("任务中断", 2),
        super::BoardTaskState::Empty => ("暂无任务", 0),
        super::BoardTaskState::Unavailable => ("状态未知", 2),
    }
}

fn draw_halftone(canvas: &mut Canvas) {
    for y in (6..300).step_by(12) {
        for x in (6..400).step_by(12) {
            canvas.circle(x, y, 2, 0, true);
        }
    }
}

fn draw_text<F: u8g2_fonts::Content>(
    font: &FontRenderer,
    content: F,
    x: i32,
    baseline_y: i32,
    color: BinaryColor,
    canvas: &mut Canvas,
) -> Result<(), StatusBoardRenderError> {
    font.render(
        content,
        Point::new(x, baseline_y),
        VerticalPosition::Baseline,
        FontColor::Transparent(color),
        canvas,
    )
    .map(|_| ())
    .map_err(|error| StatusBoardRenderError::Font(format!("{error:?}")))
}

fn draw_bold_text<F: u8g2_fonts::Content + Clone>(
    font: &FontRenderer,
    content: F,
    x: i32,
    baseline_y: i32,
    color: BinaryColor,
    canvas: &mut Canvas,
) -> Result<(), StatusBoardRenderError> {
    draw_text(font, content.clone(), x, baseline_y, color, canvas)?;
    draw_text(font, content, x + 1, baseline_y, color, canvas)
}

fn draw_status_tiles(
    font: &FontRenderer,
    label: &str,
    focus_index: usize,
    canvas: &mut Canvas,
) -> Result<(), StatusBoardRenderError> {
    let glyphs: Vec<_> = label.chars().collect();
    let start_x = if glyphs.len() == 5 { 32 } else { 49 };
    let y_offsets = [104, 101, 105, 102, 106];

    for (index, glyph) in glyphs.into_iter().enumerate() {
        let x = start_x + index as i32 * 34;
        let y = y_offsets[index];
        let tile = [(x, y), (x + 31, y - 1), (x + 34, y + 33), (x - 1, y + 36)];
        let is_focus = index == focus_index;
        canvas.fill_polygon(&tile, if is_focus { 255 } else { 0 });
        if is_focus {
            canvas.stroke_polygon(&tile, 1, 0);
        }

        let mut encoded = [0; 4];
        let glyph: &str = glyph.encode_utf8(&mut encoded);
        draw_scaled_bold_text(
            font,
            glyph,
            x + 1,
            y - 4,
            2,
            if is_focus { 0 } else { 255 },
            canvas,
        )?;
    }
    Ok(())
}

fn draw_sheared_text<F: u8g2_fonts::Content + Clone>(
    font: &FontRenderer,
    content: F,
    x: i32,
    y: i32,
    color: u8,
    canvas: &mut Canvas,
) -> Result<(), StatusBoardRenderError> {
    let mut mask = Canvas::with_size(180, 88);
    draw_text(font, content, 0, 80, BinaryColor::On, &mut mask)?;
    for source_y in 0..88 {
        let shear = source_y / 13;
        for source_x in 0..180 {
            if mask.pixel(source_x, source_y) == 0 {
                canvas.set_pixel(
                    x + source_x * 17 / 20 + shear,
                    y + source_y * 17 / 20,
                    color,
                );
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn draw_scaled_bold_text<F: u8g2_fonts::Content + Clone>(
    font: &FontRenderer,
    content: F,
    x: i32,
    y: i32,
    scale: i32,
    color: u8,
    canvas: &mut Canvas,
) -> Result<(), StatusBoardRenderError> {
    draw_scaled_text(font, content.clone(), x, y, scale, color, canvas)?;
    draw_scaled_text(font, content, x + 1, y, scale, color, canvas)
}

#[allow(clippy::too_many_arguments)]
fn draw_scaled_text<F: u8g2_fonts::Content + Clone>(
    font: &FontRenderer,
    content: F,
    x: i32,
    y: i32,
    scale: i32,
    color: u8,
    canvas: &mut Canvas,
) -> Result<(), StatusBoardRenderError> {
    let mut mask = Canvas::with_size(180, 24);
    draw_text(font, content, 0, 18, BinaryColor::On, &mut mask)?;
    for source_y in 0..24 {
        for source_x in 0..180 {
            if mask.pixel(source_x, source_y) == 0 {
                canvas.fill_rect(
                    x + source_x * scale,
                    y + source_y * scale,
                    scale,
                    scale,
                    color,
                );
            }
        }
    }
    Ok(())
}

struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Canvas {
    fn new() -> Self {
        Self::with_size(WIDTH, HEIGHT)
    }

    fn with_size(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![255; (width * height) as usize],
        }
    }

    fn pixel(&self, x: i32, y: i32) -> u8 {
        self.pixels[y as usize * self.width as usize + x as usize]
    }

    fn set_pixel(&mut self, x: i32, y: i32, color: u8) {
        if x >= 0 && y >= 0 && x < self.width as i32 && y < self.height as i32 {
            self.pixels[y as usize * self.width as usize + x as usize] = color;
        }
    }

    fn fill_rect(&mut self, x: i32, y: i32, width: i32, height: i32, color: u8) {
        for row in y..y + height {
            for column in x..x + width {
                self.set_pixel(column, row, color);
            }
        }
    }

    fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, width: i32, color: u8) {
        let dx = (x1 - x0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let dy = -(y1 - y0).abs();
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut error = dx + dy;
        let (mut x, mut y) = (x0, y0);
        loop {
            let offset = width / 2;
            self.fill_rect(x - offset, y - offset, width, width, color);
            if x == x1 && y == y1 {
                break;
            }
            let doubled = 2 * error;
            if doubled >= dy {
                error += dy;
                x += sx;
            }
            if doubled <= dx {
                error += dx;
                y += sy;
            }
        }
    }

    fn circle(&mut self, center_x: i32, center_y: i32, radius: i32, color: u8, filled: bool) {
        let inner = (radius - 1).max(0);
        for y in center_y - radius..=center_y + radius {
            for x in center_x - radius..=center_x + radius {
                let squared = (x - center_x).pow(2) + (y - center_y).pow(2);
                if squared <= radius.pow(2) && (filled || squared >= inner.pow(2)) {
                    self.set_pixel(x, y, color);
                }
            }
        }
    }

    fn fill_polygon(&mut self, points: &[(i32, i32)], color: u8) {
        let min_y = points.iter().map(|point| point.1).min().unwrap_or(0);
        let max_y = points.iter().map(|point| point.1).max().unwrap_or(-1);
        for y in min_y..=max_y {
            let mut intersections = Vec::new();
            for index in 0..points.len() {
                let (x1, y1) = points[index];
                let (x2, y2) = points[(index + 1) % points.len()];
                if (y1 <= y && y < y2) || (y2 <= y && y < y1) {
                    intersections.push(x1 + (y - y1) * (x2 - x1) / (y2 - y1));
                }
            }
            intersections.sort_unstable();
            for &[start, end] in intersections.as_chunks::<2>().0 {
                for x in start..=end {
                    self.set_pixel(x, y, color);
                }
            }
        }
    }

    fn stroke_polygon(&mut self, points: &[(i32, i32)], width: i32, color: u8) {
        for index in 0..points.len() {
            let (x1, y1) = points[index];
            let (x2, y2) = points[(index + 1) % points.len()];
            self.line(x1, y1, x2, y2, width, color);
        }
    }

    fn png_bytes(self) -> Result<Vec<u8>, StatusBoardRenderError> {
        let packed = pack_monochrome_rows(&self.pixels, self.width, self.height);
        let mut output = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut output, self.width, self.height);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::One);
            let mut writer = encoder.write_header()?;
            writer.write_image_data(&packed)?;
        }
        Ok(output)
    }
}

fn pack_monochrome_rows(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    let packed_row_bytes = width.div_ceil(8) as usize;
    let mut packed = vec![0; packed_row_bytes * height as usize];
    for y in 0..height as usize {
        for x in 0..width as usize {
            if pixels[y * width as usize + x] == 255 {
                packed[y * packed_row_bytes + x / 8] |= 1 << (7 - x % 8);
            }
        }
    }
    packed
}

impl OriginDimensions for Canvas {
    fn size(&self) -> Size {
        Size::new(self.width, self.height)
    }
}

impl DrawTarget for Canvas {
    type Color = BinaryColor;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            self.set_pixel(
                point.x,
                point.y,
                if color == BinaryColor::On { 0 } else { 255 },
            );
        }
        Ok(())
    }
}
