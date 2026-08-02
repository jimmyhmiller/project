use super::*;
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityReport {
    pub project_id: u64,
    pub name: String,
    pub generated_at: String,
    pub score: u8,
    pub grade: String,
    pub last_worked_at: Option<String>,
    pub readme_needs_update: bool,
    pub summary: String,
    pub checks: Vec<QualityCheck>,
    pub recommendations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityCheck {
    pub key: String,
    pub label: String,
    pub status: String,
    pub detail: String,
}

fn quality_path() -> PathBuf {
    root().join("data/quality.json")
}

pub fn load_reports() -> Result<Vec<QualityReport>> {
    let path = quality_path();
    if !path.exists() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
}

pub fn run(id: Option<u64>) -> Result<()> {
    let projects = load()?;
    let selected: Vec<Project> = projects
        .into_iter()
        .filter(|p| id.is_none_or(|wanted| wanted == p.id))
        .collect();
    if selected.is_empty() {
        bail!("no matching projects")
    }
    let mut reports = load_reports()?
        .into_iter()
        .map(|report| (report.project_id, report))
        .collect::<HashMap<_, _>>();
    for project in &selected {
        let report = analyze(project);
        println!("{}: {} ({}/100)", report.name, report.grade, report.score);
        reports.insert(report.project_id, report);
    }
    let mut output: Vec<_> = reports.into_values().collect();
    output.sort_by(|a, b| a.name.cmp(&b.name));
    fs::write(quality_path(), serde_json::to_string_pretty(&output)?)?;
    println!("Saved quality review to {}", quality_path().display());
    Ok(())
}

pub fn analyze_all(projects: &[Project]) -> Vec<QualityReport> {
    projects.iter().map(analyze).collect()
}

pub fn write_reports(reports: &[QualityReport]) -> Result<()> {
    fs::write(quality_path(), serde_json::to_string_pretty(reports)?)?;
    Ok(())
}

fn analyze(project: &Project) -> QualityReport {
    let path = project_path(project);
    let mut checks = Vec::new();
    let mut recommendations = Vec::new();
    let mut score: i16 = 100;

    if path.is_dir() {
        check(
            &mut checks,
            "source",
            "Project directory",
            "pass",
            "project directory exists",
        );
    } else {
        check(
            &mut checks,
            "source",
            "Project directory",
            "block",
            &format!("missing: {}", path.display()),
        );
        score -= 35;
        recommendations.push("Restore the project directory or fix its registry path.".into());
    }

    let readme_path = find_readme(&path);
    let readme_text = readme_path
        .as_ref()
        .and_then(|p| fs::read_to_string(p).ok())
        .unwrap_or_default();
    let readme_needs_update = project.readme
        || readme_path.is_none()
        || readme_text.trim().len() < 80
        || ["describe the project", "todo", "tbd", "coming soon"]
            .iter()
            .any(|marker| readme_text.to_ascii_lowercase().contains(marker));
    if readme_path.is_none() {
        check(
            &mut checks,
            "readme",
            "README",
            "warning",
            "README is missing",
        );
        score -= 20;
        recommendations.push("Write a README with purpose, setup, and an example.".into());
    } else if readme_needs_update {
        check(
            &mut checks,
            "readme",
            "README",
            "warning",
            "README exists but is marked for revision or still looks like a placeholder",
        );
        score -= 12;
        recommendations.push(
            "Refresh the README: explain what it does, how to run it, and what is next.".into(),
        );
    } else {
        check(
            &mut checks,
            "readme",
            "README",
            "pass",
            "README is present and has useful content",
        );
    }

    if path.join("project.md").is_file() {
        check(
            &mut checks,
            "project-doc",
            "Project notes",
            "pass",
            "project.md is present",
        );
    } else {
        check(
            &mut checks,
            "project-doc",
            "Project notes",
            "warning",
            "project.md is missing",
        );
        score -= 8;
        recommendations
            .push("Capture the project's current purpose and next decisions in project.md.".into());
    }

    let git_exists = path.join(".git").is_dir();
    if git_exists {
        check(
            &mut checks,
            "git",
            "Git repository",
            "pass",
            "local Git repository is initialized",
        );
        if let Some(detail) = git_status(&path) {
            check(
                &mut checks,
                "working-tree",
                "Working tree",
                "warning",
                &detail,
            );
            score -= 5;
        } else {
            check(
                &mut checks,
                "working-tree",
                "Working tree",
                "pass",
                "working tree is clean",
            );
        }
    } else {
        check(
            &mut checks,
            "git",
            "Git repository",
            "block",
            "local Git repository is missing",
        );
        score -= 25;
        recommendations.push("Initialize Git and make a reviewed baseline commit.".into());
    }

    if has_tests(&path) {
        check(
            &mut checks,
            "tests",
            "Tests",
            "pass",
            "test files or a test target were found",
        );
    } else {
        check(
            &mut checks,
            "tests",
            "Tests",
            "warning",
            "no recognizable tests were found",
        );
        score -= 15;
        recommendations.push("Add a small smoke test for the project's main behavior.".into());
    }

    let last_worked_at = latest_activity(&path, project);
    let score = score.clamp(0, 100) as u8;
    let grade = match score {
        90..=100 => "excellent",
        75..=89 => "good",
        55..=74 => "needs-attention",
        _ => "at-risk",
    }
    .to_owned();
    let summary = if recommendations.is_empty() {
        "Looks ready for continued work.".into()
    } else {
        format!(
            "{} follow-up item{}.",
            recommendations.len(),
            if recommendations.len() == 1 { "" } else { "s" }
        )
    };
    QualityReport {
        project_id: project.id,
        name: project.name.clone(),
        generated_at: Utc::now().to_rfc3339(),
        score,
        grade,
        last_worked_at,
        readme_needs_update,
        summary,
        checks,
        recommendations,
    }
}

fn check(checks: &mut Vec<QualityCheck>, key: &str, label: &str, status: &str, detail: &str) {
    checks.push(QualityCheck {
        key: key.into(),
        label: label.into(),
        status: status.into(),
        detail: detail.into(),
    });
}

fn project_path(project: &Project) -> PathBuf {
    project
        .source
        .as_ref()
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .unwrap_or_else(|| Path::new(PROJECTS_ROOT).join(&project.name))
}

fn find_readme(path: &Path) -> Option<PathBuf> {
    ["README.md", "README", "readme.md"]
        .into_iter()
        .map(|name| path.join(name))
        .find(|candidate| candidate.is_file())
}

fn has_tests(path: &Path) -> bool {
    ["tests", "test", "spec"]
        .iter()
        .any(|name| path.join(name).is_dir())
        || [
            "Cargo.toml",
            "package.json",
            "pyproject.toml",
            "Package.swift",
        ]
        .iter()
        .any(|manifest| path.join(manifest).is_file())
            && WalkDir::new(path)
                .follow_links(false)
                .into_iter()
                .filter_entry(include_in_activity)
                .filter_map(Result::ok)
                .any(|entry| {
                    entry.file_type().is_file()
                        && entry
                            .file_name()
                            .to_string_lossy()
                            .to_ascii_lowercase()
                            .contains("test")
                })
}

fn git_status(path: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return Some("Git status could not be read".into());
    }
    let changed = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    (changed > 0).then(|| {
        format!(
            "{changed} uncommitted change{}",
            if changed == 1 { "" } else { "s" }
        )
    })
}

fn latest_activity(path: &Path, project: &Project) -> Option<String> {
    let git_date = Command::new("git")
        .args(["log", "-1", "--format=%cI"])
        .current_dir(path)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|value| parse_date(value.trim()));
    let filesystem_date = WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_entry(include_in_activity)
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| entry.metadata().ok()?.modified().ok())
        .map(DateTime::<Utc>::from)
        .max();
    let jim_date = project
        .last_edit_at
        .as_deref()
        .or(project.last_focus_at.as_deref())
        .and_then(parse_date);
    [git_date, filesystem_date, jim_date]
        .into_iter()
        .flatten()
        .max()
        .map(|value| value.to_rfc3339())
}

fn parse_date(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}
