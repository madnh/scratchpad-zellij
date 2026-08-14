//! Prototype: which panes in this zellij session have a scratchpad relay listening?
//!
//! The question this answers is the one that is invisible from inside a pane: an agent
//! launched WITHOUT `scratchpad exec` looks exactly like one launched with it, right up
//! until nobody hears a pad move. A relay publishes its pid (`scratchpad relay --json`);
//! zellij tells a plugin the pid running in each pane (`get_pane_pid`). Joining those two
//! is the whole idea, and it needs no change on the scratchpad side — in particular no
//! ZELLIJ_* variable, which that codebase is not allowed to know about.
//!
//! The join is not pid equality. Measured on this machine, a relay sits at two different
//! depths depending on how the pane was started:
//!
//!   pane runs the agent directly   → pane pid IS the `scratchpad exec` pid  (0 hops)
//!   pane runs a shell, agent typed → pane pid is the shell, exec is a child (1+ hops)
//!
//! So we walk UP from the relay's pid through `ps` and see whether we reach a pane's pid.
//! The hop count is displayed rather than hidden: it is the evidence that the match is
//! real and not a coincidence of two small integers.
//!
//! This prototype is READ-ONLY on purpose. `set_pane_color` takes `None` to mean "leave
//! unchanged", so there is no documented way to put a pane's colour back — colouring
//! panes in a live working session is not something a probe should do before we know it
//! works.

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use zellij_tile::prelude::*;

/// How far up the process tree a relay may sit from its pane. A shell plus a wrapper or
/// two is realistic; anything deeper is more likely a coincidence than a match.
const MAX_HOPS: usize = 12;

const DEFAULT_INTERVAL: f64 = 3.0;

/// One entry of `scratchpad relay --json`.
#[derive(Deserialize, Clone, Debug, Default)]
struct Relay {
    pid: i32,
    #[serde(default)]
    command: String,
    /// Option rather than Vec, and that is not style: Go encodes an empty slice as `null`,
    /// not `[]`, so a relay watching nothing fails a plain `Vec` deserialize outright —
    /// measured, and it took down the whole list, not just that one entry.
    #[serde(default)]
    watching: Option<Vec<Watched>>,
}

#[derive(Deserialize, Clone, Debug)]
struct Watched {
    #[serde(rename = "ref")]
    pad_ref: String,
    author: String,
    #[serde(default)]
    pending: bool,
}

/// The marker that makes a title we wrote recognisable as ours.
///
/// It has to be recognisable because the title is not ours to keep: an agent rewrites its
/// own pane title as it works (Claude Code puts the current task there). So we never store
/// "the original title" — we strip our own mark off whatever the title says RIGHT NOW, and
/// what is left is the original. A stored copy would freeze the title at the moment we
/// first saw it and quietly discard every change the agent made after.
const BADGE: &str = "[sp ";

/// Roughly how many columns of a pane's width the frame itself eats: the two corner
/// characters plus the space either side of the title.
///
/// It is an estimate, and the only part of right-alignment that is. Zellij gives no API
/// for writing to the right-hand side of a frame — that side is where it draws its own
/// indicators (SCROLL, FLOATING, exit status) — so "right-aligned" here means padding the
/// TITLE out with spaces until the badge lands near the edge. Off by a couple of columns
/// and it simply sits a couple of columns short.
const FRAME_OVERHEAD: usize = 6;

/// What to fill the gap with when the badge is pushed to the right.
///
/// `─` rather than a space, because a space leaves a visible hole in the frame — the line
/// looks broken rather than continued. This is the exact character zellij itself pads a
/// title line with (`boundary_type::HORIZONTAL`), and zellij draws the title with the same
/// colour it draws the frame (`foreground_color(&full_text, self.color)`), so the run we
/// splice in matches the real border in both glyph and colour.
///
/// Configurable because a session running zellij's simplified/ASCII UI draws its borders
/// with something else, and a plugin cannot see that setting.
const DEFAULT_PAD: &str = "─";

/// The glyph marking the author who just posted, and is therefore blocked.
const DEFAULT_BLOCKED_MARK: &str = "⊘";

/// What separates a tab's own name from the trouble count appended to it.
///
/// Distinct enough to strip back off reliably, short enough for a tab bar, and it carries
/// the same meaning as `✗` everywhere else in this plugin.
const TAB_MARK: &str = " ✗";

/// The glyph that heads a pad group.
///
/// `▤` is a square with horizontal rules — a sheet with writing on it — and it stays in
/// the same monochrome geometric family as `●`, `○`, `⊘` and `─`. The literal paper emoji
/// `📄` and `📝` are two cells wide and coloured; they read as decoration next to the rest
/// of the panel, and this row already spends its one emoji on `🔒`.
const DEFAULT_PAD_MARK: &str = "▤";

/// What the table is a table OF.
#[derive(Clone, Copy, PartialEq, Debug)]
enum View {
    /// One row per pad: who is listening to it, and how. The default, because the pad is
    /// the unit of work — a pane is only where it happens to be running, and the question
    /// people actually ask is "is anyone still listening to this conversation".
    Pad,
    /// One row per pane. Answers the other direction: "what is this terminal doing".
    Pane,
}

/// How an agent is listening to a pad.
#[derive(Clone, Copy, PartialEq)]
enum Listen {
    /// A relay is holding the line for it.
    Relay,
    /// It armed a `pad wait` itself.
    Own,
}

/// `pad get --json`, as far as this plugin cares.
///
/// Every field optional: this has to survive a scratchpad older than the flag, a newer one
/// that renames something, and Go's habit of encoding an empty slice as `null`.
#[derive(Deserialize, Default)]
struct PadGetJson {
    #[serde(default)]
    section_count: Option<u32>,
    #[serde(default)]
    authors: Option<Vec<String>>,
    #[serde(default)]
    protected: Option<bool>,
    #[serde(default)]
    turn: Option<TurnJson>,
}

#[derive(Deserialize, Default)]
struct TurnJson {
    /// Who may NOT post next. The turn holder is everyone else — which is why this is the
    /// field that matters and `waiting_for` is not: `blocked` is names, `waiting_for` is
    /// the same sentence the text output already had.
    #[serde(default)]
    blocked: Option<Vec<String>>,
}

/// What `pad get <ref>` tells us about a pad, beyond who is listening.
#[derive(Clone, Default)]
struct PadInfo {
    sections: Option<u32>,
    /// Authors who may not post next. Empty when unknown — never guessed.
    blocked: Vec<String>,
    /// Everyone who has ever posted to the pad.
    ///
    /// The difference between this and the listeners is the point: an author on the pad
    /// with nobody listening for them is a conversation that has quietly lost a
    /// participant, and showing only the listeners hides exactly that.
    authors: Vec<String>,
    protected: bool,
}

/// One agent listening to one pad.
struct Listener {
    author: String,
    how: Listen,
    /// The agent's own name, or failing that the pane it sits in — enough to walk over and
    /// look at it.
    who: String,
    pending: bool,
}

// ANSI, and it works here for the same reason it does NOT work in a pane title: the
// plugin's own pane is rendered as terminal output, so escape codes are interpreted, while
// a title is copied character by character into a styled cell buffer (`foreground_color`
// in zellij's frame renderer) that never parses them.
//
// Only the eight base colours and the two attributes are used. They are whatever the
// user's theme says they are, so the panel keeps looking like the rest of their terminal
// instead of fighting it.
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";

