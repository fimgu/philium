"""Bundled macOS check supervisor. Invoked by checks.rs with isolated Python.
Only human-approved Make targets in a disposable local clone. Never use for hostile code.
"""
import contextlib
import fcntl
import json
import os
from pathlib import Path
import re
import selectors
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time
import uuid

CAP = 32768
GIT = ['/usr/bin/git', '-c', 'core.hooksPath=/dev/null', '-c', 'core.fsmonitor=false', '-c', 'submodule.recurse=false']
ENV = {'PATH':'/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin', 'LANG':'C', 'HOME':'/var/empty', 'GIT_CONFIG_NOSYSTEM':'1', 'GIT_CONFIG_GLOBAL':'/dev/null', 'GIT_ALLOW_PROTOCOL':'file', 'GIT_TERMINAL_PROMPT':'0', 'GIT_NO_LAZY_FETCH':'1'}
SCHEMA = '''CREATE TABLE IF NOT EXISTS runs(id TEXT PRIMARY KEY,repo_id TEXT NOT NULL,task_id TEXT,sha TEXT NOT NULL,target TEXT NOT NULL,timeout INTEGER NOT NULL,state TEXT NOT NULL,started INTEGER NOT NULL,elapsed INTEGER,code INTEGER,stdout TEXT,stderr TEXT,cancel INTEGER DEFAULT 0)'''


def git_info(src):
    def read(*args):
        return subprocess.check_output(GIT + list(args), cwd=src, env=ENV, timeout=8, stderr=subprocess.DEVNULL).decode().strip()
    if Path(read('rev-parse','--show-toplevel')).resolve() != src.resolve():
        raise ValueError('Registered path is not a checkout root')
    # No external diff or unbounded untracked-file listing. Never use working files as evidence.
    if read('status','--porcelain','--untracked-files=normal','--ignore-submodules=none'):
        raise ValueError('Commit or stash changes before running a pinned check')
    return read('rev-parse','--verify','HEAD^{commit}')


def command(argv, cwd, env, deadline, abort):
    out = [bytearray(), bytearray()]
    truncated = [False, False]
    proc = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
    state = None
    with selectors.DefaultSelector() as sel:
        for i, pipe in enumerate((proc.stdout, proc.stderr)):
            sel.register(pipe, selectors.EVENT_READ, i)
        try:
            while sel.get_map() or proc.poll() is None:
                state = abort() or ('timed_out' if time.monotonic() >= deadline else None)
                if state: break
                for key, _ in sel.select(0.08):
                    chunk = os.read(key.fd, 8192)
                    if not chunk:
                        sel.unregister(key.fileobj)
                        continue
                    i = key.data
                    remain = CAP - len(out[i]); out[i].extend(chunk[:remain]); truncated[i] |= len(chunk) > remain
        finally:
            with contextlib.suppress(ProcessLookupError): os.killpg(proc.pid, signal.SIGKILL)
            proc.wait(); proc.stdout.close(); proc.stderr.close()
    text = [bytes(x).decode(errors='replace') + ('\n[output truncated]' if t else '') for x,t in zip(out,truncated)]
    return state or ('passed' if proc.returncode == 0 else 'failed'),proc.returncode,*text


def serve(root, action, p):
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    db = sqlite3.connect(root/'results.sqlite', isolation_level=None, timeout=3); db.row_factory = sqlite3.Row
    db.execute(SCHEMA)
    repo = p['repo_id']; src = Path(p['path']); lock = (root/'owner.lock').open('a')
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB); owns = True
    except BlockingIOError: owns = False
    if owns:
        # The supervisor owns the lock until cleanup. Never kill a PID from a previous boot.
        db.execute("UPDATE runs SET state='interrupted' WHERE state='running'")
    if action == 'list': return [dict(r) for r in db.execute('SELECT * FROM runs WHERE repo_id=? ORDER BY started DESC LIMIT 20',(repo,))]
    if action == 'cancel':
        if not db.execute("UPDATE runs SET cancel=1 WHERE repo_id=? AND id=? AND state='running'",(repo,p['id'])).rowcount: raise ValueError('No active check for this checkout')
        return True
    if action == 'preview': return {'sha':git_info(src)}
    if action != 'start' or not owns: raise ValueError('Another local check is active')
    target = p['target']; timeout = p['timeout']; sha = p['sha']
    if not re.fullmatch(r'[a-zA-Z0-9][a-zA-Z0-9_.-]{0,79}',target) or type(timeout) is not int or not 1 <= timeout <= 600: raise ValueError('Invalid Make target or timeout (1 to 600 seconds)')
    if git_info(src) != sha: raise ValueError('Checkout changed since preview')
    rid = uuid.uuid4().hex; start = time.monotonic(); deadline=start+timeout
    db.execute('INSERT INTO runs(id,repo_id,task_id,sha,target,timeout,state,started) VALUES(?,?,?,?,?,?,?,?)',(rid,repo,p.get('task_id'),sha,target,timeout,'running',int(time.time()*1000)))
    print(json.dumps({'id':rid}),flush=True)
    def abort():
        import select
        if select.select([sys.stdin],[],[],0)[0]: return 'interrupted'
        if db.execute('SELECT cancel FROM runs WHERE id=?',(rid,)).fetchone()[0]: return 'cancelled'
    state,code,out,err = 'failed',None,'',''
    try:
        with tempfile.TemporaryDirectory(prefix='snapshot-', dir=root) as tmp:
            d=Path(tmp).resolve(); tree=d/'source'; home=d/'home'; home.mkdir()
            # Local clone has no user hooks, config or untracked files. No source worktree registration.
            for argv,where in [(GIT+['clone','--quiet','--local','--shared','--no-checkout','--template=','--',str(src),str(tree)],d),(GIT+['checkout','--quiet','--detach',sha],tree)]:
                state,code,out,err = command(argv,where,ENV,deadline,abort)
                if state != 'passed': break
            else:
                env=dict(ENV,HOME=str(home),TMPDIR=str(d),GOPROXY='off',GOSUMDB='off',GOTOOLCHAIN='local',CARGO_NET_OFFLINE='true',npm_config_offline='true')
                profile='(version 1)(allow default)(deny network*)(deny file-write*)(allow file-write* (subpath (param "ROOT")) (literal "/dev/null"))'
                state,code,out,err=command(['/usr/bin/xcrun','--find','make'],home,ENV,deadline,abort)
                if state == 'passed':
                    make = out.strip()
                    if not os.path.isabs(make): raise ValueError('No local Make toolchain')
                    state,code,out,err=command(['/usr/bin/sandbox-exec','-D','ROOT='+str(d),'-p',profile,make,'-f','Makefile','--',target],tree,env,deadline,abort)
    except Exception as e:
        state,err='failed',str(e)
    db.execute('UPDATE runs SET state=?,code=?,elapsed=?,stdout=?,stderr=? WHERE id=?',(state,code,int(1000*(time.monotonic()-start)),out,err,rid))
    return None


if __name__ == '__main__':
    os.umask(0o077)
    try:
        result=serve(Path(sys.argv[1]),sys.argv[2],json.loads(sys.argv[3]))
        if result is not None: print(json.dumps(result),flush=True)
    except Exception as e:
        print(json.dumps({'error':str(e)}),flush=True); sys.exit(1)
