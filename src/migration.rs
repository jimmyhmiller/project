use super::*;
use anyhow::{Context, Result, bail};
use chrono::Utc;
use crossterm::event::{self, Event, KeyCode};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use serde::{Deserialize, Serialize};
use std::{fs::OpenOptions, io::Write};

const OWNER: &str = "jimmyhmiller";
const INITIAL_COMMIT: &str = "Migrated from playground";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Stage {
    Pending,
    LocalReady,
    RepoCreated,
    Pushed,
}

impl Stage {
    fn label(self) -> &'static str {
        match self {
            Self::Pending => "ready to move",
            Self::LocalReady => "local commit ready",
            Self::RepoCreated => "GitHub repo created",
            Self::Pushed => "pushed",
        }
    }
    fn next_action(self) -> &'static str {
        match self {
            Self::Pending => "move code + create initial commit",
            Self::LocalReady => "create and link GitHub repository",
            Self::RepoCreated => "push main to GitHub",
            Self::Pushed => "complete",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MigrationState {
    project_id: u64,
    name: String,
    stage: Stage,
    updated_at: String,
}

fn state_json_path() -> PathBuf {
    root().join("data/migration-state.json")
}
fn state_kdl_path() -> PathBuf {
    root().join("migration-state.kdl")
}
fn journal_path() -> PathBuf {
    root().join("migration-actions.kdl")
}

pub fn preview(id: u64) -> Result<()> {
    let projects: Vec<Project> = load()?.into_iter().filter(|p| p.elevate).collect();
    let project = projects
        .iter()
        .find(|p| p.id == id)
        .context("unknown project id")?;
    let states = load_states(&projects)?;
    let state = states
        .iter()
        .find(|s| s.project_id == id)
        .context("project is not selected")?;
    println!(
        "{}\n  stage: {}\n  next: {}\n  source: {}\n  destination: {}/{}\n  GitHub: {}/{} ({})\n  experiment: {}\n  README follow-up: {}",
        project.name,
        state.stage.label(),
        state.stage.next_action(),
        project.source.as_deref().unwrap_or("missing"),
        PROJECTS_ROOT,
        project.name,
        OWNER,
        project.name,
        project.visibility,
        project.experiment,
        project.readme
    );
    Ok(())
}

pub fn run_tui() -> Result<()> {
    let projects: Vec<Project> = load()?.into_iter().filter(|p| p.elevate).collect();
    if projects.is_empty() {
        bail!("no projects are selected in the dashboard")
    }
    let mut states = load_states(&projects)?;
    journal(
        None,
        "tui-open",
        "ok",
        &format!("{} selected projects", projects.len()),
    )?;
    let mut terminal = ratatui::init();
    let result = app_loop(&mut terminal, &projects, &mut states);
    ratatui::restore();
    let detail = result
        .as_ref()
        .map(|_| "normal exit")
        .unwrap_or("error exit");
    let _ = journal(
        None,
        "tui-close",
        if result.is_ok() { "ok" } else { "failed" },
        detail,
    );
    result
}

pub fn publish_all(delay_seconds: u64) -> Result<()> {
    let projects: Vec<Project> = load()?.into_iter().filter(|p| p.elevate).collect();
    let mut states = load_states(&projects)?;
    journal(
        None,
        "publish-all",
        "attempt",
        &format!("{} projects; {delay_seconds}s GitHub delay", projects.len()),
    )?;
    for index in 0..projects.len() {
        while states[index].stage != Stage::Pushed {
            let stage = states[index].stage;
            if matches!(stage, Stage::LocalReady | Stage::RepoCreated) && delay_seconds > 0 {
                println!(
                    "Waiting {delay_seconds}s before GitHub action for {}…",
                    projects[index].name
                );
                std::thread::sleep(Duration::from_secs(delay_seconds));
            }
            println!(
                "[{}/{}] {}: {}",
                index + 1,
                projects.len(),
                projects[index].name,
                stage.next_action()
            );
            journal(
                Some(&projects[index]),
                stage.next_action(),
                "attempt",
                "batch",
            )?;
            match execute_stage(&projects[index], stage) {
                Ok(detail) => {
                    states[index].stage = next_stage(stage);
                    states[index].updated_at = Utc::now().to_rfc3339();
                    save_states(&states)?;
                    update_registry(&projects[index], states[index].stage)?;
                    journal(Some(&projects[index]), stage.next_action(), "ok", &detail)?;
                    println!("  {detail}");
                    if states[index].stage == Stage::Pushed && delay_seconds > 0 {
                        println!("  Cooling down {delay_seconds}s after push…");
                        std::thread::sleep(Duration::from_secs(delay_seconds));
                    }
                }
                Err(error) => {
                    journal(
                        Some(&projects[index]),
                        stage.next_action(),
                        "failed",
                        &error.to_string(),
                    )?;
                    journal(
                        None,
                        "publish-all",
                        "failed",
                        &format!("{}: {error:#}", projects[index].name),
                    )?;
                    return Err(error)
                        .with_context(|| format!("batch stopped at {}", projects[index].name));
                }
            }
        }
    }
    journal(None, "publish-all", "ok", "all selected projects pushed")?;
    Ok(())
}

fn app_loop(
    terminal: &mut DefaultTerminal,
    projects: &[Project],
    states: &mut [MigrationState],
) -> Result<()> {
    let mut selected = states
        .iter()
        .position(|s| s.stage != Stage::Pushed)
        .unwrap_or(0);
    let mut confirm = false;
    let mut fast_forward = false;
    let mut acknowledge = false;
    let mut message = String::from("Navigate with ↑/↓. Press a to prepare the next step.");
    loop {
        terminal.draw(|f| draw(f, projects, states, selected, confirm, &message))?;
        if let Event::Key(key) = event::read()? {
            if acknowledge {
                match key.code {
                    KeyCode::Enter => {
                        acknowledge = false;
                        message =
                            "Checkpoint acknowledged. Press a when you want the next step.".into();
                    }
                    KeyCode::Char('q') => return Ok(()),
                    _ => {}
                }
                continue;
            }
            match key.code {
                KeyCode::Char('q') => return Ok(()),
                KeyCode::Up | KeyCode::Char('k') => {
                    selected = selected.saturating_sub(1);
                    confirm = false
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = (selected + 1).min(states.len() - 1);
                    confirm = false
                }
                KeyCode::Char('o') => {
                    let dest = Path::new(PROJECTS_ROOT).join(&projects[selected].name);
                    let target = if dest.exists() {
                        dest
                    } else {
                        PathBuf::from(projects[selected].source.as_deref().unwrap_or(PLAYGROUND))
                    };
                    let _ = Command::new("open").arg(&target).status();
                    journal(
                        Some(&projects[selected]),
                        "open-folder",
                        "ok",
                        &target.display().to_string(),
                    )?;
                    message = format!("Opened {}", target.display())
                }
                KeyCode::Char('a') if states[selected].stage != Stage::Pushed => {
                    confirm = true;
                    fast_forward = false;
                    message = format!(
                        "Confirm: press y to {}. Any other key cancels.",
                        states[selected].stage.next_action()
                    )
                }
                KeyCode::Char('f')
                    if matches!(states[selected].stage, Stage::Pending | Stage::LocalReady) =>
                {
                    confirm = true;
                    fast_forward = true;
                    message = "Confirm: press y to move/commit and create/link the repo. This will NOT push. Any other key cancels.".into();
                }
                KeyCode::Char('y') if confirm => {
                    confirm = false;
                    let stop_before_push = fast_forward;
                    let mut details = Vec::new();
                    loop {
                        let stage = states[selected].stage;
                        if stage == Stage::Pushed
                            || (stop_before_push && stage == Stage::RepoCreated)
                        {
                            break;
                        }
                        message = format!(
                            "RUNNING: {}. Please wait; input is locked.",
                            stage.next_action()
                        );
                        terminal.draw(|f| draw(f, projects, states, selected, false, &message))?;
                        journal(
                            Some(&projects[selected]),
                            stage.next_action(),
                            "attempt",
                            "",
                        )?;
                        match execute_stage(&projects[selected], stage) {
                            Ok(detail) => {
                                states[selected].stage = next_stage(stage);
                                states[selected].updated_at = Utc::now().to_rfc3339();
                                save_states(states)?;
                                update_registry(&projects[selected], states[selected].stage)?;
                                journal(
                                    Some(&projects[selected]),
                                    stage.next_action(),
                                    "ok",
                                    &detail,
                                )?;
                                details.push(detail);
                            }
                            Err(e) => {
                                journal(
                                    Some(&projects[selected]),
                                    stage.next_action(),
                                    "failed",
                                    &e.to_string(),
                                )?;
                                message = format!("FAILED: {e:#}. Press Enter to acknowledge.");
                                break;
                            }
                        }
                        if !stop_before_push {
                            break;
                        }
                    }
                    if !details.is_empty() {
                        let boundary = if stop_before_push {
                            "Stopped before push."
                        } else {
                            "Checkpoint complete."
                        };
                        message = format!(
                            "Success: {} {boundary} Inspect it, then press Enter to unlock.",
                            details.join("; ")
                        );
                    }
                    acknowledge = true;
                    drain_input()?;
                }
                _ => {
                    if confirm {
                        confirm = false;
                        message = "Cancelled; no migration action taken.".into()
                    }
                }
            }
        }
    }
}

fn next_stage(stage: Stage) -> Stage {
    match stage {
        Stage::Pending => Stage::LocalReady,
        Stage::LocalReady => Stage::RepoCreated,
        Stage::RepoCreated => Stage::Pushed,
        Stage::Pushed => Stage::Pushed,
    }
}

fn draw(
    f: &mut Frame,
    projects: &[Project],
    states: &[MigrationState],
    selected: usize,
    confirm: bool,
    message: &str,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(12),
            Constraint::Length(8),
            Constraint::Length(3),
        ])
        .split(f.area());
    let done = states.iter().filter(|s| s.stage == Stage::Pushed).count();
    f.render_widget(
        Paragraph::new(format!(
            "Project migration  ·  {done}/{} pushed  ·  state saved after every step",
            states.len()
        ))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" resumable migration "),
        ),
        chunks[0],
    );
    let items: Vec<ListItem> = projects
        .iter()
        .zip(states)
        .enumerate()
        .map(|(i, (p, s))| {
            let marker = if i == selected { "▶" } else { " " };
            let exp = if p.experiment { " · experiment" } else { "" };
            ListItem::new(format!(
                "{marker} {:<28}  {:<20}{exp}",
                p.name,
                s.stage.label()
            ))
        })
        .collect();
    let mut list_state = ListState::default().with_selected(Some(selected));
    f.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" selected projects "),
            )
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::LightGreen)
                    .add_modifier(Modifier::BOLD),
            ),
        chunks[1],
        &mut list_state,
    );
    let p = &projects[selected];
    let s = &states[selected];
    let detail = vec![
        Line::from(vec![
            Span::styled(&p.name, Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!("  [{}]", s.stage.label())),
        ]),
        Line::from(format!("next: {}", s.stage.next_action())),
        Line::from(format!(
            "from: {}",
            p.source.as_deref().unwrap_or("missing")
        )),
        Line::from(format!("to:   {PROJECTS_ROOT}/{}", p.name)),
        Line::from(format!(
            "repo: {OWNER}/{} ({}) · experiment={} · README follow-up={}",
            p.name, p.visibility, p.experiment, p.readme
        )),
    ];
    f.render_widget(
        Paragraph::new(detail)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" inspection ")),
        chunks[2],
    );
    let style = if confirm {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    f.render_widget(
        Paragraph::new(message).style(style).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" ↑/↓ navigate · a next · f all except push · y confirm · o open · q quit "),
        ),
        chunks[3],
    );
}

