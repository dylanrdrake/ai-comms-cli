can we connect to openrouter too or other "model aggregators"? — done (single active provider): `base_url` is configurable via `comms endpoint [url]`, effort-level serialization shape via `comms effort-style <flat|nested|none>` (flat for OrcaRouter, nested for OpenRouter), and extra per-request headers via `comms headers set/unset` (e.g. OpenRouter's `HTTP-Referer`/`X-Title`). Still one active API key/model/endpoint at a time — switching providers means re-running `comms login` and `comms endpoint`.

5. Provider profiles: named, saved provider configs (base URL + keyring API key + default model + effort style + headers) so you can register OrcaRouter and OpenRouter side by side and switch with one command instead of re-entering everything, e.g. `comms provider add openrouter --base-url https://openrouter.ai/api/v1 --effort-style nested`, `comms provider use openrouter`, `comms provider list`. Needs per-profile keyring entries (today there's a single `ai-comms-cli`/`api_key` entry) and every command that reads `config.default_model`/`base_url`/etc. to resolve through the active profile.

0. save chats persistently, be able to select one when you run comms agent-chat — done: `chat`/`agent-chat` sessions are saved to `~/.comms/chats.db` (SQLite) automatically, resumable via `--resume <id>`, browsable via `comms sessions list|show|delete`, and `--resume` with no id now opens a numbered picker instead of requiring the id upfront.

2. comms-only mode, presented with selector screen: chat, agent chat, agent, ask, etc..

Websearch

1. response formatting, especially newlines. Keep prompt input active while thinking with prompt queue if another is sent while thinking with the option to steer, make sure the chat history keeps track of the model and effort level of each message so each response message shows which model and effort level was used to generate it. Display the currently selected model and effort level somewhere near the new persistent prompt input field. — done: `chat` word-wraps output and spaces out turns consistently; each message records the model/effort that produced it and `sessions show`/resume label replies accordingly; the `chat` prompt stays live while a response is pending (input and the request run concurrently), a message typed mid-response is queued and sent once it finishes, and prefixing one with `/steer` cancels the in-flight request and sends it immediately instead; the prompt itself now shows `[model (effort)] You:`. Not yet extended to `agent-chat` — its turns can run tool calls with side effects, so cancelling mid-turn isn't safe without more thought.

3. display intermediate actions whilte thinking

4. connect providers directly
