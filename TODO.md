
**TODOs**
* display intermediate actions whilte thinking
* connect providers directly, like Anthropic, OpenAI, etc..

* prompt caching
* what else could be added to verbose mode?
* Live raw request/response screen?
* Skills? implement Agent Skill Standard: agentskills.io

NEXT:
* $ for running terminal commands
* a steered message is lost if the turn it joined is cancelled. `absorb` runs
  only on the arm where a turn completes (conversation.rs), so cancelling
  discards everything the task accumulated — which is intended, except that a
  steered message is the one *user* message in that set. The turn's opening
  message is pushed onto the session before the task spawns and survives; a
  steered one lives only in the task's copy and does not. On screen you see
  both, in the database only the first, and a resume shows the difference.
  Low severity (explicit cancel only, and the turn was discarded anyway) but
  asymmetric. The obvious fix — have the worker push onto the session when it
  accepts the message — collides with `absorb`, which reconciles by skipping
  the messages the session already has and taking the rest from the task's
  copy; pushing in both places double-counts. Needs thought, not a patch.
* think about being able to access the same session from 2 different terminals/processes
* picker refresh decrypts every session title and every session's last message
  every 2s, regardless of how many rows are on screen. Fine now — it scales with
  how many sessions you've accumulated, not with the screen — but that's the
  thing that would eventually want attention. Options if it does: only decrypt
  rows that are visible, or cache by (session id, updated_at) so an unchanged
  session isn't decrypted again.
* the picker renders every row into a fixed area with no scrolling, so once the
  list is taller than the terminal the extra sessions are simply not drawn —
  and there's no indication they exist. The two blank lines between sections
  cost two more rows. Wants a scroll offset that follows the selection, and
  probably some hint that the list continues past the edge.
* the transcript's reply avatar `●` (U+25CF) is East Asian Width Ambiguous,
  so some terminals draw it two cells wide and the whole reply gutter shifts
  against the wrapped continuation lines under it. Same bug the picker badges
  had. `—` for notices and `·` in the settings bar are Ambiguous too. The fix
  is a Neutral-width character: ✦ ⏺ ◉ ✻ ❖ ⟡ ✧ ❉ ✱ ⌾ ⬤ are all safe, as is
  anything in the braille block.
* the client's timeouts are hardcoded in client.rs and can't be configured:
  CONNECT_TIMEOUT 20s, REQUEST_TIMEOUT 300s, STREAM_IDLE_TIMEOUT 90s, plus
  tools.rs's 30s default for a terminal command when the model doesn't give one.
  The 90s stream idle one has stalled real turns twice. Would follow the same
  shape as sandbox/verbose: seeded config fields, `comms <name> <value>`.
* project-scoped sessions via a .comms/ folder, like .git: walk up from cwd to
  find it, sessions live there. Bigger than storing working_dir (which is done):
  it changes WHERE state lives. Costs to weigh first — storage splits from one
  global chats.db into many plus a global fallback for sessions started outside
  any project, so you carry both mechanisms; existing sessions need migrating;
  `comms sessions` from outside a project can no longer list everything, which
  matters when you resume by id; conversation history moves inside repos, so it
  gets committed by anyone who doesn't gitignore it (encrypted, but present and
  shareable); and auto-creating .comms/ wherever you happen to run litters
  directories, while requiring `comms init` adds a new concept. The stored
  working_dir is the data you'd migrate from.
* session claimed flag on session table in case 2 comms processes open the same session. What would that do? is that ok?
* should messages table be expanded to include tool calls, errors, etc.. OR errors logged somewhere
* need a model browser/search/picker
* [agent/ask] [verbose]
 <current os user>: 
* [model] [effort]
  AI:
* confirmation modal for deleting session where you type in name of session

FUTURE:
* android/ios app?
