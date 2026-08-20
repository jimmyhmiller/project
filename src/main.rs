use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime},
};
use walkdir::{DirEntry, WalkDir};

mod migration;
mod preflight;
mod quality;

const JIM_PROJECTS: &str = "/Users/jimmyhmiller/.jim/projects.json";
const JIM_STATE: &str = "/Users/jimmyhmiller/.jim/projects";
const PLAYGROUND: &str = "/Users/jimmyhmiller/Documents/Code/PlayGround";
const PROJECTS_ROOT: &str = "/Users/jimmyhmiller/Documents/Code/projects";
const EXPERIMENT_NOTICE: &str = "This is experimental software. It probably doesn't work.";

#[derive(Parser)]
#[command(name = "project", about = "Create, register, and graduate projects")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Rebuild the KDL registry and dashboard data from Jim.
    Scan,
    /// Serve the migration dashboard locally.
    Dashboard {
        #[arg(long, default_value_t = 4411)]
        port: u16,
    },
    /// Audit every selected migration without changing projects or repositories.
    Preflight,
    /// Run the local quality worker for every project, or one project by id.
    Quality { id: Option<u64> },
    /// Resume and publish every selected project, stopping on the first failure.
    PublishAll {
        #[arg(long, default_value_t = 10)]
        delay_seconds: u64,
    },
    /// Open the staged migration TUI, or preview a single project by id.
    Migrate { id: Option<u64> },
    /// Scaffold and register a brand-new project.
    New {
        name: String,
        #[arg(long, conflicts_with = "public")]
        private: bool,
        #[arg(long)]
        public: bool,
        #[arg(long)]
        readme: bool,
        /// Scaffold and register locally without calling GitHub.
        #[arg(long)]
        local_only: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Project {
    id: u64,
    jim_name: String,
    name: String,
    source: Option<String>,
    in_playground: bool,
    exists: bool,
    hidden: bool,
    last_edit_at: Option<String>,
    last_focus_at: Option<String>,
    elevate: bool,
    visibility: String,
    readme: bool,
    #[serde(default)]
    experiment: bool,
    status: String,
    #[serde(default = "jim_origin")]
    origin: String,
}

fn jim_origin() -> String {
    "jim".into()
}

#[derive(Deserialize)]
struct JimRoot {
    projects: Vec<JimProject>,
}
#[derive(Deserialize)]
struct JimProject {
    id: u64,
    name: String,
    default_cwd: Option<String>,
    hidden: bool,
}
#[derive(Default, Deserialize)]
struct JimState {
    last_edit_at: Option<String>,
    last_focus_at: Option<String>,
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
fn data_path() -> PathBuf {
    root().join("data/projects.json")
}
fn decisions_path() -> PathBuf {
    root().join("data/decisions.json")
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Commands::Scan => {
            let projects = scan()?;
            write_all(&projects)?;
            println!(
                "Indexed {} projects ({} PlayGround candidates, {} recent filesystem discoveries).",
                projects.len(),
                projects.iter().filter(|p| p.in_playground).count(),
                projects.iter().filter(|p| p.origin == "filesystem").count()
            );
        }
        Commands::Dashboard { port } => dashboard(port)?,
        Commands::Preflight => preflight::run()?,
        Commands::Quality { id } => quality::run(id)?,
        Commands::PublishAll { delay_seconds } => migration::publish_all(delay_seconds)?,
        Commands::Migrate { id } => match id {
            Some(id) => migration::preview(id)?,
            None => migration::run_tui()?,
        },
        Commands::New {
            name,
            private,
            public: _,
            readme,
            local_only,
        } => new_project(&name, !private, readme, local_only)?,
    }
    Ok(())
}

fn scan() -> Result<Vec<Project>> {
    let raw = fs::read_to_string(JIM_PROJECTS).context("reading ~/.jim/projects.json")?;
    let jim: JimRoot = serde_json::from_str(&raw).context("parsing Jim projects")?;
    let prior: HashMap<u64, Project> = fs::read_to_string(decisions_path())
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<Project>>(&s).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|p| (p.id, p))
        .collect();
    let mut out = Vec::new();
    for item in jim.projects {
        let state_path = PathBuf::from(JIM_STATE)
            .join(item.id.to_string())
            .join("state.json");
        let state: JimState = fs::read_to_string(state_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let in_playground = item
            .default_cwd
            .as_deref()
            .is_some_and(|p| Path::new(p).starts_with(PLAYGROUND));
        let old = prior.get(&item.id);
        let suggested = item
            .default_cwd
            .as_deref()
            .and_then(|p| Path::new(p).file_name())
            .and_then(|s| s.to_str())
            .unwrap_or(&item.name)
            .to_owned();
        out.push(Project {
            id: item.id,
            jim_name: item.name,
            name: old.map(|p| p.name.clone()).unwrap_or(suggested),
            exists: item
                .default_cwd
                .as_deref()
                .is_some_and(|p| Path::new(p).is_dir()),
            source: item.default_cwd,
            in_playground,
            hidden: item.hidden,
            last_edit_at: state.last_edit_at,
            last_focus_at: state.last_focus_at,
            elevate: old.map(|p| p.elevate).unwrap_or(false),
            visibility: old
                .map(|p| p.visibility.clone())
                .unwrap_or_else(|| "public".into()),
            readme: old.map(|p| p.readme).unwrap_or(false),
            experiment: old.map(|p| p.experiment).unwrap_or(false),
            status: old
                .map(|p| p.status.clone())
                .unwrap_or_else(|| "indexed".into()),
            origin: "jim".into(),
        });
    }
    add_recent_playground_projects(&mut out, &prior)?;
    out.sort_by(|a, b| {
        let a_activity = a.last_edit_at.as_ref().or(a.last_focus_at.as_ref());
        let b_activity = b.last_edit_at.as_ref().or(b.last_focus_at.as_ref());
        b_activity
            .cmp(&a_activity)
            .then_with(|| a.jim_name.cmp(&b.jim_name))
    });
    Ok(out)
}

fn write_all(projects: &[Project]) -> Result<()> {
    fs::create_dir_all(root().join("data"))?;
    fs::write(data_path(), serde_json::to_string_pretty(projects)?)?;
    fs::write(decisions_path(), serde_json::to_string_pretty(projects)?)?;
    fs::write(root().join("projects.kdl"), to_kdl(projects))?;
    Ok(())
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}
fn q(s: &str) -> String {
    format!("\"{}\"", esc(s))
}
fn to_kdl(projects: &[Project]) -> String {
    let mut s = String::from(
        "registry version=1 projects-root=\"/Users/jimmyhmiller/Documents/Code/projects\" {\n",
    );
    s.push_str(&format!("  experiment-notice {}\n", q(EXPERIMENT_NOTICE)));
    for p in projects {
        s.push_str(&format!("  project {} name={} jim-name={} origin={} in-playground={} exists={} hidden={} elevate={} visibility={} readme={} experiment={} status={} {{\n", p.id, q(&p.name), q(&p.jim_name), q(&p.origin), p.in_playground, p.exists, p.hidden, p.elevate, q(&p.visibility), p.readme, p.experiment, q(&p.status)));
        if let Some(v) = &p.source {
            s.push_str(&format!("    source {}\n", q(v)));
        }
        if let Some(v) = &p.last_edit_at {
            s.push_str(&format!("    last-edit {}\n", q(v)));
        }
        if let Some(v) = &p.last_focus_at {
            s.push_str(&format!("    last-focus {}\n", q(v)));
        }
        s.push_str("  }\n");
    }
    s.push_str("}\n");
    s
}

fn add_recent_playground_projects(
    out: &mut Vec<Project>,
    prior: &HashMap<u64, Project>,
) -> Result<()> {
    let known: HashSet<PathBuf> = out
        .iter()
        .filter_map(|p| p.source.as_ref())
        .map(PathBuf::from)
        .collect();
    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(90 * 24 * 60 * 60))
        .unwrap();
    let playground = Path::new(PLAYGROUND);
    let mut boundaries = Vec::new();
    for entry in fs::read_dir(playground)? {
        let path = entry?.path();
        if !path.is_dir()
            || path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with('.'))
        {
            continue;
        }
        if matches!(
            path.file_name().and_then(|n| n.to_str()),
            Some("claude-experiments" | "rust")
        ) {
            for child in fs::read_dir(&path)? {
                let child = child?.path();
                if child.is_dir() {
                    boundaries.push(child);
                }
            }
        } else {
            boundaries.push(path);
        }
    }
    for path in boundaries {
        if known.contains(&path) {
            continue;
        }
        let latest = WalkDir::new(&path)
            .follow_links(false)
            .into_iter()
            .filter_entry(include_in_activity)
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .filter_map(|e| e.metadata().ok()?.modified().ok())
            .max();
        let Some(latest) = latest else { continue };
        if latest < cutoff {
            continue;
        }
        let path_text = path.to_string_lossy().into_owned();
        let id = synthetic_id(&path_text);
        let old = prior.get(&id);
        let base = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("project")
            .to_owned();
        let when: chrono::DateTime<chrono::Utc> = latest.into();
        out.push(Project {
            id,
            jim_name: base.clone(),
            name: old.map(|p| p.name.clone()).unwrap_or(base),
            source: Some(path_text),
            in_playground: true,
            exists: true,
            hidden: false,
            last_edit_at: Some(when.to_rfc3339()),
            last_focus_at: None,
            elevate: old.map(|p| p.elevate).unwrap_or(false),
            visibility: old
                .map(|p| p.visibility.clone())
                .unwrap_or_else(|| "public".into()),
            readme: old.map(|p| p.readme).unwrap_or(false),
            experiment: old.map(|p| p.experiment).unwrap_or(false),
            status: old
                .map(|p| p.status.clone())
                .unwrap_or_else(|| "potential".into()),
            origin: "filesystem".into(),
        });
    }
    Ok(())
}

