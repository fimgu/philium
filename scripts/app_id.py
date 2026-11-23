"""The app identifier, read from the one source of truth: src-tauri/tauri.conf.json."""
import json
from pathlib import Path

_CONF = Path(__file__).resolve().parent.parent / 'src-tauri' / 'tauri.conf.json'
ID = json.loads(_CONF.read_text())['identifier']
PILOT = ID + '.pilot'
PREVIEW = ID + '.preview'
