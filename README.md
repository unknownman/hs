# hs — Local context-aware command memory for developers and DevOps

Standard shell history can only answer *"What did I type?"*. **hs** answers *"What worked here before?"*.

hs captures every command you run, then ranks it with context: which project it ran in, whether it succeeded, how often it succeeds, and how recently. Instead of a flat list of raw lines, you get a ranked, filterable memory of the commands that actually work — tailored to the directory you're standing in.

| | |
|---|---|
| **100% Local** | Everything lives in a SQLite database on your machine. No cloud, no telemetry, no account. |
| **SQLite + WAL** | Write-ahead logging for fast, crash-safe reads and writes. |
| **Zero latency** | Capture runs in a backgrounded subprocess from a `preexec`/`precmd` hook — the prompt is never blocked. |
| **Secret-redacted by default** | AWS keys, GitHub tokens, JWTs, Bearer tokens, and `KEY=value` secrets are masked before they ever touch disk. |
| **Shells** | v1.0 ships engineered hooks for **bash** and **zsh**. |

---

## Installation

```sh
cargo install --path .
```

Requires Rust 2024 edition (rustc 1.85+). The resulting binary is self-contained; at runtime it needs no network access.

### Enable the shell hook

**zsh** (`~/.zshrc`):

```sh
eval "$(hs init zsh)"
```

**bash** (`~/.bashrc`):

```sh
eval "$(hs init bash)"
```

The hook runs in the background for every interactive command you type, capturing the command text, the current directory, its exit code, and its duration in milliseconds. It never captures itself, and it silences all of its own output.

### Import your legacy history

If you already have years of shell history, bring it in:

```sh
hs import
```

hs looks for `~/.zsh_history` (zsh extended-format `: 1234567890;command` lines, including multi-line continuations) then `~/.bash_history`. Every line is:

1. **Sanitized** through the same secret-redaction engine used for live capture,
2. **Deduplicated** against commands already in the database, and
3. **Stored** with its original timestamp.

```sh
$ hs import
[hs] Imported 3 commands (0 duplicates skipped, 1 secrets redacted)
```

Rerunning `hs import` is safe — previously imported commands are skipped:

```sh
$ hs import
[hs] Imported 0 commands (3 duplicates skipped, 1 secrets redacted)
```

You can also point at a specific file: `hs import ~/.local/share/history`.

---

## Usage

### Interactive recall

Run `hs` — with **no arguments** for a full browse, or with a **query**
(e.g. `hs deploy`) to open the same browser already narrowed to matching
commands. It lists ranked results in two sections — **`Current project`**
and **`Global / other`** — with a preview panel showing the full command,
its run statistics, and a live risk classification badge.

```
┌ hs — What worked here before? ─────────────┐
│ ── Current project ─────────────────────── │
│ > migrate --database=prod   1.17 1  100%  │
│ ── Global / other ──────────────────────── │
│   make migrate              0.64 3   66%  │
├─────────────────────────────── Preview ──  ┤
│  migrate --database=prod                  │
│  [SAFE]  Runs: 1 · Success: 100%          │
│  Last run: 2 h ago                        │
└───────────────────────────────────────────┘
↑/↓ navigate   Enter run   Esc / Ctrl-C exit
```

