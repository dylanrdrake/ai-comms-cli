**TODO** — one line each, in the order I'd tackle them.

Full reasoning for anything here, including sizings and the traps found while
looking into it, is in `git log -p TODO.md`. Compressed 2026-09-04.

NOW
* picker doesn't scroll — sessions past the terminal height simply aren't drawn and nothing says they exist; wants a scroll offset that follows the selection
* client timeouts hardcoded in client.rs (connect 20s, request 300s, stream-idle 90s; tools.rs 30s) — the 90s one has stalled real turns twice; make them config like sandbox/verbose
* nothing bounds what's sent to the model — every turn ships the whole history, so a long chat eventually overruns the context window; sliding window vs compaction, needs a token counter first
* terminal freezes on Windows, especially when logging and launching
* a steered message is lost if its turn is cancelled — `absorb` only runs on completion, and the steered one is the single *user* message in the discarded set; collides with absorb's reconciliation, so needs thought not a patch

NEXT
* read-only viewing of a session another process holds — `Chat` holds a non-optional `Conversation`, so ~150-250 lines across TUI/worker/picker, plus badging held rows in the picker
* answering an approval for a run you aren't attached to — the real fix for what `--headless` worked around; persist the ApprovalRequest, add a response column and a request id, make the CLI's stdin read selectable
* nothing clank prints is machine-readable — `agent` interleaves progress and reply on one stream and there's no `--json` anywhere; precondition for `--headless` returning
* token counter, in-session (part of verbose?) — also the prerequisite for bounding history above
* `clank ask` with no text should prompt for it and send what you type
* model browser/search/picker
* `$` output is captured, not streamed — the box sits empty then fills all at once; needs a different execution path and an event per chunk
* `$` is TUI-only — the CLI's blocking loop has no box to show output in, so it would have to mean run-and-print with no send step
* how to handle `$` commands that need an answer on stdin

LATER
* UNIQUE(session_id, seq) on messages — defence in depth behind the claim; needs a table rebuild and any existing duplicates resolved first
* remaining East Asian Ambiguous glyphs: `—` on notices and `·` in the settings bar (the reply avatar is done, both front ends draw braille now)
* picker decrypts every title and last message every 2s regardless of what's on screen — fine today, scales with sessions accumulated rather than with the screen
* editing messages in the transcript
* confirmation modal for deleting a session, where you type the name
* expand the messages table to hold tool calls and errors, or log errors somewhere
* display intermediate actions while thinking
* what else belongs in verbose mode?
* live raw request/response screen
* transcript header format: `[agent/ask] [verbose]` / `<user>:` then `[model] [effort]` / `AI:`
* keep a session's process alive after backing out of the terminal, so the agent carries on working
* `--resume` an 'elsewhere' session then ctrl-c — where does the terminal land?
* if `$` is ever reverted, keep the stdin fix in tools.rs — `.stdin(Stdio::null())` stops a child inheriting the TUI's raw-mode terminal, a pre-existing bug in the agent's own tool

BIGGER BETS
* `--headless`, removed in 51f4ef7 — worth returning for the up-front refusal and the nesting marker, but detect the TTY rather than declaring it, and add a per-run `--yes` instead of requiring `approval all off` globally
* `clank agent --resume`, removed in 771f556~1 — fix the note ordering, give it the interactive pick `clank session --resume` has, and decide about `set_agentic` firing before the run
* prompt caching — collides with any history trimming, since a cache hit needs a stable prefix; design the two against each other
* connect providers directly (Anthropic, OpenAI) rather than only OpenAI-compatible
* Skills — implement the Agent Skill Standard, agentskills.io
* project-scoped sessions in a `.clank/` folder — changes where state lives: splits storage, needs migration, and puts conversation history inside repos

SOMEDAY
* android/ios app
