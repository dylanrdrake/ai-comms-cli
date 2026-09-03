# Test plan

Everything added or fixed across the last ten commits, `04e9240` through
`628d9e4`, ordered by how much real-world exposure it has had rather than by
when it landed.

If you only have time for two, do **steering** and **a second `$` while one is
waiting**. Both have real state machines and neither has met a live model.

---

## 1. Never tested against a real model

### Steering — a message joins the turn already running
`f8001e4`

Start an agent turn that will run a few tools, then type while it works.

- [ ] The message appears in the transcript at the moment the loop takes it,
      not at the end of the turn
- [ ] The model acts on it — "actually, check Windows too" mid-turn should
      change what it does next
- [ ] In **ask** mode the same message waits and becomes its own turn instead
- [ ] `Esc` mid-turn with a message still waiting drops it

Watch for: a message landing *between* a tool call and its result would make
the next request invalid. If you see a 400 after steering, that is the thing
to report.

### `web_fetch` — reading a page without curl
`61be7c1`

Ask the agent to read a documentation page.

- [ ] The approval prompt says `web_fetch: https://…` — **the real question**,
      since nothing but the tool description steers it away from `curl`
- [ ] What comes back is prose, not markup
- [ ] `file:///etc/passwd` is refused by scheme, pointing at `read_file`
- [ ] A 404 is reported as a failing status, not handed over as page content

If it reaches for `curl` instead, the description is not pulling its weight
and that is worth knowing before building on it.

### `highlight` and `selection` settings
`628d9e4`

```bash
clank highlight off     # new sessions start without the band
clank highlight         # reads it back
clank selection off     # launch screen's selected row
```

- [ ] A new session has no band behind your messages
- [ ] `/highlight on` inside that session turns it on for that session only
- [ ] `Ctrl-B` out and resume — the session remembers its own setting
- [ ] `clank status` lists both
- [ ] `/status` lists highlighting

---

## 2. Partly tested

### `$` — running a command yourself
`0496feb`

You have used the basic path. These are the parts you have not:

- [ ] `$ ls`, then **before answering**, run `$ pwd` — the first must land in
      the transcript marked `not sent`, not vanish
- [ ] `$ sleep 5`, then try another `$` while it runs — refused with a notice
- [ ] `$ sleep 5` — the box appears at once, the spinner actually animates for
      the full five seconds, and typing still works
- [ ] `$ sudo -n true` — fails in **milliseconds** with its own error, not a
      thirty-second hang
- [ ] `$ pwd` reports the session's directory, not wherever the TUI launched
- [ ] Something with a large output — the box caps rather than filling the
      screen
- [ ] `Ctrl-S` sends without prompting a reply; the next message you type
      carries it to the model
- [ ] `Ctrl-D` discards, and the command still shows in the transcript

### Chord fallbacks, in Zed
`0496feb`

Zed's terminal claims `Ctrl-S`. Every chord has a typed twin:

- [ ] `/send` and `/discard` answer the `$` box
- [ ] `/allow` and `/deny` answer an approval — **the important pair**;
      without them an approval in Zed cannot be answered and the turn stalls
      with nothing but `Esc` to escape it
- [ ] `/back` returns to the launch screen (tmux claims `Ctrl-B`)

---

## 3. Fixes worth confirming

### stdin is closed for terminal commands
This one is the **agent's** tool, not just `$`. Ask the agent to run something
interactive.

- [ ] It errors immediately rather than hanging until the timeout kills it

### The gutter mark matches the picker
`fbe5d08`, corrected later

- [ ] The braille square on a session's row and the glyph beside replies
      inside that session are the same shape — they disagreed until the app
      started holding the full session id

### An approval no longer eats your draft
`a483a89`

- [ ] Start typing, let an approval arrive, answer it with `Ctrl-Y` — what you
      had typed is still in the box
- [ ] You can finish and send that message while the decision is outstanding
- [ ] Both boxes open at once: `Ctrl-Y` answers the approval and leaves the
      `$` output alone; `Ctrl-S` does the reverse

### `clank sessions list` shows state
`628d9e4`

- [ ] With a session working in another terminal, the listing says `working`
- [ ] A session waiting on an approval prints what it is asking about beneath
- [ ] Columns line up — `[ask]` and `[agent]` are padded to the same width

### `chat` is `ask` everywhere
- [ ] Picker, `clank sessions list`, `clank sessions show`, and the settings
      row all say `ask`

### The queued count falls as it drains
`4a3a67c`

- [ ] Queue several messages, let them run — the count reaches zero rather
      than sticking at its high-water mark

---

## 4. Already confirmed

Picker live monitoring and its states, the column layout, session marks, the
pending-messages box, the approval box rework, and the spacing between the
transcript and the boxes above the prompt.

---

## Known open questions

- The terminal background query runs at every TUI launch, with a one-second
  worst case on terminals that never answer, and carries three dependencies
  for it. With `clank highlight off` now available, a fixed band may be the
  better trade.
- `$` is TUI-only; the CLI refuses it rather than doing something different.
- `$` output is captured, not streamed — the box fills all at once when the
  command exits.
- A steered message is lost if the turn it joined is cancelled. Low severity,
  noted in TODO.
