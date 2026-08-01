use super::*;
use anyhow::{Context, Result};
use chrono::Utc;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde::Serialize;
use std::process::Command;

const LARGE_WARNING: u64 = 10 * 1024 * 1024;
const GITHUB_LIMIT: u64 = 100 * 1024 * 1024;

#[derive(Serialize)]
struct Report {
    generated_at: String,
    selected: usize,
    passed: usize,
    warned: usize,
    blocked: usize,
    projects: Vec<ProjectReport>,
}

#[derive(Serialize)]
struct ProjectReport {
    id: u64,
    name: String,
    source: String,
    destination: String,
    visibility: String,
    experiment: bool,
    status: String,
    outcome: String,
    included_files: u64,
    included_bytes: u64,
    ignored_risky_paths: Vec<String>,
    large_files: Vec<FileFinding>,
    secret_indicators: Vec<String>,
    nested_git: Vec<String>,
    external_symlinks: Vec<String>,
    readme: String,
    github: String,
    warnings: Vec<String>,
    blockers: Vec<String>,
}

#[derive(Serialize)]
struct FileFinding {
    path: String,
    bytes: u64,
}

pub fn run() -> Result<()> {
    let projects: Vec<Project> = load()?.into_iter().filter(|p| p.elevate).collect();
    let mut reports = Vec::new();
    println!("Preflighting {} selected projects…", projects.len());
    for (index, project) in projects.iter().enumerate() {
        print!("[{}/{}] {} … ", index + 1, projects.len(), project.name);
        use std::io::Write;
        std::io::stdout().flush()?;
        let report = inspect(project)?;
        println!("{}", report.outcome);
        reports.push(report);
    }
    let report = Report {
        generated_at: Utc::now().to_rfc3339(),
        selected: reports.len(),
        passed: reports.iter().filter(|p| p.outcome == "pass").count(),
        warned: reports.iter().filter(|p| p.outcome == "warning").count(),
        blocked: reports.iter().filter(|p| p.outcome == "blocked").count(),
        projects: reports,
    };
    write_report(&report)?;
    println!(
        "\nPreflight: {} passed, {} warnings, {} blocked.\nReports:\n  {}\n  {}",
        report.passed,
        report.warned,
        report.blocked,
        root().join("preflight.kdl").display(),
        root().join("preflight.md").display()
    );
    Ok(())
}

