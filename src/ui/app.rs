//! The chat application: state, event loop, input editing and agent plumbing.
//!
//! This file owns *what the app is doing*. Two sibling modules extend `App`:
//! - [`super::view`] draws it (header, chat, input box, status bar, popups)
//! - [`super::actions`] executes slash commands
//!
//! The loop is deliberately cheap: it redraws only when something changed or
//! an animation is visible, and chat lines are cached per entry so a long
//! conversation costs nothing while idle.

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use ratatui::text::Line;
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

use crate::agent::tools::ConfirmRequest;
use crate::agent::types::Message;
use crate::agent::claude_code::{self, AuthStatus};
use crate::agent::{build_claude_context, build_system_prompt, Agent, AgentEvent};
use crate::commands::{self, CmdDef};
use crate::config::{self, Config};
use crate::mcp::McpManager;
use crate::notify;
use crate::sessions::Session;
use crate::skills::Skill;
use crate::sysinfo::SystemPoller;
use crate::theme::Theme;
use crate::ui::wizards::EnvWizard;

// ------------------------------------------------------------------- types

/// One block in the chat transcript.
pub struct Entry {
    pub kind: EntryKind,
    pub text: String,
    /// Wrapped lines for a given width; rebuilt only when text/width/theme change.
    pub(super) cache: Option<(u16, Vec<Line<'static>>)>,
}

impl Entry {
    pub fn new(kind: EntryKind, text: impl Into<String>) -> Self {
        Self { kind, text: text.into(), cache: None }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum EntryKind {
    User,
    Assistant,
    /// A tool call; `text` holds its one-line preview.
    Tool { name: String, state: ToolState },
    Info,
    Success,
    Warn,
    Error,
    Reminder,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ToolState {
    Running,
    Ok,
    Failed,
}

/// A row in the `/loadsession` picker.
pub(super) struct SessionItem {
    pub id: String,
    pub title: String,
    pub when: String,
    /// "beyond" or "claude" (imported from Claude Code).
    pub source: &'static str,
}

/// The overlay currently on screen (at most one).
pub(super) enum Popup {
    None,
    /// Slash-command suggestions above the input box.
    Autocomplete { items: Vec<&'static CmdDef>, sel: usize },
    /// `/theme` picker; `original` is restored on Esc (navigation previews live).
    ThemePicker { items: Vec<(String, String)>, sel: usize, original: Box<Theme> },
    SessionPicker { items: Vec<SessionItem>, sel: usize },
    /// A tool is waiting for permission.
    Confirm { title: String, detail: String, respond: oneshot::Sender<bool> },
    /// "Close Beyond?" — shown before Ctrl+C / Ctrl+D actually exits.
    QuitConfirm,
    /// A short list of (id, description) to pick one of: models, modes.
    ChoicePicker { kind: Choice, items: Vec<(String, String)>, sel: usize },
}

/// What a [`Popup::ChoicePicker`] is choosing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Choice {
    Model,
    Mode,
}

/// The input box is temporarily collecting something other than a message.
pub(super) enum Prompt {
    /// Hidden API-key entry; `limited` = expire the login after 48 h.
    ApiKey { limited: bool },
}

/// A running agent turn.
pub struct AgentHandle {
    pub rx: mpsc::UnboundedReceiver<AgentEvent>,
    pub interrupt: Arc<AtomicBool>,
}

/// How long a `/limitedlogin` lasts.
pub(super) const LIMITED_LOGIN_SECS: u64 = 48 * 60 * 60;

// ------------------------------------------------------------------- state

/// The whole running application.
pub struct App {
    pub(super) cfg: Config,
    pub(super) theme: Theme,

    // conversation
    pub(super) messages: Vec<Message>,
    pub(super) entries: Vec<Entry>,
    pub(super) session: Session,
    pub(super) session_tokens: u64,

    // input box
    pub(super) input: String,
    /// Byte offset into `input` (always on a char boundary).
    pub(super) cursor: usize,
    history: Vec<String>,
    hist_idx: Option<usize>,
    pub(super) popup: Popup,
    pub(super) prompt: Option<Prompt>,

    /// Lines scrolled up from the bottom (0 = follow the newest output).
    pub(super) scroll: usize,

    // agent turn
    pub agent: Option<AgentHandle>,
    stream_entry: Option<usize>,
    pub(super) activity: String,
    pub(super) turn_started: Instant,
    interrupted: bool,
    /// Messages typed while a turn was running; sent when it finishes.
    pub(super) queued: VecDeque<String>,
    /// Permission titles the user answered "always" to (this run only).
    always_allow: HashSet<String>,
    confirm_rx: mpsc::UnboundedReceiver<ConfirmRequest>,
    confirm_tx: mpsc::UnboundedSender<ConfirmRequest>,

    // environment
    pub(super) skills: Vec<Skill>,
    pub(super) mcps: Arc<McpManager>,
    pub(super) sys: SystemPoller,
    pub(super) project: Option<PathBuf>,
    env_suggested: bool,
    pub(super) wizard: Option<EnvWizard>,

    // background feeds
    reminder_rx: mpsc::UnboundedReceiver<notify::Reminder>,
    pub(super) listening: bool,
    /// The running dictation session, if any (dropping it stops the mic).
    voice: Option<crate::voice::Session>,
    voice_rx: Option<mpsc::UnboundedReceiver<crate::voice::Event>>,
    /// Did the current session recognize anything yet?
    voice_heard: bool,

    /// Seconds of animation time (frozen when animations are off).
    pub(super) frame_t: f32,
    pub(super) dirty: bool,
    /// Repaint every cell on the next draw (the window was resized, so what
    /// the terminal shows no longer matches what we last drew).
    pub(super) full_redraw: bool,
    pub(super) quit: bool,

    // Claude Code backend
    /// The installed `claude` launcher, if there is one.
    pub(super) claude_cli: Option<std::path::PathBuf>,
    /// Its sign-in state (`None` until the first check comes back).
    pub(super) claude_auth: Option<AuthStatus>,
    claude_tx: std::sync::mpsc::Sender<AuthStatus>,
    claude_rx: std::sync::mpsc::Receiver<AuthStatus>,
    /// What this session cost at API prices.
    pub(super) session_cost: f64,
}

impl App {
    pub fn new(
        cfg: Config,
        theme: Theme,
        skills: Vec<Skill>,
        mcps: Arc<McpManager>,
        reminder_rx: mpsc::UnboundedReceiver<notify::Reminder>,
    ) -> Self {
        let (confirm_tx, confirm_rx) = mpsc::unbounded_channel();
        let (claude_tx, claude_rx) = std::sync::mpsc::channel();
        let session = Session::new(&cfg.model);
        // Launching inside a folder means "work here" — except from the home
        // directory (Start-menu launches), where nothing is tagged yet.
        let project = std::env::current_dir().ok().filter(|p| Some(p) != dirs::home_dir().as_ref());

        let mut app = Self {
            cfg,
            theme,
            messages: Vec::new(),
            entries: Vec::new(),
            session,
            session_tokens: 0,
            input: String::new(),
            cursor: 0,
            history: Vec::new(),
            hist_idx: None,
            popup: Popup::None,
            prompt: None,
            scroll: 0,
            agent: None,
            stream_entry: None,
            activity: String::new(),
            turn_started: Instant::now(),
            interrupted: false,
            queued: VecDeque::new(),
            always_allow: HashSet::new(),
            confirm_rx,
            confirm_tx,
            skills,
            mcps,
            sys: SystemPoller::new(),
            project,
            env_suggested: false,
            wizard: None,
            reminder_rx,
            listening: false,
            voice: None,
            voice_rx: None,
            voice_heard: false,
            frame_t: 0.0,
            dirty: true,
            full_redraw: true,
            quit: false,
            // tests never look for (or run) a real Claude Code install
            claude_cli: if cfg!(test) { None } else { claude_code::find_cli() },
            claude_auth: None,
            claude_tx,
            claude_rx,
            session_cost: 0.0,
        };
        app.check_claude_auth(1);
        app.session.cwd = app.project.as_ref().map(|p| p.to_string_lossy().to_string());
        app.enforce_login_expiry();
        for w in app.mcps.warnings.clone() {
            app.push(EntryKind::Warn, format!("MCP: {w}"));
        }
        app
    }

    // ------------------------------------------------------------ helpers

    /// Append a transcript entry and jump back to the bottom.
    /// The welcome screen stays up until the conversation really starts: a
    /// couple of one-line notices ("model set to…") don't replace it.
    pub(super) fn show_welcome(&self) -> bool {
        self.entries.len() <= 3
            && self.entries.iter().all(|e| {
                matches!(e.kind, EntryKind::Info | EntryKind::Success | EntryKind::Warn | EntryKind::Error)
                    && e.text.len() <= 160
                    && !e.text.contains('\n')
            })
    }

    pub(super) fn push(&mut self, kind: EntryKind, text: impl Into<String>) {
        self.entries.push(Entry::new(kind, text));
        self.scroll = 0;
        self.dirty = true;
    }

    /// Change the config in memory *and* on disk. Goes through
    /// `config::update` so background writes (token refresh) aren't clobbered.
    pub(super) fn persist(&mut self, f: impl Fn(&mut Config)) {
        f(&mut self.cfg);
        if let Err(e) = config::update(&f) {
            self.push(EntryKind::Warn, format!("could not save config: {e:#}"));
        }
    }

    /// Drop cached chat lines (after a theme change).
    pub(super) fn restyle(&mut self) {
        for e in &mut self.entries {
            e.cache = None;
        }
        self.dirty = true;
    }

    /// Wipe credentials once a `/limitedlogin` window has passed.
    fn enforce_login_expiry(&mut self) {
        if self.cfg.auth_expired() {
            self.persist(|c| c.clear_auth());
            self.push(EntryKind::Warn, "your 48-hour login expired — run `/login` or `/limitedlogin` to sign in again");
        }
    }

    // ---------------------------------------------------------- main loop

    pub fn run(&mut self, term: &mut crate::ui::Tui) -> Result<()> {
        let mut last_tick = Instant::now();
        let mut last_draw: Option<Instant> = None;

        // Windows Terminal takes over and resizes the window a moment after
        // launch, sometimes without telling us. Repaint from scratch a few
        // times early on so a shifted screen never sticks around.
        let started = Instant::now();
        let mut settle = [400u64, 1200, 2500].into_iter().map(Duration::from_millis).peekable();

        while !self.quit {
            let now = Instant::now();
            if settle.next_if(|at| started.elapsed() >= *at).is_some() {
                self.full_redraw = true;
            }
            let repaint = std::mem::take(&mut self.full_redraw);
            self.dirty |= repaint;
            if self.cfg.animations {
                self.frame_t += now.duration_since(last_tick).as_secs_f32().min(0.25);
            }
            last_tick = now;

            self.sys.tick();
            self.drain_agent();
            self.drain_claude_auth();
            self.drain_confirm();
            self.drain_reminders();
            self.drain_voice();

            // ~15 fps while something moves, 1 fps otherwise (clock, meters).
            let animating = self.is_animating();
            let frame = if animating { Duration::from_millis(66) } else { Duration::from_secs(1) };
            if self.dirty || last_draw.is_none_or(|t| t.elapsed() >= frame) {
                // One atomic update per frame: the terminal shows the finished
                // frame only, so a full repaint never flashes. Terminals that
                // don't know the sequence ignore it.
                let _ = crossterm::execute!(std::io::stdout(), BeginSynchronizedUpdate);
                let drawn = (|| -> std::io::Result<()> {
                    if repaint {
                        term.clear()?;
                    }
                    term.draw(|f| self.draw(f))?;
                    Ok(())
                })();
                let _ = crossterm::execute!(std::io::stdout(), EndSynchronizedUpdate);
                drawn?;
                self.dirty = false;
                last_draw = Some(Instant::now());
            }

            let busy = animating || self.agent.is_some();
            let wait = Duration::from_millis(if busy { 33 } else { 120 });
            if event::poll(wait)? {
                // drain everything queued so a burst (paste, key repeat)
                // costs one redraw instead of one per event
                loop {
                    self.on_event(event::read()?);
                    if !event::poll(Duration::ZERO)? {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    /// Is anything on screen moving right now?
    fn is_animating(&self) -> bool {
        self.wizard.is_some()
            || (self.cfg.animations && (self.agent.is_some() || self.show_welcome() || self.listening))
    }

    fn on_event(&mut self, ev: Event) {
        match ev {
            // Windows reports key releases too; only act on press/repeat.
            Event::Key(k) if k.kind != KeyEventKind::Release => self.on_key(k),
            Event::Paste(s) => {
                if self.wizard.is_none() && !matches!(self.popup, Popup::Confirm { .. }) {
                    self.insert_str(&s.replace("\r\n", "\n").replace('\r', "\n"));
                }
            }
            Event::Mouse(m) => match m.kind {
                MouseEventKind::ScrollUp => self.scroll += 3,
                MouseEventKind::ScrollDown => self.scroll = self.scroll.saturating_sub(3),
                _ => return,
            },
            Event::Resize(..) => self.full_redraw = true,
            _ => {}
        }
        self.dirty = true;
    }

    // ------------------------------------------------------- agent plumbing

    /// Handle a submitted chat message (not a slash command).
    pub(super) fn send_message(&mut self, text: String) {
        self.enforce_login_expiry();
        if !self.signed_in() {
            self.push(EntryKind::Error, "not signed in — run `/login`");
            self.input = text;
            self.cursor = self.input.len();
            return;
        }
        if self.agent.is_some() {
            self.queued.push_back(text);
            self.dirty = true;
            return;
        }
        self.maybe_suggest_env(&text);
        self.push(EntryKind::User, text.clone());
        self.messages.push(Message::user_text(text));
        self.start_turn();
    }

    /// Spawn the agent loop for the current conversation.
    fn start_turn(&mut self) {
        // pick up tokens refreshed by an earlier turn
        if let Some(live) = config::snapshot() {
            self.cfg.oauth = live.oauth;
        }
        let interrupt = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::unbounded_channel();
        if self.uses_claude() {
            self.spawn_claude_turn(tx, interrupt.clone());
        } else {
            self.spawn_api_turn(tx, interrupt.clone());
        }
        self.agent = Some(AgentHandle { rx, interrupt });
        self.stream_entry = None;
        self.interrupted = false;
        self.activity = "thinking".into();
        self.turn_started = Instant::now();
        self.dirty = true;
    }

    /// Run the turn through the installed Claude Code CLI (its own sign-in,
    /// tools and billing).
    fn spawn_claude_turn(&mut self, tx: mpsc::UnboundedSender<AgentEvent>, interrupt: Arc<AtomicBool>) {
        let Some(cli) = self.claude_cli.clone() else { return };
        // Claude Code keeps the conversation itself; it only needs the newest message.
        let prompt = self
            .messages
            .iter()
            .rev()
            .find(|m| m.role == crate::agent::types::Role::User)
            .map(|m| m.plain_text())
            .unwrap_or_default();
        let opts = claude_code::TurnOpts {
            cli,
            prompt,
            model: self.cfg.model.clone(),
            resume: self.session.claude_session.clone(),
            cwd: self.project.clone(),
            permission_mode: self.cfg.permission_mode.clone(),
            system_append: build_claude_context(&self.cfg, &self.skills, self.project.as_deref()),
        };
        let mut messages = self.messages.clone();
        tokio::spawn(async move {
            let out = claude_code::run_turn(opts, tx.clone(), interrupt).await;
            if !out.text.trim().is_empty() {
                messages.push(Message::assistant_text(out.text));
            }
            let _ = tx.send(AgentEvent::TurnComplete {
                messages,
                usage: out.usage,
                cost_usd: out.cost_usd,
                claude_session: out.session_id,
            });
        });
    }

    /// Run the turn with Beyond's own agent loop against the API (stored key).
    fn spawn_api_turn(&mut self, tx: mpsc::UnboundedSender<AgentEvent>, interrupt: Arc<AtomicBool>) {
        let agent = Agent::new(
            &self.cfg,
            self.project.clone(),
            self.mcps.clone(),
            self.confirm_tx.clone(),
            interrupt.clone(),
        );
        let system = build_system_prompt(&self.cfg, &self.skills, &self.mcps, self.project.as_deref());
        let messages = self.messages.clone();
        let flag = interrupt.clone();
        tokio::spawn(async move {
            let (messages, usage) = agent.run_turn(&system, messages, tx.clone(), flag).await;
            let _ = tx.send(AgentEvent::TurnComplete { messages, usage, cost_usd: 0.0, claude_session: None });
        });
    }

    // ------------------------------------------------------ Claude Code state

    /// Will the next message run through Claude Code (rather than the API key)?
    pub(super) fn uses_claude(&self) -> bool {
        let signed_in = self.claude_auth.as_ref().is_some_and(|a| a.logged_in);
        match self.cfg.backend {
            config::Backend::Api => false,
            config::Backend::Claude => self.claude_cli.is_some(),
            // prefer Claude Code when it is signed in, or when there is no key anyway
            config::Backend::Auto => self.claude_cli.is_some() && (signed_in || !self.cfg.has_auth()),
        }
    }

    /// Is there any way to answer a message right now?
    pub(super) fn signed_in(&self) -> bool {
        if self.uses_claude() {
            // unknown yet counts as yes: the run itself reports a missing login
            self.claude_auth.as_ref().is_none_or(|a| a.logged_in)
        } else {
            self.cfg.has_auth()
        }
    }

    /// Ask Claude Code for its sign-in state off the UI thread. With
    /// `attempts > 1` it keeps asking every few seconds until it is signed in
    /// (used while the user completes `/login` in the other window).
    pub(super) fn check_claude_auth(&self, attempts: u32) {
        let Some(cli) = self.claude_cli.clone() else { return };
        let tx = self.claude_tx.clone();
        std::thread::spawn(move || {
            for i in 0..attempts {
                if i > 0 {
                    std::thread::sleep(Duration::from_secs(3));
                }
                let Some(status) = claude_code::auth_status(&cli) else { continue };
                let done = status.logged_in;
                if tx.send(status).is_err() || done {
                    break;
                }
            }
        });
    }

    fn drain_claude_auth(&mut self) {
        while let Ok(status) = self.claude_rx.try_recv() {
            let was_in = self.claude_auth.as_ref().map(|a| a.logged_in);
            if status.logged_in && was_in == Some(false) {
                self.push(EntryKind::Success, "signed in through Claude Code — you're ready to go");
            }
            self.claude_auth = Some(status);
            self.dirty = true;
        }
    }

    // ------------------------------------------------------------ turn events

    /// Ask the running turn to stop (it finishes the current step first).
    fn interrupt(&mut self) {
        if let Some(a) = &self.agent {
            a.interrupt.store(true, Ordering::Relaxed);
            self.interrupted = true;
            self.activity = "stopping".into();
            self.queued.clear();
        }
        // a pending permission question is answered "no"
        if let Popup::Confirm { respond, .. } = std::mem::replace(&mut self.popup, Popup::None) {
            let _ = respond.send(false);
        }
    }

    fn drain_agent(&mut self) {
        let mut events = Vec::new();
        let mut disconnected = false;
        if let Some(agent) = self.agent.as_mut() {
            loop {
                match agent.rx.try_recv() {
                    Ok(ev) => events.push(ev),
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        if !events.is_empty() {
            self.dirty = true;
        }
        for ev in events {
            self.apply_agent_event(ev);
        }
        // task died without a TurnComplete (panic): don't stay "busy" forever
        if disconnected && self.agent.is_some() {
            self.agent = None;
            self.push(EntryKind::Error, "the agent task stopped unexpectedly");
        }
    }

    fn apply_agent_event(&mut self, ev: AgentEvent) {
        match ev {
            AgentEvent::TextDelta(t) => {
                let idx = match self.stream_entry {
                    Some(i) => i,
                    None => {
                        self.entries.push(Entry::new(EntryKind::Assistant, ""));
                        self.stream_entry = Some(self.entries.len() - 1);
                        self.entries.len() - 1
                    }
                };
                let e = &mut self.entries[idx];
                e.text.push_str(&t);
                e.cache = None;
                self.activity = "writing".into();
            }
            AgentEvent::ToolPending { name } => {
                self.stream_entry = None;
                self.activity = format!("preparing {name}");
            }
            AgentEvent::ToolStart { name, preview } => {
                self.stream_entry = None;
                self.activity = format!("running {name}");
                self.entries.push(Entry::new(
                    EntryKind::Tool { name, state: ToolState::Running },
                    preview,
                ));
            }
            AgentEvent::ToolDone { name, ok } => {
                let done = if ok { ToolState::Ok } else { ToolState::Failed };
                if let Some(e) = self.entries.iter_mut().rev().find(|e| {
                    matches!(&e.kind, EntryKind::Tool { name: n, state: ToolState::Running } if *n == name)
                }) {
                    e.kind = EntryKind::Tool { name, state: done };
                    e.cache = None;
                }
                self.activity = "thinking".into();
            }
            AgentEvent::Failed(e) => self.push(EntryKind::Error, e),
            AgentEvent::TurnComplete { messages, usage, cost_usd, claude_session } => {
                self.agent = None;
                self.stream_entry = None;
                self.messages = messages;

                let tokens = usage.input_tokens + usage.output_tokens;
                self.session_tokens += tokens;
                self.session_cost += cost_usd;
                if tokens > 0 || cost_usd > 0.0 {
                    self.persist(move |c| c.usage.add_turn(tokens, cost_usd));
                }
                if claude_session.is_some() {
                    self.session.claude_session = claude_session;
                }

                self.session.messages = self.messages.clone();
                self.session.refresh_title();
                if let Err(e) = self.session.save() {
                    self.push(EntryKind::Warn, format!("could not save session: {e:#}"));
                }

                // anything still marked running was cut short
                for e in &mut self.entries {
                    if let EntryKind::Tool { state: s @ ToolState::Running, .. } = &mut e.kind {
                        *s = ToolState::Failed;
                        e.cache = None;
                    }
                }
                if self.interrupted {
                    self.push(EntryKind::Info, "interrupted");
                }
                if let Some(next) = self.queued.pop_front() {
                    self.send_message(next);
                }
            }
        }
    }

    /// Show the next permission request (one at a time; the rest wait).
    fn drain_confirm(&mut self) {
        if !matches!(self.popup, Popup::None | Popup::Autocomplete { .. }) || self.wizard.is_some() {
            return;
        }
        while let Ok(req) = self.confirm_rx.try_recv() {
            if self.agent.is_none() {
                let _ = req.respond.send(false); // stale request from a finished turn
            } else if self.always_allow.contains(&req.title) {
                let _ = req.respond.send(true);
            } else {
                self.popup = Popup::Confirm { title: req.title, detail: req.detail, respond: req.respond };
                self.dirty = true;
                return;
            }
        }
    }

    /// Reminders that came due: show them and offer the task as the next message.
    fn drain_reminders(&mut self) {
        while let Ok(r) = self.reminder_rx.try_recv() {
            self.push(EntryKind::Reminder, format!("reminder: {}", r.task));
            if self.input.is_empty() && self.prompt.is_none() {
                self.input = r.task;
                self.cursor = self.input.len();
            }
        }
    }

    /// Start dictating, or stop if already listening (F9 or `/voice`).
    pub(super) fn start_voice(&mut self) {
        self.dirty = true;
        if let Some(session) = &self.voice {
            session.stop(); // the reader thread then reports `Done`
            return;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        match crate::voice::start(tx) {
            Ok(session) => {
                self.voice = Some(session);
                self.voice_rx = Some(rx);
                self.voice_heard = false;
                self.listening = true;
            }
            Err(e) => self.push(EntryKind::Warn, format!("voice: {e}")),
        }
    }

    /// Move recognized phrases into the input box as they arrive.
    fn drain_voice(&mut self) {
        use crate::voice::{Event, Failure};
        loop {
            let Some(rx) = self.voice_rx.as_mut() else { return };
            let event = match rx.try_recv() {
                Ok(event) => event,
                Err(mpsc::error::TryRecvError::Empty) => return,
                Err(mpsc::error::TryRecvError::Disconnected) => Event::Done(Ok(())),
            };
            self.dirty = true;
            match event {
                Event::Phrase(text) => {
                    if !self.input.is_empty() && !self.input.ends_with([' ', '\n']) {
                        self.insert_str(" ");
                    }
                    self.insert_str(&text);
                    self.voice_heard = true;
                }
                Event::Done(outcome) => {
                    self.voice = None;
                    self.voice_rx = None;
                    self.listening = false;
                    match outcome {
                        Ok(()) if !self.voice_heard => {
                            self.push(EntryKind::Warn, "voice: didn't catch anything — press F9 to try again")
                        }
                        Ok(()) => {}
                        Err(Failure::NoRecognizer) => {
                            // no built-in recognizer here: hand over to Windows'
                            // own voice typing, which types into this window
                            crate::win::open_voice_typing();
                            self.push(
                                EntryKind::Info,
                                "voice: opened Windows voice typing (Win+H) — speak, and it types here",
                            );
                        }
                        Err(Failure::Other(e)) => self.push(EntryKind::Warn, format!("voice: {e}")),
                    }
                    return;
                }
            }
        }
    }

    /// Nudge towards `/env` once, when a message names several project folders.
    fn maybe_suggest_env(&mut self, text: &str) {
        if self.env_suggested || self.cfg.env.last_setup.is_some() {
            return;
        }
        let dirs = text
            .split_whitespace()
            .map(|w| w.trim_matches(|c: char| "\"'`,;()[]".contains(c)))
            .filter(|w| w.len() > 3)
            .map(std::path::Path::new)
            .filter(|p| p.is_absolute() && p.is_dir())
            .count();
        if dirs >= 2 {
            self.env_suggested = true;
            self.push(
                EntryKind::Info,
                "you mentioned several project folders — `/env` can detect and install the toolchains they need",
            );
        }
    }

    // ---------------------------------------------------------- keyboard

    fn on_key(&mut self, key: KeyEvent) {
        // 1. the /env wizard owns the keyboard while it is open
        if let Some(w) = self.wizard.as_mut() {
            if w.on_key(key) {
                let result = w.take_result();
                self.wizard = None;
                if let Some(profile) = result {
                    let n = profile.repos.len();
                    self.persist(move |c| c.env = profile.clone());
                    self.push(EntryKind::Success, format!("environment saved — {n} repo(s) tracked"));
                }
            }
            return;
        }

        // 2. a permission question blocks everything else
        if matches!(self.popup, Popup::Confirm { .. }) {
            self.on_confirm_key(key);
            return;
        }

        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        // Terminals disagree on how Ctrl+letter arrives (lowercase, uppercase,
        // or the raw control character) — fold them all into Ctrl + lowercase.
        let (code, ctrl) = match key.code {
            KeyCode::Char(c @ '\u{1}'..='\u{1a}') => (KeyCode::Char((c as u8 + 0x60) as char), true),
            KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => {
                (KeyCode::Char(c.to_ascii_lowercase()), true)
            }
            other => (other, key.modifiers.contains(KeyModifiers::CONTROL)),
        };
        let key = KeyEvent::new(code, key.modifiers);

        // 3. "Close Beyond?" — y / Enter / a second Ctrl+C closes, anything
        //    else goes back to the app
        if matches!(self.popup, Popup::QuitConfirm) {
            self.popup = Popup::None;
            self.quit = matches!(key.code, KeyCode::Char('y' | 'Y') | KeyCode::Enter)
                || (ctrl && matches!(key.code, KeyCode::Char('c' | 'd')));
            return;
        }

        // 4. global shortcuts
        match key.code {
            KeyCode::Char('c') if ctrl => {
                if self.agent.is_some() {
                    self.interrupt();
                } else if !self.input.is_empty() || self.prompt.is_some() {
                    self.cancel_input();
                } else if matches!(self.popup, Popup::None | Popup::Autocomplete { .. }) {
                    self.popup = Popup::QuitConfirm;
                }
                return;
            }
            KeyCode::Char('d') if ctrl && self.input.is_empty() => {
                if matches!(self.popup, Popup::None | Popup::Autocomplete { .. }) {
                    self.popup = Popup::QuitConfirm;
                }
                return;
            }
            KeyCode::Char('l') if ctrl => {
                self.apply_cmd(commands::Cmd::Clear);
                return;
            }
            KeyCode::F(9) => {
                self.start_voice();
                return;
            }
            KeyCode::PageUp => {
                self.scroll += 10;
                return;
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_sub(10);
                return;
            }
            _ => {}
        }

        // 4. list popups
        if matches!(self.popup, Popup::ThemePicker { .. } | Popup::SessionPicker { .. } | Popup::ChoicePicker { .. }) {
            self.on_picker_key(key);
            return;
        }
        if matches!(self.popup, Popup::Autocomplete { .. }) && self.on_autocomplete_key(key) {
            return;
        }

        // 5. editing
        match key.code {
            KeyCode::Enter if shift || alt => self.insert_str("\n"),
            KeyCode::Char('j') if ctrl => self.insert_str("\n"),
            KeyCode::Enter => self.submit(),
            KeyCode::Esc => {
                if self.prompt.is_some() {
                    self.cancel_input();
                } else if self.agent.is_some() {
                    self.interrupt();
                }
            }
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.input.len(),
            KeyCode::Char('u') if ctrl => self.cancel_input(),
            KeyCode::Char('w') if ctrl => self.delete_range(self.word_left(), self.cursor),
            KeyCode::Backspace if ctrl || alt => self.delete_range(self.word_left(), self.cursor),
            KeyCode::Backspace => self.delete_range(self.prev_boundary(), self.cursor),
            KeyCode::Delete => self.delete_range(self.cursor, self.next_boundary()),
            KeyCode::Left if ctrl => self.cursor = self.word_left(),
            KeyCode::Right if ctrl => self.cursor = self.word_right(),
            KeyCode::Left => self.cursor = self.prev_boundary(),
            KeyCode::Right => self.cursor = self.next_boundary(),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.input.len(),
            KeyCode::Up => self.history_step(-1),
            KeyCode::Down => self.history_step(1),
            KeyCode::Char(c) if !ctrl => {
                let mut buf = [0u8; 4];
                self.insert_str(c.encode_utf8(&mut buf));
            }
            _ => {}
        }
    }

    /// y / Enter = allow once · a = always (this run) · n / Esc = deny.
    fn on_confirm_key(&mut self, key: KeyEvent) {
        let answer = match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => Some((true, false)),
            KeyCode::Char('a') | KeyCode::Char('A') => Some((true, true)),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Some((false, false)),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.interrupt();
                None
            }
            _ => None,
        };
        let Some((allow, always)) = answer else { return };
        if let Popup::Confirm { title, respond, .. } = std::mem::replace(&mut self.popup, Popup::None) {
            if always {
                self.always_allow.insert(title);
            }
            let _ = respond.send(allow);
        }
    }

    fn on_picker_key(&mut self, key: KeyEvent) {
        let len = match &self.popup {
            Popup::ThemePicker { items, .. } => items.len(),
            Popup::SessionPicker { items, .. } => items.len(),
            Popup::ChoicePicker { items, .. } => items.len(),
            _ => return,
        };
        let step = |sel: usize, down: bool| -> usize {
            if len == 0 {
                0
            } else if down {
                (sel + 1) % len
            } else {
                (sel + len - 1) % len
            }
        };
        match key.code {
            KeyCode::Up | KeyCode::Down => {
                let down = key.code == KeyCode::Down;
                match &mut self.popup {
                    Popup::ThemePicker { items, sel, .. } => {
                        *sel = step(*sel, down);
                        // live preview while browsing
                        if let Some(t) = items.get(*sel).and_then(|(id, _)| crate::theme::resolve(id).ok()) {
                            self.theme = t;
                            self.restyle();
                        }
                    }
                    Popup::SessionPicker { sel, .. } | Popup::ChoicePicker { sel, .. } => *sel = step(*sel, down),
                    _ => {}
                }
            }
            KeyCode::Enter => match std::mem::replace(&mut self.popup, Popup::None) {
                Popup::ThemePicker { items, sel, .. } => {
                    if let Some((id, _)) = items.into_iter().nth(sel) {
                        self.set_theme(&id);
                    }
                }
                Popup::SessionPicker { items, sel } => {
                    if let Some(item) = items.into_iter().nth(sel) {
                        self.load_session(&item.id);
                    }
                }
                Popup::ChoicePicker { kind, items, sel } => {
                    if let Some((id, _)) = items.into_iter().nth(sel) {
                        match kind {
                            Choice::Model => self.set_model(&id),
                            Choice::Mode => self.set_mode(&id),
                        }
                    }
                }
                other => self.popup = other,
            },
            KeyCode::Esc => {
                if let Popup::ThemePicker { original, .. } = std::mem::replace(&mut self.popup, Popup::None) {
                    self.theme = *original;
                    self.restyle();
                }
            }
            _ => {}
        }
    }

    /// Returns `true` when the key was consumed by the suggestion list.
    fn on_autocomplete_key(&mut self, key: KeyEvent) -> bool {
        let Popup::Autocomplete { items, sel } = &mut self.popup else { return false };
        let len = items.len().max(1);
        match key.code {
            KeyCode::Up => *sel = (*sel + len - 1) % len,
            KeyCode::Down => *sel = (*sel + 1) % len,
            KeyCode::Esc => self.popup = Popup::None,
            KeyCode::Tab | KeyCode::Enter => {
                let Some(def) = items.get(*sel).copied() else { return false };
                let typed_fully = self.input[1..].eq_ignore_ascii_case(def.name);
                let needs_args = def.args.starts_with('<');
                if key.code == KeyCode::Enter && (typed_fully || !needs_args) {
                    // nothing more to type: run it
                    self.input = format!("/{}", def.name);
                    self.popup = Popup::None;
                    self.submit();
                } else {
                    self.input = format!("/{} ", def.name);
                    self.cursor = self.input.len();
                    self.popup = Popup::None;
                }
            }
            _ => return false,
        }
        true
    }

    // ------------------------------------------------------- input editing

    fn submit(&mut self) {
        let text = std::mem::take(&mut self.input).trim().to_string();
        self.cursor = 0;
        self.hist_idx = None;
        self.popup = Popup::None;
        self.scroll = 0;

        if let Some(prompt) = self.prompt.take() {
            self.finish_prompt(prompt, text); // secrets never enter history
            return;
        }
        if text.is_empty() {
            return;
        }
        if self.history.last() != Some(&text) {
            self.history.push(text.clone());
        }
        if text.starts_with('/') {
            self.apply_cmd(commands::dispatch(&text));
        } else {
            self.send_message(text);
        }
    }

    /// Clear the input box (and leave any hidden prompt).
    fn cancel_input(&mut self) {
        self.input.clear();
        self.cursor = 0;
        self.hist_idx = None;
        self.popup = Popup::None;
        if self.prompt.take().is_some() {
            self.push(EntryKind::Info, "cancelled");
        }
    }

    pub(super) fn insert_str(&mut self, s: &str) {
        self.input.insert_str(self.cursor, s);
        self.cursor += s.len();
        self.after_edit();
    }

    fn delete_range(&mut self, from: usize, to: usize) {
        if from < to {
            self.input.replace_range(from..to, "");
            self.cursor = from;
            self.after_edit();
        }
    }

    fn after_edit(&mut self) {
        self.hist_idx = None;
        self.refresh_autocomplete();
        self.dirty = true;
    }

    /// Show command suggestions while the input is a bare `/word`.
    fn refresh_autocomplete(&mut self) {
        if !matches!(self.popup, Popup::None | Popup::Autocomplete { .. }) {
            return;
        }
        if self.prompt.is_none() && self.input.starts_with('/') && !self.input.contains(char::is_whitespace) {
            let q = self.input[1..].to_ascii_lowercase();
            let items: Vec<&'static CmdDef> =
                commands::registry().iter().filter(|c| c.name.starts_with(&q)).collect();
            if !items.is_empty() {
                let sel = match &self.popup {
                    Popup::Autocomplete { sel, .. } => (*sel).min(items.len() - 1),
                    _ => 0,
                };
                self.popup = Popup::Autocomplete { items, sel };
                return;
            }
        }
        self.popup = Popup::None;
    }

    /// Up/Down through previously sent messages (single-line input only).
    fn history_step(&mut self, dir: i32) {
        if self.history.is_empty() || self.input.contains('\n') || self.prompt.is_some() {
            return;
        }
        let last = self.history.len() - 1;
        self.hist_idx = match (self.hist_idx, dir) {
            (None, -1) => Some(last),
            (None, _) => return,
            (Some(i), -1) => Some(i.saturating_sub(1)),
            (Some(i), _) if i < last => Some(i + 1),
            (Some(_), _) => None,
        };
        self.input = self.hist_idx.map(|i| self.history[i].clone()).unwrap_or_default();
        self.cursor = self.input.len();
    }

    fn prev_boundary(&self) -> usize {
        self.input[..self.cursor].char_indices().next_back().map(|(i, _)| i).unwrap_or(0)
    }

    fn next_boundary(&self) -> usize {
        self.input[self.cursor..].chars().next().map(|c| self.cursor + c.len_utf8()).unwrap_or(self.input.len())
    }

    fn word_left(&self) -> usize {
        let head = self.input[..self.cursor].trim_end();
        head.char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0)
    }

    fn word_right(&self) -> usize {
        let rest = &self.input[self.cursor..];
        let ws = rest.len() - rest.trim_start().len();
        let word = rest[ws..].find(char::is_whitespace).unwrap_or(rest.len() - ws);
        self.cursor + ws + word
    }
}

/// A signed-out app with default config, for UI tests (touches no files).
#[cfg(test)]
pub(super) fn test_app() -> App {
    let (_tx, rx) = mpsc::unbounded_channel();
    let mcps = Arc::new(McpManager { servers: vec![], warnings: vec![] });
    App::new(Config::default(), crate::theme::galaxy(), vec![], mcps, rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(app: &mut App, code: KeyCode, mods: KeyModifiers) {
        app.on_event(Event::Key(KeyEvent::new(code, mods)));
    }

    fn type_text(app: &mut App, s: &str) {
        for c in s.chars() {
            press(app, KeyCode::Char(c), KeyModifiers::NONE);
        }
    }

    #[test]
    fn editing_keys() {
        let mut app = test_app();
        type_text(&mut app, "héllo wörld");
        press(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);
        assert_eq!(app.input, "héllo ");
        press(&mut app, KeyCode::Home, KeyModifiers::NONE);
        press(&mut app, KeyCode::Right, KeyModifiers::NONE);
        press(&mut app, KeyCode::Delete, KeyModifiers::NONE); // removes the 2-byte é
        assert_eq!(app.input, "hllo ");
        press(&mut app, KeyCode::Enter, KeyModifiers::SHIFT);
        assert_eq!(app.input, "h\nllo ");
        app.on_event(Event::Paste("a\r\nb".into()));
        assert_eq!(app.input, "h\na\nbllo ");
    }

    #[test]
    fn slash_opens_suggestions_and_enter_runs() {
        let mut app = test_app();
        type_text(&mut app, "/he");
        assert!(matches!(&app.popup, Popup::Autocomplete { items, .. } if items[0].name == "help"));
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.input.is_empty());
        assert!(app.entries.last().unwrap().text.contains("**Commands**"));

        // a command that needs an argument is completed, not run
        type_text(&mut app, "/loadth");
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.input, "/loadtheme ");
    }

    #[test]
    fn history_and_secret_prompt() {
        let mut app = test_app();
        type_text(&mut app, "/nope");
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        press(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.input, "/nope");
        press(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);

        // a rejected key is never saved, echoed, or added to history
        app.prompt = Some(Prompt::ApiKey { limited: true });
        type_text(&mut app, "hunter2");
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.prompt.is_none() && app.cfg.auth_expires_at.is_none());
        assert!(app.entries.iter().all(|e| !e.text.contains("hunter2")));
        assert!(app.history.iter().all(|h| h != "hunter2"));
    }

    #[test]
    fn ctrl_c_clears_then_quits() {
        for code in [KeyCode::Char('c'), KeyCode::Char('C')] {
            let mut app = test_app();
            type_text(&mut app, "draft");
            press(&mut app, code, KeyModifiers::CONTROL);
            assert!(app.input.is_empty() && !app.quit);
            // second press asks first, third confirms
            press(&mut app, code, KeyModifiers::CONTROL);
            assert!(matches!(app.popup, Popup::QuitConfirm) && !app.quit);
            press(&mut app, code, KeyModifiers::CONTROL);
            assert!(app.quit);
        }
        // raw control character, as some terminals send it
        let mut app = test_app();
        press(&mut app, KeyCode::Char('\u{3}'), KeyModifiers::NONE);
        assert!(matches!(app.popup, Popup::QuitConfirm));
        // "n" (or anything that isn't yes) goes back to the app
        press(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
        assert!(matches!(app.popup, Popup::None) && !app.quit && app.input.is_empty());
        // "y" closes
        press(&mut app, KeyCode::Char('\u{3}'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('y'), KeyModifiers::NONE);
        assert!(app.quit);
    }
}
