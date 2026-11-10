#!/usr/bin/env python3
"""Publish CNB native release assets; publish the draft only after both targets exist."""
import argparse
import json
import os
from pathlib import Path
import urllib.error
import urllib.parse
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('tag')
    parser.add_argument('assets', nargs='+', type=Path)
    args = parser.parse_args()
    token = os.environ['CNB_TOKEN']
    repo = os.environ.get('CNB_REPO_SLUG', 'miuda.ai/realtime-quality')
    base = 'https://api.cnb.cool/' + repo + '/-/releases'

    def api(method, url, body=None):
        if urllib.parse.urlparse(url).netloc != 'api.cnb.cool':
            raise ValueError('Unexpected CNB API host')
        req = urllib.request.Request(url, data=json.dumps(body).encode() if body is not None else None,
            headers={'Authorization': 'Bearer ' + token, 'Accept': 'application/vnd.cnb.api+json',
                     'Content-Type': 'application/json'}, method=method)
        with urllib.request.urlopen(req, timeout=60) as response:
            content = response.read()
            return json.loads(content) if content else None

    tag_url = base + '/tags/' + urllib.parse.quote(args.tag, safe='')
    try:
        release = api('GET', tag_url)
    except urllib.error.HTTPError as error:
        if error.code != 404:
            raise
        try:
            release = api('POST', base, {'tag_name': args.tag, 'name': args.tag,
                'target_commitish': os.environ['CNB_COMMIT'], 'draft': True,
                'prerelease': '-' in args.tag, 'make_latest': 'false',
                'body': 'Linux x86_64 and macOS arm64. Model assets are downloaded separately; no audio or models are bundled. See README for setup.'})
        except urllib.error.HTTPError as error:
            if error.code not in (400, 409, 422):
                raise
            release = api('GET', tag_url)  # Other target may have created it concurrently.
    release_url = base + '/' + str(release['id'])
    for path in args.assets:
        if not path.is_file() or not (path.name.endswith('.tar.gz') or path.name.endswith('.tar.gz.sha256')):
            raise ValueError(f'Not a release package: {path}')
        upload = api('POST', release_url + '/asset-upload-url',
                     {'asset_name': path.name, 'size': path.stat().st_size, 'overwrite': True})
        # Signed object-store URLs do not receive the CNB Authorization header.
        with path.open('rb') as stream:
            req = urllib.request.Request(upload['upload_url'], data=stream,
                headers={'Content-Length': str(path.stat().st_size), 'Content-Type': 'application/octet-stream'}, method='PUT')
            with urllib.request.urlopen(req, timeout=300) as response:
                response.read()
        api('POST', upload['verify_url'])
        print(f'Uploaded {path.name}')
    release = api('GET', release_url)
    names = {asset['name'] for asset in release.get('assets', [])}
    expected = {f'realtime-quality-{args.tag}-{target}.tar.gz{suffix}'
                for target in ['x86_64-unknown-linux-gnu', 'aarch64-apple-darwin']
                for suffix in ['', '.sha256']}
    if expected <= names:
        api('PATCH', release_url, {'draft': False, 'make_latest': 'false' if '-' in args.tag else 'true'})
        print('Both platform packages present; release published')
    else:
        print('Draft retained until both platform packages and checksums are present')


if __name__ == '__main__':
    main()
