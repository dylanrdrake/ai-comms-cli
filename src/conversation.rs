//! A conversation as a background worker.
//!
//! Front ends that aren't a blocking terminal loop — the TUI, and a GUI
//! later — can't call the agent loop inline: they need to keep rendering and
//! accepting input while a turn runs, and to interrupt it. This wraps a
//! [`ChatSession`] and the agent loop in a task driven by [`Command`]s and
//! reporting through [`Event`]s, so a front end only ever talks to two
//! channels and never touches the loop directly.
//!
//! The design assumes exactly one turn in flight at a time. Sending while
//! busy joins that turn in agent mode, where the loop has iterations to
//! inject between, and queues in ask mode, where a single request has no
//! seam; cancelling aborts the in-flight turn and drops both.

use crate::agent::{self, Steering};
use crate::client::Client;
use crate::config::{ApprovalSettings, SessionGates};
use crate::session::ChatSession;
use crate::store::Activity;
use crate::ui::{AgentEvent, AgentUi, ApprovalRequest, Submission};
use anyhow::Result;
use std::collections::VecDeque;
use std::future::Future;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

/// What a front end asks the conversation to do.
#[derive(Debug)]
pub enum Command {
    /// Send a user message. Queued if a turn is already running.
    Send(String),
    /// Answer the outstanding [`Event::ApprovalRequested`].
    Approve(bool),
    /// Abort the in-flight turn and drop anything queued behind it.
    Cancel,
    /// Switch the model for subsequent turns. A turn already running keeps
    /// the model it started with.
    SetModel(String),
    /// Switch between plain and agent (tool-calling) mode for subsequent
    /// turns. A turn already running keeps the mode it started with.
    SetAgentic(bool),
    /// Switch the reasoning effort for subsequent turns. `None` nullifies
    /// it — no effort field is sent until set again. A turn already running
    /// keeps the effort it started with.
    SetEffort(Option<String>),
    /// Reads the *currently* configured default effort and saves that
    /// concrete value to the session now (`/effort default`), distinct from
    /// [`Command::SetEffort`]`(None)`, which nullifies instead.
    ResetEffort,
    /// Toggle verbose tool detail in the TUI view. Purely a display
    /// setting; the agent loop never sees it.
    /// Show full tool-call detail for this session, or stop.
    SetVerbose(bool),
    /// Stream replies token-by-token for this session, or wait for the whole reply.
    SetStream(bool),
    /// Rename this session.
    SetTitle(String),
    /// Confine the agent's file writes to the working directory, or let
    /// them go anywhere.
    SetSandbox(bool),
    /// Switch the tool-calling iteration cap per turn (agent mode only).
    /// `None` nullifies it — a turn falls back to whatever the configured
    /// default is when it runs. A turn already running keeps the cap it
    /// started with.
    SetMaxIterations(Option<usize>),
    /// Reads the *currently* configured default iteration cap and saves
    /// that concrete value to the session now (`/max-iterations default`),
    /// distinct from [`Command::SetMaxIterations`]`(None)`.
    ResetMaxIterations,
    /// Switch one tool-approval gate (`"read"`/`"write"`/`"terminal"`/`"all"`)
    /// for subsequent turns. A turn already running keeps the gates it
    /// started with.
    SetApproval { category: String, enabled: bool },
    /// Switch the sampling temperature for subsequent turns. `None`
    /// nullifies it, same deal as [`Command::SetMaxIterations`]. A turn
    /// already running keeps the temperature it started with.
    SetTemperature(Option<f32>),
    /// Reads the *currently* configured default temperature and saves that
    /// concrete value to the session now (`/temperature default`), distinct
    /// from [`Command::SetTemperature`]`(None)`.
    ResetTemperature,
}

