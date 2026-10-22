#!/bin/sh
set -u

[ "$#" -eq 1 ] || {
  printf 'usage: %s <companion-binary>\n' "$0" >&2
  exit 2
}

companion_bin=$1
codex_home=${CODEX_HOME:-"$HOME/.codex"}
codex_bin=${CODEX_ZECTRIX_CODEX_BIN:-codex}
staging_root=${CANARY_STAGING_ROOT:-"$codex_home/.tmp/marketplaces/.staging"}
launchctl_bin=${CANARY_LAUNCHCTL_BIN:-/bin/launchctl}
launch_agent_label=${CANARY_LAUNCH_AGENT_LABEL:-com.barrybarrywu.codex-zectrix-dashboard}
initialization_timeout=${CANARY_INITIALIZATION_TIMEOUT_SECONDS:-180}
initialization_quiet=${CANARY_INITIALIZATION_QUIET_SECONDS:-30}
steady_seconds=${CANARY_STEADY_SECONDS:-600}
sample_seconds=${CANARY_SAMPLE_SECONDS:-1}
shutdown_timeout=${CANARY_SHUTDOWN_TIMEOUT_SECONDS:-15}
max_disk_drop=${CANARY_MAX_DISK_DROP_KIB:-524288}

for value in "$initialization_timeout" "$initialization_quiet" "$steady_seconds" "$sample_seconds" "$shutdown_timeout" "$max_disk_drop"; do
  case "$value" in
    ''|*[!0-9]*) printf 'gate_failed=invalid_duration\n' >&2; exit 2 ;;
  esac
done
for value in "$initialization_timeout" "$initialization_quiet" "$steady_seconds" "$sample_seconds" "$shutdown_timeout"; do
  [ "$value" -gt 0 ] || { printf 'gate_failed=invalid_duration\n' >&2; exit 2; }
done

[ -x "$companion_bin" ] || { printf 'gate_failed=companion_not_executable\n' >&2; exit 2; }
[ -d "$codex_home" ] || { printf 'gate_failed=codex_home_missing\n' >&2; exit 2; }

user_domain="gui/$(id -u)"
companion_pid=
companion_pgid=
app_pid=
app_pgid=
baseline_count=unknown
baseline_size=unknown
baseline_entries=unknown
initial_disk=unknown
minimum_disk=unknown
staging_count=0
staging_size=0
staging_entries=0
cleanup_running=no
launch_agent_state=unknown
owned_pid_log=$(mktemp "${TMPDIR:-/tmp}/codex-zectrix-canary-pids.XXXXXX")
owned_group_log=$(mktemp "${TMPDIR:-/tmp}/codex-zectrix-canary-groups.XXXXXX")
baseline_mcp_log=$(mktemp "${TMPDIR:-/tmp}/codex-zectrix-canary-mcp-baseline.XXXXXX")
final_mcp_log=$(mktemp "${TMPDIR:-/tmp}/codex-zectrix-canary-mcp-final.XXXXXX")
new_mcp_log=$(mktemp "${TMPDIR:-/tmp}/codex-zectrix-canary-mcp-new.XXXXXX")
launchctl_log=$(mktemp "${TMPDIR:-/tmp}/codex-zectrix-canary-launchctl.XXXXXX")
codex_wrapper=$(mktemp "${TMPDIR:-/tmp}/codex-zectrix-canary-codex.XXXXXX")
printf '%s\n' '#!/bin/sh' \
  'printf "%s\n" "$$" >>"$CANARY_OWNED_PID_LOG"' \
  'ps -o pgid= -p "$$" | tr -d " " >>"$CANARY_OWNED_GROUP_LOG"' \
  'exec "$CANARY_REAL_CODEX_BIN" "$@"' >"$codex_wrapper"
chmod 700 "$codex_wrapper"

fail() {
  printf 'gate_failed=%s\n' "$1" >&2
  exit 1
}

