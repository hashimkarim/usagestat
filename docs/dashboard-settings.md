# Dashboard settings

The source dashboard includes a **Settings** tab at `/dashboard#settings`.
This feature follows the published v2.0.0 binary; rebuilding the source daemon
includes the page. The existing immutable v2.0.0 release is unchanged.

Appearance settings include dark, light and system themes, four accents,
comfortable or compact cards, icons, the Overview summary, and Used/Remaining
quota display. Set the view refresh interval between 15 seconds and 5 minutes.
That interval reads the backend's saved observations; provider polling continues
to follow the shared daemon's schedule.

For each enabled provider, choose visibility, a display name, position, and the
quota used in the Overview summary. Hiding a provider removes its Overview card
and tab; it keeps collection and saved History intact.
Overview spend totals and contributors follow manual provider visibility;
History still includes all saved providers. **Hide inactive providers**
automatically hides providers that have an error and no quota or recent tracked
usage. Explicitly hidden providers stay hidden even when inactive providers are
shown. Missing quota values remain unknown; summary selection never creates a
quota that the provider did not report.

Preferences use browser local storage at `usagestat.dashboard.prefs`, scoped to
the dashboard's origin and browser profile. Other tabs at the same origin adopt
saved changes. UsageStat Bar's settings are separate. Provider credentials,
authentication and collection settings remain with the shared backend.

**Export settings** and **Import settings** transfer display preferences in
versioned JSON. Imports validate supported values and discard credentials,
consent and unknown fields. **Reset dashboard** restores display defaults and
offers an undo until the page is reloaded. If browser storage is blocked, the
page applies settings for the current session and reports that they were not
saved.

## Antigravity access errors

A Google HTTP 403 quota denial is different from an expired access token.
Antigravity still tries supported model quota fallbacks, but if quota access
remains denied the dashboard reports: “Google denied Antigravity quota access
for this account.” Open the latest Antigravity or agy client and check account
access. Usagestat cannot grant the account a Google license or infer missing
quota. HTTP 401 can still trigger one normal OAuth refresh.
