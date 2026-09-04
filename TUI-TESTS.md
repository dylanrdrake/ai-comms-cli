# Manual test plan

Started on `ea6c987` · `204b418` · `8c6a317` · `8adb1a6` · `144e511` (TODO
only, nothing to test); later sections are appended as changes land, so the
list is no longer five commits long.

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
- Directory column: `.` for the directory you are in, `~/…` under home, a
  full path above home, `dir not recorded` for sessions saved before it was
  tracked.
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

## 6. A session surviving being reopened

Fixed after this plan was written; the exact sequence that lost one:

1. Launch `clank`, choose **New session**, give it a name.
2. `Ctrl-B` straight back out without typing anything.
3. Resume it from the picker.
4. `Ctrl-B` back out again.

It should still be listed. Before the fix it was deleted here, because
reopening rebuilt "was this named?" from whether anyone had spoken in it.

The same root caused a second one worth checking: name a session, back out,
resume it, and *then* type something. The name you gave it should survive —
it used to be replaced by one derived from that first message.

## 7. Commands, as you type them

New: the leading `/command` in the input box turns cyan once it names a real
command, and a row above the box says what it could still be. Type these
without pressing Enter and watch the box.

```
/hel                  plain — not a command yet (the row above the box
                      does list "help"; that part is the next block)
/help                 the whole word turns cyan
/helpful              plain again — a longer word is a different word
/effort               cyan
/effort high          the name stays cyan, "high" does not
/etc/hosts            plain, the case this must never claim
/mdoel gpt-5          plain — send it and it is still caught as a typo
```

- Narrow the terminal until `/max-iterations` wraps mid-name. The colour
  should break where the row breaks and pick up on the next row, under the
  right characters.
- Backspace through a lit command — it should go plain the moment the name
  stops being one, not a keystroke later.
- `↑` to recall a command from history — it should come back lit.

Then the row above the box, and `Tab`:

```
/m                    lists: models  model  max-iterations
Tab                   /models, and "models" is marked on the row
Tab Tab               /model, then /max-iterations
Tab                   back round to /models
/hel then Tab         /help — one match, so it just fills it in
/te then Tab          /temp — as far as temperature and temp agree,
                      and no further
/                     lists as many as fit, then a "+N" count
Tab × 25              steps through all 21 and round again; the row
                      should slide so the marked one is always visible
/approval             the row becomes the command's form
/approval read on     the form stays up while you type the arguments
hello                 no row at all
/etc/hosts            no row at all
```

- The bottom row should read `Tab complete …` only while there is a list
  above the box — not while the form is showing, where Tab does nothing.
- Tab in the middle of an ordinary message must do nothing at all.
- Tab with the `/models` browser open must do nothing (the browser owns the
  keyboard; the box is its filter).
- The line-based `clank session` does none of this, by design.

## 8. Backing out of a working session, and going back in

New, and the part with no automated coverage — it needs a live model call,
and there is no fake client to run a worker against, so this section is the
test.

Start an **agent** session (`/agent`) with a task that takes a while and
writes something, then:

1. `Ctrl-B` while it is working.
2. The launch screen should show that session `working`, animating, with the
   tool it is running on the line beneath.
3. Watch it change on its own — the detail line should follow what it is
   doing, without you touching anything.
4. When it wants to write or run something, the badge should become `?` with
   what it is asking about. It must **wait** there, not deny itself.
5. Press Enter on that row. You should land back in the session — the whole
   transcript, including everything it did while you were away — with the
   approval box up and answerable (`Ctrl-Y` / `Ctrl-N`).
6. Answer it. The turn should carry on from where it paused.
7. Back out again mid-turn, let it finish this time. The row should go to
   `✓ replied` on its own, and opening it then should work the ordinary way.

Then the things that must not have broken:

- **Two at once.** Back out of a working session, start a second one, set it
  working, back out of that too. Both should show as working and both should
  be resumable. A third, opened and left idle, should not disturb them.
- **Another terminal still cannot touch them.** With a session parked here,
  open `clank` in a second terminal: that session must be refused, saying it
  is in use. This is the point — parking does not make a claimed session
  shareable, it makes *this* clank able to hand its own screen back.
- **Renaming a parked session** from the picker (`r`) should stick. Go back
  into it afterwards and the header should show the new name, not the old.
- **Deleting a parked session** (`d`) should be refused while it works, and
  allowed once it has finished.
- **Type at it after resuming.** Steering a resumed turn should work exactly
  as it does in one you never left.
- `Esc` mid-turn still cancels. `Ctrl-C` still quits promptly — it must not
  hang waiting for parked sessions, and a tool subprocess (`sleep 60`) must
  not be left behind. Check with `ps` after.
- Back out of an *idle* session — instant, and an empty unnamed one is still
  discarded rather than left in the list.

## 9. Two smaller ones

- **`? waiting` in the settings row.** In a session, get an approval to come
  up (agent mode, ask it to write a file). The row above the key hints should
  stop animating `working` and read `? waiting` for as long as the box is up,
  then go back to `working` when you answer. The launch screen's badge for
  the same session should agree.
- **Discarded output is dimmed.** Run `$ ls`, press `Ctrl-D` to discard. The
  output should stay in the transcript, dimmed to the same grey as a `default`
  value in `/status`. Then run `$ ls` again and press `Ctrl-S` — that one
  should stay at full brightness. Scroll back and forth: the two should be
  distinguishable at a glance, without reading `sent` / `not sent`.

## 10. From the round before, worth a glance

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
