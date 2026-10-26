# Plan: the agent reviews first

## Problem

A reviewer of PR 9387 (kontrakcja) found three gaps in a cicerone review:

1. A finding about a change to public API behavior went out as NIT. It started as RISK. The chat changed it to NIT because the change was intended.
2. The review did not clearly check the new documentation page.
3. The posted review did not say what the agent reviewed, so the reader could not trust that it covered everything.

The causes are in the design:

- The mechanical detector runs first and removes hunks from what the agent sees. Nothing reviews those hunks.
- Hunks past the size limit become a one-line note.
- The review prompt does not define the severities and does not ask about docs, API contracts, or versions.
- The tour must place every hunk, but the review does not have to look at every hunk.
- The posted summary is the tour intent. It names only the main change.
- The agent log records only stderr, so nobody can see what the agent read.

## Decision

The agent reviews the full change first. Then it classifies every hunk, and then it plans the tour from the review and the classes. The tour starts only when the agent is done. The automatic tour shows while the agent works.

The language-specific code stays, but it no longer decides what the agent sees:

- The mechanical detector becomes a hint. The agent confirms or rejects each hint.
- Tree-sitter keeps the exact zoom levels and highlighting for Haskell and Elm.
- The agent can fold any file with labeled holes, so zoom works for every language.

## Tasks

1. **Severities.** Define each severity in the prompt. Add `change` for intended changes to behavior or contracts that users or callers must know about. The chat rules say that "intended" turns a bug or risk into `change`, never into `nit`.
2. **Review checklist.** Add public API behavior and contracts, docs and changelogs that do not match the code, version bumps, and compatibility with existing data and instances.
3. **Full material.** The agent gets every hunk. A hunk that the detector marks mechanical carries the hint and its reason.
4. **New order.** Review, then classify, then plan, then cover. Classes: `core`, `supporting`, `wiring`, `docs`, `tests`, `mechanical`, each with a reason. The classify turn also lists every distinct change in the PR. cicerone checks that every hunk has exactly one class, and asks again for the missing ones. The mechanical stage comes from the agent's classes.
5. **Coverage note.** The GitHub review body and the report state what the agent covered: the files and hunks, the count per class, and the mechanical hunks with their reasons. The summary lists every distinct change.
6. **Docs as documents.** The tour shows a docs hunk as a full lens in its own step, not in a group.
7. **Zoom for every language.** A file or hunk lens with holes gets a skeleton level that folds the holes. cicerone keeps the holes inside the lens.
8. **Tool log.** cicerone records each tool call of the agent in `tools.log` in the cache.
9. **Large diffs.** When the diff is too large for one session, helper sessions review parts of it in parallel. The main session then checks their findings, removes duplicates, adds findings that cross the parts, and classifies all hunks.

## Cache

`classes.json` holds the classes and the list of changes. A tour from an older cache, without `classes.json`, uses the detector as before.
