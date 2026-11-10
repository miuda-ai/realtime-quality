#!/usr/bin/env python3
"""Reject model, recording, local configuration and build outputs in tracked files."""
from pathlib import Path
import subprocess

for name in subprocess.check_output(['git', 'ls-files', '-z']).decode().split('\0'):
    if not name:
        continue
    path = Path(name)
    if path.suffix.lower() in {'.wav', '.aiff', '.aif', '.pcm', '.raw', '.mp3', '.ogg', '.flac', '.m4a', '.onnx', '.ort', '.bz2', '.gz', '.zip'}:
        raise SystemExit(f'Forbidden asset in repository: {name}')
    if path.parts[0] in {'models', 'test-audio', 'target', 'dist'} or path.name in {'config.local.toml', 'sample-evaluation.json', '.env'}:
        raise SystemExit(f'Local asset in repository: {name}')
print('Tracked files contain no model or audio assets')