fn inspect(p: &Project) -> Result<ProjectReport> {
    let source = PathBuf::from(
        p.source
            .as_ref()
            .context("selected project has no source")?,
    );
    let destination = Path::new(PROJECTS_ROOT).join(&p.name);
    let mut warnings = Vec::new();
    let mut blockers = Vec::new();
    if !source.is_dir() {
        blockers.push(format!("source directory is missing: {}", source.display()));
    }
    if p.in_playground && destination.exists() {
        blockers.push(format!(
            "destination already exists: {}",
            destination.display()
        ));
    }
    let ignore = build_ignore(&source)?;
    let mut included_files = 0;
    let mut included_bytes = 0;
    let mut ignored_risky_paths = Vec::new();
    let mut large_files = Vec::new();
    let mut secret_indicators = Vec::new();
    let mut nested_git = Vec::new();
    let mut external_symlinks = Vec::new();
    if source.is_dir() {
        let canonical_source = source.canonicalize()?;
        let mut walk = WalkDir::new(&source).follow_links(false).into_iter();
        while let Some(entry) = walk.next() {
            let entry = match entry {
                Ok(v) => v,
                Err(e) => {
                    warnings.push(format!("walk error: {e}"));
                    continue;
                }
            };
            let path = entry.path();
            if path == source {
                continue;
            }
            let relative = path.strip_prefix(&source).unwrap_or(path);
            let is_dir = entry.file_type().is_dir();
            if entry.file_name() == ".git" && is_dir {
                if relative.components().count() > 1 {
                    nested_git.push(relative.display().to_string());
                }
                walk.skip_current_dir();
                continue;
            }
            if ignore
                .matched_path_or_any_parents(relative, is_dir)
                .is_ignore()
            {
                if risky_ignored(relative, is_dir) && ignored_risky_paths.len() < 30 {
                    ignored_risky_paths.push(relative.display().to_string())
                }
                if is_dir {
                    walk.skip_current_dir()
                }
                continue;
            }
            if entry.file_type().is_symlink() {
                if let Ok(target) = path.canonicalize()
                    && !target.starts_with(&canonical_source)
                {
                    external_symlinks.push(format!(
                        "{} -> {}",
                        relative.display(),
                        target.display()
                    ))
                }
                continue;
            }
            if !entry.file_type().is_file() {
                continue;
            }
            let metadata = entry.metadata()?;
            included_files += 1;
            included_bytes += metadata.len();
            if metadata.len() >= LARGE_WARNING {
                large_files.push(FileFinding {
                    path: relative.display().to_string(),
                    bytes: metadata.len(),
                });
                if metadata.len() >= GITHUB_LIMIT {
                    blockers.push(format!(
                        "{} is {} bytes (over GitHub's 100 MiB limit)",
                        relative.display(),
                        metadata.len()
                    ))
                }
            }
            if let Some(reason) = secret_indicator(path, relative, metadata.len()) {
                secret_indicators.push(format!("{} ({reason})", relative.display()))
            }
        }
    }
    if included_files == 0 {
        blockers.push("no files would be included".into())
    }
    if !secret_indicators.is_empty() {
        blockers.push(format!(
            "{} possible secret-bearing files require review",
            secret_indicators.len()
        ))
    }
    if !nested_git.is_empty() {
        warnings.push(format!(
            "{} nested Git directories will be flattened",
            nested_git.len()
        ))
    }
    if !external_symlinks.is_empty() {
        warnings.push(format!(
            "{} symlinks point outside the project",
            external_symlinks.len()
        ))
    }
    if !ignored_risky_paths.is_empty() {
        warnings.push(format!(
            "{} potentially meaningful paths are excluded by merged ignore rules",
            ignored_risky_paths.len()
        ))
    }
    if large_files
        .iter()
        .any(|f| f.bytes >= LARGE_WARNING && f.bytes < GITHUB_LIMIT)
    {
        warnings.push(format!(
            "{} files are at least 10 MiB",
            large_files
                .iter()
                .filter(|f| f.bytes < GITHUB_LIMIT)
                .count()
        ))
    }
    let readme_path = ["README.md", "README", "readme.md"]
        .into_iter()
        .map(|n| source.join(n))
        .find(|p| p.is_file());
    let readme = match readme_path {
        Some(path) => {
            let text = fs::read_to_string(path).unwrap_or_default();
            if p.experiment && !text.contains(EXPERIMENT_NOTICE) {
                warnings.push(
                    "experiment README is missing the standard notice (migration will prepend it)"
                        .into(),
                );
                "present; experiment notice missing".into()
            } else {
                "present".into()
            }
        }
        None => {
            if p.readme {
                "missing; follow-up recorded".into()
            } else {
                "missing".into()
            }
        }
    };
    let repo = format!("jimmyhmiller/{}", p.name);
    let github_exists = Command::new("gh")
        .args(["repo", "view", &repo])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    let expected = matches!(p.status.as_str(), "repo-created" | "migrated");
    let github = if github_exists { "exists" } else { "available" }.to_owned();
    if github_exists && !expected {
        blockers.push(format!(
            "GitHub repository {repo} already exists unexpectedly"
        ))
    }
    if !github_exists && expected {
        blockers.push(format!(
            "registry expects {repo} to exist, but GitHub does not report it"
        ))
    }
    let outcome = if !blockers.is_empty() {
        "blocked"
    } else if !warnings.is_empty() {
        "warning"
    } else {
        "pass"
    }
    .to_owned();
    Ok(ProjectReport {
        id: p.id,
        name: p.name.clone(),
        source: source.display().to_string(),
        destination: destination.display().to_string(),
        visibility: p.visibility.clone(),
        experiment: p.experiment,
        status: p.status.clone(),
        outcome,
        included_files,
        included_bytes,
        ignored_risky_paths,
        large_files,
        secret_indicators,
        nested_git,
        external_symlinks,
        readme,
        github,
        warnings,
        blockers,
    })
}