/// What the conversation reports back. Agent progress is forwarded verbatim
/// as [`Event::Agent`]; the rest is worker-level state a front end needs to
/// render status.
#[derive(Debug)]
pub enum Event {
    /// Progress from the agent loop.
    Agent(AgentEvent),
    /// A tool needs a decision; reply with [`Command::Approve`].
    ApprovalRequested(ApprovalRequest),
    /// A turn began or ended. Front ends use this to show busy state and to
    /// decide whether a new message sends immediately or queues.
    Busy(bool),
    /// A message was accepted but won't be sent until the current turn ends.
    Queued {
        pending: usize,
    },
    /// The in-flight turn was cancelled.
    Cancelled,
    /// A user message was accepted and is now part of the transcript. Front
    /// ends echo this rather than assuming, so what's displayed matches what
    /// was actually recorded.
    UserMessage(String),
    /// The model changed (or a change failed). Front ends re-label from
    /// here rather than assuming the command succeeded.
    ModelChanged {
        model: String,
        effort_level: Option<String>,
    },
    /// Agent (tool-calling) mode was turned on or off (or a change failed).
    /// Front ends re-label from here rather than assuming the command
    /// succeeded.
    AgenticChanged {
        agentic: bool,
    },
    /// The reasoning effort changed (or a change failed). Front ends
    /// re-label from here rather than assuming the command succeeded.
    EffortChanged {
        effort_level: Option<String>,
    },
    /// Verbose tool detail was turned on or off.
    VerboseChanged {
        verbose: bool,
    },
    SandboxChanged {
        sandbox: bool,
    },
    StreamChanged {
        stream: bool,
    },
    /// The tool-calling iteration cap changed (or a change failed). Front
    /// ends re-label from here rather than assuming the command succeeded.
    /// `None` means nullified — turns fall back to the configured default.
    MaxIterationsChanged {
        max_iterations: Option<usize>,
    },
    /// A tool-approval gate changed (or a change failed). Front ends
    /// re-label from here rather than assuming the command succeeded.
    ApprovalSettingsChanged {
        approval: ApprovalSettings,
    },
    /// The sampling temperature changed (or a change failed). Front ends
    /// re-label from here rather than assuming the command succeeded.
    /// `None` means nullified, same deal as `MaxIterationsChanged`.
    TemperatureChanged {
        temperature: Option<f32>,
    },
    /// The session's title changed — set once a session goes from
    /// "Untitled" to a title derived from its first user message.
    TitleChanged {
        title: String,
    },
}

/// Handle to a running conversation worker.
/// The [`Command`] a submission turns into, or `None` when the front end
/// answers it itself.
///
/// Shared so the TUI and the CLI can't drift on what a command means. The
/// split is: anything that *changes* session state goes to the worker, which
/// owns that state; anything that only reads it — or reports a mistyped
/// command — is answered locally from what the front end already holds, with
/// no round trip.
///
/// Deliberately exhaustive. A new [`Submission`] variant won't compile until
/// it's classified here, which is the one place that decides the question for
/// every front end at once.
pub fn command_for(submission: &Submission) -> Option<Command> {
    match submission {
        Submission::Message(text) => Some(Command::Send(text.clone())),
        Submission::SetModel(model) => Some(Command::SetModel(model.clone())),
        Submission::SetAgentic(agentic) => Some(Command::SetAgentic(*agentic)),
        Submission::SetEffort(effort_level) => Some(Command::SetEffort(effort_level.clone())),
        Submission::ResetEffort => Some(Command::ResetEffort),
        Submission::SetVerbose(verbose) => Some(Command::SetVerbose(*verbose)),
        Submission::SetStream(stream) => Some(Command::SetStream(*stream)),
        Submission::SetTitle(title) => Some(Command::SetTitle(title.clone())),
        Submission::SetSandbox(sandbox) => Some(Command::SetSandbox(*sandbox)),
        Submission::SetMaxIterations(max_iterations) => {
            Some(Command::SetMaxIterations(*max_iterations))
        }
        Submission::ResetMaxIterations => Some(Command::ResetMaxIterations),
        Submission::SetTemperature(temperature) => Some(Command::SetTemperature(*temperature)),
        Submission::ResetTemperature => Some(Command::ResetTemperature),
        Submission::SetApproval { category, enabled } => Some(Command::SetApproval {
            category: category.clone(),
            enabled: *enabled,
        }),

        // Read-only, and front-end specific in how they're shown. `/model`
        // bare is here too: the TUI round-trips it through the worker so the
        // answer reflects what the session actually holds, while the CLI
        // reads its own `ChatSession` directly.
        Submission::ShowModel
        | Submission::ShowApproval
        | Submission::ShowSandbox
        | Submission::ShowStatus
        | Submission::ShowVerbose
        | Submission::ShowStream
        | Submission::ShowTitle
        | Submission::ShowTemperature
        | Submission::UnknownCommand(_) => None,
    }
}