fn include_in_activity(entry: &DirEntry) -> bool {
    !matches!(
        entry.file_name().to_str(),
        Some(
            ".git" | "target" | "node_modules" | ".next" | "dist" | "build" | ".cache" | ".claude"
        )
    )
}

fn synthetic_id(path: &str) -> u64 {
    let mut hash = 14695981039346656037u64;
    for b in path.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(1099511628211)
    }
    1_000_000_000 + hash % 8_000_000_000
}

fn load() -> Result<Vec<Project>> {
    let path = decisions_path();
    if !path.exists() {
        let p = scan()?;
        write_all(&p)?;
    }
    serde_json::from_str(&fs::read_to_string(path)?).context("parsing decisions")
}

fn dashboard(port: u16) -> Result<()> {
    if !data_path().exists() {
        write_all(&scan()?)?;
    }
    let address = format!("127.0.0.1:{port}");
    let server = tiny_http::Server::http(&address).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let url = format!("http://{address}");
    let _ = Command::new("open").arg(&url).status();
    println!("Project migration dashboard: {url}");
    for mut req in server.incoming_requests() {
        let path = req.url().split('?').next().unwrap_or(req.url());
        let (status, content_type, body) = match (req.method().as_str(), path) {
            ("GET", "/") => (
                200,
                "text/html; charset=utf-8",
                include_str!("../dashboard.html").to_owned(),
            ),
            ("GET", "/api/projects") => (
                200,
                "application/json",
                fs::read_to_string(decisions_path()).unwrap_or_else(|_| "[]".into()),
            ),
            ("GET", "/api/quality") => (
                200,
                "application/json",
                fs::read_to_string(root().join("data/quality.json"))
                    .unwrap_or_else(|_| "[]".into()),
            ),
            ("POST", "/api/projects") => {
                let mut body = String::new();
                req.as_reader().read_to_string(&mut body)?;
                match serde_json::from_str::<Vec<Project>>(&body) {
                    Ok(projects) => match write_all(&projects) {
                        Ok(_) => (200, "application/json", "{\"ok\":true}".into()),
                        Err(e) => (
                            500,
                            "application/json",
                            format!("{{\"error\":{}}}", q(&e.to_string())),
                        ),
                    },
                    Err(e) => (
                        400,
                        "application/json",
                        format!("{{\"error\":{}}}", q(&e.to_string())),
                    ),
                }
            }
            ("POST", "/api/quality") => {
                let mut request_body = String::new();
                req.as_reader().read_to_string(&mut request_body)?;
                let ids = serde_json::from_str::<QualityRequest>(&request_body)
                    .ok()
                    .and_then(|request| request.ids);
                let projects = load()?;
                let selected: Vec<Project> = projects
                    .iter()
                    .filter(|project| {
                        ids.as_ref()
                            .is_none_or(|wanted| wanted.contains(&project.id))
                    })
                    .cloned()
                    .collect();
                let new_reports = quality::analyze_all(&selected);
                let mut all_reports: HashMap<u64, quality::QualityReport> =
                    quality::load_reports()?
                        .into_iter()
                        .map(|report| (report.project_id, report))
                        .collect();
                for report in new_reports {
                    all_reports.insert(report.project_id, report);
                }
                let mut reports: Vec<_> = all_reports.into_values().collect();
                reports.sort_by(|a, b| a.name.cmp(&b.name));
                match quality::write_reports(&reports) {
                    Ok(_) => (200, "application/json", serde_json::to_string(&reports)?),
                    Err(error) => (
                        500,
                        "application/json",
                        format!("{{\"error\":{}}}", q(&error.to_string())),
                    ),
                }
            }
            _ => (404, "text/plain", "Not found".into()),
        };
        let response = tiny_http::Response::from_string(body)
            .with_status_code(status)
            .with_header(tiny_http::Header::from_bytes("Content-Type", content_type).unwrap());
        let _ = req.respond(response);
    }
    Ok(())
}

