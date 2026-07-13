//! The full-screen terminal UI: event loop, application state, and rendering.

mod ui;

use std::collections::HashMap;
use std::io::{self, Stdout};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event as CEvent, EventStream, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Terminal;
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::config::{Config, State};
use crate::docker::model::{UpdateInfo, UpdateStatus};
use crate::docker::{
    ApplyProgress, ApplyStage, ContainerInfo, ContainerStats, DiskUsage, DockerClient, ImageInfo,
    StageState,
};
use crate::registry;
use crate::theme::Theme;

/// Top-level view tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Images,
    Containers,
    Space,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Images, Tab::Containers, Tab::Space];

    pub fn title(&self) -> &'static str {
        match self {
            Tab::Images => "Images",
            Tab::Containers => "Containers",
            Tab::Space => "Space",
        }
    }

    fn index(&self) -> usize {
        Self::ALL.iter().position(|t| t == self).unwrap_or(0)
    }

    fn next(&self) -> Tab {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    fn prev(&self) -> Tab {
        Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

/// Which modal overlay (if any) is currently active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    None,
    Help,
    Logs,
    Changelog,
    Prune,
    /// Fuzzy command palette.
    Palette,
    /// Details of the selected image's update check (versions, dates, changelog).
    UpdateDetails,
    /// Live progress of an in-flight container update.
    ApplyProgress,
    /// A yes/no confirmation, carrying the action to run on confirm.
    Confirm(ConfirmAction),
}

/// An action invokable from the command palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteAction {
    Refresh,
    CheckAll,
    OpenPrune,
    PruneAll,
    CycleTheme,
    GotoImages,
    GotoContainers,
    GotoSpace,
    ApplySelected,
    Quit,
}

impl PaletteAction {
    /// The full catalog of palette actions with display labels.
    pub fn catalog() -> &'static [(&'static str, PaletteAction)] {
        &[
            ("Refresh data", PaletteAction::Refresh),
            ("Check all for updates", PaletteAction::CheckAll),
            ("Prune menu", PaletteAction::OpenPrune),
            ("Prune everything unused", PaletteAction::PruneAll),
            ("Cycle theme", PaletteAction::CycleTheme),
            ("Go to: Images", PaletteAction::GotoImages),
            ("Go to: Containers", PaletteAction::GotoContainers),
            ("Go to: Space", PaletteAction::GotoSpace),
            ("Apply update to selected container", PaletteAction::ApplySelected),
            ("Quit", PaletteAction::Quit),
        ]
    }
}

/// Actions that require confirmation before running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmAction {
    RemoveContainer(String, String),
    PruneImages(bool),
    PruneContainers,
    PruneVolumes,
    PruneBuildCache,
    PruneAll,
    ApplyUpdate(String, String),
}

/// A UI action triggerable by a key or a mouse click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiAction {
    Refresh,
    CheckSelected,
    CheckAll,
    Details,
    Apply,
    StartStop,
    Restart,
    Logs,
    Changelog,
    Remove,
    Defer,
    OpenPrune,
    CycleTheme,
    Help,
    OpenPalette,
    Quit,
    PruneImages(bool),
    PruneContainers,
    PruneVolumes,
    PruneBuildCache,
    PruneAll,
    ConfirmYes,
    ConfirmNo,
    CloseOverlay,
    PaletteItem(usize),
}

/// What a clickable screen region maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickTarget {
    Tab(Tab),
    Row(usize),
    Action(UiAction),
}

/// A rectangular hit region recorded during rendering.
#[derive(Debug, Clone, Copy)]
pub struct ClickRegion {
    pub rect: Rect,
    pub target: ClickTarget,
}

impl ClickRegion {
    /// Whether the given screen coordinate falls inside this region.
    fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.rect.x
            && x < self.rect.x + self.rect.width
            && y >= self.rect.y
            && y < self.rect.y + self.rect.height
    }
}

/// The status of a single apply step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Pending,
    Running,
    Done,
    Failed,
}

/// Live state of an in-progress (or finished) container update.
#[derive(Debug, Clone)]
pub struct ApplyState {
    pub title: String,
    pub steps: Vec<(ApplyStage, StepStatus)>,
    pub rollback: Option<StepStatus>,
    pub log: Vec<String>,
    pub error: Option<String>,
    pub finished: bool,
    pub success: bool,
}

impl ApplyState {
    fn new(title: String) -> Self {
        Self {
            title,
            steps: ApplyStage::SEQUENCE
                .iter()
                .map(|s| (*s, StepStatus::Pending))
                .collect(),
            rollback: None,
            log: Vec::new(),
            error: None,
            finished: false,
            success: false,
        }
    }

