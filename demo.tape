Output demo.gif
Set FontSize 14
Set Width 1200
Set Height 800
Set Theme "Dracula"

# ── Build + isolate ────────────────────────────────────────────────
# The build happens off-screen so the demo stays tight.  We install
# to the real CARGO_HOME (which is on PATH), then switch HOME so the
# hs database lives entirely inside /tmp/hs-demo-vhs.
Hide
Type "cargo install --path ." Enter
Sleep 120s
Env HOME "/tmp/hs-demo-vhs"
Type "rm -rf /tmp/hs-demo-vhs && mkdir -p /tmp/hs-demo-vhs" Enter
Type "clear" Enter
Show

# ── 1. Install the hook ────────────────────────────────────────────
Type 'eval "$(hs init bash)"' Enter
Sleep 1s

# ── 2. Capture some commands ───────────────────────────────────────
Type "echo hs-demo-hello-world" Enter
Sleep 1s
Type "false" Enter
Sleep 1s
Type 'curl -H "Authorization: Bearer hs-demo-fake-secret" https://example.com/api' Enter
Sleep 2s

# ── 3. Prove health + redaction ────────────────────────────────────
Type "hs doctor" Enter
Sleep 3s

# ── 4. Print the ranked table ──────────────────────────────────────
Type "hs --print echo" Enter
Sleep 3s

# ── 5. Open the TUI, select, and re-run ────────────────────────────
Type 'hs "echo hs-demo-hello"' Enter
Sleep 1s
Down
Sleep 1s
Enter
Sleep 3s
