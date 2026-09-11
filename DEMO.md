# hs — 60-second demo walkthrough

A fully reproducible script that showcases every feature of **hs v1.0** in a
clean terminal. Paste each block into **zsh** (swap `zsh` for `bash` and
`~/.zshrc` → `~/.bashrc` if you prefer). Every command below produces
deterministic, "yes, exactly that" output.

```sh
# 0. Build from source ONCE
cargo install --path .
```

## 1. Install the live-capture hook

```sh
eval "$(hs init zsh)"
```

The hook stays active for the rest of this shell session. For it to survive
reboots, add `eval "$(hs init zsh)"` to `~/.zshrc` (see the README).

## 2. Run some commands — one success, one failure, one fake secret

```sh
echo hs-demo-hello-world
# EXPECT: hs-demo-hello-world

npm run missing-script
# EXPECT: npm error — a failed exit code, captured as failure

curl -H "Authorization: Bearer hs-demo-fake-secret" https://example.com/api
# EXPECT: curl output. The Bearer token is real-looking enough to matter,
# but will be [REDACTED] before it ever reaches disk.

DEMO_TOKEN=hs-demo-fake-secret node -e 'console.log("ran")'
# EXPECT: ran
```

Every one of these was captured in the background — the prompt never lagged.

## 3. Prove database health and zero leaks

```sh
hs doctor
# EXPECT: every [✓] green; volume shows "4 unique commands · 4 executions".
# The two fake secrets must NOT appear anywhere.

hs --print DEMO_TOKEN
# EXPECT: a row whose command shows DEMO_TOKEN=[REDACTED]
```

`hs doctor` also runs a `PRAGMA integrity_check` on the SQLite file, so "zero
leaks" is verified both by the redaction engine and by a live search.

## 4. Pin a command and filter by time

```sh
hs --print echo
# EXPECT: a numbered table of echo commands. Note the ID of the first row.

hs pin 1
# EXPECT: [hs] Pinned command #1

hs pins
# EXPECT: a table showing the pinned command with its ID, command string,
# project path, and pin timestamp.
```

Pinned commands receive a ranking boost so they always surface to the top of
search results. You can also filter by a precise time window:

```sh
hs --last 30m echo
# EXPECT: only echo commands run within the last 30 minutes.
# The sub-day precision means a command run 45 minutes ago is excluded.
```

Supported window specifiers: `30m`, `1h`, `2d`, `1w`.

## 5. Interactive recall — a query opens the TUI, pre-filtered

A query is a pre-filled filter for the interactive TUI, not a read-only
table shortcut. Re-running the command you captured in step 2 is now a
single command:

```sh
hs "echo hs-demo-hello-world"
# EXPECT: the TUI opens directly on echo hs-demo-hello-world — pre-filtered,
# with a 100% success badge — ready to be executed. The live preview panel
# shows the command and its risk classification (SAFE).
# Press Enter to run it through the execution guard:
#   [hs] Running: echo hs-demo-hello-world
#   hs-demo-hello-world
# The TUI exits and execution stats are updated. Or press Esc/Ctrl-C to
# return to the prompt without running anything.
```

When you want the **read-only** ranked table instead — to pipe it into a
file or a script — that is exactly what `--print` is for:

```sh
hs --print "echo hs-demo-hello-world"
# EXPECT: a ranked table of the echo commands. Read-only: nothing is
# executed, no TUI is opened.
```

The scenario filters still work as TUI pre-filters too:

```sh
hs --project --ok echo
# EXPECT: the TUI opens showing only successful echo commands from the
# current project.
```

## 6. The Execution Guard

```sh
mkdir -p /tmp/hs-demo-cleanup
# Make the guard target exist, so removing it is visibly harmless.

rm -rf /tmp/hs-demo-cleanup
# Capture a genuinely destructive command (empty temp dir — harmless).

hs
# The TUI lists "rm -rf /tmp/hs-demo-cleanup". Select it and press Enter.
# EXPECT: the guard, NOT the command:
#   [!] CAUTION: This command is flagged as High Risk
#       rm -rf /tmp/hs-demo-cleanup
#   ? Are you sure you want to execute this? › n
# Type n and press Enter:
#   [hs] Aborted — nothing was executed.
#   echo $? → 130
```

> Shell nuance: zsh records a compound line (`mkdir && rm`) as a single entry;
> bash's `DEBUG` trap records each command in the list separately. The guard
> step uses a single command so it reproduces identically in both shells.

The guard fails closed (declines if there's no TTY), defaults to **no**, and
is the only gate between "that worked" and "that ran again".

---

**Timing:** ~60 seconds, zero dependencies beyond the `hs` binary. Clean up
afterward with nothing more than `rm -rf /tmp/hs-demo-cleanup` if the last
step raced past the prompt.