pub struct Conversation {
    commands: mpsc::UnboundedSender<Command>,
    events: mpsc::UnboundedReceiver<Event>,
    task: JoinHandle<()>,
}

impl Conversation {
    /// Starts the worker. `session` carries the history (possibly resumed)
    /// that turns will extend.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        client: Arc<Client>,
        session: ChatSession,
        max_iterations: Option<usize>,
        temperature: Option<f32>,
        effort_level_default: Option<String>,
        agentic: bool,
    ) -> Self {
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        let worker = Worker {
            client,
            // Built before the session moves in, and shared with every turn
            // this worker spawns so a mid-turn `/approval` reaches it.
            gates: SessionGates::new(session.approval().clone(), session.sandbox()),
            steering: Steering::default(),
            session,
            max_iterations,
            temperature,
            effort_level_default,
            agentic,
            events: event_tx,
        };
        let task = tokio::spawn(worker.run(command_rx));

        Conversation {
            commands: command_tx,
            events: event_rx,
            task,
        }
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    /// Next event, or `None` once the worker has stopped.
    pub async fn next_event(&mut self) -> Option<Event> {
        self.events.recv().await
    }

    /// Stops the worker and waits for it to finish, so the session's final
    /// writes land before the process exits.
    pub async fn shutdown(self) {
        drop(self.commands);
        let _ = self.task.await;
    }
}

struct Worker {
    client: Arc<Client>,
    session: ChatSession,
    /// The configured default iteration cap this session started from
    /// (itself possibly `None`, if nothing is configured anywhere) — only
    /// consulted by `/max-iterations default`. A turn never falls back to
    /// this: a nullified `max_iterations` is a hard error instead, since
    /// there's no provider to hand an empty value to the way there is for
    /// `temperature`/`effort_level`.
    max_iterations: Option<usize>,
    /// The configured default sampling temperature this session started
    /// from — same deal as `max_iterations`, but only consulted by
    /// `/temperature default`; a nullified temperature is sent as no
    /// temperature at all, not an error.
    temperature: Option<f32>,
    /// The configured default effort level this session started from — same
    /// deal as `temperature`; only consulted by `/effort default`.
    effort_level_default: Option<String>,
    /// Whether turns run the tool-calling loop (`agent-chat`) or are a plain
    /// exchange (`chat`).
    agentic: bool,
    /// The session's approval gates as a shared handle. A running turn holds
    /// a clone of this rather than a copy of the settings, so `/approval`
    /// typed mid-turn applies to the turn's next tool call — see
    /// [`ApprovalGates`].
    gates: SessionGates,
    /// Messages typed while a turn is running. Like `gates`, a running turn
    /// holds a clone, so what is typed reaches the turn already in progress
    /// rather than the one after it. Only agent mode can use it: a single
    /// request has no seam to inject at, so ask mode queues instead.
    steering: Steering,
    events: mpsc::UnboundedSender<Event>,
}