/// Where the badge goes in the pane title.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Align {
    /// `[sp ●] pane-name` — cannot be cut off, because zellij trims the END of a title
    /// that will not fit.
    Left,
    /// `pane-name        [sp ●]` — padded out towards the right edge of the frame.
    Right,
}

/// A terminal pane in this session, with whatever we have learned about it.
#[derive(Clone, Debug)]
struct Pane {
    id: u32,
    /// Which tab holds this pane. `PaneManifest` is keyed by tab position, so this costs
    /// nothing to collect — and it is what lets a tab report only its OWN trouble.
    tab: usize,
    /// The title as zellij reports it, badge and all.
    raw_title: String,
    /// The same title with our badge removed — what the pane would be called if this
    /// plugin had never run.
    title: String,
    pid: Option<i32>,
    /// How wide the pane is, for right-aligning the badge.
    columns: usize,
    relay: Option<Relay>,
    hops: usize,
    /// Every `pad wait` this pane is running itself: (ref, author).
    ///
    /// A list, not one: an agent takes part in several pads at once and arms a wait for
    /// each, so stopping at the first one found reports whichever pid the scan happened to
    /// reach first — and never mentions the pad it joined most recently.
    waiting: Vec<(String, String)>,
}

struct State {
    /// The scratchpad binary. `run_command` runs on the zellij SERVER, whose PATH is not
    /// the one your shell has — hence configurable, and hence the error is shown.
    bin: String,
    interval: f64,
    relays: Vec<Relay>,
    /// pid -> ppid for every process on the machine, from one `ps` call. One call, not one
    /// per pane: the cost must not grow with the number of agents.
    parents: BTreeMap<i32, i32>,
    /// ppid -> its children, the same `ps` read from the other end. Walking UP finds a
    /// pane's relay; walking DOWN finds the agent a pane is running.
    children: BTreeMap<i32, Vec<i32>>,
    /// pid -> command name (basename of argv[0]), for display.
    commands: BTreeMap<i32, String>,
    /// pid -> the agent name found ANYWHERE in its argv, not just argv[0].
    ///
    /// Measured: gemini runs as `node /opt/homebrew/bin/gemini`, so argv[0] is `node` and
    /// matching on it misses an agent that IS on the default list. Every agent shipped as
    /// a script — node, python, npx, bun — fails the same way, which makes argv[0] the
    /// wrong field rather than an incomplete one.
    agent_of: BTreeMap<i32, String>,
    /// pid -> (pad ref, author) for every live `pad wait`. An agent with no relay is
    /// supposed to arm one of these itself; finding it is the difference between "nobody
    /// is listening" and "it is listening for itself".
    waits: BTreeMap<i32, (String, String)>,
    /// Every live `scratchpad serve --stdio` — an MCP server an agent spawned for itself.
    ///
    /// This is EVIDENCE where the `agents` name list is a guess: it proves the process
    /// above it talks to scratchpad, whatever that process is called. It also answers the
    /// question that actually matters. "Is this pane an agent" is not it — an agent that
    /// never touches scratchpad needs no relay and deserves no warning. "Does this pane
    /// use scratchpad" is the question, and this is the answer to it.
    mcp: BTreeSet<i32>,
    /// pane id -> pid, asked once and kept.
    ///
    /// A pane's pid does not change while the pane lives, so re-asking is pure cost — and
    /// it was worse than cost: renaming a pane produces a PaneUpdate, which re-asked every
    /// pane, which renamed again. Under that storm every `get_pane_pid` came back empty
    /// and the table showed `?` for panes it had read correctly seconds earlier.
    pids: BTreeMap<u32, i32>,
    panes: Vec<Pane>,
    error: Option<String>,
    permission: Option<bool>,
    /// Whether we are writing badges onto pane frames. Off until asked: this is the one
    /// thing here that changes something outside the plugin's own pane.
    labelling: bool,
    view: View,
    align: Align,
    /// The glyph the right-aligned gap is filled with.
    pad: String,
    /// The glyph that points at whoever may post next.
    turn_mark: String,
    /// The glyph heading each pad group.
    pad_mark: String,
    /// Whether to append a trouble count to each tab's name.
    tab_label: bool,
    /// Tabs as zellij last reported them: (stable id, position, stripped name).
    tabs: Vec<(usize, usize, String)>,
    /// Whether to colour the FRAME of a pane running an agent nobody is listening for.
    ///
    /// This goes through `highlight_and_unhighlight_panes`, not `set_pane_color`. The
    /// difference is the way back: highlight has an explicit un-highlight, whereas
    /// `set_pane_color` reads `None` as "leave unchanged" and offers no route to the
    /// default. The colour itself comes from the theme and cannot be chosen.
    highlighting: bool,
    /// Which panes we have highlighted, so only the difference is ever sent.
    highlighted: BTreeSet<u32>,
    /// pad ref -> what `pad get` says about it.
    pads: BTreeMap<String, PadInfo>,
    /// Ticks since the last `pad get` sweep. Pad metadata moves at conversation speed, not
    /// at the speed of a process table, so it is refreshed far less often — one command
    /// per pad is cheap once a minute and wasteful every three seconds.
    since_pad_sweep: u32,
    /// Whether the key legend is expanded.
    keys_open: bool,
    /// This instance's own plugin id, from `get_plugin_ids()`.
    me: u32,
    /// Whether this instance is the one allowed to write pane titles.
    ///
    /// Two instances writing titles is not a hypothetical: every change to the `-c` string
    /// starts a NEW instance rather than reloading the old one, so they pile up. Both then
    /// compute a title from a name the other one just rewrote, and the frame visibly
    /// flickers between the two answers every few seconds.
    ///
    /// Coordination needs no protocol, because each instance can see the others: plugin
    /// panes appear in `PaneManifest` with their `plugin_url`. Lowest plugin id among the
    /// panes sharing our url does the writing; everyone else keeps rendering its own view
    /// and touches nothing.
    writer: bool,
    /// How many other instances of this plugin are up, to say so on screen.
    twins: usize,
    /// How many PaneUpdate events have arrived. On screen because the question "does
    /// zellij tell us when a pane is RESIZED" cannot be answered by reading the code as
    /// fast as by watching this number while resizing something.
    updates: u64,
    /// Command names that count as "an agent" — a pane running one of these with no relay
    /// is the case worth warning about. Nothing else gets a warning badge, because a shell
    /// or an editor without a relay is not a problem, it is a shell.
    agents: Vec<String>,
}

