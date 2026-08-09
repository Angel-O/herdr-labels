#!/bin/sh

set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM

mkdir -p "$tmp/default/shell" "$tmp/default/target/release" "$tmp/override" "$tmp/home" "$tmp/zdot"
cp "$root/shell/hook.zsh" "$tmp/default/shell/hook.zsh"
cp "$root/shell/hook.bash" "$tmp/default/shell/hook.bash"

make_stub() {
  stub=$1
  cat >"$stub" <<'EOF'
#!/bin/sh
[ -z "${HERDR_LABELS_TEST_DELAY:-}" ] || sleep "$HERDR_LABELS_TEST_DELAY"
printf '%s\n' "$*" >>"$HERDR_LABELS_TEST_LOG"
exit "${HERDR_LABELS_TEST_STATUS:-0}"
EOF
  chmod +x "$stub"
}

make_stub "$tmp/default/target/release/herdr-labels"
make_stub "$tmp/override/herdr-labels"

fail() {
  printf 'hook tests: %s\n' "$1" >&2
  exit 1
}

wait_for_lines() {
  file=$1
  expected=$2
  attempts=0
  while [ "$(wc -l <"$file" | tr -d ' ')" -lt "$expected" ] && [ "$attempts" -lt 100 ]; do
    sleep 0.01
    attempts=$((attempts + 1))
  done
}

run_shell_test() {
  shell_name=$1
  shell_bin=$2
  hook=$3
  log=$tmp/$shell_name.log
  : >"$log"

  env -u HERDR_ENV -u HERDR_PANE_ID -u HERDR_TAB_ID -u HERDR_SOCKET_PATH \
    -u HERDR_LABELS_BIN -u BASH_ENV -u ENV HOME="$tmp/home" ZDOTDIR="$tmp/zdot" \
    HERDR_LABELS_TEST_LOG="$log" \
    "$shell_bin" -c '. "$1"; ! typeset -f "$2" >/dev/null' hooks "$hook" "_herdr_labels_${shell_name}_precmd"
  [ ! -s "$log" ] || fail "$shell_name did not no-op outside Herdr"

  env -u BASH_ENV -u ENV -u ZDOTDIR -u HERDR_LABELS_BIN \
    HOME="$tmp/home" ZDOTDIR="$tmp/zdot" \
    HERDR_ENV=1 HERDR_PANE_ID=pane HERDR_TAB_ID=tab HERDR_SOCKET_PATH=socket \
    HERDR_LABELS_TEST_LOG="$log" \
    "$shell_bin" -c '
      preexec_functions=()
      precmd_functions=()
      . "$1"
      . "$1"
      [ "${#preexec_functions[@]}" -eq 0 ] || exit 8
      _herdr_labels_'"$shell_name"'_precmd
      preexec_count=0
      precmd_count=0
      for hook_function in "${preexec_functions[@]}"; do
        [ "$hook_function" = _herdr_labels_'"$shell_name"'_preexec ] && preexec_count=$((preexec_count + 1))
      done
      for hook_function in "${precmd_functions[@]}"; do
        [ "$hook_function" = _herdr_labels_'"$shell_name"'_precmd ] && precmd_count=$((precmd_count + 1))
      done
      [ "$preexec_count" -eq 1 ] && [ "$precmd_count" -eq 1 ] || exit 9
      herdr_test_function() { :; }
      _herdr_labels_'"$shell_name"'_preexec "/bin/echo hello world"
      _herdr_labels_'"$shell_name"'_preexec "cd /tmp"
      _herdr_labels_'"$shell_name"'_preexec "herdr_test_function argument"
      _herdr_labels_'"$shell_name"'_preexec "if true; then :; fi"
      _herdr_labels_'"$shell_name"'_preexec "missing-herdr-command argument"
      wait
    ' hooks "$hook"

  wait_for_lines "$log" 7
  [ "$(wc -l <"$log" | tr -d ' ')" -eq 7 ] || fail "$shell_name emitted an unexpected number of calls"
  [ "$(grep -Fxc "preexec --shell $shell_name --program /bin/echo" "$log")" -eq 1 ] || fail "$shell_name external command classification failed"
  [ "$(grep -Fxc "preexec --shell $shell_name --sample" "$log")" -eq 4 ] || fail "$shell_name sample classification failed"
  [ "$(grep -Ec "^init --shell $shell_name --shell-pid [0-9]+$" "$log")" -eq 1 ] || fail "$shell_name eager hook failed"
  [ "$(grep -Ec "^precmd --shell $shell_name --shell-pid [0-9]+$" "$log")" -eq 1 ] || fail "$shell_name prompt hook failed"

  : >"$log"
  env -u BASH_ENV -u ENV HOME="$tmp/home" ZDOTDIR="$tmp/zdot" \
    HERDR_ENV=1 HERDR_PANE_ID=pane HERDR_TAB_ID=tab HERDR_SOCKET_PATH=socket \
    HERDR_LABELS_BIN="$tmp/override/herdr-labels" HERDR_LABELS_TEST_LOG="$log" \
    "$shell_bin" -c 'preexec_functions=(); precmd_functions=(); . "$1"; _herdr_labels_'"$shell_name"'_precmd; wait' hooks "$root/shell/hook.$shell_name"
  wait_for_lines "$log" 2
  [ "$(grep -Ec "^(init|precmd) --shell $shell_name --shell-pid [0-9]+$" "$log")" -eq 2 ] || fail "$shell_name binary override failed"
}