    /// Fold a progress event into the state.
    fn apply(&mut self, progress: ApplyProgress) {
        match progress {
            ApplyProgress::Log(line) => {
                self.log.push(line);
                if self.log.len() > 300 {
                    let excess = self.log.len() - 300;
                    self.log.drain(0..excess);
                }
            }
            ApplyProgress::Stage(ApplyStage::Done, state) => {
                self.finished = true;
                self.success = matches!(state, StageState::Done);
                if let StageState::Failed(m) = state {
                    if self.error.is_none() {
                        self.error = Some(m);
                    }
                }
            }
            ApplyProgress::Stage(ApplyStage::Rollback, state) => {
                self.rollback = Some(step_status(&state));
            }
            ApplyProgress::Stage(stage, state) => {
                if let Some(entry) = self.steps.iter_mut().find(|(s, _)| *s == stage) {
                    entry.1 = step_status(&state);
                }
                if let StageState::Failed(m) = state {
                    self.error = Some(m);
                }
            }
        }
    }
}

fn step_status(state: &StageState) -> StepStatus {
    match state {
        StageState::Start => StepStatus::Running,
        StageState::Done => StepStatus::Done,
        StageState::Failed(_) => StepStatus::Failed,
    }
}

/// Messages delivered to the main loop from background tasks and input.
enum AppEvent {
    Input(CEvent),
    Tick,
    /// An update check completed for an image reference.
    UpdateResult(String, UpdateInfo),
    /// Data reloaded from the daemon.
    Reloaded(ReloadData),
    /// Container logs loaded.
    Logs(Vec<String>),
    /// Changelog releases loaded.
    Changelog(String, Vec<registry::Release>),
    /// A stats sample for a container.
    Stats(String, ContainerStats),
    /// A structured container-update progress event.
    Apply(ApplyProgress),
    /// A transient status message.
    Message(String),
}

/// A batch of freshly loaded daemon data.
struct ReloadData {
    images: Vec<ImageInfo>,
    containers: Vec<ContainerInfo>,
    usage: DiskUsage,
}

/// The application state.
pub struct App {
    client: DockerClient,
    config: Config,
    state: State,
    theme: Theme,
    tab: Tab,
    overlay: Overlay,

    images: Vec<ImageInfo>,
    containers: Vec<ContainerInfo>,
    usage: Option<DiskUsage>,

    /// Selection index per tab.
    selected: usize,

    /// image reference -> latest known update status.
    updates: HashMap<String, UpdateInfo>,
    /// image reference -> latest stats sample.
    stats: HashMap<String, ContainerStats>,

    /// Scrollback (styled) for the logs / changelog overlay.
    overlay_view: Vec<Line<'static>>,
    overlay_scroll: usize,
    overlay_title: String,

    status_message: String,
    should_quit: bool,

    /// Command palette query and selected index.
    palette_query: String,
    palette_index: usize,

    /// State of an in-progress or finished container update.
    apply: Option<ApplyState>,

    /// Clickable regions recorded during the last render.
    regions: Vec<ClickRegion>,

    tx: UnboundedSender<AppEvent>,
}

/// Run the TUI. Sets up the terminal, runs the loop, and always restores it.
pub async fn run(config: Config) -> Result<()> {
    let client = DockerClient::connect_local().await?;

    let mut terminal = setup_terminal()?;
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();

    let theme = Theme::by_name(&config.theme);
    let state = State::load();
    // Restore previously fetched update results so they show immediately.
    let updates = state.update_cache.clone();
    let mut app = App {
        client,
        state,
        theme,
        tab: Tab::Images,
        overlay: Overlay::None,
        images: Vec::new(),
        containers: Vec::new(),
        usage: None,
        selected: 0,
        updates,
        stats: HashMap::new(),
        overlay_view: Vec::new(),
        overlay_scroll: 0,
        overlay_title: String::new(),
        status_message: "Loading…".to_string(),
        should_quit: false,
        palette_query: String::new(),
        palette_index: 0,
        apply: None,
        regions: Vec::new(),
        config,
        tx: tx.clone(),
    };

    // Input reader task.
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut reader = EventStream::new();
            let mut tick = tokio::time::interval(Duration::from_millis(1000));
            loop {
                tokio::select! {
                    maybe_event = reader.next() => {
                        match maybe_event {
                            Some(Ok(ev)) => {
                                if tx.send(AppEvent::Input(ev)).is_err() { break; }
                            }
                            Some(Err(_)) | None => break,
                        }
                    }
                    _ = tick.tick() => {
                        if tx.send(AppEvent::Tick).is_err() { break; }
                    }
                }
            }
        });
    }

    // Initial data load.
    app.spawn_reload();

    // Scheduled background update checks + notifications.
    if app.config.schedule.enabled {
        let client = app.client.clone();
        let interval = Duration::from_secs(app.config.schedule.interval_minutes.max(1) * 60);
        let notify_url = app.config.notify.url.clone();
        let scheduler_tx = tx.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.tick().await; // consume the immediate first tick (no startup spam)
            loop {
                ticker.tick().await;
                let containers = client.list_containers(true).await.unwrap_or_default();
                let mut updated = Vec::new();
                for c in &containers {
                    if let Ok(true) = client.check_update(&c.image).await {
                        updated.push(format!("{} ({})", c.name, c.image));
                        let _ = scheduler_tx.send(AppEvent::UpdateResult(
                            c.image.clone(),
                            UpdateInfo::from_status(UpdateStatus::UpdateAvailable),
                        ));
                    }
                }
                if !updated.is_empty() {
                    if let Some(url) = &notify_url {
                        let body = updated.join("\n");
                        let _ = crate::notify::notify(url, "Docker updates available", &body).await;
                    }
                    let _ = scheduler_tx.send(AppEvent::Message(format!(
                        "scheduler: {} update(s) available",
                        updated.len()
                    )));
                }
            }
        });
    }

    // Main loop.
    let result = loop {
        if app.should_quit {
            break Ok(());
        }
        let mut regions: Vec<ClickRegion> = Vec::new();
        if let Err(e) = terminal.draw(|f| ui::draw(f, &app, &mut regions)) {
            break Err(e.into());
        }
        app.regions = regions;
        match rx.recv().await {
            Some(event) => {
                if let Err(e) = app.handle_event(event).await {
                    app.status_message = format!("error: {e}");
                }
            }
            None => break Ok(()),
        }
    };

    restore_terminal(&mut terminal)?;
    // Persist runtime state (deferred updates, changelog sources).
    let _ = app.state.save();
    result
}

