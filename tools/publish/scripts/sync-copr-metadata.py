#!/usr/bin/env python3
"""Sync public COPR metadata without changing build settings or credentials."""
import json
from pathlib import Path
import sys


def sync(proxy, project, metadata):
    if set(metadata) != {'homepage', 'description'} or any(
            not isinstance(value, str) or not value.strip() for value in metadata.values()):
        raise ValueError('COPR metadata must contain only a homepage and description')
    owner, name = project.split('/')
    target = dict(ownername=owner, projectname=name)
    current = proxy.get(**target)
    changed = {key: value for key, value in metadata.items() if current.get(key) != value}
    if changed:
        # The SDK leaves build settings unset; CLI modify also supplies defaults.
        proxy.edit(**target, **changed)
    return bool(changed)


def main():
    # Installed with copr-cli in the publishing job's virtual environment.
    from copr.v3 import Client

    config, project, metadata_path = sys.argv[1:]
    client = Client.create_from_config_file(config)
    changed = sync(client.project_proxy, project, json.loads(Path(metadata_path).read_text()))
    print(f'COPR {project}: metadata {"updated" if changed else "already current"}')


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        # Avoid exposing credentials or request details from client exceptions.
        print(f'::error::COPR metadata sync failed ({type(error).__name__}); check project admin access.',
              file=sys.stderr)
        sys.exit(1)