impl Worker {
    async fn run(mut self, mut commands: mpsc::UnboundedReceiver<Command>) {
        let mut queue: VecDeque<String> = VecDeque::new();

        while let Some(command) = commands.recv().await {
            match command {
                Command::Send(text) => {
                    // Reaching here means no turn is running, so the queue is
                    // empty; anything sent mid-turn is queued inside
                    // `run_turn` instead. The queue can still grow while we
                    // work, which is why this drains rather than runs once.
                    queue.push_back(text);
                    while let Some(text) = queue.pop_front() {
                        // The count is what is still waiting, so it has to
                        // fall as each one is taken, not only rise as they
                        // are added — otherwise the indicator sticks at the
                        // high-water mark for the rest of the session.
                        let _ = self.events.send(Event::Queued {
                            pending: queue.len(),
                        });
                        match self.run_turn(text, &mut commands, &mut queue).await {
                            TurnOutcome::Completed => {}
                            TurnOutcome::Cancelled => {
                                queue.clear();
                                let _ = self.events.send(Event::Cancelled);
                                break;
                            }
                            TurnOutcome::Disconnected => return,
                        }
                    }
                }
                Command::SetModel(model) => self.set_model(model),
                Command::SetAgentic(agentic) => self.set_agentic(agentic),
                Command::SetEffort(effort_level) => self.set_effort(effort_level),
                Command::ResetEffort => self.reset_effort(),
                Command::SetVerbose(verbose) => self.set_verbose(verbose),
                Command::SetStream(stream) => self.set_stream(stream),
                Command::SetTitle(title) => self.rename(title),
                Command::SetSandbox(sandbox) => self.set_sandbox(sandbox),
                Command::SetMaxIterations(max_iterations) => {
                    self.set_max_iterations(max_iterations)
                }
                Command::ResetMaxIterations => self.reset_max_iterations(),
                Command::SetApproval { category, enabled } => self.set_approval(&category, enabled),
                Command::SetTemperature(temperature) => self.set_temperature(temperature),
                Command::ResetTemperature => self.reset_temperature(),
                // Only meaningful while a turn is running, where they're
                // handled inline by `run_turn`.
                Command::Approve(_) | Command::Cancel => {}
            }
        }

        // The front end has gone. Clear any activity first: a `working` left
        // behind would show as busy forever in everyone else's list.
        self.session.set_activity(None, None);
        // If the conversation was never actually used, don't leave an empty
        // session behind.
        let _ = self.session.discard_if_unused();
    }