fn build_ignore(source: &Path) -> Result<Gitignore> {
    let mut builder = GitignoreBuilder::new(source);
    let inherited = fs::read_to_string(Path::new(PLAYGROUND).join(".gitignore"))?;
    for line in inherited.lines() {
        builder.add_line(None, line)?;
    }
    let local = source.join(".gitignore");
    if local.exists() {
        for line in fs::read_to_string(local)?.lines() {
            builder.add_line(None, line)?;
        }
    }
    builder.build().map_err(Into::into)
}

fn risky_ignored(path: &Path, is_dir: bool) -> bool {
    if is_dir {
        return matches!(
            path.components()
                .next()
                .and_then(|c| c.as_os_str().to_str()),
            Some("packages")
        );
    }
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("svg" | "bin" | "wav" | "pt" | "a" | "dylib")
    )
}

fn secret_indicator(path: &Path, relative: &Path, size: u64) -> Option<&'static str> {
    let name = relative.file_name()?.to_string_lossy().to_ascii_lowercase();
    if matches!(
        name.as_str(),
        "id_rsa" | "id_ed25519" | "credentials.json" | "secrets.json"
    ) || name.ends_with(".pem")
    {
        return Some("suspicious filename");
    }
    if size > 1_000_000 {
        return None;
    }
    let text = fs::read_to_string(path).ok()?;
    for (needle, label) in [
        ("-----BEGIN PRIVATE KEY", "private key"),
        ("AKIA", "AWS access-key pattern"),
        ("ghp_", "GitHub token pattern"),
        ("xoxb-", "Slack token pattern"),
        ("sk-proj-", "API token pattern"),
    ] {
        if text.contains(needle) {
            return Some(label);
        }
    }
    None
}

fn write_report(report: &Report) -> Result<()> {
    fs::write(
        root().join("data/preflight.json"),
        serde_json::to_string_pretty(report)?,
    )?;
    let mut kdl = format!(
        "preflight generated-at={} selected={} passed={} warnings={} blocked={} {{\n",
        q(&report.generated_at),
        report.selected,
        report.passed,
        report.warned,
        report.blocked
    );
    for p in &report.projects {
        kdl.push_str(&format!(
            "  project {} name={} outcome={} files={} bytes={} github={} {{\n",
            p.id,
            q(&p.name),
            q(&p.outcome),
            p.included_files,
            p.included_bytes,
            q(&p.github)
        ));
        for warning in &p.warnings {
            kdl.push_str(&format!("    warning {}\n", q(warning)))
        }
        for blocker in &p.blockers {
            kdl.push_str(&format!("    blocker {}\n", q(blocker)))
        }
        kdl.push_str("  }\n")
    }
    kdl.push_str("}\n");
    fs::write(root().join("preflight.kdl"), kdl)?;
    let mut md = format!(
        "# Migration preflight\n\nGenerated: {}\n\n**{} passed · {} warnings · {} blocked**\n\n",
        report.generated_at, report.passed, report.warned, report.blocked
    );
    for p in &report.projects {
        md.push_str(&format!(
            "## {} — {}\n\n- Files: {} ({:.2} MiB)\n- GitHub: {}\n- README: {}\n",
            p.name,
            p.outcome,
            p.included_files,
            p.included_bytes as f64 / 1048576.0,
            p.github,
            p.readme
        ));
        for b in &p.blockers {
            md.push_str(&format!("- BLOCKER: {b}\n"))
        }
        for w in &p.warnings {
            md.push_str(&format!("- Warning: {w}\n"))
        }
        for s in &p.secret_indicators {
            md.push_str(&format!("- Secret indicator: `{s}`\n"))
        }
        for f in &p.large_files {
            md.push_str(&format!("- Large file: `{}` ({} bytes)\n", f.path, f.bytes))
        }
        for i in &p.ignored_risky_paths {
            md.push_str(&format!("- Risky ignored path: `{i}`\n"))
        }
        md.push('\n')
    }
    fs::write(root().join("preflight.md"), md)?;
    Ok(())
}
