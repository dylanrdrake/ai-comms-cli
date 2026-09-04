# Manual test plan: the last five commits

`ea6c987` · `204b418` · `8c6a317` · `8adb1a6` · `144e511` (TODO only, nothing to test)

Rebuild first — `cargo install --path .`, or run `./target/release/clank`.
Ordered by how likely each is to be broken, not by how new it is.

## 1. Every in-session command — highest risk

`204b418` moved the TUI's submission dispatch out of the key handler: 416
lines of `src/tui/mod.rs`, touching the wiring of *every* slash command.
Five paths have tests. The rest are held up by the compiler and nothing
else, so this is where a silent breakage would be.

In a TUI session (`clank`, open or start one), run each and check it does
what it claims:

```
/help                 lists every command
/status               shows the session's settings
/model                reports the current model
/model <name>         switches it; settings bar updates
/effort               reports the level
/effort high          sets it
/temperature 0.5      sets it
/temp                 reports it
/verbose on           tool arguments and results start showing
/highlight off        the band behind your messages goes
/sandbox              reports whether writes are confined
/stream               reports streaming
/approval             shows the three gates
/approval write off   changes one; takes effect immediately
/session title Foo    renames; the header updates
/agent                tools on
/ask                  tools off
/max-iterations 5     sets the cap
/back                 returns to the launch screen
```

Then the non-slash paths through the same code:

```bash
hello                 # a plain message still sends
$ echo hi             # output box appears
                      # Ctrl-S sends it, Ctrl-D discards
                      # /send and /discard do the same
                      # Ctrl-Y / Ctrl-N answer a tool approval
/mdoel gpt-5          # a typo is reported, NOT sent to the model
/effor                # near-miss suggestion, not prose
```

## 2. `/models` — new, and shipped broken once

It was routed to the worker while the code opening the box sat on the
branch for things the worker *doesn't* handle, so it displayed nothing at
all. Fixed in `204b418`; worth confirming for real.

```
/models                     box opens at once with a spinner, then the list
type "claude"               filters as you type
↑ ↓                         moves; stops at both ends rather than wrapping
Enter                       sets that model — check the settings bar
Esc                         closes, changes nothing
```

Edge cases:

- A filter matching nothing → "nothing matches", not an empty box.
- `/models` then `Esc` before the list arrives → stays closed, does not
  reopen when the fetch lands.
- `/models` in the line-based `clank session` → says it is a TUI command.
- `/models` with an argument (`/models claude`) → a usage error, not prose.

## 3. Session list, now flat

`8adb1a6` removed the "In this directory" / "Elsewhere" split.

- `clank` → one list, newest first, no section headings.
- Directory column: `~` for home itself (even when that is where you are),
  `.` for any other directory you are in, `~/…` under home, a full path
  above home, `dir not recorded` for sessions saved before it was tracked.
- Arrow top to bottom — the cursor should never skip a row or land
  somewhere that cannot be opened.
- Delete one with `d` → the list closes up, the cursor stays sensible.

## 4. Sorting and spacing

- `clank models` → alphabetical. The truncated twenty are now the first
  twenty *alphabetically*, not whatever the endpoint led with.
- `/models` → same order.
- With `/models` open → exactly one blank row between the box and the last
  line of the transcript.

## 5. The busy animation

The rotating dot circle is now two braille cells of scattered dots,
regenerated each tick. It should look like noise, not a clock.

- A running turn in the TUI — the `working` indicator in the settings bar.
- The same session watched from the launch screen in another terminal — the
  badge should animate identically, and the badge column is one cell wider
  than it was, so check nothing in the list sits ragged.
- `clank ask "..."` — the CLI spinner uses the same frames.
- `/models` while it fetches.

## 6. From the round before, worth a glance

- **Picker scrolls.** Accumulate more sessions than fit, or shrink the
  terminal. The list should follow the cursor, and the rule above it should
  say `N more below` / `N above · N below`.
- **`clank timeout`** — shows four values; `clank timeout stream-idle 180`
  sets one; `clank timeout bogus` and `clank timeout connect 0` are refused.
- **Held sessions** show `⎚` in the list. Open a session in one terminal,
  look at the launch screen in another; opening it there should be refused
  with a one-line notice, and the notice should clear on the next keypress.
- **Bare `/effort`** reports the level instead of erroring.

## Known, not worth reporting

- `clank model --clear extra-arg` exits 0 and silently ignores the name.
  Pre-existing; `--clear` just wins.
