# Inspiration repositories

Before every provider comparison or sync, pull all repositories under `inspo/`
to their latest active upstream branches. Check the live default branch with
`git ls-remote --symref origin HEAD`, then use
`git pull --ff-only --no-tags origin <branch>` in each repository. Preserve local
changes and existing tags; do not reset or force-update a reference checkout.

Record the exact upstream commits reviewed in a local dated sync report. Keep
`docs/inspo-sync-*.md` reports local; do not commit or push them. A previous
checkout, fetched reference, or sync ledger is not evidence of the latest state.