    async fn run_turn(
        &mut self,
        text: String,
        commands: &mut mpsc::UnboundedReceiver<Command>,
        queue: &mut VecDeque<String>,
    ) -> TurnOutcome {
        self.session.push_user(text.clone());
        let _ = self.events.send(Event::UserMessage(text));
        let _ = self.events.send(Event::Busy(true));
        self.session.set_activity(Some(Activity::Working), None);

        // The agent loop runs on its own task so commands stay responsive
        // while it works; `approvals` carries decisions back into it.
        let (approval_tx, mut approval_rx) =
            mpsc::unbounded_channel::<(ApprovalRequest, oneshot::Sender<bool>)>();
        let ui = ChannelUi {
            events: self.events.clone(),
            approvals: approval_tx,
        };

        let client = Arc::clone(&self.client);
        let mut messages = self.session.messages().to_vec();
        let model = self.session.model().to_string();
        let effort_level = self.session.effort_level().map(|s| s.to_string());
        let max_iterations = self.session.max_iterations();
        let temperature = self.session.temperature();
        let gates = self.gates.clone();
        let steering = self.steering.clone();
        // Snapshotted per turn, like model and effort: streaming shapes how
        // the next request is made, not what a running tool may do.
        let stream = self.session.stream();
        let agentic = self.agentic;

        let mut turn = tokio::spawn(async move {
            let mut ui = ui;
            let result = if agentic {
                agent::run_agent_turn(
                    &client,
                    &mut ui,
                    &mut messages,
                    &model,
                    max_iterations,
                    temperature,
                    &gates,
                    effort_level,
                    stream,
                    &steering,
                )
                .await
            } else {
                agent::run_chat_turn(
                    &client,
                    &mut ui,
                    &mut messages,
                    &model,
                    temperature,
                    effort_level,
                    stream,
                )
                .await
            };
            (result, messages)
        });

        // A oneshot waiting on the user's answer to an approval prompt, if
        // one is currently outstanding.
        let mut pending_approval: Option<oneshot::Sender<bool>> = None;
        // Outlives the turn as an activity, so a session that broke can be
        // found from the list rather than only from its transcript.
        let mut failed = false;

        let outcome = loop {
            tokio::select! {
                // Bias toward the turn finishing so its result is handled
                // before any late command.
                biased;

                finished = &mut turn => {
                    match finished {
                        Ok((result, messages)) => {
                            failed = result.is_err();
                            self.absorb(result, messages);
                            break TurnOutcome::Completed;
                        }
                        Err(e) if e.is_cancelled() => break TurnOutcome::Cancelled,
                        Err(e) => {
                            failed = true;
                            let _ = self.events.send(Event::Agent(AgentEvent::Error {
                                message: format!("Turn failed: {e}"),
                            }));
                            break TurnOutcome::Completed;
                        }
                    }
                }

                request = approval_rx.recv() => {
                    if let Some((request, responder)) = request {
                        // Recorded with the request itself: a list of
                        // sessions is far more useful saying *what* is being
                        // asked than merely that something is.
                        self.session.set_activity(
                            Some(Activity::AwaitingApproval),
                            Some(&crate::ui::approval_summary(&request)),
                        );
                        pending_approval = Some(responder);
                    }
                }

                command = commands.recv() => {
                    match command {
                        None => {
                            turn.abort();
                            break TurnOutcome::Disconnected;
                        }
                        Some(Command::Approve(allowed)) => {
                            if let Some(responder) = pending_approval.take() {
                                self.session.set_activity(Some(Activity::Working), None);
                                let _ = responder.send(allowed);
                            }
                        }
                        Some(Command::Cancel) => {
                            turn.abort();
                            // Awaiting the aborted task lets its Drop run —
                            // which, with kill_on_drop, is what actually
                            // reaps a tool subprocess still executing.
                            let _ = (&mut turn).await;
                            break TurnOutcome::Cancelled;
                        }
                        Some(Command::Send(text)) => {
                            // Agent mode has iterations to inject between, so
                            // the message joins the running turn. Ask mode is
                            // a single request with no seam, so it waits and
                            // becomes a turn of its own.
                            let pending = if agentic {
                                queue.len() + self.steering.push(text)
                            } else {
                                queue.push_back(text);
                                queue.len()
                            };
                            let _ = self.events.send(Event::Queued { pending });
                        }
                        // These apply from the next turn on; the running
                        // one already captured its model/mode/effort.
                        Some(Command::SetModel(model)) => self.set_model(model),
                        Some(Command::SetAgentic(agentic)) => self.set_agentic(agentic),
                        Some(Command::SetEffort(effort_level)) => self.set_effort(effort_level),
                        Some(Command::ResetEffort) => self.reset_effort(),
                        Some(Command::SetVerbose(verbose)) => self.set_verbose(verbose),
                        Some(Command::SetStream(stream)) => self.set_stream(stream),
                        Some(Command::SetTitle(title)) => self.rename(title),
                        // Like approval, this reaches the running turn: it
                        // decides what a tool may do, not what the next
                        // request looks like.
                        Some(Command::SetSandbox(sandbox)) => self.set_sandbox(sandbox),
                        Some(Command::SetMaxIterations(max_iterations)) => {
                            self.set_max_iterations(max_iterations)
                        }
                        Some(Command::ResetMaxIterations) => self.reset_max_iterations(),
                        // Approval is the exception: it reaches the
                        // running turn too, at its next tool call. A gate is
                        // a safety control, so someone flipping one during a
                        // turn means *this* turn — waiting for the next one
                        // would be exactly backwards.
                        Some(Command::SetApproval { category, enabled }) => {
                            self.set_approval(&category, enabled)
                        }
                        Some(Command::SetTemperature(temperature)) => {
                            self.set_temperature(temperature)
                        }
                        Some(Command::ResetTemperature) => self.reset_temperature(),
                    }
                }
            }
        };

        // A turn can end with messages still waiting — the iteration cap was
        // reached, the turn failed, or it was cancelled — and losing one
        // that was typed is the worst outcome available here. Anything the
        // loop never took becomes its own turn instead.
        for text in self.steering.take() {
            queue.push_back(text);
        }
        if !queue.is_empty() {
            let _ = self.events.send(Event::Queued {
                pending: queue.len(),
            });
        }

        self.persist();
        // Cleared on success: the stored messages already say what happened,
        // and this column only speaks when they can't.
        self.session
            .set_activity(failed.then_some(Activity::Failed), None);
        let _ = self.events.send(Event::Busy(false));
        outcome
    }

