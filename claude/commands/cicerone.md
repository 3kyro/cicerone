Open cicerone — the guided diff-review tool — in a new kitty tab wired to this Claude Code session.

The tab runs `cicerone review <range>`: a staged tour of the change (map, vocabulary, shapes, skeletons, full code, mechanical patterns). On first open, an agent reviews every hunk, classifies each one, and then plans the tour from its review (cached per range; the automatic tour shows meanwhile). Findings show inline in the steps that show their lines (`]`/`[` jump between them) and in a closing Review stage. `c`/`a` open a chat with that same agent session. The agent only has Read/Grep/Glob — no shell, no MCP servers.

## Arguments

- `pr`, `pr <number|url>`, or a bare PR number/URL: review that PR. With no number given, resolve the current branch's PR with `gh pr view --json number -q .number`; if that finds none, ask which PR instead of falling back to the working tree. cicerone takes no PR argument itself, so turn the PR into a range:
  ```sh
  read base head < <(gh pr view <n|url> --json baseRefName,headRefOid -q '"\(.baseRefName) \(.headRefOid)"')
  git fetch -q origin "$base" "pull/<n>/head"
  ```
  and use the range `origin/$base...$head` (three dots: the PR's changes since its merge base).
- A git range or revision (`A..B`, `A...B`, a sha, a branch): pass it through as the range.
- No arguments: the working tree against HEAD, plain `cicerone review`.
- `--tour <file>`, `--no-agent`, `--replan`, `--model <m>`: pass through.

## Steps

1. If `cicerone` is not on PATH, install it with `cargo install -q --git https://github.com/3kyro/cicerone`.
2. If `$CLAUDE_CODE_MESSAGING_SOCKET` is empty, this session cannot receive messages. Say so in one line and continue. Only `E` (report) needs it: without it, cicerone saves the report but cannot ask this session to publish it.
3. Open the tab as a plain shell tab and type the command into it, so the shell loads the usual rc files and survives cicerone exiting:
   ```sh
   root=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
   win=$(kitty @ launch --type=tab --cwd "$root" --tab-title "cicerone: $(basename "$root")" \
     --env CLAUDE_CODE_MESSAGING_SOCKET="$CLAUDE_CODE_MESSAGING_SOCKET" \
     --env CLAUDE_CODE_MESSAGING_TOKEN="$CLAUDE_CODE_MESSAGING_TOKEN")
   kitty @ send-text --match "id:$win" -- $'cicerone review <range>\n'
   ```
   For a PR, add `--pr <number>` (so `S` in cicerone can post the review to it): `cicerone review --pr <number> <range>`, and title the tab `cicerone: PR #<number>`. Keep `--cwd "$root"`: the range only resolves inside the PR's own clone.
   If kitty remote control is unavailable, report the exact error and give the command so the user can run it in any terminal inside the repo.
4. Confirm in one line that cicerone is open, then continue with whatever you were doing.
