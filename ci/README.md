# ci/

Staging area for GitHub Actions workflow changes.

This directory exists because session credentials cannot write
`.github/workflows/*` — see admin `DECISIONS.md` ADR-8. To change CI:

1. Put the intended workflow file in `workflows/`.
2. A maintainer promotes it with the admin `rollout/apply-ci-folders.sh`
   script.

## Promoted, 2026-09-22

The one file that was staged here is now live, moved by the rollout
script rather than edited: `workflows/docs.yml` is
`.github/workflows/docs.yml`. Nothing is pending, and nothing else lives
here. Read the workflow itself rather than a description of it here.
