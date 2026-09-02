
**TODOs**
* display intermediate actions whilte thinking
* connect providers directly, like Anthropic, OpenAI, etc..

* prompt caching
* what else could be added to verbose mode?
* Live raw request/response screen?
* Skills? implement Agent Skill Standard: agentskills.io

NEXT:
* ready, thinking/working, approval pending, done check, tool call. do we need to poll the messages table?
* steering: send while a turn is running and have it join that turn, not the next
  one. Inject drained messages as user messages at the TOP of a run_agent_turn
  iteration only — never between an assistant's tool_calls and its tool results,
  which breaks tool_use/tool_result pairing. Shared handle like SessionGates.
  Falls back to queueing in ask mode (no loop to inject into), at the
  max_iterations boundary, and for anything left undrained when a turn ends —
  losing a typed message is the worst failure here. Takes effect at the next
  model call, not mid-request. TUI first: it already accepts input mid-turn.
  Show it: a growing list of pending messages above the message input, so what
  is waiting to join the turn is visible rather than implied by a count.
* Websearch
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
* $ for running terminal commands
