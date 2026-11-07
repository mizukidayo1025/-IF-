#!/usr/bin/env python3
"""Plan or apply public settings only. All writes target the destination."""
import argparse
import json
import subprocess
from urllib.parse import quote

SOURCE = 'jphacks/tkb_2615'
DESTINATION = 'mizukidayo1025/-IF-'
FIELDS = ['description', 'homepage', 'default_branch', 'has_issues', 'has_wiki',
          'has_projects', 'has_discussions', 'allow_squash_merge', 'allow_merge_commit',
          'allow_rebase_merge', 'allow_auto_merge', 'delete_branch_on_merge',
          'allow_update_branch', 'web_commit_signoff_required',
          'squash_merge_commit_message', 'squash_merge_commit_title',
          'merge_commit_message', 'merge_commit_title']


def api(path, method='GET', payload=None, paginate=False):
    args = ['gh', 'api', '--method', method, path]
    if paginate:
        args += ['--paginate', '--slurp']
    if payload is not None:
        args += ['--input', '-']
    result = subprocess.run(args, input=json.dumps(payload) if payload is not None else None,
                            text=True, capture_output=True, check=False)
    if result.returncode:
        raise RuntimeError(f'{method} {path}: {result.stderr.strip()}')
    value = json.loads(result.stdout) if result.stdout.strip() else None
    return [item for page in value for item in page] if paginate else value


def plan():
    source = api(f'repos/{SOURCE}')
    destination = api(f'repos/{DESTINATION}')
    operations = []
    settings = {key: source[key] for key in FIELDS if key in source
                and source[key] != destination.get(key)}
    # GitHub accepts an empty string for an unset homepage or description.
    for key in ('homepage', 'description'):
        if key in settings and settings[key] is None:
            settings[key] = ''
    if settings:
        operations.append(('PATCH', f'repos/{DESTINATION}', settings))
    if source.get('topics', []) != destination.get('topics', []):
        operations.append(('PUT', f'repos/{DESTINATION}/topics', {'names': source.get('topics', [])}))

    # Read definitions before any writes; do not delete destination definitions.
    labels = api(f'repos/{SOURCE}/labels?per_page=100', paginate=True)
    target_labels = {item['name']: item for item in api(f'repos/{DESTINATION}/labels?per_page=100', paginate=True)}
    for label in labels:
        payload = {key: label.get(key) for key in ('name', 'color', 'description')}
        current = target_labels.get(label['name'])
        if current is None:
            operations.append(('POST', f'repos/{DESTINATION}/labels', payload))
        elif any(current.get(key) != value for key, value in payload.items()):
            operations.append(('PATCH', f'repos/{DESTINATION}/labels/{quote(label["name"], safe="")}',
                               {'new_name': payload['name'], 'color': payload['color'], 'description': payload['description']}))

    milestones = api(f'repos/{SOURCE}/milestones?state=all&per_page=100', paginate=True)
    target_milestones = {item['title']: item for item in api(f'repos/{DESTINATION}/milestones?state=all&per_page=100', paginate=True)}
    for milestone in milestones:
        payload = {key: milestone.get(key) for key in ('title', 'state', 'description', 'due_on')}
        current = target_milestones.get(milestone['title'])
        if current is None:
            operations.append(('POST', f'repos/{DESTINATION}/milestones', payload))
        elif any(current.get(key) != value for key, value in payload.items()):
            operations.append(('PATCH', f'repos/{DESTINATION}/milestones/{current["number"]}', payload))
    return operations


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--apply', action='store_true', help='Apply the printed public-settings plan to the destination.')
    args = parser.parse_args()
    operations = plan()
    print(json.dumps({'source': SOURCE, 'destination': DESTINATION,
                      'scope': 'public basic settings, topics, label definitions, milestone definitions only',
                      'operations': [{'method': method, 'path': path, 'payload': payload}
                                     for method, path, payload in operations]}, ensure_ascii=False, indent=2))
    if not args.apply:
        print('Dry run: no settings were changed. Use --apply to execute these operations.')
        return
    for method, path, payload in operations:
        assert path == f'repos/{DESTINATION}' or path.startswith(f'repos/{DESTINATION}/')
        api(path, method, payload)
        print(f'Applied {method} {path}')
    print('Public-settings plan applied. Administrative settings and secret values are outside this script.')


if __name__ == '__main__':
    main()