impl Default for State {
    fn default() -> Self {
        State {
            bin: "scratchpad".to_owned(),
            interval: DEFAULT_INTERVAL,
            relays: vec![],
            parents: BTreeMap::new(),
            children: BTreeMap::new(),
            commands: BTreeMap::new(),
            agent_of: BTreeMap::new(),
            waits: BTreeMap::new(),
            mcp: BTreeSet::new(),
            pids: BTreeMap::new(),
            panes: vec![],
            error: None,
            permission: None,
            labelling: false,
            view: View::Pad,
            align: Align::Left,
            pad: DEFAULT_PAD.to_owned(),
            turn_mark: DEFAULT_BLOCKED_MARK.to_owned(),
            pad_mark: DEFAULT_PAD_MARK.to_owned(),
            tab_label: false,
            tabs: vec![],
            highlighting: false,
            highlighted: BTreeSet::new(),
            pads: BTreeMap::new(),
            since_pad_sweep: u32::MAX,
            keys_open: false,
            me: 0,
            writer: true,
            twins: 0,
            updates: 0,
            agents: vec!["claude".into(), "codex".into(), "gemini".into()],
        }
    }
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        if let Some(bin) = configuration.get("scratchpad_bin") {
            self.bin = bin.clone();
        }
        if let Some(secs) = configuration.get("interval").and_then(|s| s.parse().ok()) {
            self.interval = secs;
        }
        if let Some(list) = configuration.get("agents") {
            self.agents = list
                .split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect();
        }
        self.labelling = configuration.get("label").is_some_and(|v| v == "true");
        if configuration.get("label_align").is_some_and(|v| v == "right") {
            self.align = Align::Right;
        }
        if let Some(pad) = configuration.get("label_pad") {
            if !pad.is_empty() {
                self.pad = pad.clone();
            }
        }
        if let Some(mark) = configuration.get("pad_mark") {
            if !mark.is_empty() {
                self.pad_mark = mark.clone();
            }
        }
        if let Some(mark) = configuration.get("blocked_mark") {
            if !mark.is_empty() {
                self.turn_mark = mark.clone();
            }
        }
        self.highlighting = configuration.get("highlight").is_some_and(|v| v == "true");
        self.tab_label = configuration.get("tab_label").is_some_and(|v| v == "true");
        if configuration.get("view").is_some_and(|v| v == "pane") {
            self.view = View::Pane;
        }
        self.me = get_plugin_ids().plugin_id;
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::RunCommands,
            // Only used for `rename_pane_with_id`, and only while labelling is on.
            PermissionType::ChangeApplicationState,
        ]);
        subscribe(&[
            EventType::PaneUpdate,
            EventType::TabUpdate,
            EventType::RunCommandResult,
            EventType::Timer,
            EventType::PermissionRequestResult,
            EventType::Key,
            // The way out: a plugin that is closed while holding every pane title hostage
            // would leave the session permanently marked up.
            EventType::BeforeClose,
        ]);
        set_timeout(0.0);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::PermissionRequestResult(status) => {
                self.permission = Some(status == PermissionStatus::Granted);
                self.refresh();
                true
            }
            Event::Timer(_) => {
                self.refresh();
                // Roughly a minute between pad sweeps, whatever the refresh interval is —
                // but immediately for a pad we have never seen. The first sweep runs
                // before any PaneUpdate has arrived, so it asks about nothing, and without
                // this a freshly loaded plugin shows no pad detail for a whole minute.
                let every = ((60.0 / self.interval.max(1.0)) as u32).max(1);
                self.since_pad_sweep = self.since_pad_sweep.saturating_add(1);
                if self.has_unknown_pad() {
                    self.since_pad_sweep = every;
                }
                if self.since_pad_sweep >= every {
                    self.since_pad_sweep = 0;
                    self.sweep_pads();
                }
                set_timeout(self.interval);
                false
            }
            Event::TabUpdate(tabs) => {
                self.tabs = tabs
                    .iter()
                    .map(|t| (t.tab_id, t.position, strip_tab_mark(&t.name)))
                    .collect();
                self.sync_tabs();
                false
            }
            Event::PaneUpdate(manifest) => {
                self.updates += 1;
                self.take_panes(manifest);
                self.rejoin();
                self.sync_labels();
                self.sync_highlights();
                self.sync_tabs();
                true
            }
            Event::RunCommandResult(exit, stdout, stderr, context) => {
                self.take_command_result(exit, stdout, stderr, context);
                self.rejoin();
                self.sync_labels();
                self.sync_highlights();
                self.sync_tabs();
                true
            }
            Event::Key(key) => match key.bare_key {
                BareKey::Char('t') => {
                    self.labelling = !self.labelling;
                    self.sync_labels();
                    true
                }
                BareKey::Char('a') => {
                    self.align = match self.align {
                        Align::Left => Align::Right,
                        Align::Right => Align::Left,
                    };
                    // Take the old badge off before putting the new one on: the two
                    // alignments write different strings, and a switch that only added
                    // would leave the previous one behind.
                    let was = self.labelling;
                    self.labelling = false;
                    self.sync_labels();
                    self.labelling = was;
                    self.sync_labels();
                    true
                }
                BareKey::Char('?') => {
                    self.keys_open = !self.keys_open;
                    true
                }
                BareKey::Char('v') => {
                    self.view = match self.view {
                        View::Pad => View::Pane,
                        View::Pane => View::Pad,
                    };
                    true
                }
                BareKey::Char('h') => {
                    self.highlighting = !self.highlighting;
                    self.sync_highlights();
                    true
                }
                _ => false,
            },
            Event::BeforeClose => {
                self.labelling = false;
                self.highlighting = false;
                self.tab_label = false;
                self.sync_labels();
                self.sync_highlights();
                self.sync_tabs();
                false
            }
            _ => false,
        }
    }

    fn render(&mut self, rows: usize, cols: usize) {
        // The pane it is given decides what it is. One or two rows in a layout makes it a
        // status line — the summary is the whole point there, and a table would be clipped
        // to its header. Anything taller gets the full view.
        //
        // This is what lets a single instance be both things: parked on one row all day,
        // and expanded (zellij's own fullscreen toggle) when a question comes up.
        if rows <= 2 {
            self.render_summary();
            return;
        }
        // Everything shown is scoped to THIS session. `PaneManifest` only ever holds this
        // session's panes, and `scratchpad relay --json` — the one machine-wide input —
        // is narrowed by the process-tree join: a relay under another session's zellij
        // server reaches no pane here, so it never appears. Counting the leftovers was
        // worse than useless: it put a number on screen that no pane below explained.
        let relayed = self.panes.iter().filter(|p| p.relay.is_some()).count();
        let own = self
            .panes
            .iter()
            .filter(|p| p.relay.is_none() && !p.waiting.is_empty())
            .count();
        let unattended = self.panes.iter().filter(|p| self.is_unattended(p)).count();

        // The counts lead, and the alarming one leads them — the number of agents nobody
        // is listening for is the only figure here that ever needs acting on. It is only
        // painted red when it is not zero, so the colour means something.
        let alarm = if unattended > 0 { RED } else { DIM };
        let mut head = vec![format!(
            "  {BOLD}scratchpad{RESET}  {alarm}✗ {unattended} unattended{RESET} {DIM}·{RESET} {GREEN}● {relayed} relay{RESET} {DIM}·{RESET} {YELLOW}◌ {own} self-waiting{RESET} {DIM}· {} pad{RESET}",
            self.by_pad().len()
        )];
        if self.permission == Some(false) {
            head.push(format!(
                "  {RED}PERMISSION DENIED — the plugin cannot read anything.{RESET}"
            ));
        }
        if self.twins > 0 {
            head.push(format!(
                "  {YELLOW}{} MORE instance(s) of this plugin running{RESET} {DIM}— {}. Close the extras.{RESET}",
                self.twins,
                if self.writer {
                    "this one writes the labels"
                } else {
                    "this one does NOT write labels (yielding to the lower plugin id)"
                }
            ));
        }
        if let Some(err) = &self.error {
            head.push(format!("  {RED}lỗi:{RESET} {}", truncate(err, cols)));
        }

        if self.view == View::Pad {
            let body = self.render_pads();
            paint(rows, cols, &head, &body, &self.render_keys());
            return;
        }

        let body = self.render_panes();
        paint(rows, cols, &head, &body, &self.render_keys());
    }
}