read_launch_agent_state() {
  if "$launchctl_bin" print "$user_domain/$launch_agent_label" >"$launchctl_log" 2>&1; then
    launch_agent_state=loaded
  elif grep -q 'Could not find service' "$launchctl_log"; then
    launch_agent_state=unloaded
  else
    launch_agent_state=unknown
  fi
}

launch_agent_unloaded() {
  read_launch_agent_state
  [ "$launch_agent_state" = unloaded ]
}

read_staging_stats() {
  if [ ! -d "$staging_root" ]; then
    staging_count=0
    staging_size=0
    staging_entries=0
    return
  fi
  staging_count=$(find "$staging_root" -mindepth 1 -maxdepth 1 -type d -name 'marketplace-upgrade-*' -print | wc -l | tr -d ' ')
  staging_size=$(find "$staging_root" -mindepth 1 -maxdepth 1 -type d -name 'marketplace-upgrade-*' -exec du -sk {} + 2>/dev/null | awk '{total += $1} END {print total + 0}')
  staging_entries=$(find "$staging_root" -mindepth 2 -maxdepth 2 -path "$staging_root/marketplace-upgrade-*/*" -print 2>/dev/null | wc -l | tr -d ' ')
}

staging_matches_baseline() {
  [ "$staging_count" -eq "$baseline_count" ] &&
    [ "$staging_size" -eq "$baseline_size" ] &&
    [ "$staging_entries" -eq "$baseline_entries" ]
}

snapshot_mcp_pids() {
  ps -axo pid=,command= | awk '/mcp/ && !/awk/ {print $1}' | sort -u
}

pid_list() {
  awk 'NF {if (seen) printf ","; printf "%s", $1; seen=1} END {print ""}' "$1"
}

owned_app_rows() {
  ps -axo pid=,ppid=,pgid=,command= |
    awk -v parent="$companion_pid" '$2 == parent && index($0, " app-server --stdio") {print $1, $3}'
}