fn execute_stage(project: &Project, stage: Stage) -> Result<String> {
    match stage {
        Stage::Pending => move_and_commit(project),
        Stage::LocalReady => create_repo(project),
        Stage::RepoCreated => push(project),
        Stage::Pushed => Ok("already complete".into()),
    }
}

fn move_and_commit(p: &Project) -> Result<String> {
    let source = PathBuf::from(p.source.as_ref().context("missing source")?);
    let dest = Path::new(PROJECTS_ROOT).join(&p.name);
    if source.exists() && dest.exists() {
        bail!("both source and destination exist; refusing to choose between them")
    }
    if source.exists() {
        let canonical = source.canonicalize()?;
        if !canonical.starts_with(PLAYGROUND) {
            bail!("source is outside PlayGround: {}", canonical.display())
        }
        fs::rename(&source, &dest)
            .with_context(|| format!("moving {} to {}", source.display(), dest.display()))?;
        journal(
            Some(p),
            "move-directory",
            "ok",
            &format!("{} -> {}", source.display(), dest.display()),
        )?
    } else if !dest.exists() {
        bail!("neither source nor destination exists")
    }
    if dest.join(".git").exists() {
        fs::remove_dir_all(dest.join(".git"))?;
        journal(
            Some(p),
            "remove-nested-git",
            "ok",
            "removed migrated .git metadata",
        )?
    }
    let nested_git: Vec<PathBuf> = WalkDir::new(&dest)
        .min_depth(2)
        .follow_links(false)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_dir() && entry.file_name() == ".git")
        .map(|entry| entry.into_path())
        .collect();
    for git_dir in nested_git {
        fs::remove_dir_all(&git_dir)?;
        journal(
            Some(p),
            "remove-nested-git",
            "ok",
            &git_dir.display().to_string(),
        )?;
    }
    logged_command(
        p,
        "git-init",
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&dest),
    )?;
    if !dest.join("project.md").exists() {
        fs::write(
            dest.join("project.md"),
            format!("# {}\n\nMigrated from `{}`.\n", p.name, source.display()),
        )?;
        journal(Some(p), "create-project-md", "ok", "project.md")?
    }
    if p.experiment {
        prepend_experiment_notice(&dest)?;
        journal(
            Some(p),
            "experiment-notice",
            "ok",
            "applied when README exists",
        )?
    }
    ensure_gitignore(p, &dest)?;
    logged_command(
        p,
        "git-add",
        Command::new("git").args(["add", "."]).current_dir(&dest),
    )?;
    let head = Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(&dest)
        .output()?;
    if !head.status.success() {
        logged_command(
            p,
            "git-commit",
            Command::new("git")
                .args(["commit", "-m", INITIAL_COMMIT])
                .current_dir(&dest),
        )?
    }
    Ok(format!("moved and committed at {}", dest.display()))
}

