use crate::desktop::{app::*, *};
use crate::inkcards::PAGE_THEMES;
use eframe::egui;

#[derive(Clone, Copy)]
struct CardDrag(usize);

pub fn show(app: &mut DesktopApp, ui: &mut egui::Ui) {
    ui.label("一张直接配置，多张拖动排序。额度窗口自动适配实际账户。");
    ui.add_space(10.0);
    let editing = !app.jobs.is_operation_busy();
    ui.add_enabled_ui(editing, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label("墨卡数量");
            for count in 1..=5 {
                if ui
                    .selectable_label(app.model.cards.count() == count, format!("{count} 张"))
                    .clicked()
                {
                    app.model.cards.set_count(count);
                }
            }
        });
    });
    ui.add_space(8.0);
    let count = app.model.cards.count();
    if app.model.cards.multiple {
        ui.label(format!(
            "从上到下对应设备墨卡 1–{count}。抓住“拖动”调整顺序，也可用上移 / 下移。"
        ));
    }
    let target = if app.model.cards.multiple {
        format!("1–{count}")
    } else {
        app.model.cards.single_page.to_string()
    };
    ui.small(format!(
        "保存后更新墨卡 {target}；未选中的墨卡保留原画面，停止自动更新。"
    ));
    ui.horizontal_wrapped(|ui| {
        let configured = app.model.status.as_ref().is_some_and(|s| s.configured);
        if ui
            .add_enabled(
                app.can_modify() && configured,
                egui::Button::new("保存并应用"),
            )
            .clicked()
        {
            app.action(UiAction::SaveCards);
        }
        if ui
            .add_enabled(
                editing && app.model.cards.dirty,
                egui::Button::new("撤销未保存修改"),
            )
            .clicked()
            && let Some(status) = &app.model.status
        {
            app.model.cards.discard(status);
        }
        if app.model.cards.dirty {
            ui.colored_label(accent(), "有未保存修改");
        }
    });
    if !app.model.status.as_ref().is_some_and(|s| s.configured) {
        ui.small("请先在设备配置页连接设备，之后即可在这里直接配置墨卡。");
    }
    ui.add_space(12.0);
    ui.horizontal_wrapped(|ui| {
        ui.small("风格示例：");
        ui.selectable_value(&mut app.model.sample_dual, false, "单额度 · 7 天");
        ui.selectable_value(&mut app.model.sample_dual, true, "双额度 · 5 小时 + 7 天");
    });
    ui.small("下方为模拟预览，不会上传模拟额度。");
    ui.add_space(8.0);
    ui.add_enabled_ui(editing, |ui| {
        if !app.model.cards.multiple {
            ui.horizontal_wrapped(|ui| {
                ui.label("设备墨卡");
                egui::ComboBox::from_id_salt("single-card-page")
                    .selected_text(format!("墨卡 {}", app.model.cards.single_page))
                    .show_ui(ui, |ui| {
                        for page in 1..=5 {
                            if ui
                                .selectable_value(
                                    &mut app.model.cards.single_page,
                                    page,
                                    format!("墨卡 {page}"),
                                )
                                .changed()
                            {
                                app.model.cards.dirty = true;
                            }
                        }
                    });
                if theme_picker(ui, "single-card-theme", &mut app.model.cards.single_theme) {
                    app.model.cards.dirty = true;
                }
            });
            ui.add_space(8.0);
            if let Some(png) = app.sample_png(app.model.cards.single_theme, app.model.sample_dual) {
                app.show_png(ui, &png, 400.0);
            }
        } else {
            let themes = app.model.cards.themes.clone();
            let mut move_to = None;
            let mut remove = None;
            for (index, theme) in themes.iter().copied().enumerate() {
                let (_, dropped) = ui.dnd_drop_zone::<CardDrag, _>(
                    egui::Frame::group(ui.style()).inner_margin(10),
                    |ui| {
                        ui.set_min_width((ui.available_width() - 2.0).max(1.0));
                        ui.horizontal_top(|ui| {
                            ui.dnd_drag_source(
                                egui::Id::unique(("inkcard-drag", index)),
                                CardDrag(index),
                                |ui| {
                                    ui.add_sized([32.0, 58.0], egui::Label::new("拖动"));
                                },
                            );
                            if let Some(png) = app.sample_png(theme, app.model.sample_dual) {
                                app.show_png(ui, &png, 72.0);
                            }
                            ui.vertical(|ui| {
                                ui.strong(format!("墨卡 {}", index + 1));
                                if theme_picker(
                                    ui,
                                    ("card-theme", index),
                                    &mut app.model.cards.themes[index],
                                ) {
                                    app.model.cards.dirty = true;
                                }
                                ui.horizontal_wrapped(|ui| {
                                    if ui
                                        .add_enabled(index > 0, egui::Button::new("上移"))
                                        .clicked()
                                    {
                                        move_to = Some((index, index - 1));
                                    }
                                    if ui
                                        .add_enabled(index + 1 < count, egui::Button::new("下移"))
                                        .clicked()
                                    {
                                        move_to = Some((index, index + 1));
                                    }
                                    if ui
                                        .add_enabled(count > 2, egui::Button::new("移除"))
                                        .clicked()
                                    {
                                        remove = Some(index);
                                    }
                                });
                            });
                        });
                    },
                );
                if let Some(from) = dropped {
                    move_to = Some((from.0, index));
                }
                ui.add_space(6.0);
            }
            if let Some((from, to)) = move_to {
                app.model.cards.move_card(from, to);
            }
            if let Some(index) = remove {
                app.model.cards.remove_card(index);
            }
            if ui
                .add_enabled(count < 5, egui::Button::new("＋ 添加墨卡"))
                .clicked()
            {
                app.model.cards.add_card();
            }
            ui.small("每张可独立选风格；也可在上方直接选择需要的张数。");
        }
    });
    ui.add_space(12.0);
    ui.collapsing("查看设备最近上传的真实画面", |ui| {
        let pages: Vec<_> = app
            .model
            .status
            .as_ref()
            .map(|s| s.pages.iter().map(|p| (p.page, p.theme)).collect())
            .unwrap_or_default();
        ui.horizontal_wrapped(|ui| {
            for (page, theme) in pages {
                ui.selectable_value(
                    &mut app.model.selected_page,
                    page,
                    format!("墨卡 {page} · {}", theme.label()),
                );
            }
        });
        let png = app
            .model
            .status
            .as_ref()
            .and_then(|s| s.pages.iter().find(|p| p.page == app.model.selected_page))
            .and_then(|p| p.uploaded_png.clone());
        if let Some(png) = png {
            app.show_png(ui, &png, 400.0);
        } else {
            ui.label("暂无这一页的成功上传画面。");
        }
        ui.small("云端接收后，NOTE4 会按设备同步周期刷新。");
    });
}
fn theme_picker(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    theme: &mut BoardTheme,
) -> bool {
    let mut changed = false;
    egui::ComboBox::from_id_salt(id)
        .width(165.0)
        .selected_text(theme.label())
        .show_ui(ui, |ui| {
            for choice in PAGE_THEMES {
                changed |= ui.selectable_value(theme, choice, choice.label()).changed();
            }
        });
    changed
}
