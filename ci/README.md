# ci/

No CI scripts live here today. The workflows under `.github/workflows/`
carry their own steps: `ci.yml` builds and tests both runtimes against a
sibling parser checkout, and `docs.yml` runs the prose gate
(`make prose` runs the same check locally).

To change CI, edit `.github/workflows/` in a reviewed pull request.
Session credentials push workflow files (admin `DECISIONS.md` ADR-8, as
amended 2026-09-24), so staging a workflow here first for a maintainer
to promote is optional. Sessions still cannot push tags, so a maintainer
pushes any tag that a tag-triggered workflow needs.

The amendment also asks for the same change in `tabnas/admin` wherever
admin keeps a copy of the workflow. If admin's `rollout/workflows/`
holds a `lsp__<file>.yml` template for the workflow you changed, make
the same edit there: admin `scripts/verify.sh` compares each template
with its deployed copy, and a maintainer's
`rollout/apply-workflows.sh --apply` would push the older text back over
yours.

## Promoted, 2026-09-22

The one file that was staged here is now live, moved by the rollout
script rather than edited: `workflows/docs.yml` is
`.github/workflows/docs.yml`. Nothing is pending. Read the workflow
itself rather than a description of it here.
