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
printf '%s\n' "$*" >>"$HERDR_LABELS_TEST_LOG"
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
      _herdr_labels_'"$shell_name"'_precmd
      wait
    ' hooks "$hook"

  wait_for_lines "$log" 6
  [ "$(wc -l <"$log" | tr -d ' ')" -eq 6 ] || fail "$shell_name emitted an unexpected number of calls"
  [ "$(grep -Fxc "preexec --shell $shell_name --program /bin/echo" "$log")" -eq 1 ] || fail "$shell_name external command classification failed"
  [ "$(grep -Fxc "preexec --shell $shell_name --sample" "$log")" -eq 4 ] || fail "$shell_name sample classification failed"
  [ "$(grep -Ec "^precmd --shell $shell_name --shell-pid [0-9]+$" "$log")" -eq 1 ] || fail "$shell_name prompt hook failed"

  : >"$log"
  env -u BASH_ENV -u ENV HOME="$tmp/home" ZDOTDIR="$tmp/zdot" \
    HERDR_ENV=1 HERDR_PANE_ID=pane HERDR_TAB_ID=tab HERDR_SOCKET_PATH=socket \
    HERDR_LABELS_BIN="$tmp/override/herdr-labels" HERDR_LABELS_TEST_LOG="$log" \
    "$shell_bin" -c 'preexec_functions=(); precmd_functions=(); . "$1"; _herdr_labels_'"$shell_name"'_precmd; wait' hooks "$root/shell/hook.$shell_name"
  wait_for_lines "$log" 1
  [ "$(grep -Ec "^precmd --shell $shell_name --shell-pid [0-9]+$" "$log")" -eq 1 ] || fail "$shell_name binary override failed"
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
      eval "$PROMPT_COMMAND"
      /bin/echo user-command >/dev/null
      wait
    ' hooks "$root/shell/hook.bash"
  [ "$(grep -Fxc 'preexec --shell bash --program /bin/echo' "$log")" -eq 1 ] || fail "bash DEBUG fallback emitted more than one preexec per prompt"
  [ "$(grep -Ec '^precmd --shell bash --shell-pid [0-9]+$' "$log")" -eq 1 ] || fail "bash DEBUG fallback missed precmd"
}

test_zsh_job_notifications() {
  output=$(
    env -u BASH_ENV -u ENV -u ZDOTDIR \
      HOME="$tmp/home" ZDOTDIR="$tmp/zdot" \
      HERDR_ENV=1 HERDR_PANE_ID=pane HERDR_TAB_ID=tab HERDR_SOCKET_PATH=socket \
      HERDR_LABELS_BIN="$tmp/override/herdr-labels" HERDR_LABELS_TEST_LOG="$tmp/zsh-jobs.log" \
      zsh -fic '. "$1"; _herdr_labels_zsh_run test; sleep 0.1' hooks "$root/shell/hook.zsh" 2>&1
  )
  [ -z "$output" ] || fail "zsh exposed background job notifications"
}

command -v zsh >/dev/null 2>&1 || fail 'zsh is required'
command -v bash >/dev/null 2>&1 || fail 'bash is required'

run_shell_test zsh zsh "$tmp/default/shell/hook.zsh"
run_shell_test bash bash "$tmp/default/shell/hook.bash"
test_bash_debug_fallback
test_zsh_job_notifications

printf 'hook tests passed\n'