record_app_processes() {
  [ -n "$app_pid" ] || return
  rows=$(ps -axo pid=,ppid=,pgid= | awk -v root="$app_pid" '
    { pids[NR]=$1; parents[NR]=$2; groups[NR]=$3 }
    END {
      owned[root]=1
      changed=1
      while (changed) {
        changed=0
        for (row=1; row<=NR; row++) {
          if (owned[parents[row]] && !owned[pids[row]]) {
            owned[pids[row]]=1
            changed=1
          }
        }
      }
      for (row=1; row<=NR; row++) {
        if (owned[pids[row]]) print pids[row], groups[row]
      }
    }
  ')
  printf '%s\n' "$rows" | awk 'NF {print $1}' >>"$owned_pid_log"
  printf '%s\n' "$rows" | awk -v primary="$app_pgid" 'NF && ($1 == $2 || $2 == primary) {print $2}' >>"$owned_group_log"
}

read_disk() {
  current_disk=$(df -k "$codex_home" | awk 'NR == 2 {print $4}')
  [ "$minimum_disk" = unknown ] || [ "$current_disk" -ge "$minimum_disk" ] || minimum_disk=$current_disk
  if [ "$initial_disk" != unknown ]; then
    disk_drop=$((initial_disk - current_disk))
    [ "$disk_drop" -le "$max_disk_drop" ] || fail disk_availability_dropped
  fi
}

observe_sources() {
  phase=$1
  diagnostics=$("$companion_bin" diagnostics 2>&1) || fail "${phase}_diagnostics_failed"
  quota_source=$(printf '%s\n' "$diagnostics" | awk -F= '$1 == "quota_source" {print $2; exit}')
  task_activity_source=$(printf '%s\n' "$diagnostics" | awk -F= '$1 == "task_activity_source" {print $2; exit}')
  [ "$quota_source" = current ] || fail "${phase}_quota_source_not_current"
  [ "$task_activity_source" = inferred ] || fail "${phase}_task_activity_source_invalid"
  printf '%s_quota_source=%s\n%s_task_activity_source=%s\n' \
    "$phase" "$quota_source" "$phase" "$task_activity_source"
}

verify_runtime() {
  kill -0 "$companion_pid" 2>/dev/null || fail companion_exited
  launch_agent_unloaded || fail launch_agent_not_proven_unloaded
  rows=$(owned_app_rows)
  row_count=$(printf '%s\n' "$rows" | awk 'NF {count++} END {print count + 0}')
  [ "$row_count" -eq 1 ] || fail "owned_app_server_count_$row_count"
  current_app_pid=$(printf '%s\n' "$rows" | awk 'NF {print $1; exit}')
  current_app_pgid=$(printf '%s\n' "$rows" | awk 'NF {print $2; exit}')
  [ "$current_app_pid" = "$app_pid" ] || fail app_server_replaced
  [ "$current_app_pgid" = "$app_pgid" ] || fail app_server_group_changed
  record_app_processes
  read_disk
}

cleanup() {
  result=$?
  [ "$cleanup_running" = no ] || exit "$result"
  cleanup_running=yes
  trap - EXIT INT TERM HUP

  if [ -n "$companion_pid" ]; then
    record_app_processes
    kill -TERM "$companion_pid" 2>/dev/null || true
    shutdown_started=$(date +%s)
    while kill -0 "$companion_pid" 2>/dev/null; do
      now=$(date +%s)
      if [ $((now - shutdown_started)) -ge "$shutdown_timeout" ]; then
        kill -KILL "$companion_pid" 2>/dev/null || true
        printf 'gate_failed=companion_shutdown_timeout\n' >&2
        result=1
        break
      fi
      sleep 1
    done
    wait "$companion_pid" 2>/dev/null || true
  fi

  cleanup_failed=no
  owned_cleanup_failed=no
  if [ -s "$owned_group_log" ]; then
    for group in $(sort -nu "$owned_group_log"); do
      [ -n "$group" ] || continue
      [ "$group" = "$companion_pgid" ] && continue
      if ps -axo pgid= | awk -v group="$group" '$1 == group {found=1} END {exit !found}'; then
        cleanup_failed=yes
        owned_cleanup_failed=yes
        kill -TERM "-$group" 2>/dev/null || true
        sleep 1
      fi
      if ps -axo pgid= | awk -v group="$group" '$1 == group {found=1} END {exit !found}'; then
        kill -KILL "-$group" 2>/dev/null || true
        cleanup_failed=yes
      fi
    done
  fi
  if [ -s "$owned_pid_log" ]; then
    live_owned_pid=no
    for pid in $(sort -nu "$owned_pid_log"); do
      if kill -0 "$pid" 2>/dev/null; then
        cleanup_failed=yes
        owned_cleanup_failed=yes
        live_owned_pid=yes
        kill -TERM "$pid" 2>/dev/null || true
      fi
    done
    if [ "$live_owned_pid" = yes ]; then
      sleep 1
      for pid in $(sort -nu "$owned_pid_log"); do
        kill -0 "$pid" 2>/dev/null && kill -KILL "$pid" 2>/dev/null || true
      done
    fi
  fi

  snapshot_mcp_pids >"$final_mcp_log"
  comm -13 "$baseline_mcp_log" "$final_mcp_log" >"$new_mcp_log"
  global_new_mcp_processes=$(awk 'NF {count++} END {print count + 0}' "$new_mcp_log")
  read_launch_agent_state
  case "$launch_agent_state" in
    unloaded)
      final_launch_agent_unloaded=yes
      ;;
    loaded)
      printf 'gate_failed=final_launch_agent_loaded\n' >&2
      cleanup_failed=yes
      if "$launchctl_bin" bootout "$user_domain/$launch_agent_label" >>"$launchctl_log" 2>&1 && launch_agent_unloaded; then
        final_launch_agent_unloaded=yes
      else
        printf 'gate_failed=final_launch_agent_recovery_failed\n' >&2
        final_launch_agent_unloaded=no
      fi
      ;;
    *)
      printf 'gate_failed=final_launch_agent_state_unknown\n' >&2
      cleanup_failed=yes
      final_launch_agent_unloaded=no
      ;;
  esac

  final_companions=$(ps -axo command= | awk 'index($0, "codex-zectrix-dashboard") && $NF == "companion" {count++} END {print count + 0}')
  final_app_servers=$(ps -axo command= | awk '/codex([[:space:]]|$).*app-server/ && !/awk/ {count++} END {print count + 0}')
  final_disk=$(df -k "$codex_home" | awk 'NR == 2 {print $4}')
  read_staging_stats
  if [ "$initial_disk" != unknown ] && [ $((initial_disk - final_disk)) -gt "$max_disk_drop" ]; then
    printf 'gate_failed=final_disk_availability_dropped\n' >&2
    cleanup_failed=yes
  fi
  if [ "$baseline_count" != unknown ] && ! staging_matches_baseline; then
    printf 'gate_failed=final_staging_changed\n' >&2
    cleanup_failed=yes
  fi
  printf 'final_launch_agent_unloaded=%s\nfinal_companion_count=%s\nfinal_app_server_count=%s\n' \
    "$final_launch_agent_unloaded" "$final_companions" "$final_app_servers"
  printf 'initial_global_mcp_pids=%s\nfinal_global_mcp_pids=%s\nnew_global_mcp_pids=%s\nglobal_new_mcp_processes=%s\n' \
    "$(pid_list "$baseline_mcp_log")" "$(pid_list "$final_mcp_log")" "$(pid_list "$new_mcp_log")" "$global_new_mcp_processes"
  printf 'final_disk_available_kib=%s\nfinal_staging_count=%s\nfinal_staging_kib=%s\nfinal_staging_entries=%s\nphysical_note4=not_run\n' \
    "$final_disk" "$staging_count" "$staging_size" "$staging_entries"

  find "$owned_pid_log" "$owned_group_log" "$baseline_mcp_log" "$final_mcp_log" "$new_mcp_log" "$launchctl_log" "$codex_wrapper" -delete 2>/dev/null || true
  if [ "$owned_cleanup_failed" = yes ]; then
    printf 'gate_failed=owned_process_cleanup\n' >&2
  fi
  if [ "$cleanup_failed" = yes ]; then
    exit 1
  fi
  printf 'owned_process_cleanup=passed\n'
  exit "$result"
}