impl App {
    // ── Accessors used by the renderer ─────────────────────────────────────
    pub fn theme(&self) -> &Theme {
        &self.theme
    }
    pub fn tab(&self) -> Tab {
        self.tab
    }
    pub fn overlay(&self) -> &Overlay {
        &self.overlay
    }
    pub fn images(&self) -> &[ImageInfo] {
        &self.images
    }
    pub fn containers(&self) -> &[ContainerInfo] {
        &self.containers
    }
    pub fn usage(&self) -> Option<&DiskUsage> {
        self.usage.as_ref()
    }
    pub fn selected(&self) -> usize {
        self.selected
    }
    pub fn updates(&self) -> &HashMap<String, UpdateInfo> {
        &self.updates
    }

    /// The selected row's image reference plus its update info, if any.
    pub fn selected_update(&self) -> Option<(String, Option<&UpdateInfo>)> {
        let image = self.selected_image_ref()?;
        let info = self.updates.get(&image);
        Some((image, info))
    }
    pub fn stats(&self) -> &HashMap<String, ContainerStats> {
        &self.stats
    }
    pub fn status_message(&self) -> &str {
        &self.status_message
    }
    pub fn overlay_view(&self) -> &[Line<'static>] {
        &self.overlay_view
    }
    pub fn overlay_scroll(&self) -> usize {
        self.overlay_scroll
    }
    pub fn overlay_title(&self) -> &str {
        &self.overlay_title
    }
    pub fn palette_query(&self) -> &str {
        &self.palette_query
    }
    pub fn apply(&self) -> Option<&ApplyState> {
        self.apply.as_ref()
    }
    pub fn palette_index(&self) -> usize {
        self.palette_index
    }

    /// Palette actions whose label contains the (case-insensitive) query.
    pub fn palette_matches(&self) -> Vec<(&'static str, PaletteAction)> {
        let q = self.palette_query.to_lowercase();
        PaletteAction::catalog()
            .iter()
            .filter(|(label, _)| q.is_empty() || label.to_lowercase().contains(&q))
            .copied()
            .collect()
    }

    fn run_palette_action(&mut self, action: PaletteAction) {
        match action {
            PaletteAction::Refresh => {
                self.status_message = "Refreshing…".to_string();
                self.spawn_reload();
            }
            PaletteAction::CheckAll => self.check_all_updates(),
            PaletteAction::OpenPrune => self.overlay = Overlay::Prune,
            PaletteAction::PruneAll => {
                self.overlay = Overlay::Confirm(ConfirmAction::PruneAll)
            }
            PaletteAction::CycleTheme => self.cycle_theme(),
            PaletteAction::GotoImages => self.set_tab(Tab::Images),
            PaletteAction::GotoContainers => self.set_tab(Tab::Containers),
            PaletteAction::GotoSpace => self.set_tab(Tab::Space),
            PaletteAction::ApplySelected => self.apply_selected_update(),
            PaletteAction::Quit => self.should_quit = true,
        }
    }

    /// Number of rows in the currently active list.
    fn row_count(&self) -> usize {
        match self.tab {
            Tab::Images => self.images.len(),
            Tab::Containers => self.containers.len(),
            Tab::Space => 0,
        }
    }

    /// The image reference for the current selection, if applicable.
    fn selected_image_ref(&self) -> Option<String> {
        match self.tab {
            Tab::Images => self
                .images
                .get(self.selected)
                .and_then(|i| i.primary_reference()),
            Tab::Containers => self.containers.get(self.selected).map(|c| c.image.clone()),
            Tab::Space => None,
        }
    }

