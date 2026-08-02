# Project

## Purpose

Create and register projects, maintain a KDL index of all projects, and graduate selected PlayGround experiments into standalone GitHub repositories through an inspectable one-at-a-time workflow.

## Data

- `projects.kdl` — readable project registry and migration decisions.
- `data/projects.json` — generated Jim scan data.
- `data/decisions.json` — dashboard interchange cache.
- `data/quality.json` — cached, read-only quality worker reports and recommendations.
- `~/.jim/projects.json` and `~/.jim/projects/<id>/state.json` — Jim discovery and activity inputs.

The `readme` decision is a follow-up marker, not a migration instruction.
The `experiment` decision explicitly classifies a project as an experiment; it is descriptive metadata and does not control migration.
Explicit experiments use the standard disclaimer: “This is experimental software. It probably doesn't work.” Migration prepends it to an existing README without creating a missing README.

## New project contract

`cargo run -- new <name>` creates a path-safe project under the projects root, initializes Git, writes the starter documents and ignore rules, makes an initial scaffold commit, registers the project, creates its GitHub repository, and pushes `main`. `--local-only` stops after local registration. Registry status is persisted as each milestone completes.

Quality reviews are intentionally advisory and read-only. They check whether the project exists, has a useful README and project notes, has Git, has recognizable tests, has a clean working tree, and when it was last active. Findings are stored in `data/quality.json` and rendered by the dashboard.

## Migration contract

Migration is controlled by a resumable TUI and has three explicit checkpoints per project: move and local initial commit, GitHub repository creation, and push. The initial commit message is `Migrated from playground`. State is stored in `migration-state.kdl`; every attempted and completed action is appended to `migration-actions.kdl`.