#[derive(Default, Deserialize)]
struct QualityRequest {
    ids: Option<Vec<u64>>,
}

pub(crate) fn prepend_experiment_notice(project: &Path) -> Result<()> {
    let readme = ["README.md", "README", "readme.md"]
        .into_iter()
        .map(|name| project.join(name))
        .find(|path| path.is_file());
    let Some(readme) = readme else { return Ok(()) };
    let existing = fs::read_to_string(&readme)?;
    if existing.contains(EXPERIMENT_NOTICE) {
        return Ok(());
    }
    fs::write(readme, format!("> {EXPERIMENT_NOTICE}\n\n{existing}"))?;
    Ok(())
}

fn new_project(name: &str, public: bool, readme: bool, local_only: bool) -> Result<()> {
    validate_project_name(name)?;
    let dest = PathBuf::from(PROJECTS_ROOT).join(name);
    if dest.exists() {
        bail!("destination already exists: {}", dest.display());
    }
    let mut projects = if decisions_path().exists() {
        load()?
    } else {
        Vec::new()
    };
    if projects.iter().any(|project| project.name == name) {
        bail!("a project named {name} is already in the registry");
    }
    fs::create_dir_all(&dest)?;
    run(Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(&dest))?;
    fs::write(
        dest.join("project.md"),
        format!("# {name}\n\n## Summary\n\nDescribe the project.\n"),
    )?;
    if readme {
        fs::write(dest.join("README.md"), format!("# {name}\n"))?;
    }
    write_scaffold_gitignore(&dest)?;
    run(Command::new("git").args(["add", "."]).current_dir(&dest))?;
    run(Command::new("git")
        .args(["commit", "-m", "Initial project scaffold"])
        .current_dir(&dest))?;

    let mut id = synthetic_id(&dest.to_string_lossy());
    while projects.iter().any(|project| project.id == id) {
        id = id.wrapping_add(1);
    }
    let now = Utc::now().to_rfc3339();
    let mut project = Project {
        id,
        jim_name: name.into(),
        name: name.into(),
        source: Some(dest.to_string_lossy().into_owned()),
        in_playground: false,
        exists: true,
        hidden: false,
        last_edit_at: Some(now.clone()),
        last_focus_at: Some(now),
        elevate: false,
        visibility: if public { "public" } else { "private" }.into(),
        readme: false,
        experiment: false,
        status: "scaffolded".into(),
        origin: "scaffold".into(),
    };
    projects.push(project.clone());
    write_all(&projects)?;

    if !local_only {
        let repo = format!("jimmyhmiller/{name}");
        let visibility = if public { "--public" } else { "--private" };
        run(Command::new("gh").args([
            "repo",
            "create",
            &repo,
            visibility,
            "--source",
            dest.to_str().context("project path is not valid UTF-8")?,
            "--remote",
            "origin",
        ]))?;
        project.status = "repo-created".into();
        update_registered_project(&project)?;
        run(Command::new("git")
            .args(["push", "-u", "origin", "main"])
            .current_dir(&dest))?;
        project.status = "migrated".into();
        update_registered_project(&project)?;
    }
    println!(
        "Created and registered {} ({}).{}",
        format!("~/Documents/Code/projects/{name}"),
        if public { "public" } else { "private" },
        if local_only {
            " GitHub was skipped (--local-only)."
        } else {
            " GitHub repository created and main pushed."
        }
    );
    Ok(())
}