impl State {
    /// Turn the per-pane picture inside out: pad -> everyone listening to it.
    ///
    /// Both halves of the join feed the same map, which is the point — from a pad's side
    /// it does not matter whether someone is reachable through a relay or through their
    /// own `pad wait`, only whether they are reachable at all. The `how` is kept so the
    /// row can still say it.
    fn by_pad(&self) -> BTreeMap<String, Vec<Listener>> {
        let mut pads: BTreeMap<String, Vec<Listener>> = BTreeMap::new();
        for pane in &self.panes {
            let who = self
                .agent_name(pane)
                .unwrap_or_else(|| truncate(&pane.title, 16));
            if let Some(relay) = &pane.relay {
                for w in relay.watching.as_deref().unwrap_or_default() {
                    pads.entry(w.pad_ref.clone()).or_default().push(Listener {
                        author: w.author.clone(),
                        how: Listen::Relay,
                        who: who.clone(),
                        pending: w.pending,
                    });
                }
            }
            for (pad_ref, author) in &pane.waiting {
                pads.entry(pad_ref.clone()).or_default().push(Listener {
                    author: author.clone(),
                    how: Listen::Own,
                    who: who.clone(),
                    pending: false,
                });
            }
        }
        pads
    }

    /// One group per pad: a heading, then a line per listener.
    ///
    /// Grouped rather than tabulated because the two levels answer different questions and
    /// a flat table makes you re-read the pad ref on every row to tell them apart. The
    /// blank line between groups is doing real work — it is what lets the eye count pads
    /// without reading them.
    fn render_pads(&self) -> Vec<String> {
        let mut out = vec![];
        let pads = self.by_pad();
        if pads.is_empty() {
            out.push(format!("  {DIM}No pad has a listener in this session.{RESET}"));
            return out;
        }
        for (pad_ref, listeners) in &pads {
            let info = self.pads.get(pad_ref);
            let lock = if info.is_some_and(|i| i.protected) {
                " 🔒"
            } else {
                ""
            };
            let sections = info
                .and_then(|i| i.sections)
                .map(|n| format!(" {DIM}§{n}{RESET}"))
                .unwrap_or_default();
            // Authors on the pad that nobody in this session is listening for.
            let silent: Vec<&String> = info
                .map(|i| {
                    i.authors
                        .iter()
                        .filter(|a| !listeners.iter().any(|l| &l.author == *a))
                        .collect()
                })
                .unwrap_or_default();
            // Always listening-out-of-total, never a bare count. Printing "(2)" when
            // nobody was missing and "(1/3)" when somebody was meant the same position
            // held two different quantities, and the only way to tell which was to count
            // the rows underneath. The colour carries "somebody is missing"; the number
            // just states the fact.
            //
            // A protected pad gets "(2/?)": the listeners are known, the author list is
            // not, and inventing a total there would claim everyone is present.
            let ratio = match info {
                Some(i) if i.authors.is_empty() => {
                    format!("{DIM}({}/?){RESET}", listeners.len())
                }
                _ => {
                    let total = listeners.len() + silent.len();
                    let colour = if silent.is_empty() { DIM } else { YELLOW };
                    format!("{colour}({}/{total}){RESET}", listeners.len())
                }
            };
            out.push(format!(
                "  {CYAN}{} {BOLD}{pad_ref}{RESET}{lock} {ratio}{sections}",
                self.pad_mark
            ));

            // Who may post next: everyone on the pad except those the turn blocks.
            //
            // Derived from `turn.blocked` — a list of names — rather than from the sentence
            // the text output carries. It is no longer PRINTED as its own line: `⊘` marks
            // the one blocked author and every unmarked row is therefore open, so a turn
            // line would restate what the rows already show, one row further from the
            // names it talks about. The values still decide how a silent author is
            // described below.
            let holders: Vec<&String> = info
                .map(|i| {
                    i.authors
                        .iter()
                        .filter(|a| !i.blocked.contains(a))
                        .collect()
                })
                .unwrap_or_default();

            for l in listeners {
                let (mark, colour, how) = if l.how == Listen::Relay {
                    ("●", GREEN, "relay")
                } else {
                    ("◌", YELLOW, "waiting")
                };
                let bang = if l.pending {
                    format!(" {RED}!{RESET}")
                } else {
                    String::new()
                };
                // Marks whoever just posted. It leads the row rather than trailing it, so
                // the eye finds it by scanning one column instead of reading to the end of
                // every line; rows without it are indented by the same width, which is what
                // keeps the status dots in a straight line.
                let turn = turn_marker(
                    info.is_some_and(|i| i.blocked.contains(&l.author)),
                    DIM,
                    &self.turn_mark,
                );
                out.push(format!(
                    "     {turn}{colour}{mark}{RESET} {:<12} {DIM}{:<7}{RESET} {}{}",
                    truncate(&l.author, 12),
                    how,
                    truncate(&l.who, 18),
                    bang
                ));
            }
            // "Not listening" is said carefully: this plugin only sees one zellij session,
            // so an author could be alive and well in another one. Claiming they are gone
            // would be a stronger statement than the evidence supports.
            for author in silent {
                // An author who MAY speak and is not listening is the worst state a pad
                // can be in: it is open to somebody nobody here can reach. One who just
                // posted and is not listening is ordinary — they said their piece.
                //
                // The difference goes in the text, not the marker column: that column now
                // means one thing only, and overloading it with a second meaning is how a
                // legend stops being readable.
                let holds = holders.iter().any(|h| *h == author);
                let turn = turn_marker(!holds, DIM, &self.turn_mark);
                let note = if holds {
                    format!("{RED}not listening — the pad is open to them{RESET}")
                } else {
                    format!("{DIM}not listening in this session{RESET}")
                };
                out.push(format!(
                    "     {turn}{DIM}○ {:<12} {:<7}{RESET} {note}",
                    truncate(author, 12),
                    "—"
                ));
            }
            out.push(String::new());
        }
        // The trailing blank separates groups; at the end it just wastes a row.
        out.pop();
        out
    }

