# Bimetal

Bimetal builds one Rust API into host packages: Python (a CPython extension) and TypeScript (a Wasm module or a native Node.js addon). A `bimetal.toml` beside a Cargo package chooses the hosts.

`packages/` holds the crates that others depend on: `runtime` (the `bimetal` crate), `macros` and `ir`. `apps/` holds what uses them: `cli`, `examples`, `trials` and `benchmark`. Tasks live in `mise.toml`.

## Rules

- Rust follows the [`google-rust-style`](.claude/skills/google-rust-style/SKILL.md) skill, including its recorded decisions. Code comments follow [`antislop-code`](.claude/skills/antislop-code/SKILL.md), and docs follow [`antislop-copywriting`](.claude/skills/antislop-copywriting/SKILL.md).
- Use the simplest complete implementation. Reuse existing dependencies before adding new ones, and remove replaced code paths instead of keeping fallbacks.
- Keep one code path per concern across hosts. Decode each value once and move it; when a change touches a host boundary, count the copies per host and direction.
- Docs and comments describe current behaviour; history and the reasons for past changes go in commit messages. A sample in the docs comes from a program in `apps/examples/` or `apps/trials/`, which `apps/cli/tests/docs.rs` checks. A change to a promise or the build's rules updates `docs/paper/bimetal.tex` and rebuilds its PDF as `docs/paper/README.md` says. The paper is connected prose with no contractions: a paragraph opens with its point, and facts that relate share a sentence with the word that relates them (but, because, so, while).

## Workflow

- The examples commit their generated packages. After changing the CLI, the macros, or the runtime, regenerate them with `mise run example`.
- Until the first release, `main` is one commit, and each change is squashed into it. From that release on, `main` takes changes only through pull requests, which merge as one squashed commit. Write the pull request's title as that commit's subject, because it also becomes a line of the next release's notes.
- Before handing off, run `mise run lint`, `mise run test`, `mise run example`, and `mise run check-examples`, then the manual searches in the style skill's "Reviewing or auditing code" section. Report the checks you actually ran.

<!-- antislop:start -->
## antislop
For copy or code comments work, read `.claude/skills/antislop/SKILL.md` (core) and then the skill for the task:
- Copy & text: `.claude/skills/antislop-copywriting/SKILL.md`
- Code comments: `.claude/skills/antislop-code/SKILL.md`
Before starting, follow the core's "Two Usage Modes" section in strict order: explicit session instruction first, then global preference, then ask. A session instruction always wins. For a resolved mode, say `antislop active: <mode> (session override).` or `antislop active: <mode> (global preference).` once before presenting findings or making edits, using the actual mode and source. Acknowledging the user's request without naming the source does not replace this notice.
Only an explicit choice of antislop during or after selects a session mode. A request to review, audit, or avoid file edits does not select a mode; read the global preference in that case. Another skill's mode does not select antislop's mode.
If the mode is unresolved, ask during/after and end the response; wait for the answer before any review, planning, or concept. For read-only tasks, put the active-mode notice only at the start of the final answer, never in progress messages. For editing tasks, announce before the first edit and omit it from the final answer.
To update antislop later: download `antislop.md` again, or run `npx antislop-ai --update` if it was installed as skill folders.
<!-- antislop:end -->