fn validate_project_name(name: &str) -> Result<()> {
    let path = Path::new(name);
    if name.is_empty()
        || name == "."
        || name == ".."
        || path.components().count() != 1
        || path.file_name().and_then(|value| value.to_str()) != Some(name)
        || name.chars().any(char::is_whitespace)
    {
        bail!("project name must be one path-safe non-empty name: {name:?}");
    }
    Ok(())
}

fn write_scaffold_gitignore(dest: &Path) -> Result<()> {
    let mut lines = vec![".DS_Store", ".claude/settings.local.json"];
    if dest.join("Cargo.toml").exists() {
        lines.push("/target/");
    }
    if dest.join("package.json").exists() {
        lines.extend(["/node_modules/", "/dist/", "/.next/"]);
    }
    let path = dest.join(".gitignore");
    let existing = fs::read_to_string(&path).unwrap_or_default();
    let mut output = existing;
    for line in lines {
        if !output.lines().any(|existing| existing.trim() == line) {
            output.push_str(line);
            output.push('\n');
        }
    }
    fs::write(path, output)?;
    Ok(())
}

fn update_registered_project(project: &Project) -> Result<()> {
    let mut projects = load()?;
    let existing = projects
        .iter_mut()
        .find(|candidate| candidate.id == project.id)
        .context("new project disappeared from registry")?;
    *existing = project.clone();
    write_all(&projects)
}

fn run(command: &mut Command) -> Result<()> {
    let rendered = format!("{command:?}");
    let status = command
        .status()
        .with_context(|| format!("running {rendered}"))?;
    if !status.success() {
        bail!("command failed: {rendered}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_names_are_path_safe() {
        assert!(validate_project_name("new-project").is_ok());
        assert!(validate_project_name("a/b").is_err());
        assert!(validate_project_name("../outside").is_err());
        assert!(validate_project_name("has space").is_err());
    }

    #[test]
    fn registry_serializes_scaffold_origin() {
        let project = Project {
            id: 1,
            jim_name: "demo".into(),
            name: "demo".into(),
            source: Some("/tmp/demo".into()),
            in_playground: false,
            exists: true,
            hidden: false,
            last_edit_at: None,
            last_focus_at: None,
            elevate: false,
            visibility: "private".into(),
            readme: false,
            experiment: false,
            status: "scaffolded".into(),
            origin: "scaffold".into(),
        };
        let kdl = to_kdl(&[project]);
        assert!(kdl.contains("origin=\"scaffold\""));
        assert!(kdl.contains("status=\"scaffolded\""));
    }
}
