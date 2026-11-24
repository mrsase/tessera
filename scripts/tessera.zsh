# Tessera shell integration for zsh.
#
# Source this from ~/.zshrc:
#   [ -f /path/to/tessera/scripts/tessera.zsh ] && source /path/to/tessera/scripts/tessera.zsh
#
# Emits OSC 133 prompt/command markers and OSC 7 working-directory reports, and
# records every executed command with its cwd into Tessera's JSONL history.

[[ -o interactive ]] || return 0

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
    print -rn -- "$s"
}

__tessera_precmd() {
    local status=$?
    print -n '\e]133;A\e\\'
    print -n "\e]7;file://${HOST:-localhost}${PWD// /%20}\e\\"
    return $status
}

__tessera_preexec() {
    print -n '\e]133;C\e\\'
    local cmd=$1
    print -r -- "{\"cmd\":\"$(__tessera_escape "$cmd")\",\"cwd\":\"$(__tessera_escape "$PWD")\",\"ts\":$(date +%s)}" >> "$TESSERA_HISTORY"
}

autoload -Uz add-zsh-hook
add-zsh-hook precmd __tessera_precmd
add-zsh-hook preexec __tessera_preexec