    // ── Event handling ─────────────────────────────────────────────────────
    async fn handle_event(&mut self, event: AppEvent) -> Result<()> {
        match event {
            AppEvent::Input(CEvent::Key(key)) => self.handle_key(key).await?,
            AppEvent::Input(CEvent::Mouse(mouse)) => self.handle_mouse(mouse),
            AppEvent::Input(_) => {}
            AppEvent::Tick => {
                // Refresh stats for visible running containers on the Containers tab.
                if self.tab == Tab::Containers && self.overlay == Overlay::None {
                    self.spawn_stats_refresh();
                }
            }
            AppEvent::UpdateResult(image, info) => {
                if info.status == UpdateStatus::UpdateAvailable {
                    let detail = info
                        .transition()
                        .map(|t| format!(": {t}"))
                        .unwrap_or_default();
                    self.status_message = format!("update available: {image}{detail}");
                }
                // Persist conclusive results (not transient errors) so they
                // survive restarts.
                if !matches!(info.status, UpdateStatus::Error(_)) {
                    self.state.update_cache.insert(image.clone(), info.clone());
                    let _ = self.state.save();
                }
                self.updates.insert(image, info);
            }
            AppEvent::Reloaded(data) => {
                self.images = data.images;
                self.containers = data.containers;
                self.usage = Some(data.usage);
                self.invalidate_stale_updates();
                self.clamp_selection();
                self.status_message = format!(
                    "{} images · {} containers",
                    self.images.len(),
                    self.containers.len()
                );
            }
            AppEvent::Logs(lines) => {
                self.overlay_view = lines.into_iter().map(Line::from).collect();
                self.overlay_scroll = self.overlay_view.len().saturating_sub(1);
                self.overlay = Overlay::Logs;
            }
            AppEvent::Changelog(title, releases) => {
                self.overlay_title = title;
                self.overlay_view = self.changelog_lines(&releases);
                self.overlay_scroll = 0;
                self.overlay = Overlay::Changelog;
            }
            AppEvent::Stats(image, stats) => {
                self.stats.insert(image, stats);
            }
            AppEvent::Apply(progress) => {
                let mut finished = false;
                if let Some(a) = self.apply.as_mut() {
                    a.apply(progress);
                    finished = a.finished;
                }
                if finished {
                    // Refresh the container/image lists after the update settles.
                    self.spawn_reload();
                }
            }
            AppEvent::Message(msg) => {
                self.status_message = msg;
            }
        }
        Ok(())
    }

    async fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if key.kind == KeyEventKind::Release {
            return Ok(());
        }

