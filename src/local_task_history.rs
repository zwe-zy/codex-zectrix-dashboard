//! Metadata-only compatibility adapter for Desktop's paginated history store.
//! The private app-server reports another process's active turn as interrupted.
//! Query only ids, lifecycle status and times for roots enumerated by that API.
use crate::{CodexStatusError, TaskState};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

pub struct LocalTaskHistory {
    connection: Connection,
}

impl LocalTaskHistory {
    /// Desktop continuation rollouts retain the logical thread ID but append
    /// the execution ID used by the history projection. Only trust the path
    /// supplied for this enumerated root; never scan files or message contents.
    pub fn read_state_for_rollout(
        &self,
        thread_id: &str,
        rollout: Option<&Path>,
        now: i64,
    ) -> Result<Option<TaskState>, CodexStatusError> {
        let history_id = history_id(thread_id, rollout)?;
        let state = self.read_state(history_id, now)?;
        if history_id != thread_id && state.is_none() {
            // Projection lag or stale activity must not revive the previous
            // execution's ended state, including through the private server.
            return Ok(Some(TaskState::Unavailable));
        }
        Ok(state)
    }

    pub fn from_environment() -> Result<Self, CodexStatusError> {
        let directory = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                    .map(|home| PathBuf::from(home).join(".codex"))
            })
            .ok_or(CodexStatusError::Unavailable)?;
        Self::open(&directory)
    }

    pub fn open(directory: &Path) -> Result<Self, CodexStatusError> {
        if !directory.is_absolute() {
            return Err(CodexStatusError::Incompatible);
        }
        // Never silently use an abandoned old store after Codex migrates it.
        for entry in std::fs::read_dir(directory).map_err(|_| CodexStatusError::Unavailable)? {
            let entry = entry.map_err(|_| CodexStatusError::Unavailable)?;
            let name = entry.file_name();
            if name
                .to_str()
                .and_then(|s| s.strip_prefix("thread_history_"))
                .and_then(|s| s.strip_suffix(".sqlite"))
                .and_then(|s| s.parse::<u32>().ok())
                .is_some_and(|version| version != 1)
            {
                return Err(CodexStatusError::Incompatible);
            }
        }
        let connection = Connection::open_with_flags(
            directory.join("thread_history_1.sqlite"),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|_| CodexStatusError::Unavailable)?;
        connection
            .busy_timeout(Duration::from_millis(100))
            .map_err(|_| CodexStatusError::Unavailable)?;
        connection
            .execute_batch("PRAGMA query_only=ON; BEGIN DEFERRED")
            .map_err(|_| CodexStatusError::Unavailable)?;
        Ok(Self { connection })
    }

    pub fn read_state(
        &self,
        thread_id: &str,
        now: i64,
    ) -> Result<Option<TaskState>, CodexStatusError> {
        let turn=self.connection.query_row(
            "SELECT turn_id,status,started_at,completed_at FROM thread_turns WHERE thread_id=?1 ORDER BY rollout_ordinal DESC LIMIT 1",
            [thread_id], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,Option<i64>>(2)?,row.get::<_,Option<i64>>(3)?)))
            .optional().map_err(|_| CodexStatusError::Incompatible)?;
        let Some((turn_id, status, started, completed)) = turn else {
            return Ok(None);
        };
        let Some(started) = started.filter(|t| *t >= 0 && *t <= now) else {
            return Ok(None);
        };
        if status == "inProgress" {
            if completed.is_some() {
                return Ok(None);
            };
            // No message bodies, item_json, error_json, tool arguments or titles.
            let times=self.connection.query_row(
                "SELECT MAX(created_at_ms),MAX(started_at_ms),MAX(completed_at_ms) FROM thread_items WHERE thread_id=?1 AND turn_id=?2",
                [thread_id,&turn_id], |row| Ok([row.get::<_,Option<i64>>(0)?,row.get::<_,Option<i64>>(1)?,row.get::<_,Option<i64>>(2)?]))
                .map_err(|_| CodexStatusError::Incompatible)?;
            let last = times
                .into_iter()
                .flatten()
                .map(|t| t / 1000)
                .chain([started])
                .max()
                .unwrap_or(started);
            return Ok(
                (last <= now && now.saturating_sub(last) < 180).then_some(TaskState::Running)
            );
        }
        let Some(ended) =
            completed.filter(|t| *t >= started && *t <= now && now.saturating_sub(*t) < 86400)
        else {
            return Ok(None);
        };
        let _ = ended;
        Ok(match status.as_str() {
            "completed" => Some(TaskState::Completed),
            "failed" => Some(TaskState::Failed),
            "interrupted" => Some(TaskState::Interrupted),
            _ => None,
        })
    }
}

pub(crate) fn history_id<'a>(
    thread_id: &'a str,
    rollout: Option<&'a Path>,
) -> Result<&'a str, CodexStatusError> {
    let Some(path) = rollout else {
        return Ok(thread_id);
    };
    let invalid = || CodexStatusError::Incompatible;
    if !path.is_absolute() || !uuid(thread_id) {
        return Err(invalid());
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(invalid)?;
    let body = name
        .strip_prefix("rollout-")
        .and_then(|name| name.strip_suffix(".jsonl"))
        .ok_or_else(invalid)?;
    let timestamp = body.get(..19).ok_or_else(invalid)?;
    if !timestamp.bytes().enumerate().all(|(i, byte)| match i {
        4 | 7 | 13 | 16 => byte == b'-',
        10 => byte == b'T',
        _ => byte.is_ascii_digit(),
    }) {
        return Err(invalid());
    }
    let ids = body
        .get(19..)
        .and_then(|s| s.strip_prefix('-'))
        .ok_or_else(invalid)?;
    if ids == thread_id {
        return Ok(thread_id);
    }
    let execution = ids
        .strip_prefix(thread_id)
        .and_then(|s| s.strip_prefix('_'))
        .filter(|id| uuid(id))
        .ok_or_else(invalid)?;
    Ok(execution)
}

fn uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, byte)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}
