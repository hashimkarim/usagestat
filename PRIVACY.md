# UsageStat privacy policy

Effective date: 26 October 2026.

This policy applies to the UsageStat (`usagestat`) CLI, daemon, bundled provider
plugins and local dashboard, including the `HashimKarim.UsageStat.Alpha` Windows
package. UsageStat is maintained by Hashim Karim. It runs on your computer under
your OS account. There is no UsageStat-hosted account service or telemetry
collector receiving your usage data by default. UsageStat does not sell your
personal information or include advertising or third-party analytics SDKs.

## Information accessed and its purpose

UsageStat accesses information to show provider quotas, token usage, account
status and estimated or reported costs. Depending on the enabled provider and
source mode, this includes:

- Provider API keys, OAuth access/refresh tokens and session cookies supplied
  through configuration, environment variables, supported OS credential stores,
  or the existing login files of provider CLIs and IDEs. Auto discovery can use
  existing local logins without asking you to paste the credentials again.
- Account identifiers, email addresses or account labels returned by providers;
  organization, workspace and project identifiers; subscription plans, balances,
  credits, quota limits, reset times and usage/billing summaries.
- Local AI tool files, databases and transcripts, including Codex and Claude
  session logs, to extract timestamps, model names, token counts, costs, session
  or request identifiers, and project paths/labels. Parsing reads transcript
  files that may also contain conversation text. UsageStat's native transcript
  summaries and caches retain accounting metadata, not copies of conversation
  messages. Provider-specific import tools and custom plugins have their own
  input and output behavior.
- Installed tool locations, running local language-server information and
  supported CLI/IDE state needed to find the selected usage source.
- Your provider configuration, collection settings and dashboard preferences.

Some bundled providers are enabled by default and try available sources when
you run a probe or start the polling daemon. Providers without usable credentials
can report that authentication is required. An ingestion-only daemon started
with `--no-poll` does not discover or probe provider credentials or scan their
transcripts.

## Credentials and network requests

Enabled provider probes send the credentials needed for that provider to its
authentication, usage or billing endpoints. OAuth integrations may refresh
tokens and update the supported original login store or private provider state.
Credentials are used for account access, not sent to the UsageStat maintainer.
Providers and their infrastructure receive normal connection information such
as your IP address, request headers and the account/resource selectors needed
for the request. Their own privacy policies apply to those requests.

Some sources communicate with a local CLI, IDE or language server instead of a
remote API. Optional usage helpers can run installed commands or package runners
such as `npx`; those tools may contact their package registry or their own
services. Custom commands execute with your OS account's permissions. Additional
plugins can access the host capabilities you grant them, including files,
processes, credential stores and network requests. Install only code you trust.

UsageStat does not upload local transcripts to the maintainer. If you explicitly
configure SDK run ingestion and forwarding, accepted usage records are stored
locally and forwarded to the destination you choose. Those records can include
host, account, subject, provider and model identifiers, timestamps, measurements
and configured labels; the supported ingestion schema excludes prompts,
responses, tool payloads and credentials. Disable forwarding in its configuration
to stop future delivery. This does not delete records already delivered to
another system, which is controlled by that system's operator.

Downloading releases, installing/updating packages, following provider links and
opening this policy can contact GitHub, package registries or provider websites.
Those services process requests under their own policies. Package-manager or
OS backup/sync behavior is separate from UsageStat's collection.

## Local storage and protection

UsageStat stores configuration and optional saved credentials, provider state,
the latest snapshots, quota history, daily/model usage summaries and transcript
accounting caches in its configuration/data directories. Records can contain
account labels, provider-returned text, project paths, session identifiers and
other usage metadata. Errors, diagnostic output and service logs can also contain
account or path information. Review exports and diagnostics before sharing them.

Default application directories are:

