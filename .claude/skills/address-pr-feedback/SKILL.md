---
name: address-pr-feedback
description: >-
  End-to-end runbook for addressing pull request review feedback in Permit
  Denied: audit the full main...HEAD diff, fix and push, reply in-thread to
  every addressed comment (when/how) and every unimplemented comment (why),
  resolve all review threads, and post a top-level PR summary comment. Use
  whenever the user asks to review, handle, resolve, or address feedback or
  comments on a PR.
---

# Address PR Feedback

When the user asks to **"Address the feedback on this PR"** (or review/resolve PR comments), execute every step below in order without skipping any. The request authorizes pushing to the PR's head branch and posting the replies, resolutions and summary comment.

The `gh` commands below are written on one line (aside from GraphQL query strings) so they run unchanged in either Bash or PowerShell.

---

## 1. Fetch all review threads, reviews and comments

Query GitHub via `gh api graphql` for all review threads (`id`, `isResolved`, `path`, `line`, and each comment's `databaseId`, `author`, `body`, `createdAt`):

```bash
gh api graphql -f query='
query($owner: String!, $repo: String!, $pr: Int!) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $pr) {
      number
      title
      headRefName
      reviewThreads(first: 100) {
        nodes {
          id
          isResolved
          path
          line
          startLine
          comments(first: 20) {
            nodes {
              id
              databaseId
              author { login }
              body
              createdAt
            }
          }
        }
      }
    }
  }
}' -F owner="Columbia-Cloudworks-LLC" -F repo="permit-denied-rust-edition" -F pr=<PR_NUMBER>
```

---

## 2. Pre-push adversarial & baseline-regression audit

To minimize review rounds, **never** push a narrow point-fix that only patches the single line cited in a comment. Before committing, audit the entire `main...HEAD` diff across four dimensions:

1. **Deleted-line baseline audit (`git diff main...HEAD`):**
   - Inspect every removed (`-`) line against `main`. Verify no existing keybinding, physics drag/multiplier, state reset or UI guard was unintentionally dropped when a function or module was rewritten.
2. **Sibling & equivalence-class sweep.** For every finding, check all sibling paths in the same module:
   - **Input & UI modal guards:** every keyboard shortcut (`Escape`, `Enter`, `B`, `Tab`, `1/2/3`, `Space/F/R/Shift`) and every `HudButtonAction` across `Title`, `Playing`, `Paused`, `Paused + PendingUpgrade` and `NewGameModal`.
   - **Kinematics & physics symmetry:** **Forward vs. Reverse vs. Stationary**, **Left turn vs. Right turn vs. Straight**, and **`BladeUp` vs. `BladeDown (powered surge)` vs. `BladeDown (sustained post-surge drag)`**.
   - **Boundary conditions:** strict vs. non-strict inequalities (`> 0.0` vs. `>= 0.0` on dot products, timers and thresholds) and zero/tangential inputs.
3. **Documentation & skill contract alignment:**
   - Cross-check the implementation against `README.md`, `CLAUDE.md` and `.claude/skills/*/SKILL.md` so documented controls and invariants match runtime behavior.
4. **Implement fixes, verify gates, push:**
   - Fix all valid review feedback plus any sibling/baseline issues the audit uncovers, adding unit/integration tests that lock in each behavior.
   - Pass the verification gates (`CLAUDE.md` Rule Group E):
     - `cargo clippy --workspace --all-targets -- -D warnings`
     - `cargo test --workspace`
   - Commit and push to the PR's head branch (`git push`), then record the commit SHA (`git rev-parse --short HEAD`) and UTC/local timestamp.

---

## 3. Reply to every review comment thread

Every unresolved review thread gets a direct in-thread reply via the REST endpoint `repos/{owner}/{repo}/pulls/{pr}/comments/{comment_database_id}/replies`:

- **Addressed:** state **when** (commit SHA and timestamp/date) and **how** (files, functions, mechanism):
  ```bash
  gh api repos/Columbia-Cloudworks-LLC/permit-denied-rust-edition/pulls/<PR_NUMBER>/comments/<COMMENT_DATABASE_ID>/replies -f body="Addressed in commit <SHORT_SHA> (<TIMESTAMP>): <concise explanation of how the issue was resolved and verified>."
  ```
- **Not implemented:** state **why** (architectural constraint, intentional design invariant, false positive, …):
  ```bash
  gh api repos/Columbia-Cloudworks-LLC/permit-denied-rust-edition/pulls/<PR_NUMBER>/comments/<COMMENT_DATABASE_ID>/replies -f body="Not implemented: <clear technical rationale explaining why this suggestion was not applied>."
  ```

Write longer bodies to a file and pass `-F body=@reply.md` to avoid shell-quoting problems with backticks and `$`.

---

## 4. Resolve every review conversation thread

After replying, resolve each thread with the GraphQL `resolveReviewThread` mutation using the thread's node `id` (`PRRT_...`):

```bash
gh api graphql -f query='
mutation($threadId: ID!) {
  resolveReviewThread(input: { threadId: $threadId }) {
    thread {
      id
      isResolved
    }
  }
}' -F threadId="<PRRT_NODE_ID>"
```

Re-run the step 1 query and confirm zero unresolved review threads remain.

---

## 5. Post a top-level PR summary comment

Post a summary via `gh pr comment <PR_NUMBER>` containing:

1. **Commit & verification summary:** the pushed commit SHA and confirmation that `cargo clippy` and `cargo test` passed.
2. **Addressed feedback:** a bulleted list or table of each finding addressed, with reviewer, file/symbol, and how it was resolved.
3. **Unaddressed feedback:** each item not implemented and why — or explicitly `"None — all review feedback items were implemented."`