    /// One group per state, in the order they need attention.
    ///
    /// Grouped rather than tabulated for the same reason the pad view is, plus one the pad
    /// view does not have: a flat list sorted by pane id scatters the panes that need
    /// action among the ones that never will. Here the unattended agents sit together at
    /// the top, and the panes that are none of scratchpad's business sit last — present,
    /// because "did it even see this pane" is a fair question, but out of the way.
    fn render_panes(&self) -> Vec<String> {
        let mut unattended = vec![];
        let mut own = vec![];
        let mut relayed = vec![];
        let mut idle = vec![];

        for pane in &self.panes {
            let name = truncate(&pane.title, 26);
            let pid = pane.pid.map(|p| p.to_string()).unwrap_or_else(|| "?".into());
            match &pane.relay {
                Some(r) => {
                    let watching = r.watching.as_deref().unwrap_or_default();
                    let pads = if watching.is_empty() {
                        format!("{DIM}no pads yet{RESET}")
                    } else {
                        watching
                            .iter()
                            .map(|w| {
                                let bang = if w.pending {
                                    format!("{RED}!{RESET}")
                                } else {
                                    String::new()
                                };
                                format!("{}{bang} {DIM}({}){RESET}", w.pad_ref, w.author)
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    relayed.push(format!(
                        "     {GREEN}●{RESET} {name:<26} {DIM}{:<8}{RESET} {pads}",
                        self.agent_name(pane).unwrap_or_else(|| r.command.clone())
                    ));
                }
                None if !pane.waiting.is_empty() => {
                    let pads = pane
                        .waiting
                        .iter()
                        .map(|(pad_ref, author)| format!("{pad_ref} {DIM}({author}){RESET}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    own.push(format!(
                        "     {YELLOW}◌{RESET} {name:<26} {DIM}{:<8}{RESET} {pads}",
                        self.agent_name(pane).unwrap_or_default()
                    ));
                }
                None if self.is_unattended(pane) => unattended.push(format!(
                    "     {RED}✗{RESET} {name:<26} {DIM}{:<8}{RESET} {RED}nobody listening{RESET} {DIM}pid {pid}{RESET}",
                    self.agent_name(pane).unwrap_or_else(|| "agent".to_owned())
                )),
                None => idle.push(format!("     {DIM}— {name}{RESET}")),
            }
        }

        let mut out = vec![];
        for (title, colour, group) in [
            ("UNATTENDED", RED, unattended),
            ("self-waiting", YELLOW, own),
            ("relay listening", GREEN, relayed),
            ("not using scratchpad", DIM, idle),
        ] {
            if group.is_empty() {
                continue;
            }
            out.push(format!(
                "  {colour}▌{BOLD}{title}{RESET} {DIM}({}){RESET}",
                group.len()
            ));
            out.extend(group);
            out.push(String::new());
        }
        out.pop();
        out
    }

    /// The key legend, pinned to the bottom: one line normally, three on `?`.
    fn render_keys(&self) -> Vec<String> {
        let on = |b: bool| if b { GREEN } else { DIM };
        if self.keys_open {
            vec![
                format!(
                    "  {BOLD}v{RESET} view {CYAN}{}{RESET}  {BOLD}t{RESET} labels {}{}{RESET}  {BOLD}a{RESET} align {CYAN}{}{RESET}  {BOLD}h{RESET} frame colour {}{}{RESET}",
                    if self.view == View::Pad { "pad" } else { "pane" },
                    on(self.labelling),
                    if self.labelling { "on" } else { "off" },
                    if self.align == Align::Right { "right" } else { "left" },
                    on(self.highlighting),
                    if self.highlighting { "on" } else { "off" },
                ),
                format!(
                    "  {GREEN}●{RESET} a relay listens   {YELLOW}◌{RESET} agent waits itself   {RED}✗{RESET} nobody listens   {DIM}⊘{RESET} posted last, blocked   {RED}!{RESET} unread"
                ),
                format!("  {DIM}(listening/authors) · ? unknown total · 🔒 protected · §N sections · ? close{RESET}"),
            ]
        } else {
            vec![format!(
                "  {DIM}{BOLD}v{RESET}{DIM} view · {BOLD}t{RESET}{DIM} labels · {BOLD}a{RESET}{DIM} align · {BOLD}h{RESET}{DIM} frame · {BOLD}?{RESET}{DIM} keys{RESET}"
            )]
        }
    }

    /// One line, for when the plugin lives in a status row.
    ///
    /// It leads with the thing that has to be seen without being looked for: how many
    /// panes are running an agent that nobody is listening for. That number is the reason
    /// this plugin exists — forgetting `scratchpad exec` is invisible until an answer
    /// never arrives.
    fn render_summary(&self) {
        let watched = self.panes.iter().filter(|p| p.relay.is_some()).count();
        let unattended = self
            .panes
            .iter()
            .filter(|p| self.is_unattended(p))
            .count();

        if self.permission == Some(false) {
            print!(" scratchpad: permission not granted");
            return;
        }
        if let Some(err) = &self.error {
            print!(" scratchpad: lỗi — {}", truncate(err, 60));
            return;
        }
        if unattended > 0 {
            print!(" scratchpad ✗ {unattended} agent(s) UNATTENDED · {watched} relayed");
        } else if watched > 0 {
            print!(" scratchpad ● {watched} pane(s) with a relay listening");
        } else {
            print!(" scratchpad — no agents in this session");
        }
    }

    /// Ask for both halves of the join. Two commands, regardless of how many panes exist.
    fn refresh(&self) {
        run_command(&[&self.bin, "relay", "--json"], ctx("relay"));
        // `args=` rather than `comm=`, and the difference is a whole class of answers:
        // `comm` is just the executable name, while the full argv of a live
        // `scratchpad pad wait <ref> … --as <author>` names the pad AND the identity. That
        // is the only way to see an agent that is listening for itself instead of having a
        // relay listen for it — without it, an agent doing exactly what it was told gets
        // flagged as unattended.
        //
        // The trailing `=` on each field suppresses the header, so every line is data and
        // a blank result means "no processes", not "the header scrolled off".
        run_command(&["ps", "-Ao", "pid=,ppid=,args="], ctx("ps"));
    }

    /// Is anyone listening to a pad we know nothing about yet?
    fn has_unknown_pad(&self) -> bool {
        self.by_pad().keys().any(|r| !self.pads.contains_key(r))
    }

    /// Ask scratchpad about each pad someone is listening to.
    ///
    /// One command per pad, so it is deliberately rare: a pad's turn state changes when
    /// somebody posts, which is minutes apart, while the process table changes constantly.
    /// Refs come from the join we already have, so this never enumerates the store — a pad
    /// nobody in this session listens to is none of this plugin's business.
    fn sweep_pads(&mut self) {
        let refs: BTreeSet<String> = self
            .panes
            .iter()
            .flat_map(|p| {
                let watched = p
                    .relay
                    .as_ref()
                    .and_then(|r| r.watching.as_deref())
                    .unwrap_or_default()
                    .iter()
                    .map(|w| w.pad_ref.clone());
                let waited = p.waiting.iter().map(|(r, _)| r.clone());
                watched.chain(waited).collect::<Vec<_>>()
            })
            .collect();
        // Drop anything nobody listens to any more, so a deleted pad cannot linger.
        self.pads.retain(|k, _| refs.contains(k));
        for pad_ref in refs {
            let mut c = ctx("pad");
            c.insert("ref".to_owned(), pad_ref.clone());
            run_command(&[&self.bin, "pad", "get", &pad_ref, "--json"], c);
        }
    }

    fn take_panes(&mut self, manifest: PaneManifest) {
        self.elect_writer(&manifest);
        let mut panes = vec![];
        for (tab, tab_panes) in &manifest.panes {
            for info in tab_panes {
                // Plugin panes have no process of their own to match, and unselectable
                // panes are UI furniture (the status bar, the tab bar).
                if info.is_plugin || !info.is_selectable {
                    continue;
                }
                // Ask the host only for what is not already known. Everything else about
                // the process — its command, its children — comes from the single `ps`
                // call, which costs the same whether there is one pane or twenty.
                let pid = match self.pids.get(&info.id) {
                    Some(&pid) => Some(pid),
                    None => {
                        let pid = get_pane_pid(PaneId::Terminal(info.id)).ok();
                        if let Some(pid) = pid {
                            self.pids.insert(info.id, pid);
                        }
                        pid
                    }
                };
                panes.push(Pane {
                    id: info.id,
                    tab: *tab,
                    raw_title: info.title.clone(),
                    title: strip_badge(&info.title),
                    pid,
                    columns: info.pane_columns,
                    relay: None,
                    hops: 0,
                    waiting: vec![],
                });
            }
        }
        panes.sort_by_key(|p| p.id);
        self.panes = panes;
    }

    fn take_command_result(
        &mut self,
        exit: Option<i32>,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        context: BTreeMap<String, String>,
    ) {
        let which = context.get("q").map(String::as_str).unwrap_or("");

        // A pad that will not open is not an error to report at the top of the screen —
        // it is nearly always a protected pad we have no password for, which is a fact
        // about that pad and belongs on its own row.
        if which == "pad" {
            let Some(pad_ref) = context.get("ref").cloned() else {
                return;
            };
            if exit != Some(0) {
                // Two different failures land here and they must not read the same. A
                // scratchpad without `--json` is the operator's to fix and says so once,
                // at the top; a pad that will not open is a fact about that pad and
                // belongs on its own row.
                let stderr = String::from_utf8_lossy(&stderr);
                if stderr.contains("unknown flag") {
                    self.error = Some(
                        "scratchpad has no `pad get --json` — install a build that does".into(),
                    );
                    return;
                }
                self.pads.insert(
                    pad_ref,
                    // Cannot be read: almost always a protected pad with no password.
                    // `protected` alone drives the display — the lock glyph and the "/?"
                    // total say everything the reader can act on.
                    PadInfo {
                        protected: true,
                        ..Default::default()
                    },
                );
                return;
            }
            match parse_pad_get(&stdout) {
                Some(info) => {
                    self.pads.insert(pad_ref, info);
                    self.error = None;
                }
                None => {
                    self.error = Some(format!("cannot parse `pad get --json` for {pad_ref}"));
                }
            }
            return;
        }

        if exit != Some(0) {
            let msg = String::from_utf8_lossy(&stderr);
            let msg = msg.trim();
            self.error = Some(format!(
                "`{which}` exited with {exit:?}{}",
                if msg.is_empty() {
                    String::new()
                } else {
                    format!(": {msg}")
                }
            ));
            return;
        }
        match which {
            // Option for the same reason as `Relay.watching`: with no relay running at all
            // the whole document is the four bytes `null`, which is a valid answer and not
            // an error.
            "relay" => match serde_json::from_slice::<Option<Vec<Relay>>>(&stdout) {
                Ok(relays) => {
                    self.relays = relays.unwrap_or_default();
                    self.error = None;
                }
                Err(e) => self.error = Some(format!("cannot parse relay --json: {e}")),
            },
            "ps" => {
                self.parents.clear();
                self.children.clear();
                self.commands.clear();
                self.waits.clear();
                self.mcp.clear();
                // Matched against the configured binary's own name, not the literal
                // "scratchpad": this tool is renameable and the plugin is told where it
                // lives, so there is no reason to hardcode what it is called.
                let bin_name = self.bin.rsplit('/').next().unwrap_or(&self.bin).to_owned();
                for line in String::from_utf8_lossy(&stdout).lines() {
                    let mut f = line.split_whitespace();
                    let (Some(pid), Some(ppid)) = (f.next(), f.next()) else {
                        continue;
                    };
                    let (Ok(pid), Ok(ppid)) = (pid.parse::<i32>(), ppid.parse::<i32>()) else {
                        continue;
                    };
                    self.parents.insert(pid, ppid);
                    self.children.entry(ppid).or_default().push(pid);

                    let args: Vec<&str> = f.collect();
                    if let Some(argv0) = args.first() {
                        let name = argv0.rsplit('/').next().unwrap_or(argv0);
                        self.commands.insert(pid, name.to_owned());
                        if name == bin_name && args.contains(&"serve") {
                            self.mcp.insert(pid);
                        }
                    }
                    // Look for an agent name in EVERY argument, skipping flags. `node
                    // /opt/homebrew/bin/gemini` hides the name in argv[1], and so does
                    // every agent shipped as a script.
                    for arg in args.iter().filter(|a| !a.starts_with('-')) {
                        let name = arg.rsplit('/').next().unwrap_or(arg);
                        if self.agents.iter().any(|a| a == name) {
                            self.agent_of.insert(pid, name.to_owned());
                            break;
                        }
                    }
                    if let Some(wait) = parse_wait(&args) {
                        self.waits.insert(pid, wait);
                    }
                }
            }
            _ => {}
        }
    }

    /// Match every pane against every relay by walking the process tree upward.
    fn rejoin(&mut self) {
        let relays = self.relays.clone();
        let parents = std::mem::take(&mut self.parents);
        let waits = std::mem::take(&mut self.waits);
        let children = std::mem::take(&mut self.children);
        for pane in self.panes.iter_mut() {
            pane.relay = None;
            pane.hops = 0;
            pane.waiting.clear();
            let Some(pane_pid) = pane.pid else { continue };
            pane.waiting = find_waits(&children, &waits, pane_pid);
            // Closest match wins: a relay one hop away belongs to this pane more than one
            // five hops away that merely shares an ancestor.
            let mut best: Option<(usize, &Relay)> = None;
            for relay in &relays {
                if let Some(hops) = hops_up_to(&parents, relay.pid, pane_pid) {
                    if best.is_none_or(|(b, _)| hops < b) {
                        best = Some((hops, relay));
                    }
                }
            }
            if let Some((hops, relay)) = best {
                pane.relay = Some(relay.clone());
                pane.hops = hops;
            }
        }
        self.parents = parents;
        self.children = children;
        self.waits = waits;
    }
}

/// Every `pad wait` running somewhere below `root`.
///
/// The whole subtree is walked rather than stopped at the first hit. Duplicates by pad ref
/// are collapsed: re-arming a wait can leave the old process up for a moment, and the same
/// pad listed twice reads as two pads.
fn find_waits(
    children: &BTreeMap<i32, Vec<i32>>,
    waits: &BTreeMap<i32, (String, String)>,
    root: i32,
) -> Vec<(String, String)> {
    let mut found: BTreeMap<String, String> = BTreeMap::new();
    let mut frontier = vec![root];
    for _ in 0..MAX_HOPS {
        let mut next = vec![];
        for pid in frontier {
            if let Some((pad_ref, author)) = waits.get(&pid) {
                found.insert(pad_ref.clone(), author.clone());
            }
            if let Some(kids) = children.get(&pid) {
                next.extend(kids);
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    found.into_iter().collect()
}

impl State {
    /// What this pane's frame should say, or None if it should say nothing.
    ///
    /// A pane with no relay gets a badge ONLY when it is running something in `agents`.
    /// Marking every other pane "no relay" would be true and useless — a shell without a
    /// relay is a shell, and a warning that fires on everything reads as decoration.
    fn badge(&self, pane: &Pane) -> Option<String> {
        match &pane.relay {
            Some(relay) => {
                let watching = relay.watching.as_deref().unwrap_or_default();
                Some(match watching {
                    [] => "[sp ●]".to_owned(),
                    [one] => format!("[sp ● {}]", one.pad_ref),
                    many => format!("[sp ● {} pads]", many.len()),
                })
            }
            // No relay, but the agent armed a wait itself — which is exactly what it is
            // supposed to do when nothing is listening for it. Reporting that as a problem
            // was the plugin's own mistake, and a warning that fires on correct behaviour
            // is worse than no warning: it teaches you to ignore the mark.
            None => match pane.waiting.as_slice() {
                [] if self.looks_like_agent(pane) => Some("[sp ✗ unattended]".to_owned()),
                [] => None,
                [(pad_ref, _)] => Some(format!("[sp ◌ {pad_ref}]")),
                many => Some(format!("[sp ◌ {} pads]", many.len())),
            },
        }
    }

    /// A pane running an agent that neither has a relay nor is waiting for itself. This is
    /// the only state worth an alarm.
    fn is_unattended(&self, pane: &Pane) -> bool {
        pane.relay.is_none() && pane.waiting.is_empty() && self.looks_like_agent(pane)
    }

    /// Decide whether this instance is the one that writes titles.
    ///
    /// Our own url is read from the manifest entry for our own plugin id rather than
    /// configured — nothing has to be told where it was loaded from.
    fn elect_writer(&mut self, manifest: &PaneManifest) {
        let mut mine: Option<&String> = None;
        for tab_panes in manifest.panes.values() {
            for info in tab_panes {
                if info.is_plugin && info.id == self.me {
                    mine = info.plugin_url.as_ref();
                }
            }
        }
        let Some(url) = mine else {
            // We cannot see ourselves yet; assume we are alone rather than going silent,
            // because a plugin that writes nothing is indistinguishable from a broken one.
            self.writer = true;
            self.twins = 0;
            return;
        };
        let siblings: Vec<u32> = manifest
            .panes
            .values()
            .flatten()
            .filter(|p| p.is_plugin && p.plugin_url.as_ref() == Some(url))
            .map(|p| p.id)
            .collect();
        self.twins = siblings.len().saturating_sub(1);
        self.writer = siblings.iter().min() == Some(&self.me);
    }

    /// Does this pane hold something that should have someone listening for it?
    ///
    /// Two kinds of answer, and they are not equal:
    ///
    ///   - EVIDENCE: a live `<bin> serve --stdio` below this pane. That process exists only
    ///     because an agent opened an MCP connection to scratchpad, so it proves the pane
    ///     uses scratchpad no matter what the agent is called.
    ///   - GUESS: argv[0] matches the `agents` name list. Needed because an agent driving
    ///     scratchpad through the CLI spawns no server, and it leaves no lasting trace —
    ///     `pad post` is over in milliseconds, long before the next scan.
    ///
    /// The guess alone was the whole test at first, and it fails the dangerous way round:
    /// an agent whose name is not on the list is invisible, and invisible is exactly the
    /// state this plugin exists to abolish.
    ///
    /// Depth matters for the same reason it does when matching a relay: the pane's own
    /// process is usually a shell, and the agent is its child — or its grandchild once
    /// `scratchpad exec` is in between.
    fn looks_like_agent(&self, pane: &Pane) -> bool {
        self.agent_name(pane).is_some()
    }

    /// The name of the agent this pane is running, if it is running one.
    ///
    /// Doubles as the agent test, so the table can SAY which agent it found rather than
    /// just that it found one — the name is already known by then, and printing "◌ waiting"
    /// while knowing it is gemini was throwing away an answer we already had.
    fn agent_name(&self, pane: &Pane) -> Option<String> {
        let root = pane.pid?;
        let mut frontier = vec![root];
        let mut via_mcp: Option<String> = None;
        for _ in 0..MAX_HOPS {
            let mut next = vec![];
            for pid in frontier {
                // A name from the list is the better answer, so it wins outright.
                if let Some(name) = self.agent_of.get(&pid) {
                    return Some(name.clone());
                }
                // An MCP server proves an agent is there even when its name is not on the
                // list; the process that spawned it IS that agent, so borrow its name.
                if self.mcp.contains(&pid) && via_mcp.is_none() {
                    via_mcp = self
                        .parents
                        .get(&pid)
                        .and_then(|parent| self.commands.get(parent))
                        .cloned()
                        .or_else(|| Some("agent".to_owned()));
                }
                if let Some(kids) = self.children.get(&pid) {
                    next.extend(kids);
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        via_mcp
    }

    /// Bring every pane's title in line with what it should be, and touch nothing else.
    ///
    /// The comparison is against the RAW title rather than a record of what we last wrote.
    /// That is what makes it self-healing: if the agent rewrote its own title in between,
    /// the badge is simply missing from the raw title and gets written again — whereas a
    /// bookkeeping map would say "already done" and the badge would vanish for good.
    fn sync_labels(&mut self) {
        if !self.writer {
            return;
        }
        let mut renames: Vec<(u32, String)> = vec![];
        for pane in &self.panes {
            let wanted = if self.labelling {
                self.badge(pane)
            } else {
                None
            };
            let title = match wanted {
                Some(badge) => {
                    compose_title(&pane.title, &badge, self.align, pane.columns, &self.pad)
                }
                None => pane.title.clone(),
            };
            if title != pane.raw_title {
                renames.push((pane.id, title));
            }
        }
        for (id, title) in renames {
            rename_pane_with_id(PaneId::Terminal(id), &title);
            if let Some(pane) = self.panes.iter_mut().find(|p| p.id == id) {
                // Keep our own view consistent, so the next tick does not re-issue the
                // same rename while waiting for a PaneUpdate to come back.
                pane.raw_title = title;
            }
        }
    }
}

/// Put the badge and the pane's own name together.
///
/// Right alignment pads with spaces rather than positioning anything, because positioning
/// is not on offer. When the name is already too wide to leave room, it falls back to a
/// single space — a badge one column from where you wanted it beats no badge at all.
fn compose_title(name: &str, badge: &str, align: Align, columns: usize, pad: &str) -> String {
    match align {
        Align::Left => format!("{badge} {name}"),
        Align::Right => {
            // The two extra columns are the spaces either side of the dash run, the way
            // zellij itself sets off ` SCROLL: 3/84 ` from the border.
            let used = name.chars().count() + badge.chars().count() + FRAME_OVERHEAD + 2;
            match columns.saturating_sub(used) {
                0 => format!("{name} {badge}"),
                dashes => format!("{name} {} {badge}", pad.repeat(dashes)),
            }
        }
    }
}

impl State {
    /// Colour the frame of every pane running an agent with nobody listening for it, and
    /// only those.
    ///
    /// Only the DIFFERENCE is sent. Each call makes the server re-render the screen, so
    /// re-asserting the same set every few seconds would repaint the session forever — the
    /// same shape of mistake as re-asking for a pid that cannot have changed.
    /// Append each tab's own trouble count to its name.
    ///
    /// Per tab, not per session: `PaneManifest` is keyed by tab position, so a tab can
    /// report only the unattended agents it actually holds. A session-wide number pinned
    /// to every tab would send you to the wrong one.
    ///
    /// Only trouble is written. A tab with nothing wrong keeps its name untouched, because
    /// this is a warning and a warning that is always present is wallpaper. It is also the
    /// least invasive thing to do to a name the user chose.
    fn sync_tabs(&mut self) {
        if !self.writer {
            return;
        }
        let mut renames: Vec<(usize, String)> = vec![];
        for (id, position, clean) in &self.tabs {
            let trouble = self
                .panes
                .iter()
                .filter(|p| p.tab == *position && self.is_unattended(p))
                .count();
            let wanted = if self.tab_label && trouble > 0 {
                format!("{clean}{TAB_MARK}{trouble}")
            } else {
                clean.clone()
            };
            // `self.tabs` already holds the STRIPPED name, so compare against what zellij
            // would show. Nothing is stored about what we last wrote: the mark is cut off
            // whatever the name says now, exactly like the pane badge.
            renames.push((*id, wanted));
        }
        for (id, name) in renames {
            // `TabInfo.tab_id` is usize; the rename call takes u64.
            rename_tab_with_id(id as u64, &name);
        }
    }

    fn sync_highlights(&mut self) {
        if !self.writer {
            return;
        }
        let want: BTreeSet<u32> = if self.highlighting {
            self.panes
                .iter()
                .filter(|p| self.is_unattended(p))
                .map(|p| p.id)
                .collect()
        } else {
            BTreeSet::new()
        };
        if want == self.highlighted {
            return;
        }
        let on: Vec<PaneId> = want
            .difference(&self.highlighted)
            .map(|id| PaneId::Terminal(*id))
            .collect();
        let off: Vec<PaneId> = self
            .highlighted
            .difference(&want)
            .map(|id| PaneId::Terminal(*id))
            .collect();
        highlight_and_unhighlight_panes(on, off);
        self.highlighted = want;
    }
}

/// Remove a tab mark this plugin wrote, leaving the name the user gave the tab.
fn strip_tab_mark(name: &str) -> String {
    match name.rfind(TAB_MARK) {
        Some(at) if name[at + TAB_MARK.len()..].chars().all(|c| c.is_ascii_digit()) => {
            name[..at].to_owned()
        }
        _ => name.to_owned(),
    }
}

/// Remove a badge this plugin wrote, leaving the title the pane would otherwise have.
///
/// Handles both alignments, because the badge has to come off even if the setting changed
/// since it went on — otherwise switching from right to left leaves the old one stranded
/// and the pane collects a badge per switch.
fn strip_badge(title: &str) -> String {
    // Unconditional, and it is the repair for a real failure: padding has been seen to
    // survive when the badge did not. The mark is then gone, nothing recognises the run as
    // ours, and it becomes part of the pane's "original" name — growing by one run every
    // time the badge is rewritten. Neither trailing spaces nor trailing border glyphs ever
    // carry meaning in a pane name, so drop them always.
    let title = title.trim_end_matches(|c: char| c == ' ' || c == '─' || c == '-');
    if title.ends_with(']') {
        if let Some(at) = title.rfind(BADGE) {
            return title[..at]
                .trim_end_matches(|c: char| c == ' ' || c == '─' || c == '-')
                .to_owned();
        }
    }
    if title.starts_with(BADGE) {
        if let Some(close) = title.find("] ") {
            return title[close + 2..].to_owned();
        }
    }
    title.to_owned()
}

/// The leading marker for the author who just posted, or the blank that keeps the column
/// straight.
///
/// It marks the BLOCKED author, not the ones who may speak, and that is the whole design:
/// the turn rule blocks exactly one author, so on a pad of n agents, n-1 of them hold the
/// turn. Marking those meant n-1 markers pointing at the ordinary case, and the single
/// interesting row — who just spoke — was the one with nothing on it.
///
/// `⊘` (U+2298) rather than an arrow: an arrow says "look here, act", which is wrong for a
/// row that needs nothing. It is also East Asian Width N and absent from Unicode's emoji
/// data, unlike `▶` U+25B6 and `◀` U+25C0, which some terminals draw two cells wide and
/// would push the status dot out of line.
///
/// Configurable, with one requirement: whatever you put here must occupy one cell.
fn turn_marker(holds: bool, colour: &str, mark: &str) -> String {
    if holds {
        format!("{colour}{mark}{RESET} ")
    } else {
        " ".repeat(mark.chars().count() + 1)
    }
}

/// Lay the panel out as three fixed blocks: header, body, keys pinned to the bottom.
///
/// The rules come from the pane's height rather than from the content, which is what makes
/// the footer stay put while rows come and go above it — a legend that drifts up and down
/// with the data has to be found again every time you look.
///
/// The last line is written WITHOUT a newline. One `println!` too many scrolls the whole
/// panel by a row and the header walks off the top.
fn paint(rows: usize, cols: usize, head: &[String], body: &[String], foot: &[String]) {
    let rule = format!("{DIM}{}{RESET}", "─".repeat(cols.min(400)));
    let overhead = head.len() + foot.len() + 2; // 2 rules
    let room = rows.saturating_sub(overhead);

    let mut lines: Vec<String> = Vec::with_capacity(rows);
    lines.extend_from_slice(head);
    lines.push(rule.clone());
    for line in body.iter().take(room) {
        lines.push(line.clone());
    }
    // Push the footer down to the floor.
    while lines.len() + foot.len() + 1 < rows {
        lines.push(String::new());
    }
    lines.push(rule);
    lines.extend_from_slice(foot);

    let last = lines.len().saturating_sub(1);
    for (i, line) in lines.iter().enumerate() {
        if i == last {
            print!("{line}");
        } else {
            println!("{line}");
        }
    }
}

/// Read the `key: value` header `pad get` prints before its table of contents.
///
/// Only the header is parsed, and on purpose: the table below it has optional columns
/// (`to`, `re`, `task`) that are blank more often than not, so a section's title cannot be
/// located by counting fields. Guessing it wrong would put another agent's words on screen
/// under the wrong pad, which is worse than not showing a title at all.
fn parse_pad_get(out: &[u8]) -> Option<PadInfo> {
    let doc: PadGetJson = serde_json::from_slice(out).ok()?;
    let turn = doc.turn.unwrap_or_default();
    Some(PadInfo {
        sections: doc.section_count,
        blocked: turn.blocked.unwrap_or_default(),
        authors: doc.authors.unwrap_or_default(),
        protected: doc.protected.unwrap_or(false),
    })
}

/// Pull the pad ref and the author out of a live `scratchpad pad wait …` command line.
///
/// Matched positionally on `pad wait` rather than on the binary's name, because the same
/// command shows up wrapped in a shell (`zsh -c '… scratchpad pad wait …'`) and under an
/// absolute path. What must be exact is the pair that follows: the ref is the word after
/// `wait`, the author the word after `--as`. A line missing either is not reported at all
/// — a half-known wait would show up as a pad nobody can be matched to.
fn parse_wait(args: &[&str]) -> Option<(String, String)> {
    let at = args.windows(2).position(|w| w == ["pad", "wait"])?;
    let pad_ref = args.get(at + 2)?;
    if pad_ref.starts_with('-') {
        return None;
    }
    let as_at = args.iter().position(|a| *a == "--as")?;
    let author = args.get(as_at + 1)?;
    Some((pad_ref.to_string(), author.to_string()))
}

/// How many parent links separate `from` and `target`, or None if they are unrelated.
///
/// Stops at pid 1: everything on the machine descends from init, so continuing past it
/// would match every pane against every relay.
fn hops_up_to(parents: &BTreeMap<i32, i32>, from: i32, target: i32) -> Option<usize> {
    let mut pid = from;
    for hop in 0..MAX_HOPS {
        if pid == target {
            return Some(hop);
        }
        match parents.get(&pid) {
            Some(&parent) if parent > 1 => pid = parent,
            _ => return None,
        }
    }
    None
}

fn ctx(which: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("q".to_owned(), which.to_owned())])
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}
