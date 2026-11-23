#!/usr/bin/env python3
"""Exercise an actual macOS pilot bundle, without LaunchServices or user profiles."""
import os
from pathlib import Path
import plistlib
import subprocess
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parent))
import app_id  # noqa: E402


def main():
    app = Path(sys.argv[1]).resolve(strict=True)
    with (app / 'Contents/Info.plist').open('rb') as stream:
        info = plistlib.load(stream)
    if info['CFBundleIdentifier'] != app_id.PILOT:
        raise SystemExit('Refusing to invoke a non-pilot bundle')
    subprocess.run(['codesign', '--verify', '--deep', '--strict', str(app)], check=True)
    exe = app / 'Contents/MacOS' / info['CFBundleExecutable']
    if not (exe.parent / 'hal-desktop-server').is_file():
        raise SystemExit('Missing bundled HAL')
    with tempfile.TemporaryDirectory(prefix='pilot-check-', dir=os.environ.get('PHILIUM_SCRATCH_DIR') or None) as scratch:
        root = Path(scratch).resolve()
        home = root / 'home'; home.mkdir()
        temp = root / 'tmp'; temp.mkdir()
        (home / '.philium-pilot-fixture').write_text('disposable\n')
        # Bad inherited overrides must never be used. No credentials in the harness environment.
        decoy = root / 'not-the-profile'
        env = {'HOME': str(home), 'TMPDIR': str(temp), 'PATH': '/usr/bin:/bin:/usr/sbin:/sbin',
               'PHILIUM_ROOT': str(decoy), 'PHILIUM_DATA_DIR': str(decoy), 'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': '/dev/null'}
        def run(phase, error=None):
            r = subprocess.run([str(exe), '--pilot-check', phase], env=env, stdin=subprocess.DEVNULL,
                               capture_output=True, text=True, timeout=90)
            if (r.returncode == 0) != (error is None) or (error and error not in r.stderr):
                raise RuntimeError(f'{phase}: {r.returncode} {r.stderr[-600:]}')
            if not error:
                print(r.stdout.strip())
            if decoy.exists():
                raise RuntimeError('Inherited profile was touched')
        run('recover', 'Unexpected profile state'); run('seed'); run('seed', 'Unexpected profile state'); run('recover'); run('recover')
        bad = root / 'link-test'; bad.mkdir(); (bad / 'Philium Pilot').symlink_to(home, target_is_directory=True)
        env['HOME'] = str(bad); run('seed', 'Not a private directory')
    if Path(scratch).exists():
        raise RuntimeError('Test profile not cleaned up')
    print('Signature, bundled HAL, recovery, peer isolation and cleanup passed. No GUI was launched.')

if __name__ == '__main__':
    main()
