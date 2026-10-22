use codex_zectrix_dashboard::{desktop::*, inkcards::*, status_board::BoardTheme::*};

#[test]
fn count_changes_keep_order_and_single_page_preferences() {
    let mut cards = CardEditor {
        single_page: 4,
        single_theme: Eva,
        themes: vec![CyberHud, PopCut, Eva, SpaceGauge, PixelConsole],
        ..Default::default()
    };
    cards.set_count(3);
    assert_eq!(
        cards.configuration().unwrap().pages.unwrap(),
        vec![
            InkcardPage {
                page: 1,
                theme: CyberHud
            },
            InkcardPage {
                page: 2,
                theme: PopCut
            },
            InkcardPage {
                page: 3,
                theme: Eva
            }
        ]
    );
    cards.set_count(2);
    assert_eq!(cards.themes, vec![CyberHud, PopCut]);
    cards.set_count(4);
    assert_eq!(&cards.themes[..2], &[CyberHud, PopCut]);
    assert_eq!(cards.count(), 4);
    cards.set_count(1);
    assert_eq!(
        cards.configuration().unwrap().pages.unwrap(),
        vec![InkcardPage {
            page: 4,
            theme: Eva
        }]
    );
    cards.dirty = false;
    cards.set_count(0);
    cards.set_count(6);
    cards.set_count(1);
    assert!(!cards.dirty);
}

#[test]
fn single_card_saves_directly_and_multi_reorder_is_only_a_draft_until_saved() {
    let mut model = AppModel::default();
    model.cards.single_page = 4;
    model.cards.single_theme = Eva;
    let Some(DesktopCommand::SetCards(config)) = model.request_action(UiAction::SaveCards) else {
        panic!("single save")
    };
    assert_eq!(
        config.pages.unwrap(),
        vec![InkcardPage {
            page: 4,
            theme: Eva
        }]
    );
    model.cards.multiple = true;
    model.cards.themes = vec![PopCut, Eva, CyberHud];
    model.cards.move_card(2, 0);
    assert_eq!(model.cards.themes, vec![CyberHud, PopCut, Eva]);
    let Some(DesktopCommand::SetCards(config)) = model.request_action(UiAction::SaveCards) else {
        panic!("multi save")
    };
    assert_eq!(
        config.pages.unwrap(),
        vec![
            InkcardPage {
                page: 1,
                theme: CyberHud
            },
            InkcardPage {
                page: 2,
                theme: PopCut
            },
            InkcardPage {
                page: 3,
                theme: Eva
            }
        ]
    );
}

#[test]
fn card_editor_retains_draft_after_failed_save_and_bounds_add_remove() {
    let mut model = AppModel::default();
    model.cards.multiple = true;
    model.cards.move_card(0, 4);
    let edited = model.cards.themes.clone();
    model.request_action(UiAction::SaveCards);
    model.apply_reply(
        JobId(1),
        DesktopReply::Failed(DesktopError::LocalUnavailable),
    );
    assert_eq!(model.cards.themes, edited);
    assert!(model.cards.dirty);
    model.cards.add_card();
    assert_eq!(model.cards.themes.len(), 5);
    for _ in 0..6 {
        model.cards.remove_card(0);
    }
    assert_eq!(model.cards.themes.len(), 2);
    model.cards.move_card(40, 0);
    assert_eq!(model.cards.themes.len(), 2);
}

#[test]
fn saved_with_restart_warning_clears_dirty_draft_and_keeps_actionable_warning() {
    let mut model = AppModel::default();
    model.cards.move_card(0, 3);
    let expected = model.cards.themes.clone();
    model.apply_reply(
        JobId(1),
        DesktopReply::Completed(OperationReport {
            kind: OperationKind::CardsSaved,
            warning: Some("后台重启失败，请启动后台".into()),
        }),
    );
    assert!(!model.cards.dirty);
    assert_eq!(model.cards.themes, expected);
    assert!(model.notice.as_deref().unwrap().contains("请启动后台"));
}