fn create_repo(p: &Project) -> Result<String> {
    let dest = Path::new(PROJECTS_ROOT).join(&p.name);
    if !dest.join(".git").exists() {
        bail!("local Git repository is missing")
    }
    let repo = format!("{OWNER}/{}", p.name);
    let exists = Command::new("gh")
        .args(["repo", "view", &repo])
        .output()?
        .status
        .success();
    if !exists {
        let visibility = if p.visibility == "public" {
            "--public"
        } else {
            "--private"
        };
        logged_command(
            p,
            "gh-repo-create",
            Command::new("gh").args(["repo", "create", &repo, visibility]),
        )?
    }
    let remote = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(&dest)
        .output()?;
    let expected = format!("git@github.com:{repo}.git");
    if remote.status.success() {
        let actual = String::from_utf8_lossy(&remote.stdout).trim().to_owned();
        if actual != expected && actual != format!("https://github.com/{repo}.git") {
            bail!("origin already points to {actual}")
        }
    } else {
        logged_command(
            p,
            "git-remote-add",
            Command::new("git")
                .args(["remote", "add", "origin", &expected])
                .current_dir(&dest),
        )?
    }
    Ok(format!("created and linked https://github.com/{repo}"))
}

fn push(p: &Project) -> Result<String> {
    let dest = Path::new(PROJECTS_ROOT).join(&p.name);
    reject_tracked_artifacts(&dest)?;
    logged_command(
        p,
        "git-push",
        Command::new("git")
            .args(["push", "-u", "origin", "main"])
            .current_dir(&dest),
    )?;
    Ok(format!("pushed {OWNER}/{}", p.name))
}