test_eager_claim() {
  shell_name=$1
  shell_bin=$2
  log=$tmp/$shell_name-eager.log
  : >"$log"

  env -u BASH_ENV -u ENV -u ZDOTDIR \
    HOME="$tmp/home" ZDOTDIR="$tmp/zdot" \
    HERDR_ENV=1 HERDR_PANE_ID=pane HERDR_TAB_ID=tab HERDR_SOCKET_PATH=socket \
    HERDR_LABELS_BIN="$tmp/override/herdr-labels" HERDR_LABELS_TEST_LOG="$log" \
    HERDR_LABELS_TEST_DELAY=0.05 \
    "$shell_bin" -c '. "$1"; [ -s "$HERDR_LABELS_TEST_LOG" ]; . "$1"' hooks "$root/shell/hook.$shell_name"

  wait_for_lines "$log" 1
  [ "$(wc -l <"$log" | tr -d ' ')" -eq 1 ] || fail "$shell_name eager claim was not emitted exactly once"
  [ "$(grep -Ec "^init --shell $shell_name --shell-pid [0-9]+$" "$log")" -eq 1 ] || fail "$shell_name eager claim was invalid"
}

test_bash_debug_fallback() {
  log=$tmp/bash-fallback.log
  : >"$log"
  env -u BASH_ENV -u ENV -u HERDR_LABELS_BIN \
    HOME="$tmp/home" ZDOTDIR="$tmp/zdot" \
    HERDR_ENV=1 HERDR_PANE_ID=pane HERDR_TAB_ID=tab HERDR_SOCKET_PATH=socket \
    HERDR_LABELS_TEST_LOG="$log" HERDR_LABELS_BIN="$tmp/override/herdr-labels" \
    bash -c '
      unset preexec_functions precmd_functions
      PROMPT_COMMAND=":"
      . "$1"
      [ -z "$(trap -p DEBUG)" ]
      eval "$PROMPT_COMMAND"
      /bin/echo user-command >/dev/null
      wait
    ' hooks "$root/shell/hook.bash"
  [ "$(grep -Fxc 'preexec --shell bash --program /bin/echo' "$log")" -eq 1 ] || fail "bash DEBUG fallback emitted more than one preexec per prompt"
  [ "$(grep -Ec '^init --shell bash --shell-pid [0-9]+$' "$log")" -eq 1 ] || fail "bash DEBUG fallback missed eager update"
  [ "$(grep -Ec '^precmd --shell bash --shell-pid [0-9]+$' "$log")" -eq 1 ] || fail "bash DEBUG fallback missed prompt update"
}

test_bash_late_preexec_array() {
  log=$tmp/bash-late-array.log
  : >"$log"
  env -u BASH_ENV -u ENV -u HERDR_LABELS_BIN \
    HOME="$tmp/home" ZDOTDIR="$tmp/zdot" \
    HERDR_ENV=1 HERDR_PANE_ID=pane HERDR_TAB_ID=tab HERDR_SOCKET_PATH=socket \
    HERDR_LABELS_TEST_LOG="$log" HERDR_LABELS_BIN="$tmp/override/herdr-labels" \
    bash -c '
      unset preexec_functions precmd_functions
      . "$1"
      [ -z "$(trap -p DEBUG)" ]
      preexec_functions=()
      _herdr_labels_bash_precmd
      [ "${preexec_functions[0]-}" = _herdr_labels_bash_preexec ]
      [ -z "$(trap -p DEBUG)" ]
      wait
    ' hooks "$root/shell/hook.bash"
}

