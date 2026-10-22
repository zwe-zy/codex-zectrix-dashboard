use codex_zectrix_dashboard::status_board::{
    BoardTaskState, StatusBoardModelError, StatusBoardState,
};
use codex_zectrix_dashboard::{QuotaStatus, TaskState};
use serde_json::json;

fn board_from(task_state: TaskState, quota: Option<QuotaStatus>) -> StatusBoardState {
    StatusBoardState::from_codex_status(task_state, quota, false, false, Some(1_788_134_400))
        .unwrap()
}

#[test]
fn conversion_maps_every_normalized_task_state_to_fixed_board_state() {
    let cases = [
        (TaskState::Running, BoardTaskState::Running),
        (TaskState::Completed, BoardTaskState::Completed),
        (TaskState::Failed, BoardTaskState::Failed),
        (TaskState::Interrupted, BoardTaskState::Interrupted),
        (TaskState::Empty, BoardTaskState::Empty),
        (TaskState::Unavailable, BoardTaskState::Unavailable),
    ];

    for (source, expected) in cases {
        assert_eq!(board_from(source, None).task_state, expected);
    }
}

#[test]
fn board_states_expose_only_fixed_labels_and_codes() {
    let cases = [
        (BoardTaskState::Running, "运行中", "RUNNING"),
        (BoardTaskState::Completed, "本轮完成", "DONE"),
        (BoardTaskState::Failed, "失败", "FAILED"),
        (BoardTaskState::Interrupted, "已中断", "STOPPED"),
        (BoardTaskState::Empty, "无任务", "EMPTY"),
        (BoardTaskState::Unavailable, "状态未知", "OFFLINE"),
    ];

    for (state, large_label, small_code) in cases {
        assert_eq!(state.large_label(), large_label);
        assert_eq!(state.small_code(), small_code);
    }
}

#[test]
fn conversion_retains_only_privacy_safe_status_and_window_fields() {
    let board = StatusBoardState::from_codex_status(
        TaskState::Running,
        Some(QuotaStatus::single(73.0, Some(1_788_177_600), Some(300))),
        true,
        true,
        Some(1_788_134_400),
    )
    .unwrap();

    assert_eq!(
        serde_json::to_value(board).unwrap(),
        json!({
            "theme": "pop-cut",
            "task_state": "Running",
            "quota_remaining_percent": 73.0,
            "quota_window_duration_mins": 300,
            "quota_secondary": null,
            "is_sample": false,
            "quota_resets_at_epoch_seconds": 1_788_177_600_i64,
            "quota_stale": true,
            "task_stale": true,
            "task_partial": false,
            "uploaded_at_epoch_seconds": 1_788_134_400_i64,
        })
    );
}

#[test]
fn conversion_rejects_an_out_of_range_quota_instead_of_displaying_it() {
    let result = StatusBoardState::from_codex_status(
        TaskState::Running,
        Some(QuotaStatus::single(101.0, Some(1_788_177_600), Some(300))),
        false,
        false,
        None,
    );

    assert_eq!(result, Err(StatusBoardModelError::QuotaOutOfRange));
}