fn ensure_gitignore(p: &Project, dest: &Path) -> Result<()> {
    let inherited = fs::read_to_string(Path::new(PLAYGROUND).join(".gitignore"))
        .context("reading PlayGround's top-level .gitignore")?;
    let path = dest.join(".gitignore");
    let project_specific = fs::read_to_string(&path).unwrap_or_default();
    let mut updated = String::new();
    for line in inherited.lines().chain(project_specific.lines()) {
        if !updated.lines().any(|existing| existing == line) {
            updated.push_str(line);
            updated.push('\n');
        }
    }
    let mut patterns = vec![".DS_Store", ".claude/settings.local.json"];
    if dest.join("Cargo.toml").exists() {
        patterns.push("/target/");
    }
    if dest.join("package.json").exists() {
        patterns.extend(["/node_modules/", "/.next/", "/dist/"]);
    }
    if dest.join("Package.swift").exists() {
        patterns.push("/.build/");
    }
    if dest.join("pyproject.toml").exists()
        || dest.join("setup.py").exists()
        || dest.join("requirements.txt").exists()
    {
        patterns.extend(["__pycache__/", "*.pyc", "/.venv/"]);
    }
    if dest.join("CMakeLists.txt").exists() {
        patterns.push("/build/");
    }
    if dest.join("build.gradle").exists() || dest.join("build.gradle.kts").exists() {
        patterns.extend(["/.gradle/", "/build/"]);
    }
    for pattern in patterns {
        if !updated.lines().any(|line| line.trim() == pattern) {
            updated.push_str(pattern);
            updated.push('\n');
        }
    }
    if updated != project_specific {
        fs::write(&path, updated)?;
        journal(
            Some(p),
            "update-gitignore",
            "ok",
            &path.display().to_string(),
        )?;
    }
    Ok(())
}

