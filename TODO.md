
**TODOs**
* display intermediate actions whilte thinking
* connect providers directly, like Anthropic, OpenAI, etc..

* prompt caching
* what else could be added to verbose mode?
* Live raw request/response screen?
* Skills? implement Agent Skill Standard: agentskills.io

NEXT:
* need a --title arg for the comms session command to name the command on creation from the terminal and a /session title <new title> in-session command
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
* session claimed flag on session table in case 2 comms processes open the same session. What would that do? is that ok?
* should messages table be expanded to include tool calls, errors, etc.. OR errors logged somewhere
* workin dir needs to be svaed as a session setting. or does it matter?
* need a model browser/search/picker
* [agent/ask] [verbose]
 <current os user>: 
* [model] [effort]
  AI:
* confirmation modal for deleting session where you type in name of session
* $ for running terminal commands
