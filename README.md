# cicerone

cicerone is a terminal tool for code review. It walks you through a change in stages, from the wide view to the details. An agent reviews the whole change first, then plans the tour from its review. A chat box keeps one Claude session for the whole review.

cicerone shows the code at five zoom levels:

| Level | What you see |
|---|---|
| Map | The changed files and symbols, and how they depend on each other |
| Vocabulary | The new and changed names |
| Signature | The type signatures, with the bodies folded |
| Skeleton | The control flow, with the mechanical lines folded |
| Full | All the code, with the removed lines in place |

The tour shows names before their uses. Mechanical changes (formatting, imports, moved code, generated files, and repeated substitutions) go into one stage that you can skim. Tests come late.

A hunk is moved code when each of its changed lines, up to whitespace, comes back on the other side of the diff: in another hunk, another file, or another place in the same hunk. If only some lines of a hunk come back, the agent gets a "partly moved" hint with the hunks that hold those lines.

Haskell and Elm get syntax highlighting, and their signature and skeleton levels come from the syntax tree. For other languages, the agent zooms a file with labeled holes: the skeleton level folds each hole, and `+` opens it.

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

1. cicerone shows the automatic tour at once, so you can start to read while the agent works. The rail on the left shows each phase of the agent.
2. **Review.** The agent reviews every hunk of the change. A syntax detector marks some hunks as "likely mechanical", but that is only a hint: the agent must check those hunks too.
3. **Classify.** The agent gives every hunk one class with a reason: core, supporting, wiring, docs, tests, or mechanical. It also lists every distinct change. cicerone checks that every hunk has a class, and asks again for the missing ones.
4. **Plan.** The agent plans its tour from its review and its classes. When the first stage is ready, the agent tour replaces the automatic tour. Press `A` to switch between them.
5. **Cover.** The agent places the hunks that its tour left out. Mechanical hunks go into a closing stage, grouped by the agent's reason.

Each finding shows in the first step that shows its lines, and in a closing Review stage.

If the diff is larger than 400 KB, cicerone splits it by file into parts. Up to four helper sessions review the parts at the same time. The main session then checks their findings, removes duplicates, adds the findings that cross parts, and goes on to classify and plan.

### Severities

| Severity | Meaning |
|---|---|
| `BUG` | The code does the wrong thing for an input or state that can happen. |
| `RISK` | The code can fail under conditions that are not certain: concurrency, error paths, security, data loss, scale. |
| `CHANGE` | An intended change to behavior or to a contract that callers, API users, or existing data must know about. |
| `QUESTION` | The code does not show if it is right. The author must answer. |
| `NIT` | Style, naming, or wording only, with no effect on behavior. |

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

The chat uses the same agent session that reviewed the change and planned the tour. If an answer shows that a finding is wrong, the agent changes it, withdraws it, or adds a new one. Each finding keeps a history of its earlier states, with the reason for each change.

If you tell the agent that a behavior change is intended, the finding becomes `CHANGE`, not `NIT`. The readers of the review still must know about the change.

### Report (`E`)

The agent writes a Markdown report of the review, for another agent to act on. The report names every distinct change, and cicerone adds the coverage note (see below). cicerone saves it as `report.md` in the cache. If a Claude Code session opened cicerone (see the skill below), cicerone sends that session a request to publish the report as a claude.ai Artifact page.

### Post to GitHub (`S`)

`S` needs `--pr`. cicerone writes a draft of the review and opens it in `$GIT_EDITOR`, `$VISUAL`, or `$EDITOR` (the first one that is set, else `vi`). The draft has these parts:

- `as: claude` or `as: me`. With `as: claude`, the review and each comment start with "🤖 **Claude:**" and a note that Claude wrote it. The review still posts from your `gh` account, because GitHub does not let a local tool post as the Claude app.
- `event: comment`, `approve`, or `request_changes`.
- A `=== body` section for the review text. It lists every distinct change and ends with a coverage note: how many hunks and files the review covered, the count per class, and the mechanical hunks with their reasons and files.
- One `=== comment path:a-b` section for each line comment. Each comment must stay inside one hunk.

Save and close the editor to post. To cancel, delete all the text. To test the flow without posting, set `CICERONE_POST_DRY_RUN=1`. cicerone then writes `github-payload.json` to the cache and does not post.

## The agent

The agent is `claude -p` in one long session. It has only the `Read`, `Grep`, and `Glob` tools, and it reads a snapshot of the changed files. It has no shell and no MCP servers, and project hooks do not run. The helper sessions for a large diff have the same limits.

cicerone records each tool call of the agent in `tools.log`, so you can see what the agent read.

## Cache

cicerone keeps each review in `~/.cache/cicerone/<repo>-<base>-<head>/` (or under `$XDG_CACHE_HOME`):

| File | Content |
|---|---|
| `tour.json` | The agent tour |
| `review.json` | The findings and their history |
| `classes.json` | The class of each hunk, and the list of changes |
| `tools.log` | Each tool call of the agent, one JSON line each |
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
