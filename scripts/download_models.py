#!/usr/bin/env python3
"""Download only pinned encoder/decoder/joiner/tokens assets, never sample audio."""
import argparse
import hashlib
import json
from pathlib import Path
import urllib.request


def checksum(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--directory', type=Path, required=True)
    parser.add_argument('--language', choices=['en', 'id', 'both'], default='both')
    args = parser.parse_args()
    destination = args.directory.expanduser().resolve()
    project = Path(__file__).resolve().parent.parent
    if destination == project or project in destination.parents:
        parser.error('models must be stored outside the project directory')
    manifest = json.loads(Path(__file__).with_name('models.json').read_text())
    for language, model in manifest.items():
        if args.language not in ('both', language):
            continue
        directory = destination / language
        directory.mkdir(parents=True, exist_ok=True)
        for name, expected in model['sha256'].items():
            output = directory / name
            if output.is_file() and checksum(output) == expected:
                print(f'Verified {output}')
                continue
            url = f"https://huggingface.co/{model['repository']}/resolve/{model['revision']}/{name}"
            partial = output.with_suffix(output.suffix + '.part')
            print(f'Downloading {language}/{name}')
            try:
                with urllib.request.urlopen(url, timeout=120) as response, partial.open('wb') as stream:
                    while block := response.read(1024 * 1024):
                        stream.write(block)
                if checksum(partial) != expected:
                    raise ValueError(f'Checksum mismatch: {name}')
                partial.replace(output)
            finally:
                partial.unlink(missing_ok=True)


if __name__ == '__main__':
    main()