fn reject_tracked_artifacts(dest: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["ls-files"])
        .current_dir(dest)
        .output()?;
    if !output.status.success() {
        bail!("could not inspect tracked files before push")
    }
    let tracked = String::from_utf8_lossy(&output.stdout);
    let offenders = tracked
        .lines()
        .filter(|path| {
            path.starts_with("target/")
                || path.starts_with("node_modules/")
                || path.starts_with(".next/")
                || path.starts_with(".build/")
        })
        .take(5)
        .collect::<Vec<_>>();
    if !offenders.is_empty() {
        bail!(
            "generated artifacts are tracked; clean the initial commit before pushing (examples: {})",
            offenders.join(", ")
        )
    }
    Ok(())
}

fn drain_input() -> Result<()> {
    while event::poll(Duration::ZERO)? {
        let _ = event::read()?;
    }
    Ok(())
}

fn logged_command(p: &Project, action: &str, command: &mut Command) -> Result<()> {
    let rendered = format!("{command:?}");
    journal(Some(p), action, "attempt", &rendered)?;
    let output = command
        .output()
        .with_context(|| format!("running {rendered}"))?;
    if output.status.success() {
        journal(
            Some(p),
            action,
            "ok",
            String::from_utf8_lossy(&output.stdout).trim(),
        )?;
        Ok(())
    } else {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        journal(Some(p), action, "failed", &detail)?;
        bail!("{action} failed: {detail}")
    }
}

fn load_states(projects: &[Project]) -> Result<Vec<MigrationState>> {
    let old: HashMap<u64, MigrationState> = fs::read_to_string(state_json_path())
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<MigrationState>>(&s).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|s| (s.project_id, s))
        .collect();
    let now = Utc::now().to_rfc3339();
    let states = projects
        .iter()
        .map(|p| {
            old.get(&p.id).cloned().unwrap_or(MigrationState {
                project_id: p.id,
                name: p.name.clone(),
                stage: Stage::Pending,
                updated_at: now.clone(),
            })
        })
        .collect::<Vec<_>>();
    save_states(&states)?;
    Ok(states)
}

fn save_states(states: &[MigrationState]) -> Result<()> {
    fs::create_dir_all(root().join("data"))?;
    let temp = state_json_path().with_extension("json.tmp");
    fs::write(&temp, serde_json::to_string_pretty(states)?)?;
    fs::rename(temp, state_json_path())?;
    let mut kdl = String::from("migration-state version=1 {\n");
    for s in states {
        kdl.push_str(&format!(
            "  project {} name={} stage={} updated-at={}\n",
            s.project_id,
            q(&s.name),
            q(s.stage.label()),
            q(&s.updated_at)
        ))
    }
    kdl.push_str("}\n");
    fs::write(state_kdl_path(), kdl)?;
    Ok(())
}

fn update_registry(project: &Project, stage: Stage) -> Result<()> {
    let mut registry = load()?;
    let entry = registry
        .iter_mut()
        .find(|entry| entry.id == project.id)
        .context("project disappeared from registry")?;
    entry.status = match stage {
        Stage::Pending => "selected",
        Stage::LocalReady => "local-ready",
        Stage::RepoCreated => "repo-created",
        Stage::Pushed => "migrated",
    }
    .into();
    if stage != Stage::Pending {
        entry.source = Some(
            Path::new(PROJECTS_ROOT)
                .join(&project.name)
                .to_string_lossy()
                .into_owned(),
        );
        entry.in_playground = false;
        entry.exists = true;
    }
    write_all(&registry)
}

fn journal(project: Option<&Project>, action: &str, result: &str, detail: &str) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(journal_path())?;
    let (id, name) = project
        .map(|p| (p.id, p.name.as_str()))
        .unwrap_or((0, "project-tool"));
    writeln!(
        file,
        "event at={} project-id={} project={} action={} result={} detail={}",
        q(&Utc::now().to_rfc3339()),
        id,
        q(name),
        q(action),
        q(result),
        q(detail)
    )?;
    file.sync_data()?;
    Ok(())
}
