# Herdr Labels shell integration. Source this file from an interactive zsh.

# Do nothing outside a Herdr pane, or when this shell already loaded the hook.
[[ ${HERDR_ENV:-} == 1 && -n ${HERDR_PANE_ID:-} && -n ${HERDR_TAB_ID:-} && -n ${HERDR_SOCKET_PATH:-} ]] || return 0
[[ -z ${_HERDR_LABELS_ZSH_HOOK_INSTALLED:-} ]] || return 0

# Tests and custom installations can override the binary. Otherwise derive its
# location from this hook so both linked and downloaded plugins are relocatable.
if [[ -n ${HERDR_LABELS_BIN:-} ]]; then
  _HERDR_LABELS_ZSH_BIN=$HERDR_LABELS_BIN
else
  _herdr_labels_zsh_hook_file=${(%):-%N}
  _herdr_labels_zsh_hook_dir=${_herdr_labels_zsh_hook_file:A:h}
  _HERDR_LABELS_ZSH_BIN=${_herdr_labels_zsh_hook_dir:h}/target/release/herdr-labels
  unset _herdr_labels_zsh_hook_file _herdr_labels_zsh_hook_dir
fi

# Run command and prompt updates away from the foreground so they never wait for
# Herdr. The one-time initial claim below is synchronous to order it before
# preexec. Zsh's &! disowns later jobs and suppresses job-status messages.
_herdr_labels_zsh_run() {
  "$_HERDR_LABELS_ZSH_BIN" "$@" </dev/null >/dev/null 2>&1 &!
}

_herdr_labels_zsh_claim() {
  local claim_pid
  # Local job-control suppression keeps Zsh's process group foreground for PID
  # verification without printing background-job start or completion notices.
  setopt localoptions nomonitor
  "$_HERDR_LABELS_ZSH_BIN" init --shell zsh --shell-pid "$$" </dev/null >/dev/null 2>&1 &
  claim_pid=$!
  wait "$claim_pid" || :
}

# Zsh supplies the complete command line before execution. Its ${(z)...}
# expansion tokenizes with shell syntax without executing or evaluating input.
# Exact external commands can be verified by name; aliases, functions, builtins,
# and compound syntax use foreground-process sampling instead.
_herdr_labels_zsh_preexec() {
  local command_line=$1 word kind
  local -a words

  words=( ${(z)command_line} )
  if (( ${#words} )); then
    word=${(Q)words[1]}
    kind=$(whence -w -- "$word" 2>/dev/null)
    if [[ $kind == *': command' ]]; then
      _herdr_labels_zsh_run preexec --shell zsh --program "$word"
      return
    fi
  fi

  _herdr_labels_zsh_run preexec --shell zsh --sample
}

# Returning to the prompt means the interactive shell is foreground again.
_herdr_labels_zsh_precmd() {
  if [[ -z ${_HERDR_LABELS_ZSH_PREEXEC_ACTIVE:-} ]]; then
    add-zsh-hook preexec _herdr_labels_zsh_preexec
    typeset -g _HERDR_LABELS_ZSH_PREEXEC_ACTIVE=1
  fi
  _herdr_labels_zsh_run precmd --shell zsh --shell-pid "$$"
}

# Register through Zsh's hook API instead of replacing another integration's
# preexec or precmd handlers.
autoload -Uz add-zsh-hook
typeset -g _HERDR_LABELS_ZSH_HOOK_INSTALLED=1
_herdr_labels_zsh_claim
add-zsh-hook precmd _herdr_labels_zsh_precmd