- `↑` / `↓` — move through results
- `Enter` — run the selected command (through the [execution guard](#execution-guard))
- `Esc` / `Ctrl-C` — exit without running

### Targeted CLI queries

A query is a **pre-filter for the TUI**, not a read-only shortcut: `hs deploy`
opens the interactive browser already narrowed to deploy commands — hit
`Enter` to re-run one (safely), `Esc` to back out. The scenario filters
below still combine with a query to narrow the candidate set.

| Scenario | Command |
|---|---|
| **Dev** — working migrations for this repo | `hs --project --ok migrate` |
| **DevOps** — failed container runs, last 24 h | `hs --failed --last 1d docker` |
| **Scripting** — formatted table for pipes/logs | `hs --print build` |
| **Keepers** — pin a command you re-run constantly | `hs pin <ID>` |

When you specifically want the **read-only** ranked table — piping it to
`less`, saving to a file, or feeding a script — pass `--print`. It prints
the table and exits without opening the TUI.

### Pinned commands

Pinning keeps a critical command on a dedicated list, independent of ranking. Pinned commands also receive a **score boost** in search results, so they always surface to the top — even if their run history is sparse:

```sh
hs pin 42          # remember command #42
hs pins            # table of pinned commands (ID, Command, Project, Pinned At)
hs unpin 42        # forget it again
```

`hs pin` and `hs unpin` are idempotent (re-running them is always safe),
pinning a command that doesn't exist fails cleanly, and deleting a command
also deletes its pin. An empty pin list prints `No pinned commands.`

`--print` (or any non-TTY stdout, e.g. `hs foo | less`) prints a table and exits:

```
╭────┬─────────────────────────────┬───────┬──────┬─────────┬──────────────────╮
│ #  ┆ Command                     ┆ Score ┆ Runs ┆ Success ┆ Last Run         │
╞════╪═════════════════════════════╪═══════╪══════╪═════════╪══════════════════╡
│ →1 ┆ echo hello-from-hs-project  ┆  1.17 ┆    1 ┆    100% ┆ 2026-09-09 15:10 │
├╌╌╌╌┼╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┼╌╌╌╌╌╌╌┼╌╌╌╌╌╌┼╌╌╌╌╌╌╌╌╌┼╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┤
│  2 ┆ echo global-universal-hello ┆  0.64 ┆    1 ┆    100% ┆ 2026-09-09 15:10 │
╰────┴─────────────────────────────┴───────┴──────┴─────────┴──────────────────╯
→ commands from the current project
```

### Filters

| Flag | Meaning |
|---|---|
| `--project` | Restrict to the current git project (nearest `.git` ancestor). Mutually exclusive with `--global`. |
| `--global` | Search every project. Mutual with `--project`. |
| `--ok` | Only successful commands (exit code 0). Mutual with `--failed`. |
| `--failed` | Only failed commands (exit code > 0). Use for debugging "why did this break". |
| `--last <WINDOW>` | Time window: `30m`, `1h`, `2d`, `1w`. |
| `--print` | Print the read-only ranked table and exit instead of opening the TUI (for piping/scripting). |

### When does hs run a command?

Never automatically. A query (`hs deploy`) merely *pre-filters* the
interactive TUI; the only way a recorded command actually runs is an
explicit `Enter` in that TUI, routed through the
[execution guard](#execution-guard). A search can therefore end in a
re-run — but only because *you* pressed Enter and confirmed it.

When you want read-only output — piping into `less`, saving to a file, or
feeding a script — use `--print` (piping stdout to a non-TTY is
equivalent). `hs --print deploy` prints a ranked table and exits without
opening the TUI.

### Exit codes

`hs` exits `0` on a successful search/browse (or `--print`), propagates the exit code of any command it runs from the TUI, returns `130` when a guarded execution is cancelled by the user, and `1` on any other error. Dismissing the TUI with `Esc`/`Ctrl-C` exits `0` (nothing was run, which is not an error).

### Full CLI reference

```sh
Usage: hs [OPTIONS] [QUERY]... [COMMAND]

Commands:
  pin     Pin a command by its numeric ID
  unpin   Remove a pin from a previously pinned command
  pins    List all currently pinned commands
  import  Import standard shell history into the hs database
  init    Generate shell hook installation scripts
  doctor  Check database health and configuration
  help    Print this message or the help of the given subcommand(s)

Options:
      --project   Restrict search to the current git project
      --global    Search all commands across every project
      --ok        Only show commands that succeeded
      --failed    Only show commands that failed
      --print     Print ranked results as a table and exit
      --last <WINDOW>   e.g. 30m, 1h, 2d, 1w
  -h, --help      Print help
  -V, --version   Print version
```

---

## Safety & privacy

### Secret redaction

Every command is passed through a conservative redaction engine **before** it enters the database. What it masks:

| Family | Shape |
|---|---|
| AWS access key IDs | `AKIA` + 16 uppercase alphanumerics |
| Stripe-style keys | `sk_` / `pk_` + `live`/`test` + 16+ chars |
| GitHub personal access tokens | `ghp_` + 30+ alphanumerics |
| JWTs | `eyJ...`.`eu...`.`...` (three base64url segments) |
| Bearer credentials | `Bearer <token>` → `Bearer [REDACTED]` |
| Inline secret assignments | `password=` `secret:` `token=` `api_key=` `private_key=` `access_key=` (+ quoted values) |

```sh
# what you type
curl -H "Authorization: Bearer super-secret-api-token-12345" https://api.example.com

# what is stored
curl -H "Authorization: Bearer [REDACTED]" https://api.example.com
```

Matching is deliberately conservative: a short, word-bounded fragment that
only *looks like* a key prefix (not a real 24+ character token) is **not**
redacted, so commit messages and prose survive intact. The same engine
sanitizes everything `hs import` ingests.

### Execution guard

Before hs ever runs a stored command, it classifies it:

- **SAFE** — executed directly.
- **HIGH RISK** — `rm -rf`, `git push --force`, `dd`, and other destructive patterns are intercepted with an explicit confirmation prompt that defaults to **no**:

```
[!] CAUTION: This command is flagged as High Risk
    rm -rf /tmp/hs-demo
? Are you sure you want to execute this? (y/n) › n
[hs] Aborted — nothing was executed.
```

If the prompt cannot be shown (no TTY), the command is declined. Guarding fails closed by design — it is astonishingly rare to re-run a destructive command from memory, and safe to demand a yes.

---

## Diagnostics

`hs doctor` inspects the database and environment and reports green or red per check:

```
$ hs doctor

[✓] Database exists at ~/Library/Application Support/hs/hs.db
[✓] Parent directory writable
[✓] Journal mode: wal
[✓] Integrity check: ok
[✓] 5 unique commands · 5 executions · 56 KB on disk
[✗] no hs init hook found in ~/.bashrc (soft — skipped)
[✗] no hs init hook found in ~/.zshrc  (soft — skipped)

hs doctor: everything looks good.
```

Database health and volume checks are hard failures (`hs doctor` exits `1` if the database is missing, unwritable, not in WAL mode, or fails an integrity check). Hook-presence checks are **soft** — they inform, they don't fail the run.

The database lives at `~/Library/Application Support/hs/hs.db` on macOS (`$XDG_DATA_HOME/hs/hs.db` on Linux), in WAL mode.

---

## Honest limitations

- **Shells.** v1.0 officially supports **bash** and **zsh**. Fish, Nushell, and PowerShell are not supported yet (no hook implementations ship for them).
- **Subshell execution.** Re-running a command spawns `$SHELL -c <command>` (falling back to `/bin/sh` if `$SHELL` is not set). Commands that mutate your interactive shell—`cd`, `export`, virtualenv activation—do **not** affect the shell you ran `hs` from.
- **Single machine.** This is a purely local SQLite database. There is no multi-machine or cloud sync.
- **Single mode of working.** The execution guard is the only approval gate; there is no interactive "confirm every replay" mode beyond it.
- **FTS token boundaries.** Search treats punctuation as separators. A query that is pure punctuation (e.g. `***`) is intentionally ignored by the FTS engine.

---

## Development

```sh
cargo build            # dev build
cargo test             # 124 tests
cargo clippy --all-targets -- -D warnings
cargo fmt
```

Repository layout:

- `src/capture.rs` — live capture event handling (exit code, duration, dedup)
- `src/redaction.rs` — the secret engine (unit-tested for every family above)
- `src/ranking.rs` — the context-aware ranking model
- `src/context.rs` — git-project detection + the risk classifier
- `src/ui/tui.rs` — the ratatui browser
- `src/import.rs` — legacy shell-history ingestion
- `src/doctor.rs` — the diagnostic report
- `hooks/` — the bash and zsh hook scripts `hs init` emits

---

**hs** is deliberately small: a local, fast, honest memory of what worked. Ship it, run it, let it learn.