        // Overlay-specific keys first.
        if self.overlay != Overlay::None {
            self.handle_overlay_key(key).await?;
            return Ok(());
        }

        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            (KeyCode::Tab, _) | (KeyCode::Char('l'), KeyModifiers::CONTROL) => {
                self.tab = self.tab.next();
                self.selected = 0;
            }
            (KeyCode::BackTab, _) => {
                self.tab = self.tab.prev();
                self.selected = 0;
            }
            (KeyCode::Char('1'), _) => self.set_tab(Tab::Images),
            (KeyCode::Char('2'), _) => self.set_tab(Tab::Containers),
            (KeyCode::Char('3'), _) => self.set_tab(Tab::Space),
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => self.move_selection(1),
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => self.move_selection(-1),
            (KeyCode::Home, _) | (KeyCode::Char('g'), _) => self.selected = 0,
            (KeyCode::End, _) | (KeyCode::Char('G'), _) => {
                self.selected = self.row_count().saturating_sub(1);
            }
            (KeyCode::Char('r'), _) => {
                self.status_message = "Refreshing…".to_string();
                self.spawn_reload();
            }
            (KeyCode::Char('u'), _) => self.check_selected_update(),
            (KeyCode::Char('U'), _) => self.check_all_updates(),
            (KeyCode::Enter, _) => self.overlay = Overlay::UpdateDetails,
            (KeyCode::Char('a'), _) => self.apply_selected_update(),
            (KeyCode::Char('L'), _) => self.open_logs(),
            (KeyCode::Char('w'), _) => self.open_changelog(),
            (KeyCode::Char('s'), _) => self.toggle_start_stop(),
            (KeyCode::Char('R'), _) => self.restart_selected(),
            (KeyCode::Char('x'), _) => self.confirm_remove_selected(),
            (KeyCode::Char('d'), _) => self.defer_selected(),
            (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                self.palette_query.clear();
                self.palette_index = 0;
                self.overlay = Overlay::Palette;
            }
            (KeyCode::Char('p'), _) => self.overlay = Overlay::Prune,
            (KeyCode::Char('T'), _) => self.cycle_theme(),
            (KeyCode::Char('?'), _) => self.overlay = Overlay::Help,
            (KeyCode::Char(':'), _) => {
                self.palette_query.clear();
                self.palette_index = 0;
                self.overlay = Overlay::Palette;
            }
            _ => {}
        }
        Ok(())
    }

    async fn handle_overlay_key(&mut self, key: KeyEvent) -> Result<()> {
        match &self.overlay {
            Overlay::Confirm(action) => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    let action = action.clone();
                    self.overlay = Overlay::None;
                    self.run_confirmed(action);
                }
                KeyCode::Char('n') | KeyCode::Esc | KeyCode::Char('q') => {
                    self.overlay = Overlay::None;
                }
                _ => {}
            },
            Overlay::Prune => match key.code {
                KeyCode::Char('i') => {
                    self.overlay = Overlay::Confirm(ConfirmAction::PruneImages(false))
                }
                KeyCode::Char('I') => {
                    self.overlay = Overlay::Confirm(ConfirmAction::PruneImages(true))
                }
                KeyCode::Char('c') => {
                    self.overlay = Overlay::Confirm(ConfirmAction::PruneContainers)
                }
                KeyCode::Char('v') => {
                    self.overlay = Overlay::Confirm(ConfirmAction::PruneVolumes)
                }
                KeyCode::Char('b') => {
                    self.overlay = Overlay::Confirm(ConfirmAction::PruneBuildCache)
                }
                KeyCode::Char('a') => self.overlay = Overlay::Confirm(ConfirmAction::PruneAll),
                KeyCode::Esc | KeyCode::Char('q') => self.overlay = Overlay::None,
                _ => {}
            },
            Overlay::Logs | Overlay::Changelog | Overlay::Help => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.overlay = Overlay::None;
                    self.overlay_view.clear();
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.overlay_scroll = self
                        .overlay_scroll
                        .saturating_add(1)
                        .min(self.overlay_view.len().saturating_sub(1));
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.overlay_scroll = self.overlay_scroll.saturating_sub(1);
                }
                _ => {}
            },
            Overlay::UpdateDetails => match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {
                    self.overlay = Overlay::None;
                }
                KeyCode::Char('w') => {
                    self.overlay = Overlay::None;
                    self.open_changelog();
                }
                KeyCode::Char('u') => {
                    self.overlay = Overlay::None;
                    self.check_selected_update();
                }
                _ => {}
            },
            Overlay::ApplyProgress => {
                // Only allow dismissing once the operation has finished.
                if matches!(key.code, KeyCode::Esc | KeyCode::Char('q'))
                    && self.apply.as_ref().map(|a| a.finished).unwrap_or(true)
                {
                    self.overlay = Overlay::None;
                }
            }
            Overlay::Palette => match key.code {
                KeyCode::Esc => self.overlay = Overlay::None,
                KeyCode::Enter => {
                    let matches = self.palette_matches();
                    if let Some((_, action)) = matches.get(self.palette_index).copied() {
                        self.overlay = Overlay::None;
                        self.run_palette_action(action);
                    }
                }
                KeyCode::Backspace => {
                    self.palette_query.pop();
                    self.palette_index = 0;
                }
                KeyCode::Down => {
                    let len = self.palette_matches().len();
                    if len > 0 {
                        self.palette_index = (self.palette_index + 1) % len;
                    }
                }
                KeyCode::Up => {
                    let len = self.palette_matches().len();
                    if len > 0 {
                        self.palette_index = (self.palette_index + len - 1) % len;
                    }
                }
                KeyCode::Char(c) => {
                    self.palette_query.push(c);
                    self.palette_index = 0;
                }
                _ => {}
            },
            Overlay::None => {}
        }
        Ok(())
    }

    // ── Mouse ──────────────────────────────────────────────────────────────
    fn handle_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let hit = self
                    .regions
                    .iter()
                    .rev()
                    .find(|r| r.contains(mouse.column, mouse.row))
                    .map(|r| r.target);
                match hit {
                    Some(ClickTarget::Tab(tab)) => self.set_tab(tab),
                    Some(ClickTarget::Row(i)) => self.click_row(i),
                    Some(ClickTarget::Action(a)) => self.run_ui_action(a),
                    None => {
                        // A click outside every hit region dismisses a dismissable
                        // overlay (confirmations and in-flight updates must not be
                        // dismissed by a stray click).
                        let apply_running = matches!(self.overlay, Overlay::ApplyProgress)
                            && self.apply.as_ref().map(|a| !a.finished).unwrap_or(false);
                        if self.overlay != Overlay::None
                            && !matches!(self.overlay, Overlay::Confirm(_))
                            && !apply_running
                        {
                            self.overlay = Overlay::None;
                            self.overlay_view.clear();
                        }
                    }
                }
            }
            MouseEventKind::ScrollDown => {
                if self.overlay_is_scrollable() {
                    self.overlay_scroll = self
                        .overlay_scroll
                        .saturating_add(1)
                        .min(self.overlay_view.len().saturating_sub(1));
                } else if self.overlay == Overlay::None {
                    self.move_selection(1);
                }
            }
            MouseEventKind::ScrollUp => {
                if self.overlay_is_scrollable() {
                    self.overlay_scroll = self.overlay_scroll.saturating_sub(1);
                } else if self.overlay == Overlay::None {
                    self.move_selection(-1);
                }
            }
            _ => {}
        }
    }

    fn overlay_is_scrollable(&self) -> bool {
        matches!(self.overlay, Overlay::Logs | Overlay::Changelog | Overlay::Help)
    }

    /// Clicking a row selects it; clicking the already-selected row opens details.
    fn click_row(&mut self, i: usize) {
        if self.tab == Tab::Space {
            return;
        }
        if self.selected == i {
            self.overlay = Overlay::UpdateDetails;
        } else {
            self.selected = i;
        }
    }

    /// Run a UI action originating from a key binding or a mouse click.
    fn run_ui_action(&mut self, action: UiAction) {
        match action {
            UiAction::Refresh => {
                self.status_message = "Refreshing…".to_string();
                self.spawn_reload();
            }
            UiAction::CheckSelected => self.check_selected_update(),
            UiAction::CheckAll => self.check_all_updates(),
            UiAction::Details => self.overlay = Overlay::UpdateDetails,
            UiAction::Apply => self.apply_selected_update(),
            UiAction::StartStop => self.toggle_start_stop(),
            UiAction::Restart => self.restart_selected(),
            UiAction::Logs => self.open_logs(),
            UiAction::Changelog => self.open_changelog(),
            UiAction::Remove => self.confirm_remove_selected(),
            UiAction::Defer => self.defer_selected(),
            UiAction::OpenPrune => self.overlay = Overlay::Prune,
            UiAction::CycleTheme => self.cycle_theme(),
            UiAction::Help => self.overlay = Overlay::Help,
            UiAction::OpenPalette => {
                self.palette_query.clear();
                self.palette_index = 0;
                self.overlay = Overlay::Palette;
            }
            UiAction::Quit => self.should_quit = true,
            UiAction::PruneImages(all) => {
                self.overlay = Overlay::Confirm(ConfirmAction::PruneImages(all))
            }
            UiAction::PruneContainers => {
                self.overlay = Overlay::Confirm(ConfirmAction::PruneContainers)
            }
            UiAction::PruneVolumes => {
                self.overlay = Overlay::Confirm(ConfirmAction::PruneVolumes)
            }
            UiAction::PruneBuildCache => {
                self.overlay = Overlay::Confirm(ConfirmAction::PruneBuildCache)
            }
            UiAction::PruneAll => self.overlay = Overlay::Confirm(ConfirmAction::PruneAll),
            UiAction::ConfirmYes => {
                if let Overlay::Confirm(action) = self.overlay.clone() {
                    self.overlay = Overlay::None;
                    self.run_confirmed(action);
                }
            }
            UiAction::ConfirmNo => self.overlay = Overlay::None,
            UiAction::CloseOverlay => {
                self.overlay = Overlay::None;
                self.overlay_view.clear();
            }
            UiAction::PaletteItem(i) => {
                if let Some((_, act)) = self.palette_matches().get(i).copied() {
                    self.overlay = Overlay::None;
                    self.run_palette_action(act);
                }
            }
        }
    }

    // ── Actions ────────────────────────────────────────────────────────────
    fn set_tab(&mut self, tab: Tab) {
        self.tab = tab;
        self.selected = 0;
    }

    fn move_selection(&mut self, delta: i32) {
        let count = self.row_count();
        if count == 0 {
            return;
        }
        let cur = self.selected as i32;
        let next = (cur + delta).rem_euclid(count as i32);
        self.selected = next as usize;
    }

    fn clamp_selection(&mut self) {
        let count = self.row_count();
        if count == 0 {
            self.selected = 0;
        } else if self.selected >= count {
            self.selected = count - 1;
        }
    }

    fn cycle_theme(&mut self) {
        let next = Theme::next(self.theme.name);
        self.theme = Theme::by_name(next);
        self.config.theme = next.to_string();
        let _ = self.config.save();
        self.status_message = format!("theme: {next}");
    }

    fn spawn_reload(&self) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let images = client.list_images(false).await.unwrap_or_default();
            let containers = client.list_containers(true).await.unwrap_or_default();
            let usage = client.disk_usage().await.unwrap_or_default();
            let _ = tx.send(AppEvent::Reloaded(ReloadData {
                images,
                containers,
                usage,
            }));
        });
    }

    fn spawn_stats_refresh(&self) {
        // Sample stats for the selected running container only (cheap).
        if let Some(c) = self.containers.get(self.selected) {
            if !c.is_running() {
                return;
            }
            let client = self.client.clone();
            let tx = self.tx.clone();
            let id = c.id.clone();
            let image = c.image.clone();
            tokio::spawn(async move {
                if let Ok(stats) = client.stats_once(&id).await {
                    let _ = tx.send(AppEvent::Stats(image, stats));
                }
            });
        }
    }

    fn check_selected_update(&mut self) {
        if let Some(image) = self.selected_image_ref() {
            self.updates
                .insert(image.clone(), UpdateInfo::from_status(UpdateStatus::Checking));
            self.spawn_update_check(image);
        }
    }

    fn check_all_updates(&mut self) {
        let refs: Vec<String> = match self.tab {
            Tab::Images => self
                .images
                .iter()
                .filter_map(|i| i.primary_reference())
                .collect(),
            Tab::Containers => self.containers.iter().map(|c| c.image.clone()).collect(),
            Tab::Space => return,
        };
        self.status_message = format!("Checking {} images…", refs.len());
        for image in refs {
            if self.state.is_deferred(&image) {
                continue;
            }
            self.updates
                .insert(image.clone(), UpdateInfo::from_status(UpdateStatus::Checking));
            self.spawn_update_check(image);
        }
    }

    fn spawn_update_check(&self, image: String) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let info = client.check_update_detailed(&image).await;
            let _ = tx.send(AppEvent::UpdateResult(image, info));
        });
    }

    fn apply_selected_update(&mut self) {
        if self.tab != Tab::Containers {
            self.status_message =
                "switch to the Containers tab to apply an update".to_string();
            return;
        }
        let Some(c) = self.containers.get(self.selected) else {
            return;
        };
        self.overlay =
            Overlay::Confirm(ConfirmAction::ApplyUpdate(c.id.clone(), c.image.clone()));
    }

    fn open_logs(&mut self) {
        if self.tab != Tab::Containers {
            self.status_message = "logs are available on the Containers tab".to_string();
            return;
        }
        if let Some(c) = self.containers.get(self.selected) {
            self.overlay_title = format!("logs: {}", c.name);
            let client = self.client.clone();
            let tx = self.tx.clone();
            let id = c.id.clone();
            self.status_message = "loading logs…".to_string();
            tokio::spawn(async move {
                let lines = client.logs(&id, 200).await.unwrap_or_else(|e| vec![format!("error: {e}")]);
                let _ = tx.send(AppEvent::Logs(lines));
            });
        }
    }

    fn open_changelog(&mut self) {
        let Some(image) = self.selected_image_ref() else {
            return;
        };
        let pinned = self.state.changelog_sources.get(&image).cloned();
        let Some(repo) = registry::resolve_source(&image, pinned.as_deref()) else {
            self.status_message = format!("no changelog source known for {image}");
            return;
        };
        let token = self.config.github_token.clone();
        let tx = self.tx.clone();
        let title = format!("changelog: {repo}");
        self.status_message = format!("fetching {repo} releases…");
        tokio::spawn(async move {
            match registry::fetch_releases(&repo, token.as_deref(), 5).await {
                Ok(releases) => {
                    let _ = tx.send(AppEvent::Changelog(title, releases));
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::Message(format!("changelog error: {e}")));
                }
            }
        });
    }

    fn toggle_start_stop(&mut self) {
        if self.tab != Tab::Containers {
            return;
        }
        if let Some(c) = self.containers.get(self.selected) {
            let client = self.client.clone();
            let tx = self.tx.clone();
            let id = c.id.clone();
            let name = c.name.clone();
            let running = c.is_running();
            tokio::spawn(async move {
                let result = if running {
                    client.stop_container(&id).await
                } else {
                    client.start_container(&id).await
                };
                let msg = match result {
                    Ok(_) => format!("{} {}", if running { "stopped" } else { "started" }, name),
                    Err(e) => format!("error: {e}"),
                };
                let _ = tx.send(AppEvent::Message(msg));
            });
            self.schedule_reload();
        }
    }

    fn restart_selected(&mut self) {
        if self.tab != Tab::Containers {
            return;
        }
        if let Some(c) = self.containers.get(self.selected) {
            let client = self.client.clone();
            let tx = self.tx.clone();
            let id = c.id.clone();
            let name = c.name.clone();
            tokio::spawn(async move {
                let msg = match client.restart_container(&id).await {
                    Ok(_) => format!("restarted {name}"),
                    Err(e) => format!("error: {e}"),
                };
                let _ = tx.send(AppEvent::Message(msg));
            });
            self.schedule_reload();
        }
    }

    fn confirm_remove_selected(&mut self) {
        if self.tab != Tab::Containers {
            return;
        }
        if let Some(c) = self.containers.get(self.selected) {
            self.overlay =
                Overlay::Confirm(ConfirmAction::RemoveContainer(c.id.clone(), c.name.clone()));
        }
    }

    fn defer_selected(&mut self) {
        if let Some(image) = self.selected_image_ref() {
            // Defer for 30 days.
            let until = chrono::Utc::now() + chrono::Duration::days(30);
            self.state.deferred.insert(image.clone(), until);
            let _ = self.state.save();
            self.status_message = format!("deferred {image} for 30 days");
        }
    }

    fn run_confirmed(&mut self, action: ConfirmAction) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        match action {
            ConfirmAction::RemoveContainer(id, name) => {
                tokio::spawn(async move {
                    let msg = match client.remove_container(&id).await {
                        Ok(_) => format!("removed {name}"),
                        Err(e) => format!("error: {e}"),
                    };
                    let _ = tx.send(AppEvent::Message(msg));
                });
                self.schedule_reload();
            }
            ConfirmAction::PruneImages(all) => {
                tokio::spawn(async move {
                    let msg = match client.prune_images(all).await {
                        Ok(freed) => format!("images pruned · {}", crate::util::format_bytes(freed)),
                        Err(e) => format!("error: {e}"),
                    };
                    let _ = tx.send(AppEvent::Message(msg));
                });
                self.schedule_reload();
            }
            ConfirmAction::PruneContainers => {
                tokio::spawn(async move {
                    let msg = match client.prune_containers().await {
                        Ok(freed) => {
                            format!("containers pruned · {}", crate::util::format_bytes(freed))
                        }
                        Err(e) => format!("error: {e}"),
                    };
                    let _ = tx.send(AppEvent::Message(msg));
                });
                self.schedule_reload();
            }
            ConfirmAction::PruneVolumes => {
                tokio::spawn(async move {
                    let msg = match client.prune_volumes().await {
                        Ok(freed) => format!("volumes pruned · {}", crate::util::format_bytes(freed)),
                        Err(e) => format!("error: {e}"),
                    };
                    let _ = tx.send(AppEvent::Message(msg));
                });
                self.schedule_reload();
            }
            ConfirmAction::PruneBuildCache => {
                tokio::spawn(async move {
                    let msg = match client.prune_build_cache().await {
                        Ok(freed) => {
                            format!("build cache pruned · {}", crate::util::format_bytes(freed))
                        }
                        Err(e) => format!("error: {e}"),
                    };
                    let _ = tx.send(AppEvent::Message(msg));
                });
                self.schedule_reload();
            }
            ConfirmAction::PruneAll => {
                tokio::spawn(async move {
                    let mut freed = 0i64;
                    freed += client.prune_containers().await.unwrap_or(0);
                    freed += client.prune_images(true).await.unwrap_or(0);
                    freed += client.prune_volumes().await.unwrap_or(0);
                    freed += client.prune_build_cache().await.unwrap_or(0);
                    let _ = tx.send(AppEvent::Message(format!(
                        "pruned everything unused · {}",
                        crate::util::format_bytes(freed)
                    )));
                });
                self.schedule_reload();
            }
            ConfirmAction::ApplyUpdate(id, image) => {
                self.apply = Some(ApplyState::new(format!("Updating {image}")));
                self.overlay = Overlay::ApplyProgress;
                let tx2 = tx.clone();
                tokio::spawn(async move {
                    let _ = client
                        .apply_update(&id, &image, move |p| {
                            let _ = tx2.send(AppEvent::Apply(p));
                        })
                        .await;
                });
            }
        }
    }

    /// Trigger a reload shortly after a mutating action so the UI catches up.
    fn schedule_reload(&self) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            let images = client.list_images(false).await.unwrap_or_default();
            let containers = client.list_containers(true).await.unwrap_or_default();
            let usage = client.disk_usage().await.unwrap_or_default();
            let _ = tx.send(AppEvent::Reloaded(ReloadData {
                images,
                containers,
                usage,
            }));
        });
    }

    /// Drop cached update results whose local image id has since changed.
    fn invalidate_stale_updates(&mut self) {
        let mut id_by_ref: HashMap<String, String> = HashMap::new();
        for img in &self.images {
            if let Some(reference) = img.primary_reference() {
                id_by_ref.insert(reference, img.id.clone());
            }
        }
        let stale: Vec<String> = self
            .updates
            .iter()
            .filter_map(|(reference, info)| match (&info.local_id, id_by_ref.get(reference)) {
                (Some(cached), Some(current)) if cached != current => Some(reference.clone()),
                _ => None,
            })
            .collect();
        for reference in stale {
            self.updates.remove(&reference);
            self.state.update_cache.remove(&reference);
        }
    }

    /// Render release notes into styled, markdown-formatted lines.
    fn changelog_lines(&self, releases: &[registry::Release]) -> Vec<Line<'static>> {
        let theme = &self.theme;
        let mut lines: Vec<Line<'static>> = Vec::new();
        if releases.is_empty() {
            lines.push(Line::from(Span::styled(
                "No releases found.".to_string(),
                Style::default().fg(theme.dim),
            )));
            return lines;
        }
        for r in releases {
            let title = r.name.clone().unwrap_or_else(|| r.tag_name.clone());
            let date = r
                .published_at
                .clone()
                .unwrap_or_default()
                .chars()
                .take(10)
                .collect::<String>();
            lines.push(Line::from(Span::styled(
                format!("── {title}  ({date})"),
                Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
            )));
            if let Some(url) = &r.html_url {
                lines.push(Line::from(Span::styled(
                    url.clone(),
                    Style::default().fg(theme.dim),
                )));
            }
            if let Some(body) = &r.body {
                lines.extend(crate::md::render(body, theme));
            }
            lines.push(Line::from(String::new()));
        }
        lines
    }
}

/// Enter raw mode + alternate screen with mouse capture.
fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

/// Restore the terminal to its normal state.
fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}
