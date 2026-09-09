# hs — zsh shell hooks
#
# Source this file from ~/.zshrc:
#     eval "$(hs init zsh)"
#
# Uses zsh's preexec/precmd lifecycle for accurate timing. The capture
# is backgrounded and silenced so the prompt is never blocked.

autoload -Uz add-zsh-hook

# zsh 5.4+ ships the datetime module providing $EPOCHREALTIME.
if zmodload zsh/datetime 2>/dev/null; then
    _hs_now_ms() {
        # EPOCHREALTIME is like "1710000000.123456"; keep ms precision.
        local tms=${EPOCHREALTIME//./}
        print -r -- "${tms:0:13}"
    }
else
    _hs_now_ms() {
        local ms
        ms="$(date +%s%3N)"
        if [[ "$ms" == *%3N* ]]; then
            # BSD/macOS fallback.
            print -r -- "$(date +%s)000"
        else
            print -r -- "$ms"
        fi
    }
fi

_hs_last_cmd=""
_hs_start_ms=""

# --- Pre-exec: called right before each command runs ------------------------
_hs_preexec() {
    local begun_cmd="$1"
    # Never capture ourselves or other _hs helpers.
    [[ "$begun_cmd" == _hs_* ]] && return
    _hs_last_cmd="$begun_cmd"
    _hs_start_ms="$(_hs_now_ms)"
}

# --- Pre-prompt: the previous command finished ------------------------------
_hs_precmd() {
    local exit_code=$?
    [[ -z "$_hs_last_cmd" ]] && return
    local end_ms duration_ms=0
    end_ms="$(_hs_now_ms)"
    if [[ -n "$_hs_start_ms" && -n "$end_ms" ]]; then
        duration_ms=$(( end_ms - _hs_start_ms ))
        (( duration_ms < 0 )) && duration_ms=0
    fi

    (
        hs capture \
            --cmd "$_hs_last_cmd" \
            --cwd "$PWD" \
            --exit "$exit_code" \
            --duration-ms "$duration_ms" \
            >/dev/null 2>&1 &
    )

    _hs_last_cmd=""
    _hs_start_ms=""
}

add-zsh-hook preexec _hs_preexec
add-zsh-hook precmd _hs_precmd