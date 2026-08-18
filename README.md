# scratchpad-zellij

[![ci](https://github.com/madnh/scratchpad-zellij/actions/workflows/ci.yml/badge.svg)](https://github.com/madnh/scratchpad-zellij/actions/workflows/ci.yml)

A zellij plugin that shows which [scratchpad](https://github.com/madnh/scratchpad) pads
still have somebody listening, and marks the panes running an agent that nobody is
listening for.

The question it answers is invisible from inside a pane: an agent launched **without**
`scratchpad exec` looks exactly like one launched with it — right up until a pad moves and
nobody hears it.

```
  scratchpad  ✗ 1 unattended · ● 3 relay · ◌ 1 self-waiting · 3 pad
────────────────────────────────────────────────────────────────────
  ▤ default-8s7377 🔒 (3/?)
       ● PM           relay   codex
       ● frontend     relay   codex
     ⊘ ◌ DB           waiting gemini

  ▤ default-c68wqf (1/2) §6
       ● PM           relay   codex
       ○ backend      —       not listening — the pad is open to them
────────────────────────────────────────────────────────────────────
  v view · t labels · a align · h frame · Esc hide · q close · ? keys
```

It also writes a badge onto each pane's own frame, so you do not have to open the panel
to see the state:

```
[sp ● default-8s7377] Pane #5
[sp ◌ 2 pads] scratchpad
[sp ✗ unattended] ✦ Working…
```

## Requirements

- **zellij 0.44.3.** The plugin is built against `zellij-tile 0.44.3`; other versions may
  not load it.
- **scratchpad with `pad get --json`.** Older builds print human-formatted text only, and
  the plugin will say so rather than guess at it.
- **macOS or Linux.** The process join shells out to `ps -Ao pid=,ppid=,args=`.

## Install

Zellij loads a plugin straight from a URL, so nothing needs installing but the URL itself:

```kdl
plugins {
    scratchpad location="https://github.com/madnh/scratchpad-zellij/releases/latest/download/scratchpad-zellij.wasm" {
        scratchpad_bin "/usr/local/bin/scratchpad"
        label "true"
        highlight "true"
    }
}
```

Zellij caches the download, so the first load is the only one that touches the network.

### Or build it

```sh
rustup target add wasm32-wasip1
cargo build --release --target wasm32-wasip1
# → target/wasm32-wasip1/release/scratchpad-zellij.wasm  (~950 KB)
```

## Use

The plugin does two jobs, and **the more useful one needs no looking at**: it writes the
state onto each pane's frame. The panel is for when you want detail. So run it somewhere
permanent and cheap rather than opening it when you remember to.

### Recommended: a status row in your layout

```sh
zellij --layout ./layouts/scratchpad.kdl
```

That layout puts the plugin in a `pane size=1 borderless=true`. The plugin reads how many
rows it was given and **collapses itself**: one row prints the summary, a taller pane
prints the full panel. So a single instance covers both — focus the row and toggle
zellij's own fullscreen when you want the detail.

The one-line form leads with the number that ever needs acting on:

```
 scratchpad ✗ 2 agent(s) UNATTENDED · 1 relayed
 scratchpad ● 3 pane(s) with a relay listening
```

### Register it once, as an alias

Zellij can give a plugin a short name. Put the alias in the `plugins` block of
`~/.config/zellij/config.kdl`, **with the configuration attached to it**:

```kdl
plugins {
    // …zellij's own aliases stay as they are…

    scratchpad location="file:/path/to/scratchpad-zellij.wasm" {
        scratchpad_bin "/usr/local/bin/scratchpad"
        label "true"
        highlight "true"
    }
}
```

From then on the bare name works everywhere — layout, keybinding, CLI, and from other
plugins:

```kdl
bind "s" {
    LaunchOrFocusPlugin "scratchpad" { floating true; move_to_focused_tab true }
    SwitchToMode "Normal"
}
```

```kdl
pane size=1 borderless=true {
    plugin location="scratchpad"
}
```

```sh
zellij plugin --floating -- scratchpad
```

**Keep the configuration in the alias and nowhere else.** This is not tidiness: zellij
keys plugin instances by URL *and* configuration, so a keybinding that repeats the
settings creates a SECOND instance (finding 8) — two copies then take turns rewriting pane
titles and the frame visibly flickers. One alias means one instance, whichever way you
open it.

Two things that cost time to discover:

- **An alias only takes effect in a NEW zellij session.** Configuration is read when the
  server starts and there is no reload action — `zellij action` offers
  `start-or-reload-plugin` (reloads a *plugin*), nothing for the config. Until you restart,
  the alias fails with `Failed to resolve plugin alias` and you need the full `file:` URL.
- **The CLI needs `--` before the alias**: `zellij plugin --floating -- scratchpad`.
  Without it the argument parser rejects the bare name.

### Or: just try it

```sh
zellij plugin --floating \
  -c "scratchpad_bin=$(which scratchpad),label=true,highlight=true" \
  -- "file:$PWD/target/wasm32-wasip1/release/scratchpad-zellij.wasm"
```

On first load zellij asks for permissions. The plugin requests exactly three and nothing
else: `ReadApplicationState` (see the panes), `RunCommands` (call `scratchpad` and `ps`),
and `ChangeApplicationState` (rename panes, only while labelling is on).

**Use the SAME configuration everywhere**, which an alias gives you for free. A different
`-c` string is a different instance (finding 8); the plugin coordinates so they do not
fight over pane titles, but two copies still cost you for nothing.

**`scratchpad_bin` should be an absolute path.** `run_command` runs on the zellij *server*,
whose PATH is not your shell's.

## Configuration

| Key | Default | Meaning |
|---|---|---|
| `scratchpad_bin` | `scratchpad` | path to the scratchpad binary |
| `interval` | `3.0` | seconds between refreshes |
| `view` | `pad` | `pad` or `pane` — what the list is a list of |
| `label` | `false` | write a badge onto each pane's frame |
| `label_align` | `left` | `left` or `right` (read finding 7 before choosing `right`) |
| `label_pad` | `─` | glyph filling the gap when right-aligned |
| `highlight` | `false` | colour the frame of a pane running an unattended agent |
| `agents` | `claude,codex,gemini` | command names that count as an agent |
| `pad_mark` | `▤` | glyph heading a pad group |
| `blocked_mark` | `⊘` | glyph marking the author who posted last |

Keys inside the plugin pane: **`v`** pad ↔ pane view, **`t`** labels, **`a`** alignment,
**`h`** frame colour, **`?`** the legend, **`Esc`** hide the pane, **`q`** close it.

Hide and close are not the same thing. **`Esc`** suppresses the pane but the plugin keeps
running — badges and frame colours stay live, and the same `LaunchOrFocusPlugin`
keybinding that opened it brings it back with its state intact. **`q`** ends the plugin:
it strips its labels and highlights off every pane first, and the next launch starts
fresh.

Any glyph you configure must occupy **one terminal cell**. See finding 10.

## Reading the display

| | |
|---|---|
| `●` | a relay is listening for this agent |
| `◌` | no relay, but the agent armed its own `pad wait` |
| `✗` | running an agent, no relay, no wait — **nobody is listening** |
| `○` | an author on the pad with nobody listening for them here |
| `⊘` | posted last, so blocked by the turn rule |
| `!` | traffic this agent has not read |
| `🔒` | password-protected pad |
| `(2/3)` | 2 of the pad's 3 authors have someone listening |
| `(2/?)` | 2 listening; the author list is unreadable (protected pad) |
| `§6` | section count |

Everything is scoped to **one zellij session** — see "Scope" below.

### Two views of the same data

**By pad** (default) answers *"is anyone still listening to this conversation"*. **By pane**
(`v`) answers *"what is this terminal doing"*, grouped by how much attention it needs:
`UNATTENDED`, `self-waiting`, `relay listening`, `not using scratchpad`.

The pane view is the only place `✗` appears, because an agent listening to no pad shows up
in no pad group at all. The two views complement each other; neither replaces the other.

## How it works

A relay publishes its pid (`scratchpad relay --json`); zellij tells a plugin the pid running
in each pane (`get_pane_pid`). Joining those two is the whole idea, and it needs no change
on the scratchpad side — in particular no `ZELLIJ_*` variable, which that codebase is not
allowed to know about.

Everything else is derived from three commands, on two different clocks:

| Command | Every | Gives |
|---|---|---|
| `scratchpad relay --json` | 3s | which relays are alive, and what each watches |
| `ps -Ao pid=,ppid=,args=` | 3s | the process tree, live `pad wait`s, MCP servers |
| `scratchpad pad get <ref> --json` | ~60s | authors, section count, turn state |

No state is kept about pads. A deleted pad simply stops appearing.

## Findings

These were measured while building it. Several were wrong in ways that looked right.

**1. The pid join is not equality.** A relay sits at different depths depending on how the
pane started:

| Pane started as | `get_pane_pid` returns | Hops to the relay |
|---|---|---|
| the agent directly | the `scratchpad exec` pid | **0** |
| a shell, agent typed in | the shell's pid | **1+** |

So the plugin walks **up** the process tree from the relay's pid — one `ps` for the whole
machine, not one per pane — and stops at pid 1, since everything descends from init.

**2. Go encodes an empty slice as `null`, not `[]`.** The JSON comes from a Go binary, so
its shape is Go's to decide. A relay watching nothing has `"watching": null`, and `serde`
hitting `null` where it expects an array fails the **whole document** — the first run
showed "0 relays" while six were running. Every field the plugin reads is `Option`.

**3. Zellij caches plugins by path.** Rebuilding the `.wasm` and opening a new pane with
`zellij plugin` still runs the **old** build; only `zellij action start-or-reload-plugin`
re-reads from disk. This cost two rounds of chasing a fixed bug.

**4. Ask for a pid once — `rename` produces a `PaneUpdate`.** Turning labels on made every
pid come back `?`. The cause was a feedback loop: rename → `PaneUpdate` → re-ask every
pane's pid → rename again. A pane's pid cannot change while the pane lives, so re-asking
was both useless and the thing that caused the storm.

**5. Measuring right after a reload measures nothing.** `ps:0 relay:0` immediately after
`start-or-reload-plugin` looks exactly like a real failure. `run_command` is asynchronous;
results arrive later via `RunCommandResult`. The `ps:N relay:N` counters stayed on screen
for that reason — a clock saying "has data arrived yet" is worth more than any guess.

**6. Orphaned padding.** Right-aligned badges could lose the badge while keeping the
spaces, and with the marker gone nothing recognised the run as ours — it became part of the
pane's "original" name, growing by one run per rewrite. `strip_badge` trims trailing space
unconditionally.

**7. Right-aligned labels SWALLOW zellij's `SCROLL` indicator.** From
`zellij-server/src/ui/pane_boundaries_frame.rs`:

```rust
let left_side  = self.render_title_left_side(total_title_length);   // title takes first
let right_side = ... let space_left = total_title_length - left_len - 1;
                 self.render_title_right_side(space_left);          // SCROLL gets the rest
```

The title is served first and `SCROLL: 3/84` only gets what is left. Right-aligning means
padding the title out to nearly the full width, so `space_left` is about one column and the
indicator disappears entirely. **That is why `left` is the default.**

There is no API for writing to the right of a frame at all: that side is drawn by
`render_title_right_side` from the pane's own state (`scroll_position`, `is_floating`,
`is_pinned`).

**8. Two instances fight over pane titles, and the frame flickers.** Visible to the eye:
the badge and `SCROLL` alternating every few seconds. The trap is that
**`start-or-reload-plugin` with a DIFFERENT `-c` string starts a new instance** rather than
reloading the running one — zellij keys instances by URL *and* configuration.

The plugin handles this itself: instances can see each other, because plugin panes appear
in `PaneManifest` with their `plugin_url`. The lowest `plugin_id` among panes sharing a URL
is the only one that writes titles; the rest only display. No coordination protocol needed.

**9. `comm=` hides half the story; `args=` tells it all.** The first version knew a pad
only for panes that had a *relay*, so an agent running without `scratchpad exec` was
invisible even while taking part in that same pad. The data was in `ps` all along, in argv
rather than in the command name:

```
34459 16952 scratchpad pad wait default-ewhicx --since 4 --as backend --wake-for me,mine
```

A live `pad wait` carries both the **ref** and the **author**, and its ppid leads back to
the pane. It is also long-lived, unlike `pad post` which is over in milliseconds.

Matching is positional on the pair `pad wait`, not on the binary name, because the same
command appears three ways: direct, by absolute path, and wrapped in `zsh -c '…'`.

**10. `argv[0]` is the wrong field for recognising an agent.** gemini runs as
`node /opt/homebrew/bin/gemini` — `argv[0]` is `node`, and the name is in `argv[1]`. So an
agent **on the default list** was invisible. Every agent shipped as a script (node, python,
npx, bun) fails the same way. The plugin now scans every argument, skipping flags.

**11. "Is this an agent" is the wrong question.** The right one is **"does this pane use
scratchpad"** — an agent that never touches scratchpad needs no relay and deserves no
warning. Four signals, and they are not equal:

| Signal | Kind |
|---|---|
| a relay registration | **evidence** |
| a live `pad wait` | **evidence** |
| a live `<bin> serve --stdio` below the pane | **evidence** |
| `argv` matches the `agents` list | *guess* |

`serve --stdio` exists only because an agent opened an MCP connection to scratchpad, so it
proves the pane uses it whatever the agent is called. Verified by isolating the variable:
run with `agents=does-not-exist` and agent panes are still identified correctly.

The name list stays, because an agent driving scratchpad through the CLI spawns no server
and leaves no lasting trace.

**12. The theme is available, but only after the mode changes.** `ModeUpdate` carries
`mode_info.style.colors`, and two of its slots are named for what this panel needs:
`exit_code_error` and `exit_code_success`. Reading them makes the panel match a custom
theme, which the eight ANSI colours cannot.

The catch is when it arrives. The event fires on a mode CHANGE, not at load, so a plugin
opened mid-session renders in plain ANSI until the first `Ctrl p`, then switches to the
theme. Measured by dumping the pane's escape codes: `31`/`32` before, `38;2;205;214;244`
after. The ANSI defaults therefore have to look right on their own.

**13. Emoji presentation, not width, is what breaks column alignment.**

| Glyph | East Asian Width | In Unicode's emoji data |
|---|---|---|
| `●` `○` `▌` `▤` `⊘` | A / N | no |
| `▶` `◀` | A | **yes** |

`●` is a plain geometric character, so terminals always draw it with the text font, one
cell. `▶` and `◀` appear in the emoji data, so a terminal may switch to an emoji font and
draw them **two** cells — pushing the status column out of line on exactly the rows a
marker is meant to highlight.

**14. Mark the blocked author, not the ones who may speak.** The turn rule blocks exactly
one author, so on a pad of n agents, n−1 hold the turn. Marking those meant n−1 markers
pointing at the ordinary case, while the one interesting row — who just spoke — had nothing
on it. Marking the blocked author is always exactly one marker.

**15. One quantity, one format.** Printing `(2)` when nobody was missing and `(1/3)` when
somebody was put two different quantities in the same position, and the only way to tell
which was to count the rows below. It is always `listening/total` now; the **colour**
carries "somebody is missing".

**16. `pad get` was prose, and prose is not data.** The turn state used to arrive as
`turn: any author other than "codex" (last message: codex)`. That cannot be turned back
into a set of names without parsing English, so "who holds the turn" was not implementable
— not merely hard. `pad get --json` publishes `turn.blocked` as a list of names, and the
feature became four lines of code.

## Limits

**One session.** `PaneManifest` only ever holds the session the plugin runs in. The single
machine-wide input is `scratchpad relay --json`, and the process-tree join narrows it: a
relay under another session's zellij server reaches no pane here, so it never appears.
This is why an absent author reads `not listening in this session` rather than
`not listening` — they may be alive and well elsewhere.

**`◌` is a window, not a state.** It is visible only while a `pad wait` process is alive.
An agent wakes, the wait exits, the agent works, then re-arms — and in between, that pad
drops off its row. Not wrong, but it can flicker. `●` does not have this problem: a relay
holds its registration until the pad is deleted.

**Pads nobody listens to are invisible.** The plugin learns of a pad only through someone
listening to it, so a completely abandoned pad — arguably the most worrying kind — does not
appear. Reading `scratchpad pad list` would fix that; not done.

**Titles are not ours to keep.** Agents rewrite their own pane titles while working, so the
plugin never stores an "original title": it strips its own badge off whatever the title
says right now, and what is left is the original. Comparison is against the raw title
rather than a record of what was last written, so if an agent wipes the badge it is simply
written again.

## Security

The plugin reads the argv of **every process on the machine**. It extracts exactly two
things from a `pad wait` line: the word after `wait`, and the word after `--as`. Nothing
else reaches the screen or a pane title.

That matters because a pad password *can* end up in argv — not from scratchpad itself, but
from anything a user or agent writes by hand:

```
scratchpad pad wait <ref> --as DB --password <secret> --timeout 3600s
```

Once that runs, the secret is readable by every process running as the same user. The
plugin not copying it onto a frame is the last line of defence, not the first.

Pad refs *are* displayed for protected pads. A ref is an address; the password is the
secret. `pad get` on a protected pad is refused without the password, so those pads show
`🔒` and `(n/?)` and nothing more.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --release          # the pure functions; cargo cannot run wasm tests
```

The tests cover the functions that have actually been wrong: badge stripping, the
`pad wait` parser, the process-tree walk, and the JSON reader. Each case is a past
mistake rather than a feature.

Rebuild and reload in one go:

```sh
cargo build --release --target wasm32-wasip1 && \
zellij action start-or-reload-plugin \
  -c "scratchpad_bin=$(which scratchpad)" \
  "file:$PWD/target/wasm32-wasip1/release/scratchpad-zellij.wasm"
```

Keep exactly **one** instance: changing the `-c` string spawns another (finding 8). Count
them with `zellij action list-panes | grep scratchpad-zellij`, close extras with
`focus-pane-id <id>` then `close-pane`.

## Releasing

Push a tag; the workflow builds, tests, verifies the wasm exports and attaches the
`.wasm` to a GitHub release.

```sh
git tag v0.1.0 && git push origin v0.1.0
```

Both workflows use no third-party actions — every step past `actions/checkout` (pinned by
SHA) is a shell command against what the runner already ships, so GitHub is the only party
they have to trust.

## Licence

MIT — see [LICENSE](LICENSE).
