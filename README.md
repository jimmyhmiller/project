# project

`project` is the local project registry, scaffolder, and controlled migration tool.

```sh
cargo run -- scan
cargo run -- dashboard
cargo run -- preflight                  # audit all selected migrations
cargo run -- migrate                    # resumable staged TUI
cargo run -- publish-all                # publish all, with 10s GitHub delays
cargo run -- migrate 86                 # preview one selected project
cargo run -- quality                     # run the local quality worker for all projects
cargo run -- quality 2792111428          # review one project
cargo run -- new my-project --private --readme
cargo run -- new my-project --local-only  # scaffold/register without GitHub
```

The human-readable source of truth is `projects.kdl`. Dashboard decisions are also cached as JSON so the browser and Rust tool can exchange structured data without lossy KDL edits. Quality worker output is cached in `data/quality.json` and is always explicitly requested; reviews never edit project files.

`new` creates a path-safe project directory under `Documents/Code/projects`, initializes `main`, writes `project.md` (and an optional README), adds a baseline `.gitignore`, commits the scaffold, registers it, creates `jimmyhmiller/<name>` with the requested visibility, adds `origin`, and pushes `main`. Use `--local-only` when GitHub creation should wait. Registry status is saved after each milestone so a failed GitHub operation remains visible.

The dashboard is now a project control center as well as a migration selector. It shows README follow-up debt, last activity, registry state, and cached quality scores/recommendations. `Run quality worker` reviews docs, Git state, tests, working-tree cleanliness, and recent activity for every project; individual rows can be reviewed again without losing the other cached reports.

Migration is deliberately sequential and runs through the TUI. Each project has three separately confirmed stages: move it into `Documents/Code/projects` and create a local Git repository with the initial commit `Migrated from playground`; create and link the GitHub repository; push `main`. After every successful stage, state is atomically saved to `migration-state.kdl` (with a JSON interchange cache), and attempts/results are appended to `migration-actions.kdl`. Quit at any point and run `cargo run -- migrate` to resume. The README checkbox is planning metadata only: existing README files are preserved, but migration never creates one.

The new repository deliberately starts with clean history. Any nested `.git` metadata is removed after the project has moved and before the initial commit, with each removal recorded in the action journal. Files from those nested working trees remain part of the migrated project.

Before the initial commit, the tool merges PlayGround's top-level `.gitignore` with any project-local `.gitignore`, removes duplicate lines, and adds missing ecosystem-specific build ignores. Push is blocked if common generated artifact directories are nevertheless tracked.

In the TUI, `a` advances exactly one checkpoint. `f` fast-forwards through move/commit and repository creation but always stops before push. Long-running work displays a locked `RUNNING` state, queued input is discarded afterward, and Enter is required to unlock another action.

For a project marked as an explicit experiment, migration prepends this notice to an existing README: “This is experimental software. It probably doesn't work.” It does not create a missing README; the notice is applied when that README is revisited later.