test_bash_prompt_registration_refresh() {
  log=$tmp/bash-registration-refresh.log
  : >"$log"
  env -u BASH_ENV -u ENV -u HERDR_LABELS_BIN \
    HOME="$tmp/home" ZDOTDIR="$tmp/zdot" \
    HERDR_ENV=1 HERDR_PANE_ID=pane HERDR_TAB_ID=tab HERDR_SOCKET_PATH=socket \
    HERDR_LABELS_TEST_LOG="$log" HERDR_LABELS_BIN="$tmp/override/herdr-labels" \
    bash -c '
      unset preexec_functions precmd_functions
      PROMPT_COMMAND=before
      . "$1"
      PROMPT_COMMAND=framework
      . "$1"
      [ "$PROMPT_COMMAND" = "framework;_herdr_labels_bash_precmd" ]
      wait
    ' hooks "$root/shell/hook.bash"
  [ "$(grep -Ec '^init --shell bash --shell-pid [0-9]+$' "$log")" -eq 1 ] || fail "bash registration refresh repeated eager update"
}

test_bash_old_marker_upgrade() {
  log=$tmp/bash-old-marker.log
  : >"$log"
  env -u BASH_ENV -u ENV -u HERDR_LABELS_BIN \
    HOME="$tmp/home" ZDOTDIR="$tmp/zdot" \
    HERDR_ENV=1 HERDR_PANE_ID=pane HERDR_TAB_ID=tab HERDR_SOCKET_PATH=socket \
    HERDR_LABELS_TEST_LOG="$log" HERDR_LABELS_BIN="$tmp/override/herdr-labels" \
    bash -c '
      _HERDR_LABELS_BASH_HOOK_INSTALLED=1
      . "$1"
      declare -F _herdr_labels_bash_register_precmd >/dev/null
      wait
    ' hooks "$root/shell/hook.bash"
  [ "$(grep -Ec '^init --shell bash --shell-pid [0-9]+$' "$log")" -eq 1 ] || fail "bash old marker blocked hook upgrade"
}

test_failed_claim_is_nonfatal() {
  shell_name=$1
  shell_bin=$2
  log=$tmp/$shell_name-failed-claim.log
  : >"$log"

  env -u BASH_ENV -u ENV -u ZDOTDIR \
    HOME="$tmp/home" ZDOTDIR="$tmp/zdot" \
    HERDR_ENV=1 HERDR_PANE_ID=pane HERDR_TAB_ID=tab HERDR_SOCKET_PATH=socket \
    HERDR_LABELS_BIN="$tmp/override/herdr-labels" HERDR_LABELS_TEST_LOG="$log" \
    HERDR_LABELS_TEST_STATUS=2 \
    "$shell_bin" -c 'set -e; . "$1"; :' hooks "$root/shell/hook.$shell_name"
}

test_zsh_job_notifications() {
  output=$(
    env -u BASH_ENV -u ENV -u ZDOTDIR \
      HOME="$tmp/home" ZDOTDIR="$tmp/zdot" \
      HERDR_ENV=1 HERDR_PANE_ID=pane HERDR_TAB_ID=tab HERDR_SOCKET_PATH=socket \
      HERDR_LABELS_BIN="$tmp/override/herdr-labels" HERDR_LABELS_TEST_LOG="$tmp/zsh-jobs.log" \
      HERDR_LABELS_TEST_DELAY=0.05 \
      zsh -fic '. "$1"; _herdr_labels_zsh_run test; sleep 0.1' hooks "$root/shell/hook.zsh" 2>&1
  )
  [ -z "$output" ] || fail "zsh exposed background job notifications"
}

test_bash_job_notifications() {
  log=$tmp/bash-jobs.log
  : >"$log"
  env -u BASH_ENV -u ENV -u ZDOTDIR \
    HOME="$tmp/home" ZDOTDIR="$tmp/zdot" \
    HERDR_ENV=1 HERDR_PANE_ID=pane HERDR_TAB_ID=tab HERDR_SOCKET_PATH=socket \
    HERDR_LABELS_BIN="$tmp/override/herdr-labels" HERDR_LABELS_TEST_LOG="$log" \
    HERDR_LABELS_TEST_DELAY=0.05 \
    bash -c '
      set -m
      . "$1"
      [[ $- == *m* ]] || exit 10
      [ -z "$(jobs -p)" ] || exit 11
      _herdr_labels_bash_run test
      [ -z "$(jobs -p)" ] || exit 12
      sleep 0.1
    ' hooks "$root/shell/hook.bash"
}

command -v zsh >/dev/null 2>&1 || fail 'zsh is required'
command -v bash >/dev/null 2>&1 || fail 'bash is required'

run_shell_test zsh zsh "$tmp/default/shell/hook.zsh"
run_shell_test bash bash "$tmp/default/shell/hook.bash"
test_eager_claim zsh zsh
test_eager_claim bash bash
test_failed_claim_is_nonfatal zsh zsh
test_failed_claim_is_nonfatal bash bash
test_bash_debug_fallback
test_bash_late_preexec_array
test_bash_prompt_registration_refresh
test_bash_old_marker_upgrade
test_bash_job_notifications
test_zsh_job_notifications

printf 'hook tests passed\n'
