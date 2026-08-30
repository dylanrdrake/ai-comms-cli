can we connect to openrouter too or other "model aggregators"? — done (single active provider): `base_url` is configurable via `orca endpoint [url]`, effort-level serialization shape via `orca effort-style <flat|nested|none>` (flat for OrcaRouter, nested for OpenRouter), and extra per-request headers via `orca headers set/unset` (e.g. OpenRouter's `HTTP-Referer`/`X-Title`). Still one active API key/model/endpoint at a time — switching providers means re-running `orca login` and `orca endpoint`.

5. Provider profiles: named, saved provider configs (base URL + keyring API key + default model + effort style + headers) so you can register OrcaRouter and OpenRouter side by side and switch with one command instead of re-entering everything, e.g. `orca provider add openrouter --base-url https://openrouter.ai/api/v1 --effort-style nested`, `orca provider use openrouter`, `orca provider list`. Needs per-profile keyring entries (today there's a single `orcacli`/`api_key` entry) and every command that reads `config.default_model`/`base_url`/etc. to resolve through the active profile.

0. save chats persistently, be able to select one when you run orca agent-chat — done: `chat`/`agent-chat` sessions are saved to `~/.orcacli/chats.db` (SQLite) automatically, resumable via `--resume <id>`, and browsable via `orca sessions list|show|delete`. Still open: no interactive picker (must know/copy the id or prefix ahead of time).

2. orca only mode, presented with selector screen: chat, agent chat, agent, ask, etc..

Websearch

1. response formatting, especially newlines. Keep prompt input active while thinking with prompt queue if another is sent while thinking with the option to steer

3. display intermediate actions whilte thinking

4. connect providers directly