    /// Folds a finished turn's messages back into the session and persists
    /// them. The agent loop works on a copy (it runs on another task), so the
    /// session only learns about assistant/tool turns here.
    fn absorb(
        &mut self,
        result: Result<Option<String>>,
        messages: Vec<crate::client::ChatMessage>,
    ) {
        let already = self.session.messages().len();
        for message in messages.into_iter().skip(already) {
            self.session.push(message);
        }

        if let Err(e) = result {
            let _ = self.events.send(Event::Agent(AgentEvent::Error {
                message: e.to_string(),
            }));
        }
    }

    /// Switches the model and tells the front end what it ended up as, so
    /// the display can't drift from what the next turn will actually use.
    fn set_model(&mut self, model: String) {
        if let Err(e) = self.session.set_model(model) {
            let _ = self.events.send(Event::Agent(AgentEvent::Error {
                message: format!("Failed to switch model: {e}"),
            }));
        }
        let _ = self.events.send(Event::ModelChanged {
            model: self.session.model().to_string(),
            effort_level: self.session.effort_level().map(|s| s.to_string()),
        });
    }

    /// Switches agent (tool-calling) mode and tells the front end what it
    /// ended up as, so the display can't drift from what the next turn will
    /// actually use.
    fn set_agentic(&mut self, agentic: bool) {
        if let Err(e) = self.session.set_agentic(agentic) {
            let _ = self.events.send(Event::Agent(AgentEvent::Error {
                message: format!("Failed to switch mode: {e}"),
            }));
        }
        self.agentic = self.session.is_agentic();
        let _ = self.events.send(Event::AgenticChanged {
            agentic: self.agentic,
        });
    }

    /// Switches reasoning effort and tells the front end what it ended up
    /// as, so the display can't drift from what the next turn will
    /// actually use.
    fn set_effort(&mut self, effort_level: Option<String>) {
        if let Err(e) = self.session.set_effort_level(effort_level) {
            let _ = self.events.send(Event::Agent(AgentEvent::Error {
                message: format!("Failed to switch effort: {e}"),
            }));
        }
        let _ = self.events.send(Event::EffortChanged {
            effort_level: self.session.effort_level().map(str::to_string),
        });
    }

    /// `/effort default`: snapshots the currently configured default effort
    /// into the session, distinct from nullifying it — even if that default
    /// is itself `None`.
    fn reset_effort(&mut self) {
        self.set_effort(self.effort_level_default.clone());
    }

    /// Flips verbose tool detail against the session's own confirmed
    /// state, rather than trusting whatever the front end last rendered,
    /// so repeated toggles can't drift out of sync with it.
    fn set_verbose(&mut self, verbose: bool) {
        if let Err(e) = self.session.set_verbose(verbose) {
            let _ = self.events.send(Event::Agent(AgentEvent::Error {
                message: format!("Failed to switch verbose: {e}"),
            }));
        }
        let _ = self.events.send(Event::VerboseChanged {
            verbose: self.session.verbose(),
        });
    }

