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
//! busy queues; cancelling aborts the in-flight turn and drains the queue.

use crate::agent;
use crate::client::Client;
use crate::config::ApprovalSettings;
use crate::session::ChatSession;
use crate::ui::{AgentEvent, AgentUi, ApprovalRequest};
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
    /// Switch the reasoning effort for subsequent turns. `None` clears the
    /// override. A turn already running keeps the effort it started with.
    SetEffort(Option<String>),
    /// Toggle verbose tool detail in the TUI view. Purely a display
    /// setting; the agent loop never sees it.
    ToggleVerbose,
    /// Switch the tool-calling iteration cap per turn (agent mode only).
    /// `None` clears the override. A turn already running keeps the cap it
    /// started with.
    SetMaxIterations(Option<usize>),
    /// Switch one tool-approval gate (`"read"`/`"write"`/`"terminal"`/`"all"`)
    /// for subsequent turns. A turn already running keeps the gates it
    /// started with.
    SetApproval { category: String, enabled: bool },
    /// Switch the sampling temperature for subsequent turns. `None` clears
    /// the override. A turn already running keeps the temperature it
    /// started with.
    SetTemperature(Option<f32>),
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
    Queued { pending: usize },
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
    AgenticChanged { agentic: bool },
    /// The reasoning effort changed (or a change failed). Front ends
    /// re-label from here rather than assuming the command succeeded.
    EffortChanged { effort_level: Option<String> },
    /// Verbose tool detail was turned on or off.
    VerboseChanged { verbose: bool },
    /// The tool-calling iteration cap changed (or a change failed). Front
    /// ends re-label from here rather than assuming the command succeeded.
    MaxIterationsChanged { max_iterations: Option<usize> },
    /// A tool-approval gate changed (or a change failed). Front ends
    /// re-label from here rather than assuming the command succeeded.
    ApprovalSettingsChanged { approval: ApprovalSettings },
    /// The sampling temperature changed (or a change failed). Front ends
    /// re-label from here rather than assuming the command succeeded.
    TemperatureChanged { temperature: Option<f32> },
    /// The session's title changed — set once a session goes from
    /// "Untitled" to a title derived from its first user message.
    TitleChanged { title: String },
}

/// Handle to a running conversation worker.
pub struct Conversation {
    commands: mpsc::UnboundedSender<Command>,
    events: mpsc::UnboundedReceiver<Event>,
    task: JoinHandle<()>,
}

impl Conversation {
    /// Starts the worker. `session` carries the history (possibly resumed)
    /// that turns will extend.
    pub fn spawn(
        client: Arc<Client>,
        session: ChatSession,
        max_iterations: usize,
        temperature: f32,
        agentic: bool,
    ) -> Self {
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        let worker = Worker {
            client,
            session,
            max_iterations,
            temperature,
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
    /// The configured default iteration cap, used when the session has no
    /// `/max-iterations` override of its own.
    max_iterations: usize,
    /// The configured default sampling temperature, used when the session
    /// has no `/temperature` override of its own.
    temperature: f32,
    /// Whether turns run the tool-calling loop (`agent-chat`) or are a plain
    /// exchange (`chat`).
    agentic: bool,
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
                Command::ToggleVerbose => self.toggle_verbose(),
                Command::SetMaxIterations(max_iterations) => {
                    self.set_max_iterations(max_iterations)
                }
                Command::SetApproval { category, enabled } => self.set_approval(&category, enabled),
                Command::SetTemperature(temperature) => self.set_temperature(temperature),
                // Only meaningful while a turn is running, where they're
                // handled inline by `run_turn`.
                Command::Approve(_) | Command::Cancel => {}
            }
        }

        // The front end has gone; if the conversation was never actually
        // used, don't leave an empty session behind.
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

        // The agent loop runs on its own task so commands stay responsive
        // while it works; `approvals` carries decisions back into it.
        let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<oneshot::Sender<bool>>();
        let ui = ChannelUi {
            events: self.events.clone(),
            approvals: approval_tx,
        };

        let client = Arc::clone(&self.client);
        let mut messages = self.session.messages().to_vec();
        let model = self.session.model().to_string();
        let effort_level = self.session.effort_level().map(|s| s.to_string());
        let max_iterations = self.session.max_iterations().unwrap_or(self.max_iterations);
        let temperature = self.session.temperature().unwrap_or(self.temperature);
        let approval = self.session.approval().clone();
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
                    &approval,
                    effort_level,
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
                )
                .await
            };
            (result, messages)
        });

        // A oneshot waiting on the user's answer to an approval prompt, if
        // one is currently outstanding.
        let mut pending_approval: Option<oneshot::Sender<bool>> = None;

        let outcome = loop {
            tokio::select! {
                // Bias toward the turn finishing so its result is handled
                // before any late command.
                biased;

                finished = &mut turn => {
                    match finished {
                        Ok((result, messages)) => {
                            self.absorb(result, messages);
                            break TurnOutcome::Completed;
                        }
                        Err(e) if e.is_cancelled() => break TurnOutcome::Cancelled,
                        Err(e) => {
                            let _ = self.events.send(Event::Agent(AgentEvent::Error {
                                message: format!("Turn failed: {e}"),
                            }));
                            break TurnOutcome::Completed;
                        }
                    }
                }

                request = approval_rx.recv() => {
                    if let Some(responder) = request {
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
                            queue.push_back(text);
                            let _ = self.events.send(Event::Queued { pending: queue.len() });
                        }
                        // Applies from the next turn on; the running one
                        // already captured its model/mode/effort.
                        Some(Command::SetModel(model)) => self.set_model(model),
                        Some(Command::SetAgentic(agentic)) => self.set_agentic(agentic),
                        Some(Command::SetEffort(effort_level)) => self.set_effort(effort_level),
                        Some(Command::ToggleVerbose) => self.toggle_verbose(),
                        Some(Command::SetMaxIterations(max_iterations)) => {
                            self.set_max_iterations(max_iterations)
                        }
                        Some(Command::SetApproval { category, enabled }) => {
                            self.set_approval(&category, enabled)
                        }
                        Some(Command::SetTemperature(temperature)) => {
                            self.set_temperature(temperature)
                        }
                    }
                }
            }
        };

        self.persist();
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

    /// Flips verbose tool detail against the session's own confirmed
    /// state, rather than trusting whatever the front end last rendered,
    /// so repeated toggles can't drift out of sync with it.
    fn toggle_verbose(&mut self) {
        let verbose = !self.session.verbose();
        if let Err(e) = self.session.set_verbose(verbose) {
            let _ = self.events.send(Event::Agent(AgentEvent::Error {
                message: format!("Failed to toggle verbose: {e}"),
            }));
        }
        let _ = self.events.send(Event::VerboseChanged {
            verbose: self.session.verbose(),
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

    /// Switches one tool-approval gate and tells the front end what the
    /// session's gates ended up as, so the display can't drift from what
    /// the next turn will actually use.
    fn set_approval(&mut self, category: &str, enabled: bool) {
        let approval = self.session.approval().with_category(category, enabled);
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
    approvals: mpsc::UnboundedSender<oneshot::Sender<bool>>,
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
            let _ = events.send(Event::ApprovalRequested(request));
            if approvals.send(tx).is_err() {
                return Ok(false);
            }
            // A dropped responder (front end gone, turn cancelled) denies,
            // rather than hanging the loop forever.
            Ok(rx.await.unwrap_or(false))
        }
    }
}
