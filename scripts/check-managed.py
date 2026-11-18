"""Exercise real managed RPC on an explicitly selected local Colima profile. No pulls."""
import os, pathlib, subprocess, sys, tempfile

def main(profile, interrupted=False):
    base=pathlib.Path(tempfile.mkdtemp(prefix='managed-check-',dir=os.environ['PHILIUM_SCRATCH_DIR']))
    (base/'disposable').write_text('fixture\n')
    env=dict(os.environ,PHILIUM_MANAGED_FIXTURE=str(base),PHILIUM_TEST_PROFILE=profile)
    cargo=pathlib.Path(__file__).resolve().parents[1]/'src-tauri'
    def run(phase):
        subprocess.run(['cargo','test','--offline','--quiet','managed::tests::runtime_phase','--','--ignored','--exact'],cwd=cargo,env=dict(env,PHILIUM_MANAGED_PHASE=phase),check=True,timeout=180)
        print(phase, 'passed',flush=True)
    try:
        for phase in ['seed','recover'] + (['interrupt'] if interrupted else []) + ['expired']:run(phase)
    finally:
        try:run('cleanup')
        except Exception:
            print('Retained ownership ledger for recovery:',base,file=sys.stderr);raise
        import shutil
        shutil.rmtree(base)
        assert not base.exists()
if __name__=='__main__':
    main(sys.argv[1]); main(sys.argv[1], interrupted=True)