    fn set_stream(&mut self, stream: bool) {
        if let Err(e) = self.session.set_stream(stream) {
            let _ = self.events.send(Event::Agent(AgentEvent::Error {
                message: format!("Failed to switch streaming: {e}"),
            }));
        }
        let _ = self.events.send(Event::StreamChanged { stream });
    }

    fn rename(&mut self, title: String) {
        if let Err(e) = self.session.set_title(title) {
            let _ = self.events.send(Event::Agent(AgentEvent::Error {
                message: format!("Failed to rename the session: {e}"),
            }));
            return;
        }
        let _ = self.events.send(Event::TitleChanged {
            title: self.session.title().to_string(),
        });
    }

    /// Switches the tool-calling iteration cap and tells the front end what
    /// it ended up as, so the display can't drift from what the next turn
    /// will actually use.
    fn set_max_iterations(&mut self, max_iterations: Option<usize>) {
        if let Err(e) = self.session.set_max_iterations(max_iterations) {
            let _ = self.events.send(Event::Agent(AgentEvent::Error {
                message: format!("Failed to switch max iterations: {e}"),
            }));
        }
        let _ = self.events.send(Event::MaxIterationsChanged {
            max_iterations: self.session.max_iterations(),
        });
    }

    /// `/max-iterations default`: snapshots the currently configured
    /// default cap into the session, distinct from nullifying it.
    fn reset_max_iterations(&mut self) {
        self.set_max_iterations(self.max_iterations);
    }

    /// Switches the sampling temperature and tells the front end what it
    /// ended up as, so the display can't drift from what the next turn
    /// will actually use.
    fn set_temperature(&mut self, temperature: Option<f32>) {
        if let Err(e) = self.session.set_temperature(temperature) {
            let _ = self.events.send(Event::Agent(AgentEvent::Error {
                message: format!("Failed to switch temperature: {e}"),
            }));
        }
        let _ = self.events.send(Event::TemperatureChanged {
            temperature: self.session.temperature(),
        });
    }

    /// `/temperature default`: snapshots the currently configured default
    /// temperature into the session, distinct from nullifying it.
    fn reset_temperature(&mut self) {
        self.set_temperature(self.temperature);
    }

    /// Switches one tool-approval gate and tells the front end what the
    /// session's gates ended up as, so the display can't drift from what
    /// the next turn will actually use.
    fn set_sandbox(&mut self, sandbox: bool) {
        // Through the shared handle as well as the session, so a turn
        // already running sees it at its next tool call.
        self.gates.set_sandbox(sandbox);
        if let Err(e) = self.session.set_sandbox(sandbox) {
            let _ = self.events.send(Event::Agent(AgentEvent::Error {
                message: format!("Failed to switch the sandbox: {e}"),
            }));
        }
        let _ = self.events.send(Event::SandboxChanged { sandbox });
    }

    fn set_approval(&mut self, category: &str, enabled: bool) {
        let approval = self.session.approval().with_category(category, enabled);
        // Through the shared handle as well as the session, so a turn
        // already running sees it at its next tool call.
        self.gates.set_approval(approval.clone());
        if let Err(e) = self.session.set_approval(approval) {
            let _ = self.events.send(Event::Agent(AgentEvent::Error {
                message: format!("Failed to switch approval: {e}"),
            }));
        }
        let _ = self.events.send(Event::ApprovalSettingsChanged {
            approval: self.session.approval().clone(),
        });
    }

    /// Writes whatever the session has accumulated. Called after every turn
    /// including a cancelled one, so an interrupted exchange still keeps the
    /// user's message rather than losing it if the app then exits.
    fn persist(&mut self) {
        if let Err(e) = self.session.persist_pending() {
            let _ = self.events.send(Event::Agent(AgentEvent::Error {
                message: format!("Failed to save message: {e}"),
            }));
        }
        let _ = self.events.send(Event::TitleChanged {
            title: self.session.title().to_string(),
        });
    }
}

