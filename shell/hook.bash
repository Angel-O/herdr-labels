# Herdr Labels shell integration. Source this file from an interactive bash.

# Do nothing outside a Herdr pane, or when this shell already loaded the hook.
[[ ${HERDR_ENV:-} == 1 && -n ${HERDR_PANE_ID:-} && -n ${HERDR_TAB_ID:-} && -n ${HERDR_SOCKET_PATH:-} ]] || return 0
[[ -z ${_HERDR_LABELS_BASH_HOOK_INSTALLED:-} ]] || return 0

# Tests and custom installations can override the binary. Otherwise derive its
# location from this hook so both linked and downloaded plugins are relocatable.
if [[ -n ${HERDR_LABELS_BIN:-} ]]; then
  _HERDR_LABELS_BASH_BIN=$HERDR_LABELS_BIN
else
  _herdr_labels_bash_hook_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
  _HERDR_LABELS_BASH_BIN=$(cd -- "$_herdr_labels_bash_hook_dir/.." && pwd)/target/release/herdr-labels
  unset _herdr_labels_bash_hook_dir
fi

# Run label updates in the background so commands and prompts never wait for
# Herdr. All helper output is intentionally detached from the terminal.
_herdr_labels_bash_run() {
  "$_HERDR_LABELS_BASH_BIN" "$@" </dev/null >/dev/null 2>&1 &
}

# Bash does not provide Zsh-style parsing here, so only accept a deliberately
# narrow first word before asking `type` whether it is an executable file. This
# avoids evaluating user input. Aliases, functions, builtins, and complex shell
# syntax fall back to foreground-process sampling.
_herdr_labels_bash_preexec() {
  local command_line=${1-} word kind

  word=${command_line%%[[:space:]]*}
  if [[ -n $word && $word =~ ^[[:alnum:]_./+-]+$ ]]; then
    kind=$(type -t -- "$word" 2>/dev/null || :)
    if [[ $kind == file ]]; then
      _herdr_labels_bash_run preexec --shell bash --program "$word"
      return
    fi
  fi

  _herdr_labels_bash_run preexec --shell bash --sample
}

# Returning to the prompt means Bash is foreground again. READY also arms the
# DEBUG fallback to report exactly the next user command, rather than every
# command executed while drawing the prompt.
_herdr_labels_bash_precmd() {
  _herdr_labels_bash_run precmd --shell bash --shell-pid "$$"
  _HERDR_LABELS_BASH_READY=1
}

# Some Bash setups expose no preexec hook array. In that fallback, DEBUG fires
# before many simple commands, so this gate forwards only the first event after
# each prompt and remains closed until precmd rearms it.
_herdr_labels_bash_debug_trap() {
  [[ ${_HERDR_LABELS_BASH_READY:-1} == 1 ]] || return 0
  _HERDR_LABELS_BASH_READY=0
  _herdr_labels_bash_preexec "${1-}"
}

# Hook arrays may already contain handlers from other integrations. Check before
# appending ours so sourcing this file cannot register duplicates.
_herdr_labels_bash_array_contains() {
  local wanted=$1 item
  shift
  for item in "$@"; do
    [[ $item == "$wanted" ]] && return 0
  done
  return 1
}

# Prefer the de facto preexec_functions API when another framework provides it.
# Install a DEBUG trap only when no framework and no existing DEBUG trap exist;
# an existing trap belongs to the user and must not be replaced.
if [[ $(declare -p preexec_functions 2>/dev/null) == declare\ -a* ]]; then
  _herdr_labels_bash_array_contains _herdr_labels_bash_preexec "${preexec_functions[@]}" ||
    preexec_functions+=(_herdr_labels_bash_preexec)
elif [[ -z $(trap -p DEBUG) ]]; then
  _HERDR_LABELS_BASH_INSTALL_DEBUG=1
fi

# Bash versions and prompt frameworks represent PROMPT_COMMAND as either an
# array or a semicolon-separated string. Preserve either form and append ours.
if [[ $(declare -p precmd_functions 2>/dev/null) == declare\ -a* ]]; then
  _herdr_labels_bash_array_contains _herdr_labels_bash_precmd "${precmd_functions[@]}" ||
    precmd_functions+=(_herdr_labels_bash_precmd)
elif [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == declare\ -a* ]]; then
  _herdr_labels_bash_array_contains _herdr_labels_bash_precmd "${PROMPT_COMMAND[@]}" ||
    PROMPT_COMMAND+=(_herdr_labels_bash_precmd)
else
  case ";${PROMPT_COMMAND:-};" in
    *';_herdr_labels_bash_precmd;'*) ;;
    *) PROMPT_COMMAND="${PROMPT_COMMAND:+$PROMPT_COMMAND;}_herdr_labels_bash_precmd" ;;
  esac
fi

# Install DEBUG last because setup commands themselves would otherwise trigger
# it. The IN_DEBUG guard prevents commands inside the trap from recursing.
_HERDR_LABELS_BASH_HOOK_INSTALLED=1
if [[ ${_HERDR_LABELS_BASH_INSTALL_DEBUG:-} == 1 ]]; then
  unset _HERDR_LABELS_BASH_INSTALL_DEBUG
  _HERDR_LABELS_BASH_READY=0
  trap 'if [[ ${_HERDR_LABELS_BASH_IN_DEBUG:-0} == 0 ]]; then _HERDR_LABELS_BASH_IN_DEBUG=1; _herdr_labels_bash_debug_trap "$BASH_COMMAND"; _HERDR_LABELS_BASH_IN_DEBUG=0; fi' DEBUG
fi
