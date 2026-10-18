---
name: address-pr-feedback
description: >-
  End-to-end protocol for reviewing and addressing pull request feedback:
  pushing code changes, replying to every addressed comment with when/how it
  was fixed, replying to any unimplemented comment with why it was not
  implemented, resolving all review threads, and posting a top-level PR summary
  comment. Use this skill whenever the user asks to review, handle, or address
  feedback on a PR.
---

# Address PR Feedback Skill (`address-pr-feedback`)

Whenever the user asks to **"Address the feedback on this PR"** (or review/resolve PR comments), execute every step in this runbook in order without skipping any step.

---

## 1. Fetch All Review Threads, Reviews, and Comments

Query GitHub via `gh api graphql` to retrieve all review threads (`id`, `isResolved`, `path`, `line`, and each comment's `databaseId`, `author`, `body`, `createdAt`):

```powershell
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

## 2. Implement Fixes, Verify All Gates, and Push Changes

1. For every valid piece of feedback, implement the code fix and add unit/integration test coverage locking in the behavior.
2. Run all three workspace verification gates (`GEMINI.md` Rule Group E):
   * `cargo clippy --workspace --all-targets -- -D warnings`
   * `cargo test --workspace`
3. Commit and push the changes to the PR's head branch (`git push`), and record the resulting commit SHA (`git rev-parse --short HEAD`) and UTC/local timestamp.

---

## 3. Reply to Every Review Comment Thread

Every unresolved review thread must receive a direct in-thread reply via the REST endpoint `repos/{owner}/{repo}/pulls/{pr}/comments/{comment_database_id}/replies`:

* **If the feedback was addressed:** State **when** (commit SHA and timestamp/date) and **how** (files, functions, and mechanism) it was addressed:
  ```powershell
  gh api repos/Columbia-Cloudworks-LLC/permit-denied-rust-edition/pulls/<PR_NUMBER>/comments/<COMMENT_DATABASE_ID>/replies `
    -f body="Addressed in commit \`<SHORT_SHA>\` (<TIMESTAMP>): <concise explanation of how the issue was resolved and verified>."
  ```
* **If the feedback was ignored or not implemented:** State **why** it was ignored or not implemented (e.g., architectural constraint, intentional design invariant, false positive):
  ```powershell
  gh api repos/Columbia-Cloudworks-LLC/permit-denied-rust-edition/pulls/<PR_NUMBER>/comments/<COMMENT_DATABASE_ID>/replies `
    -f body="Not implemented: <clear technical rationale explaining why this suggestion was not applied>."
  ```

---

## 4. Resolve Every Review Conversation Thread

After replying to each review comment, resolve its conversation thread via the GraphQL `resolveReviewThread` mutation using the thread's node `id` (`PRRT_...`):

```powershell
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

Verify that zero unresolved review threads remain on the PR.

---

## 5. Post a Top-Level PR Summary Comment

Post a comprehensive summary comment on the pull request via `gh pr comment <PR_NUMBER>` containing:
1. **Commit & Verification Summary:** The commit SHA pushed and confirmation of `cargo clippy` + `cargo test` verification gates.
2. **Addressed Feedback:** A bulleted list or table of each review finding that was addressed, referencing the reviewer, file/symbol, and how it was resolved.
3. **Unaddressed / Ignored Feedback:** A clear section listing any feedback that was not implemented and why (or explicitly stating `"None — all review feedback items were implemented."` if 100% of items were addressed).
