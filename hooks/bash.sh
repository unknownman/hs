# hs — bash shell hooks
#
# Source this file from ~/.bashrc:
#     eval "$(hs init bash)"
#
# Captures every interactive command in the background. The prompt is
# never blocked: `hs capture` is launched detached and silenced.

# --- Internal state ---------------------------------------------------------
_hs_last_cmd=""
_hs_start_ms=""

# --- Millisecond clock ------------------------------------------------------
_hs_now_ms() {
    # EPOCHREALTIME is a bash 5.0+ builtin that avoids a fork.
    if [[ -n "${EPOCHREALTIME:-}" ]]; then
        local ns=${EPOCHREALTIME/./}
        printf '%s' "${ns:0:13}"
    else
        # GNU date prints e.g. 1710000000123.
        local ms
        ms="$(date +%s%3N 2>/dev/null)"
        if [[ "$ms" == *%3N* ]]; then
            # BSD/macOS: no %N support, seconds-precision ms.
            printf '%s000' "$(date +%s)"
        else
            printf '%s' "$ms"
        fi
    fi
}

# --- Pre-exec: remember what is about to run --------------------------------
_hs_preexec() {
    local begun_cmd="$BASH_COMMAND"
    # Never capture ourselves, our proxy list assignment, or other _hs helpers.
    [[ "$begun_cmd" == _hs_* || "$begun_cmd" == PROMPT_COMMAND=* ]] && return
    # Also skip direct hs invocations to prevent recursive capture.
    [[ "$begun_cmd" == "hs" || "$begun_cmd" == "hs "* ]] && return
    _hs_last_cmd="$begun_cmd"
    _hs_start_ms="$(_hs_now_ms)"
}
trap '_hs_preexec' DEBUG

# --- Pre-prompt: the previous command finished ------------------------------
_hs_precmd() {
    local exit_code=$?
    # Preserve the user's exit code even when there is nothing to capture
    # (e.g. Enter on an empty line): the PROMPT_COMMAND status becomes the
    # one the prompt displays.
    [[ -z "$_hs_last_cmd" ]] && return $exit_code
    local end_ms
    end_ms="$(_hs_now_ms)"
    local duration_ms=0
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
    # End with the user's exit code instead of the success of the
    # assignments above; otherwise $? (and prompt themes like Starship)
    # would always see 0.
    return $exit_code
}
PROMPT_COMMAND="_hs_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}"