trap cleanup EXIT
trap 'exit 130' INT TERM HUP

launch_agent_unloaded || fail launch_agent_not_proven_unloaded_before_start
initial_companions=$(ps -axo command= | awk 'index($0, "codex-zectrix-dashboard") && $NF == "companion" {count++} END {print count + 0}')
[ "$initial_companions" -eq 0 ] || fail dashboard_companion_already_running
initial_app_servers=$(ps -axo command= | awk '/codex([[:space:]]|$).*app-server/ && !/awk/ {count++} END {print count + 0}')
snapshot_mcp_pids >"$baseline_mcp_log"
initial_mcp_processes=$(awk 'NF {count++} END {print count + 0}' "$baseline_mcp_log")
read_staging_stats
baseline_count=$staging_count
baseline_size=$staging_size
baseline_entries=$staging_entries
initial_disk=$(df -k "$codex_home" | awk 'NR == 2 {print $4}')
minimum_disk=$initial_disk
printf 'initial_launch_agent_unloaded=yes\ninitial_companion_count=%s\ninitial_app_server_count=%s\ninitial_mcp_process_count=%s\n' \
  "$initial_companions" "$initial_app_servers" "$initial_mcp_processes"
printf 'initial_global_mcp_pids=%s\n' "$(pid_list "$baseline_mcp_log")"
printf 'initial_disk_available_kib=%s\nmax_disk_drop_kib=%s\ninitial_staging_count=%s\ninitial_staging_kib=%s\ninitial_staging_entries=%s\n' \
  "$initial_disk" "$max_disk_drop" "$baseline_count" "$baseline_size" "$baseline_entries"

