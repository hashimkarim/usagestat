#!/usr/bin/env bash
# Run after prepare-packages.py. Dry runs validate without publishing or credentials.
set -euo pipefail
platform="${1:?platform required}"
package_dir="$(realpath "${2:?prepared package directory required}")"
: "${RELEASE_TAG:?}"
channel="${PACKAGE_CHANNEL:-stable}"
aur_package=usagestat-bin
project=usagestat
formula=usagestat
if [[ "$channel" == alpha ]]; then
  [[ "$RELEASE_TAG" =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)-alpha\.(0|[1-9][0-9]*)$ ]] || exit 1
  aur_package=usagestat-alpha-bin
  project=usagestat-alpha
  formula=usagestat-alpha
else
  [[ "$channel" == stable && "$RELEASE_TAG" =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] || exit 1
fi
version="${RELEASE_TAG#v}"
package_version="${version/-/\~}"
dry_run="${DRY_RUN:-true}"
[[ "$dry_run" == true || "$dry_run" == false ]] || exit 1
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

require_secret() {
  if [[ -z "${!1:-}" ]]; then
    echo "::error::Missing repository secret $1. See docs/distribution.md."
    exit 1
  fi
}

assert_latest() {
  if [[ "$channel" == alpha ]]; then
    python3 "$script_dir/release_channel.py" "$RELEASE_TAG" --channel alpha
    return
  fi
  python3 - "$RELEASE_TAG" <<'PYLATEST'
import json, sys, urllib.request
with urllib.request.urlopen('https://api.github.com/repos/Hashim-K/usagestat/releases/latest', timeout=30) as response:
    latest = json.load(response)['tag_name']
if latest != sys.argv[1]:
    raise SystemExit(f'Refusing to publish {sys.argv[1]}: latest stable release is now {latest}')
PYLATEST
}

commit_and_push() {
  git config user.name 'usagestat release bot'
  git config user.email '41898282+github-actions[bot]@users.noreply.github.com'
  if git diff --cached --quiet; then
    echo "$platform already contains $RELEASE_TAG"
  else
    git commit -m "Release $RELEASE_TAG"
    git push origin HEAD
  fi
}

case "$platform" in
  aur)
    # Validation and .SRCINFO generation run as an unprivileged Arch user.
    mkdir -p "$package_dir/aur"
    cp "$package_dir/PKGBUILD" "$package_dir/aur/"
    chmod -R a+rwX "$package_dir/aur"
    docker run --rm -e EXPECTED_VERSION="$version" -v "$package_dir/aur:/package" archlinux:base-devel bash -euc '
      pacman -Sy --noconfirm --needed git
      useradd -m builder
      cd /package
      runuser -u builder -- makepkg --verifysource --noconfirm
      runuser -u builder -- makepkg --printsrcinfo > .SRCINFO
      runuser -u builder -- makepkg --nodeps --noconfirm
      pacman -U --noconfirm ./*.pkg.tar.zst
      test "$(usagestat --version)" = "usagestat $EXPECTED_VERSION"
      usagestatd --help >/dev/null
      test -f /usr/share/usagestat/plugins/codex/plugin.json
    '
    if [[ "$dry_run" == false ]]; then
      assert_latest
      require_secret AUR_SSH_PRIVATE_KEY
      : "${AUR_SSH_KNOWN_HOSTS:?Set the AUR_SSH_KNOWN_HOSTS repository variable to the verified host key}"
      ssh_dir="$(mktemp -d)"
      trap 'rm -rf "$ssh_dir"' EXIT
      chmod 700 "$ssh_dir"
      printf '%s\n' "$AUR_SSH_PRIVATE_KEY" > "$ssh_dir/key"
      printf '%s\n' "$AUR_SSH_KNOWN_HOSTS" > "$ssh_dir/known_hosts"
      chmod 600 "$ssh_dir/key"
      export GIT_SSH_COMMAND="ssh -i $ssh_dir/key -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=$ssh_dir/known_hosts"
      git clone "ssh://aur@aur.archlinux.org/$aur_package.git" "$package_dir/aur-repo"
      cp "$package_dir/aur/PKGBUILD" "$package_dir/aur/.SRCINFO" "$package_dir/aur-repo/"
      cd "$package_dir/aur-repo"
      git add PKGBUILD .SRCINFO
      commit_and_push
    fi
    ;;
  homebrew)
    ruby -c "$package_dir/usagestat.rb"
    test "$("$package_dir/x86_64/usagestat" --version)" = "usagestat $version"
    "$package_dir/x86_64/usagestatd" --help >/dev/null
    if [[ "$dry_run" == false ]]; then
      require_secret HOMEBREW_SSH_PRIVATE_KEY
      : "${HOMEBREW_TAP_REPOSITORY:?Set the HOMEBREW_TAP_REPOSITORY repository variable. See docs/distribution.md.}"
      assert_latest
      ssh_dir="$(mktemp -d)"
      trap 'rm -rf "$ssh_dir"' EXIT
      chmod 700 "$ssh_dir"
      printf '%s\n' "$HOMEBREW_SSH_PRIVATE_KEY" > "$ssh_dir/key"
      chmod 600 "$ssh_dir/key"
      # GitHub publishes its SSH host keys through its authenticated HTTPS API.
      python3 - <<'PYKEYS' > "$ssh_dir/known_hosts"
import json, urllib.request
with urllib.request.urlopen('https://api.github.com/meta', timeout=30) as response:
    for key in json.load(response)['ssh_keys']:
        print('github.com ' + key)
PYKEYS
      export GIT_SSH_COMMAND="ssh -i $ssh_dir/key -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=$ssh_dir/known_hosts"
      git clone "git@github.com:${HOMEBREW_TAP_REPOSITORY}.git" "$package_dir/tap"
      cp "$package_dir/usagestat.rb" "$package_dir/tap/Formula/$formula.rb"
      cd "$package_dir/tap"
      git add "Formula/$formula.rb"
      commit_and_push
    fi
    ;;
  copr)
    # rpmspec checks the generated version before a source build is submitted.
    rpmspec -q --qf '%{name} %{version}\n' "$package_dir/usagestat.spec"
    if [[ "$dry_run" == false ]]; then
      assert_latest
      require_secret COPR_CONFIG
      config_file="$(mktemp)"
      trap 'rm -f "$config_file"' EXIT
      chmod 600 "$config_file"
      printf '%s\n' "$COPR_CONFIG" > "$config_file"
      status=0
      python3 "$script_dir/publication-state.py" copr "$version" --channel "$channel" || status=$?
      if [[ "$status" == 2 ]]; then exit 2; fi
      if [[ "$status" == 1 ]]; then
        copr-cli --config "$config_file" build "hashimkarim/$project" --enable-net on "$package_dir/usagestat.spec"
      fi
    fi
    ;;
  ppa)
    source_dir="$package_dir/usagestat-$package_version"
    # Use the same package revision for uploads and publication checks.
    deb_version="$(python3 "$script_dir/publication-state.py" ppa "$version" --channel "$channel" --print-version)"
    cat > "$source_dir/debian/changelog" <<EOF
usagestat ($deb_version) noble; urgency=medium

  * Package usagestat $version, including the daemon and provider plugins.

 -- Hashim Karim <hashimkarim168@gmail.com>  $(date -R)
EOF
    (cd "$source_dir" && dpkg-buildpackage -S -sa -us -uc -d)
    if [[ "$dry_run" == true ]]; then
      (cd "$source_dir" && dpkg-buildpackage -b -us -uc)
      privilege=()
      if (( EUID != 0 )); then privilege=(sudo); fi
      "${privilege[@]}" dpkg -i "$package_dir/usagestat_${deb_version}_amd64.deb"
      test "$(usagestat --version)" = "usagestat $version"
      usagestatd --help >/dev/null
      test -f /usr/share/usagestat/plugins/codex/plugin.json
    else
      assert_latest
      require_secret PPA_GPG_PRIVATE_KEY
      : "${PPA_GPG_FINGERPRINT:?Set the PPA_GPG_FINGERPRINT repository variable}"
      status=0
      python3 "$script_dir/publication-state.py" ppa "$version" --channel "$channel" || status=$?
      if [[ "$status" == 0 ]]; then exit 0; fi
      if [[ "$status" != 1 ]]; then exit "$status"; fi
      export GNUPGHOME
      GNUPGHOME="$(mktemp -d)"
      chmod 700 "$GNUPGHOME"
      trap 'gpgconf --kill gpg-agent; rm -rf "$GNUPGHOME"' EXIT
      printf '%s\n' "$PPA_GPG_PRIVATE_KEY" | gpg --batch --import
      printf '%s' "${PPA_GPG_PASSPHRASE:-}" > "$GNUPGHOME/passphrase"
      chmod 600 "$GNUPGHOME/passphrase"
      cat > "$GNUPGHOME/sign" <<'EOF'
#!/bin/sh
exec gpg --batch --pinentry-mode loopback --passphrase-file "$GNUPGHOME/passphrase" "$@"
EOF
      chmod 700 "$GNUPGHOME/sign"
      debsign -p"$GNUPGHOME/sign" -k"$PPA_GPG_FINGERPRINT" "$package_dir/usagestat_${deb_version}_source.changes"
      dput "ppa:hashimkarim/$project" "$package_dir/usagestat_${deb_version}_source.changes"
      python3 "$script_dir/publication-state.py" ppa "$version" --channel "$channel" --wait
    fi
    ;;
  *) echo "Unknown publishing platform: $platform" >&2; exit 1 ;;
esac
if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  echo "- $platform: $RELEASE_TAG (dry run: $dry_run) completed" >> "$GITHUB_STEP_SUMMARY"
fi