| OS | Configuration | Data |
| --- | --- | --- |
| Windows | `%APPDATA%\usagestat` | `%LOCALAPPDATA%\usagestat` |
| macOS | `~/Library/Application Support/usagestat` | `~/Library/Application Support/usagestat` |
| Linux | `${XDG_CONFIG_HOME:-~/.config}/usagestat` | `${XDG_DATA_HOME:-~/.local/share}/usagestat` |

`USAGESTAT_CONFIG_DIR`, `USAGESTAT_DATA_DIR`, explicit file/plugin paths and
development profiles can select other locations. Provider login stores and
original AI transcripts remain in their owning tools' directories. The backend
uses private file permissions on Unix and current-user/SYSTEM ACLs on Windows
for files it creates through its private storage layer. These are OS access
controls, not application-level encryption of every file. Existing user-supplied
files, privileged software, backups and your OS account remain your responsibility.

The dashboard stores display preferences and optional custom logos in the
browser's local storage for the daemon origin. Versions with collection settings
use a private local setup key and a separate per-launch capability retained in
the tab's session storage. Provider credentials are excluded from display-setting
exports. Older alpha versions may not have these newer settings controls.

## Daemon, dashboard and HTTP API exposure

The daemon listens on `127.0.0.1:6736` by default. Its general usage/history read
API and dashboard do **not** require authentication; general JSON responses allow
cross-origin reads. Local processes, other users able to connect to the listener,
and websites that your browser permits to contact it may read exposed usage
data. General refresh requests can also trigger polling. Loopback is a network
boundary, not proof of the caller's OS identity.

Binding to a non-loopback interface exposes general routes to reachable clients;
the native listener uses HTTP. Do not publish the general API to an untrusted
network. Use access controls and an HTTPS proxy appropriate to your deployment.
The optional T3 management bridge, daemon shutdown, SDK ingestion and newer local
collection-editing routes have separate credential/access checks. Enabling one
of those credentials does not protect every general read route. Account usage
and history returned by general routes can themselves be sensitive even where
raw saved credential fields are not returned.

## Retention and deletion

General snapshots, polling history, saved daily/model summaries, configuration
and provider state have no automatic age-based deletion policy. They remain until
you replace or delete them. Stopping collection, hiding a dashboard card,
removing a provider configuration or uninstalling the package does not erase
saved history. Removing original transcripts does not erase summaries already
retained by UsageStat. Deleted caches can be rebuilt while collection still has
access to their original sources.

Optional SDK run ingestion has its own expiry/retention rules (30 days by
default, configurable by the operator, and shortened by an event's explicit
expiry). Expired events are purged; this does not promise forensic erasure from
storage media or backups. See the [ingestion contract](docs/run-ingestion.md)
for the operator controls and delivery behavior.

To remove local UsageStat data, first stop the daemon and disable its autostart
(`usagestat daemon disable`; unregister the service if removing the installation).
Then delete the selected configuration and data directories, any explicitly
configured ingestion database and its sidecar files, exported reports, and
service logs you no longer want. Clear the dashboard origin's browser storage
to remove display preferences and close its tabs to discard session capabilities.
Also check custom directories and backups. UsageStat does not delete the original
provider login stores or transcripts; manage those in the owning application.
Revoke credentials with the provider if you want to invalidate them rather than
only remove a local copy. Removing local state cannot recall data already sent
to providers, configured forwarding recipients, or people you shared exports with.

## Your controls and contact

You can disable a provider, choose its source, remove saved credentials, stop
the daemon, or use ingestion-only mode. Edit `config.toml`; newer versions also
offer collection controls in the local dashboard. Hiding a provider affects
display only. Disabling polling does not revoke a provider token. Keep the
listener local, restrict access to your OS account, and configure forwarding
only for destinations you intend to receive the records.

For privacy questions, contact Hashim Karim through the
[UsageStat repository](https://github.com/hashimkarim/usagestat). Public issues
are public: do not post tokens, cookies, personal transcripts or unredacted
account records. Any future change to this policy will update this file and its
effective date in the public repository history.
