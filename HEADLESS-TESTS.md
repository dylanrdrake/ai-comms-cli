# Manual test plan: `--headless`

Run against the installed `clank` (`~/.cargo/bin/clank`). If in doubt that it
matches the tree: `cargo install --path .` first.

Start with all three gates on — `clank approval show` — which is what §1 needs.

## 1. The refusal (the main safety claim)

```bash
clank approval show                      # expect: all three ✓ Ask

clank agent --headless "say hi"; echo "exit=$?"
```

Expect a refusal naming **read, write, terminal**, the `clank approval all off`
line, and `clank approval read off` as the one-at-a-time example. `exit=1`.
Nothing should have started — no "Starting agent task...", no session row.

```bash
clank approval read off
clank agent --headless "say hi"; echo "exit=$?"
```

Now it should name only **write, terminal** — and the example should say
`clank approval write off`, not `read`. That's the bit most likely to be wrong
(it uses the first gate still on).

```bash
clank approval all off
clank agent --headless "say hi"; echo "exit=$?"     # expect: runs, exit=0
```

Also worth one shot: `clank agent --h "say hi"` — the short alias should behave
identically.

## 2. `ask --headless` is clean enough to pipe

```bash
clank ask "name three colours" > noisy.txt
cat -A noisy.txt | head -3        # expect: ^M frames, the ✓, the model label

clank ask --headless "name three colours" > clean.txt
cat -A clean.txt | head -3        # expect: none of that — just the reply
grep -c $'\r' clean.txt           # expect: 0
```

Then the two things the flag exists for:

```bash
msg=$(clank ask --headless "one-line commit message for adding a --headless flag")
echo "[$msg]"                     # expect: no leading blank line, no ✓ inside the brackets

clank ask --headless "explain TCP backpressure in one paragraph" | awk '{print length}'
```

Resize the terminal narrow first — the lengths should stay long. If they cap
near the terminal width, the unwrapping regressed.

Failure path:

```bash
clank ask --headless "hi" -m no/such/model > out.txt; echo "exit=$?"
cat out.txt                       # expect: empty — error went to stderr, exit≠0
```

## 3. `agent --headless` attached

```bash
clank agent --headless "create headless-probe.txt containing OK" > run.log 2>&1
echo "exit=$?"; cat headless-probe.txt; grep -c $'\r' run.log
```

Expect the file created, tool notices present in the log (they're the record of
what happened), spinner absent, zero `\r`.

## 4. Detached, for real

```bash
setsid clank agent --headless --session \
  "run 'sleep 45' in the terminal, then write done.txt containing finished" \
  > detached.log 2>&1 < /dev/null &

clank sessions                    # expect: a new row, [agent_chat], working
tail -f detached.log
```

Now **close the terminal** while it runs, open a new one, and check
`clank sessions` and `done.txt`. That's the whole point of `setsid` and
`< /dev/null`, and it's the only way to really test it.

## 5. Heartbeat — the crash case

Start the same detached run, then:

```bash
pkill -9 -f 'clank agent'
clank sessions                    # within ~5s: still says `working`
sleep 35
clank sessions                    # expect: flipped to `no reply`
```

The row must settle on its own with nothing cleaned up. If it still says
`working` after ~35s, the heartbeat gating isn't taking effect.

## 6. One runner per session

While a detached `--session` run is live:

```bash
clank agent --resume <id> "another task"
```

Expect the "already being run by another process" refusal. Then `kill -9` it,
wait 35s, and run the same command — it should now be accepted.

## 7. No nesting

```bash
CLANK_HEADLESS=1 clank agent --headless "say hi"; echo "exit=$?"
CLANK_HEADLESS=1 bash -c 'clank agent --headless "say hi"'   # inheritance through a child shell
```

Both should hit "Refusing to start a headless run from inside one."

Then the real version — from inside a headless run, since `terminal` is ungated:

```bash
clank agent --headless "run this exact command: clank agent --headless 'say hi'"
```

The child must refuse and the parent must carry on rather than dying.

Worth deciding: `CLANK_HEADLESS=1 clank agent "say hi"` (no `--headless`) still
runs. The marker only blocks *headless* children. Confirm that's the rule you
want.

## 8. The case most likely to find something

The start check reads the **global** config (`main.rs:1943`), but a resumed run
uses the **session's saved** gates (`main.rs:1966`). Those can disagree:

```bash
clank approval all on
clank agent --session "say hi"           # session saves gates=ON; approve as needed, note the id
clank approval all off                   # global now clear
clank agent --headless --resume <id> "read src/store.rs and summarise it"
```

The start check passes (global gates are off), but the session runs with its own
gates on — so every tool should hit the silent
`denied: … --headless cannot ask` path and the run burns a task's worth of
tokens producing nothing. That is exactly the outcome §1 exists to prevent.
Confirm the behaviour, then decide whether the check should read the resumed
session's gates instead.

## Cleanup

```bash
rm -f noisy.txt clean.txt out.txt run.log detached.log headless-probe.txt done.txt
```

Prune the test sessions from `clank sessions`.
