# ── hs — capture, rank, and re-run what worked here before ──────────
Output demo.gif
Set Width 1200
Set Height 800
Set FontSize 15
Set Theme "Dracula"
Set WindowBar Colorful
Set Shell "bash"
Set PlaybackSpeed 1.5

# Isolate the session: hs stores its SQLite database (and the seeded
# legacy ~/.zsh_history) entirely under this throwaway HOME.
Env HOME "/tmp/hs-demo-vhs"

# Everything below runs OFF-CAMERA: build hs (if needed), isolate a fresh
# HOME, create a git project, and seed a legacy ~/.zsh_history.
# "hs" must simply be on $PATH when the recording starts.
Hide
Type "[[ -x target/debug/hs ]] || cargo build --quiet" Enter
Sleep 60s
Type `export PATH="$(pwd)/target/debug:$PATH"` Enter
Type "rm -rf /tmp/hs-demo-vhs && mkdir -p /tmp/hs-demo-vhs/app" Enter
Type `export PS1="hs-demo app$ "` Enter
Type "cd /tmp/hs-demo-vhs/app" Enter
Type "touch README.md" Enter
Type "git init -q . && git config user.email demo@example.com && git config user.name demo" Enter
# Legacy history (zsh extended format) for the `hs import` segment.
Type `printf '%s\n' ': 1710000000:5;git commit -am "chore: bump changelog"' > "$HOME/.zsh_history"` Enter
Type `printf '%s\n' ': 1710000100:1;curl -H "Authorization: Bearer hs-imported-secret-token" https://api.legacy.app' >> "$HOME/.zsh_history"` Enter
Type `printf '%s\n' ': 1710000200:2;make test' >> "$HOME/.zsh_history"` Enter
Type "clear" Enter
Sleep 1s
Show

# ── 1. Watch mode — one line, and hs remembers every command ─────────
Type `eval "$(hs init bash)"` Enter
Sleep 1s

# ── 2. Some real work, silently recorded ─────────────────────────────
Ctrl+L
Type `echo "deploy api-gateway v1.2.3"` Enter
Sleep 900ms
Type "git status --short" Enter
Sleep 900ms
Type "ls missing-deployment-target" Enter
Sleep 900ms
Type `echo "Authorization: Bearer hs-demo-secret-9f8e7d0a2c"` Enter
Sleep 900ms
Type "rm -rf /tmp/hs-demo-vhs/trash" Enter
Sleep 900ms
Type "glt status" Enter
Sleep 1s

# ── 3. Ranked recall straight from the CLI ───────────────────────────
Ctrl+L
Type "hs --print deploy" Enter
Sleep 2s
Type "hs --print status" Enter
Sleep 2s
Type "hs --failed --last 1d --print" Enter
Sleep 2s

# ── 4. Legacy history import + project scoping ───────────────────────
Ctrl+L
Type "hs import" Enter
Sleep 2s
Type "hs --project --ok --print" Enter
Sleep 2s

# ── 5. Health check ──────────────────────────────────────────────────
Ctrl+L
Type "hs doctor" Enter
Sleep 2.5s

# ── 6. Pins: keep the command you re-run forever ─────────────────────
Ctrl+L
Type "hs pin 1" Enter
Sleep 1s
Type "hs pins" Enter
Sleep 1.5s
Type "hs --print deploy" Enter
Sleep 1.5s
Type "hs unpin 1" Enter
Sleep 1s

# ── 7. Browse everything, then re-run a stored command ───────────────
Ctrl+L
Type "hs" Enter
Sleep 2.5s
Down
Sleep 500ms
Down
Sleep 500ms
Ctrl+C
Sleep 1s
Ctrl+L
Type "hs deploy" Enter
Sleep 2.5s
Enter
Sleep 2s

# ── 8. The execution guard stops a destructive replay ────────────────
Type `hs "rm -rf"` Enter
Sleep 2.5s
Enter
Sleep 1.5s
Enter
Sleep 1.5s

# ── 9. Privacy: erase a typo forever ─────────────────────────────────
Ctrl+L
Type "hs delete 6" Enter
Sleep 1s
Type "hs --print glt" Enter
Sleep 1.5s