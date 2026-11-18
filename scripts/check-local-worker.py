"""Exercise the actual macOS supervisor, subprocess group, disposable clone and SQLite."""
import json, os, subprocess as sp, sys, tempfile, time
from pathlib import Path

worker=Path(__file__).resolve().parents[1]/'src-tauri/src/check_worker.py'
with tempfile.TemporaryDirectory(dir=os.environ.get('PHILIUM_SCRATCH_DIR')) as tmp:
    root=Path(tmp); src=root/'repo'; src.mkdir(); data=root/'profile'
    def git(*a): return sp.check_output(['/usr/bin/git','-c','user.name=Fixture','-c','user.email=f@example.invalid','-c','core.hooksPath=/dev/null',*a],cwd=src,text=True).strip()
    git('init','-q'); (src/'Makefile').write_text('pass:\n\t@echo marker; echo error-marker >&2\nfail:\n\t@false\nbig:\n\t@yes x | head -c 50000\nslow:\n\t@sleep 20\nmissing:\n\t@/not-a-real-command\n')
    git('add','Makefile');git('commit','-qm','fixture'); sha=git('rev-parse','HEAD')
    def cmd(action,**args):return ['/usr/bin/python3','-I','-B',str(worker),str(data),action,json.dumps(dict({'repo_id':'r1','path':str(src),'sha':sha,'target':'pass','timeout':10},**args))]
    def rpc(action,**args):return json.loads(sp.run(cmd(action,**args),capture_output=True,text=True,timeout=15).stdout)
    def start(target,**kw):
        p=sp.Popen(cmd('start',target=target,**kw),stdin=sp.PIPE,stdout=sp.PIPE,text=True); first=json.loads(p.stdout.readline());assert 'id' in first,first;return p,first['id']
    def result(p,id):
        p.wait(timeout=12); p.stdin.close(); p.stdout.close(); rows=rpc('list');return next(r for r in rows if r['id']==id)
    assert rpc('preview')['sha']==sha
    for target,state in [('pass','passed'),('fail','failed'),('missing','failed'),('big','passed'),('slow','timed_out')]:
        p,id=start(target,timeout=1 if target=='slow' else 10); r=result(p,id); assert r['state']==state,r
        if target=='pass':assert r['stdout'].strip()=='marker' and r['stderr'].strip()=='error-marker' and r['code']==0,r
        if target=='big':assert '[output truncated]' in r['stdout'] and len(r['stdout'])<33000,r
        print(target,state)
    p,id=start('slow');assert 'error' in rpc('start');assert rpc('list',repo_id='peer')==[];assert 'error' in rpc('cancel',repo_id='peer',id=id)
    assert rpc('cancel',id=id) is True;assert result(p,id)['state']=='cancelled'
    p,id=start('slow');p.stdin.close();assert result(p,id)['state']=='interrupted'
    assert 'error' in rpc('start',sha='0'*40); (src/'dirty').touch();assert 'error' in rpc('preview'); assert not list(data.glob('snapshot-*'))
    print('cancel, parent loss, peer isolation, stale SHA, dirty checkout, cleanup: passed')
assert not root.exists()
