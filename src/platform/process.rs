#[cfg(any(unix, test))]
use std::collections::HashSet;

#[cfg(unix)]
#[path = "process_unix.rs"]
mod implementation;
#[cfg(windows)]
#[path = "process_windows.rs"]
mod implementation;

pub use implementation::{OwnedProcess, ProcessError};

#[cfg(any(unix, test))]
#[derive(Clone, Copy)]
pub(super) struct ProcessRow {
    pub pid: i32,
    pub parent_pid: i32,
    pub process_group_id: i32,
}

#[cfg(any(unix, test))]
pub(super) fn rebuild_process_snapshot(
    rows: &[ProcessRow],
    root_process_group_id: i32,
) -> (HashSet<i32>, HashSet<i32>) {
    let mut process_ids = HashSet::new();
    let mut process_group_ids = HashSet::from([root_process_group_id]);
    loop {
        let mut changed = false;
        for row in rows {
            if (process_ids.contains(&row.parent_pid)
                || process_group_ids.contains(&row.process_group_id))
                && process_ids.insert(row.pid)
            {
                changed = true;
            }
            if process_ids.contains(&row.pid)
                && row.pid == row.process_group_id
                && process_group_ids.insert(row.process_group_id)
            {
                changed = true;
            }
        }
        if !changed {
            return (process_ids, process_group_ids);
        }
    }
}

#[cfg(any(unix, test))]
pub(super) struct ProcessTreeSnapshot {
    root_process_group_id: i32,
    process_ids: HashSet<i32>,
    process_group_ids: HashSet<i32>,
    observation_failed: bool,
}

#[cfg(any(unix, test))]
impl ProcessTreeSnapshot {
    pub(super) fn new(root_pid: i32, root_process_group_id: i32) -> Self {
        Self {
            root_process_group_id,
            process_ids: HashSet::from([root_pid]),
            process_group_ids: HashSet::from([root_process_group_id]),
            observation_failed: true,
        }
    }

    pub(super) fn observe(&mut self, rows: Result<&[ProcessRow], ()>) {
        match rows {
            Ok(rows) => {
                let (process_ids, process_group_ids) =
                    rebuild_process_snapshot(rows, self.root_process_group_id);
                self.process_ids = process_ids;
                self.process_group_ids = process_group_ids;
                self.observation_failed = false;
            }
            Err(()) => {
                self.process_ids.clear();
                self.process_group_ids = HashSet::from([self.root_process_group_id]);
                self.observation_failed = true;
            }
        }
    }

    pub(super) fn has_live_processes(&self, mut process_exists: impl FnMut(i32) -> bool) -> bool {
        self.observation_failed || self.process_ids.iter().copied().any(&mut process_exists)
    }

    pub(super) fn signal_process_ids(&self) -> &HashSet<i32> {
        &self.process_ids
    }

    pub(super) fn signal_process_group_ids(&self) -> &HashSet<i32> {
        &self.process_group_ids
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{ProcessRow, ProcessTreeSnapshot, rebuild_process_snapshot};

    #[test]
    fn rebuilding_the_unix_snapshot_drops_a_reused_descendant_pid() {
        let rows = [
            ProcessRow {
                pid: 100,
                parent_pid: 1,
                process_group_id: 100,
            },
            ProcessRow {
                pid: 200,
                parent_pid: 1,
                process_group_id: 200,
            },
        ];

        let (process_ids, process_group_ids) = rebuild_process_snapshot(&rows, 100);

        assert_eq!(process_ids, [100].into_iter().collect());
        assert_eq!(process_group_ids, [100].into_iter().collect());
    }

    #[test]
    fn failed_unix_observation_stays_armed_with_the_stable_root_group() {
        let mut snapshot = ProcessTreeSnapshot::new(100, 100);
        snapshot.observe(Ok(&[
            ProcessRow {
                pid: 100,
                parent_pid: 1,
                process_group_id: 100,
            },
            ProcessRow {
                pid: 200,
                parent_pid: 100,
                process_group_id: 100,
            },
        ]));

        snapshot.observe(Err(()));

        assert!(snapshot.has_live_processes(|_| false));
        assert!(snapshot.signal_process_ids().is_empty());
        assert_eq!(snapshot.signal_process_group_ids(), &HashSet::from([100]));

        snapshot.observe(Ok(&[ProcessRow {
            pid: 100,
            parent_pid: 1,
            process_group_id: 100,
        }]));
        assert_eq!(snapshot.signal_process_group_ids(), &HashSet::from([100]));

        snapshot.observe(Ok(&[]));
        assert!(!snapshot.has_live_processes(|_| false));
    }
}
