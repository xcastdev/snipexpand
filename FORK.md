# Custom fork Git workflow

This repository is the `xcastdev/snipexpand` fork of
[`silouanwright/snipexpand`](https://github.com/silouanwright/snipexpand). It is a custom SnipExpand
distribution for features and behavior that fit xcastdev's use cases.
Changes do not need to become pull requests against the original repository.

The Git setup keeps the custom version on the default branch while preserving
a clean copy of the original repository's history. This makes upstream updates
explicit and keeps custom work separate from the upstream mirror.

## Branches and remotes

The long-lived branches have different jobs:

- `main` is the custom version. Create custom feature branches from `main`, and
  merge completed work back into `main`.
- `upstream-main` mirrors `silouanwright/snipexpand` without custom commits. Update it
  only by fast-forwarding it from `upstream/main`.

The remotes are:

- `origin`: `git@github.com:xcastdev/snipexpand.git`
- `upstream`: `git@github.com:silouanwright/snipexpand.git`

Updates move in one direction:

```text
silouanwright/snipexpand main
        |
        v
upstream/main
        |
        v
upstream-main
        |
        v
main
```

Never merge `main` into `upstream-main`. Custom commits belong on `main` or on
feature branches created from `main`.

## Add custom work

Start each change from the current custom branch:

```bash
git switch main
git pull --ff-only origin main
git switch -c feature/my-custom-change
```

After the change is ready, merge it into `main` through the normal review
process. Push `main` to `origin`, not to `upstream`.

## Check for upstream updates

Fetch both repositories before comparing branches:

```bash
git fetch origin --prune
git fetch upstream --prune
git log --oneline main..upstream/main
```

The log command lists upstream commits that `main` does not contain. No output
means that `main` already contains the fetched upstream history.

## Update the upstream mirror

Fast-forward the mirror to the latest upstream commit:

```bash
git fetch upstream --prune
git switch upstream-main
git merge --ff-only upstream/main
git push origin upstream-main
```

The `--ff-only` option stops the command if `upstream-main` contains commits
that are not in `upstream/main`. Treat that failure as a sign that the mirror
has changed unexpectedly. Do not resolve it by creating a merge commit on
`upstream-main`.

## Merge upstream updates into the custom version

Update `upstream-main` first. Then merge the mirror into `main`:

```bash
git switch main
git pull --ff-only origin main
git merge upstream-main
```

Resolve conflicts on `main`, then run `cargo test` and `cargo run -- --help`. Push
the result after the checks pass:

```bash
git push origin main
```

Use a regular merge instead of rebasing the published `main` branch. Merge
commits record each upstream integration and avoid rewriting custom history.

## Bring an active feature branch up to date

After updating `main`, merge it into an active custom feature branch:

```bash
git switch feature/my-custom-change
git merge main
```

Resolve conflicts and rerun the checks for that feature before pushing it.

## Verify the branch state

Run these commands after an upstream update:

```bash
git status --short --branch
git branch -vv
git rev-parse main upstream-main origin/main origin/upstream-main upstream/main
```

If there are no custom commits, all five revisions match after a sync. With
custom commits, `upstream-main`, `origin/upstream-main`, and
`upstream/main` still match, while `main` and `origin/main` point to the custom
history.

Do not use GitHub's **Sync fork** action for routine updates. It targets the
fork's default branch, which is the custom `main` branch, and bypasses the
`upstream-main` mirror used by this workflow.

## Keep custom changes easy to merge

Keep each behavior change in a focused commit. Avoid unrelated formatting,
renames, or dependency updates in the same change. Preserve upstream APIs and
default behavior where possible; put optional custom behavior behind explicit
configuration. These choices reduce conflicts, but upstream changes can still
require manual resolution.

## Fork account and releases

The inherited `AGENTS.md` describes access and release procedures for the
upstream repository. Push this fork using the `xcastdev` account:

```bash
gh auth switch --user xcastdev
git push origin main
```

Keep release tags and crates.io publication separate from routine fork updates.
Review the inherited release workflows before tagging a custom release.