enum TurnOutcome {
    Completed,
    Cancelled,
    /// The command channel closed — the front end is gone.
    Disconnected,
}

/// The [`AgentUi`] the worker hands to the agent loop: forwards every event
/// onto the worker's channel, and turns an approval request into a oneshot
/// the worker resolves once the user answers.
struct ChannelUi {
    events: mpsc::UnboundedSender<Event>,
    approvals: mpsc::UnboundedSender<(ApprovalRequest, oneshot::Sender<bool>)>,
}

impl AgentUi for ChannelUi {
    fn event(&mut self, event: AgentEvent) -> impl Future<Output = ()> + Send {
        let _ = self.events.send(Event::Agent(event));
        async {}
    }

    fn approve(&mut self, request: ApprovalRequest) -> impl Future<Output = Result<bool>> + Send {
        let events = self.events.clone();
        let approvals = self.approvals.clone();
        async move {
            let (tx, rx) = oneshot::channel();
            // Announce the request, then register the responder, so a front
            // end can never see the prompt before we're able to take its
            // answer.
            let _ = events.send(Event::ApprovalRequested(request.clone()));
            if approvals.send((request, tx)).is_err() {
                return Ok(false);
            }
            // A dropped responder (front end gone, turn cancelled) denies,
            // rather than hanging the loop forever.
            Ok(rx.await.unwrap_or(false))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every submission that changes session state, paired with the command
    /// it must produce. Listed rather than derived, so a mapping that
    /// silently changes shows up as a failure here.
    fn state_changing() -> Vec<(Submission, Command)> {
        vec![
            (
                Submission::Message("hi".to_string()),
                Command::Send("hi".to_string()),
            ),
            (
                Submission::SetModel("m".to_string()),
                Command::SetModel("m".to_string()),
            ),
            (Submission::SetAgentic(true), Command::SetAgentic(true)),
            (
                Submission::SetEffort(Some("high".to_string())),
                Command::SetEffort(Some("high".to_string())),
            ),
            (Submission::ResetEffort, Command::ResetEffort),
            (Submission::SetVerbose(true), Command::SetVerbose(true)),
            (Submission::SetStream(false), Command::SetStream(false)),
            (Submission::SetSandbox(false), Command::SetSandbox(false)),
            (
                Submission::SetMaxIterations(Some(5)),
                Command::SetMaxIterations(Some(5)),
            ),
            (Submission::ResetMaxIterations, Command::ResetMaxIterations),
            (
                Submission::SetTemperature(Some(1.0)),
                Command::SetTemperature(Some(1.0)),
            ),
            (Submission::ResetTemperature, Command::ResetTemperature),
            (
                Submission::SetApproval {
                    category: "write".to_string(),
                    enabled: false,
                },
                Command::SetApproval {
                    category: "write".to_string(),
                    enabled: false,
                },
            ),
        ]
    }

    #[test]
    fn state_changing_submissions_go_to_the_worker() {
        for (submission, expected) in state_changing() {
            let command = command_for(&submission)
                .unwrap_or_else(|| panic!("{submission:?} should reach the worker"));
            assert_eq!(
                format!("{command:?}"),
                format!("{expected:?}"),
                "{submission:?} mapped wrong"
            );
        }
    }

    #[test]
    fn read_only_submissions_are_answered_locally() {
        // These never reach the worker: the front end already holds what
        // they report, and a round trip would only add latency and a second
        // source of truth.
        for submission in [
            Submission::ShowModel,
            Submission::ShowApproval,
            Submission::ShowSandbox,
            Submission::ShowStatus,
            Submission::ShowVerbose,
            Submission::ShowStream,
            Submission::ShowTemperature,
            Submission::UnknownCommand("nope".to_string()),
        ] {
            assert!(
                command_for(&submission).is_none(),
                "{submission:?} should be answered by the front end"
            );
        }
    }
}
