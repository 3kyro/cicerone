# cicerone

cicerone is a terminal tool for code review. It walks you through a change in stages, from the wide view to the details. An agent plans the tour and reviews the change. A chat box keeps one Claude session for the whole review.

cicerone shows the code at five zoom levels:

| Level | What you see |
|---|---|
| Map | The changed files and symbols, and how they depend on each other |
| Vocabulary | The new and changed names |
| Signature | The type signatures, with the bodies folded |
| Skeleton | The control flow, with the mechanical lines folded |
| Full | All the code, with the removed lines in place |

The tour shows names before their uses. Mechanical changes (formatting, imports, generated files, and repeated substitutions) go into one stage that you can skim. Tests come late.

Haskell and Elm get syntax highlighting and structure. Other languages show as plain diffs.

## Install

You need Rust (edition 2024), `git`, and the `claude` CLI on `PATH`. For `S` (post to GitHub), you also need `gh`.

```sh
cargo install --git https://github.com/3kyro/cicerone
```

From a clone, run `cargo install --path .` instead.

## Use

```sh
cicerone review                  # the working tree against HEAD
cicerone review main..feature    # changes from main to feature
cicerone review main...feature   # changes on feature since its merge base with main
cicerone review <sha>            # one commit
cicerone review --pr 123 origin/master...<head-sha>   # a PR; S can post to it
```

`review` is the default command, so `cicerone main...feature` also works.

Flags for `review`:

| Flag | Effect |
|---|---|
| `--no-agent` | Use the automatic tour. Do not start an agent. |
| `--replan` | Ignore the cached tour and review, and plan again. |
| `--tour <file>` | Use a tour from a JSON file. |
| `--model <m>` | Use this model for the agent. |
| `--pr <n\|url>` | The PR for `S`. cicerone remembers it for the range. |
| `-C <dir>` | Run in this repository. |

Other commands:

| Command | Output |
|---|---|
| `cicerone tour [<range>]` | The automatic tour as JSON. Use it as a template for `--tour`. |
| `cicerone plan [<range>]` | Let the agent plan the tour, cache it, and print it as JSON. |
| `cicerone manifest [--json] [<range>]` | The changed symbols, hunks, and mechanical patterns. |

### What happens when you open a review

1. cicerone shows the automatic tour at once.
2. The agent plans its own tour. When the plan is ready, the agent tour replaces the automatic tour. Press `A` to switch between them.
3. The agent adds the hunks that its tour left out.
4. The agent reviews the change. Each finding shows in the steps that show its lines, and in a closing Review stage.

### Keys

| Key | Action |
|---|---|
| `n` `space` / `p` | Next / previous step |
| `N` / `P` | Next / previous stage |
| `j` `k` `↓` `↑` | Scroll |
| `ctrl-d` / `ctrl-u` | Half a page |
| `g` / `G` | Top / bottom |
| `h` `l` `←` `→` | Scroll the code sideways |
| `H` `L` / `0` | Half a pane sideways / back to the start |
| `]` / `[` | Next / previous step with findings |
| `tab` / `shift-tab` | Focus the next / previous lens |
| `+` / `-` | Zoom the focused lens in / out |
| `d` | Show or hide the removed lines |
| `A` | Switch between the agent tour and the automatic tour |
| `c` | Open the chat |
| `a` | Ask about the focused lens |
| `E` | Write a review report (see below) |
| `S` | Post the review on the PR (see below) |
| `m` | Turn mouse capture off to select text, or on to scroll with the wheel |
| `?` | Help |
| `q` | Quit |

In the chat:

| Key | Action |
|---|---|
| `enter` | Send |
| `esc` | Close the chat |
| `↑` `↓` `pgup` `pgdn` `ctrl-b` `ctrl-f` | Scroll |
| `ctrl-y` | Copy the last answer to the clipboard (OSC 52) |
| `ctrl-u` / `ctrl-w` | Clear the input / delete a word |

### The chat and the findings

The chat uses the same agent session that planned and reviewed the change. If an answer shows that a finding is wrong, the agent changes it, withdraws it, or adds a new one. Each finding keeps a history of its earlier states, with the reason for each change.

### Report (`E`)

The agent writes a Markdown report of the review, for another agent to act on. cicerone saves it as `report.md` in the cache. If a Claude Code session opened cicerone (see the skill below), cicerone sends that session a request to publish the report as a claude.ai Artifact page.

### Post to GitHub (`S`)

`S` needs `--pr`. cicerone writes a draft of the review and opens it in `$GIT_EDITOR`, `$VISUAL`, or `$EDITOR` (the first one that is set, else `vi`). The draft has these parts:

- `as: claude` or `as: me`. With `as: claude`, the review and each comment start with "🤖 **Claude:**" and a note that Claude wrote it. The review still posts from your `gh` account, because GitHub does not let a local tool post as the Claude app.
- `event: comment`, `approve`, or `request_changes`.
- A `=== body` section for the review text.
- One `=== comment path:a-b` section for each line comment. Each comment must stay inside one hunk.

Save and close the editor to post. To cancel, delete all the text. To test the flow without posting, set `CICERONE_POST_DRY_RUN=1`. cicerone then writes `github-payload.json` to the cache and does not post.

## The agent

The agent is `claude -p` in one long session. It has only the `Read`, `Grep`, and `Glob` tools, and it reads a snapshot of the changed files. It has no shell and no MCP servers, and project hooks do not run.

## Cache

cicerone keeps each review in `~/.cache/cicerone/<repo>-<base>-<head>/` (or under `$XDG_CACHE_HOME`):

| File | Content |
|---|---|
| `tour.json` | The agent tour |
| `review.json` | The findings and their history |
| `chat.json` | The chat |
| `session` | The agent session id |
| `report.md` | The last report |
| `pr` | The PR number for `S` |

If you open a range that resolves to the same two commits again, cicerone loads the cached tour, findings, and chat, and resumes the session. A plain `cicerone review` (the working tree) has no cache. Use `--replan` to start again.

## Claude Code skill

The `/cicerone` command opens cicerone in a new [kitty](https://sw.kovidgoyal.net/kitty/) tab, connected to the Claude Code session that runs it. The command file is [`claude/commands/cicerone.md`](claude/commands/cicerone.md).

To install it, download it into your Claude Code commands:

```sh
mkdir -p ~/.claude/commands
curl -fsSL https://raw.githubusercontent.com/3kyro/cicerone/main/claude/commands/cicerone.md \
  -o ~/.claude/commands/cicerone.md
```

From a clone, you can link it instead, so that it updates with `git pull`:

```sh
ln -s "$PWD/claude/commands/cicerone.md" ~/.claude/commands/cicerone.md
```

If `cicerone` is not on `PATH`, the skill installs it from GitHub the first time that you run it.

Then, in Claude Code:

```
/cicerone                 # the working tree
/cicerone pr              # the PR of the current branch
/cicerone pr 123          # PR 123
/cicerone main...feature  # a range
```

The skill needs kitty with remote control on (`allow_remote_control yes`). For `E`, it also needs Claude Code cross-session messaging (`/status` shows a `Peer address` row). Without kitty, the skill gives you the command to run in another terminal.

## License

MIT. See [LICENSE](LICENSE).