CODEX_ZECTRIX_CODEX_BIN="$codex_wrapper" \
  CANARY_REAL_CODEX_BIN="$codex_bin" \
  CANARY_OWNED_PID_LOG="$owned_pid_log" \
  CANARY_OWNED_GROUP_LOG="$owned_group_log" \
  "$companion_bin" companion &
companion_pid=$!
companion_pgid=$(ps -o pgid= -p "$companion_pid" | tr -d ' ')
initialization_started=$(date +%s)
initialization_deadline=$((initialization_started + initialization_timeout))
quiet_since=
peak_count=$baseline_count
peak_size=$baseline_size
peak_entries=$baseline_entries

while :; do
  now=$(date +%s)
  [ "$now" -lt "$initialization_deadline" ] || fail initialization_timeout
  kill -0 "$companion_pid" 2>/dev/null || fail companion_exited_during_initialization
  launch_agent_unloaded || fail launch_agent_not_proven_unloaded

  read_staging_stats
  [ "$staging_count" -le "$peak_count" ] || peak_count=$staging_count
  [ "$staging_size" -le "$peak_size" ] || peak_size=$staging_size
  [ "$staging_entries" -le "$peak_entries" ] || peak_entries=$staging_entries
  read_disk

  rows=$(owned_app_rows)
  row_count=$(printf '%s\n' "$rows" | awk 'NF {count++} END {print count + 0}')
  [ "$row_count" -le 1 ] || fail "owned_app_server_count_$row_count"
  if [ "$row_count" -eq 1 ]; then
    current_app_pid=$(printf '%s\n' "$rows" | awk 'NF {print $1; exit}')
    current_app_pgid=$(printf '%s\n' "$rows" | awk 'NF {print $2; exit}')
    if [ -z "$app_pid" ]; then
      app_pid=$current_app_pid
      app_pgid=$current_app_pgid
      [ "$app_pgid" != "$companion_pgid" ] || fail app_server_not_in_independent_group
    else
      [ "$current_app_pid" = "$app_pid" ] || fail app_server_replaced
      [ "$current_app_pgid" = "$app_pgid" ] || fail app_server_group_changed
    fi
    record_app_processes
  fi

  if [ -n "$app_pid" ] && staging_matches_baseline; then
    [ -n "$quiet_since" ] || quiet_since=$now
    if [ $((now - quiet_since)) -ge "$initialization_quiet" ]; then
      break
    fi
  else
    quiet_since=
  fi
  sleep "$sample_seconds"
done

initialization_finished=$(date +%s)
printf 'initialization=passed\ninitialization_elapsed_seconds=%s\ninitialization_quiet_seconds=%s\n' \
  "$((initialization_finished - initialization_started))" "$initialization_quiet"
printf 'initialization_staging_peak_count=%s\ninitialization_staging_peak_kib=%s\ninitialization_staging_peak_entries=%s\n' \
  "$peak_count" "$peak_size" "$peak_entries"
printf 'initialization_min_disk_available_kib=%s\ninitialization_app_server_pid=%s\ninitialization_app_server_pgid=%s\n' \
  "$minimum_disk" "$app_pid" "$app_pgid"
observe_sources initialization

steady_started=$(date +%s)
steady_deadline=$((steady_started + steady_seconds))
while :; do
  verify_runtime
  read_staging_stats
  staging_matches_baseline || fail steady_staging_changed
  now=$(date +%s)
  [ "$now" -lt "$steady_deadline" ] || break
  sleep "$sample_seconds"
done

steady_finished=$(date +%s)
printf 'steady_state=passed\nsteady_elapsed_seconds=%s\nsteady_app_server_pid=%s\nsteady_app_server_pgid=%s\n' \
  "$((steady_finished - steady_started))" "$app_pid" "$app_pgid"
printf 'steady_staging_count=%s\nsteady_staging_kib=%s\nsteady_staging_entries=%s\nsteady_min_disk_available_kib=%s\n' \
  "$staging_count" "$staging_size" "$staging_entries" "$minimum_disk"
observe_sources steady
