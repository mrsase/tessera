# Tessera shell integration for bash.
#
# Source this from ~/.bashrc:
#   [ -f /path/to/tessera/scripts/tessera.bash ] && source /path/to/tessera/scripts/tessera.bash
#
# It emits OSC 133 prompt/command markers and OSC 7 working-directory reports,
# and appends executed commands to Tessera's JSONL history so suggestions can be
# ranked per directory.

[[ $- == *i* ]] || return 0

__tessera_data_dir="${XDG_DATA_HOME:-$HOME/.local/share}/tessera"
mkdir -p "$__tessera_data_dir" 2>/dev/null
export TESSERA_HISTORY="$__tessera_data_dir/history.jsonl"

__tessera_escape() {
    local s=$1
    s=${s//\\/\\\\}
    s=${s//\"/\\\"}
    s=${s//$'\n'/\\n}
    s=${s//$'\r'/\\r}
    s=${s//$'\t'/\\t}
    printf '%s' "$s"
}

__tessera_record() {
    local line num cmd
    line=$(HISTTIMEFORMAT= builtin history 1 2>/dev/null) || return 0
    read -r num cmd <<< "$line"
    [[ $num =~ ^[0-9]+$ ]] || return 0
    [ "$num" = "${__TESSERA_LAST_HIST:-}" ] && return 0
    __TESSERA_LAST_HIST=$num
    cmd=${cmd#"${cmd%%[![:space:]]*}"}
    [ -n "$cmd" ] || return 0
    printf '{"cmd":"%s","cwd":"%s","ts":%s}\n' \
        "$(__tessera_escape "$cmd")" \
        "$(__tessera_escape "$PWD")" \
        "$(date +%s)" >> "$TESSERA_HISTORY"
}

__tessera_precmd() {
    local status=$?
    printf '\033]133;A\033\\'
    printf '\033]7;file://%s%s\033\\' "${HOSTNAME:-localhost}" "${PWD// /%20}"
    __tessera_record
    return $status
}

__tessera_preexec() {
    printf '\033]133;C\033\\'
}

if [[ $PS0 != *__tessera_preexec* ]]; then
    PS0='$(__tessera_preexec)'"$PS0"
fi

case ";${PROMPT_COMMAND:-};" in
    *";__tessera_precmd;"*) ;;
    *) PROMPT_COMMAND="__tessera_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}" ;;
esac
