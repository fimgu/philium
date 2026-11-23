"""Read-only public origin, real packaged form, controlled local deployment. No remote writes."""
import hashlib, http.server, json, os, plistlib, socket, subprocess, sys, tempfile, threading, time
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
import app_id  # noqa: E402

def run(app):
    info=plistlib.loads((app/'Contents/Info.plist').read_bytes()); assert info['CFBundleIdentifier']==app_id.PILOT
    subprocess.run(['codesign','--verify','--deep','--strict',app],check=True)
    exe=app/'Contents/MacOS'/info['CFBundleExecutable']; url='https://github.com/typicode/demo/pull/1090'
    head='610dcdf4f371d2e47f15c4edcad6837b4b8c862b'; base='f0b44972443d306e32eed093910687d26cd9f3f1'
    with tempfile.TemporaryDirectory(prefix='public-review-',dir=os.environ.get('PHILIUM_SCRATCH_DIR') or None) as tmp:
        root=Path(tmp); home=root/'home';home.mkdir();(home/'.philium-pilot-fixture').write_text('disposable\n'); (root/'tmp').mkdir()
        env=dict(HOME=str(home),TMPDIR=str(root/'tmp'),PATH='/usr/bin:/bin:/usr/sbin:/sbin',GIT_CONFIG_NOSYSTEM='1',GIT_CONFIG_GLOBAL='/dev/null',GIT_TERMINAL_PROMPT='0')
        def cmd(args,cwd=root):
            try:return subprocess.check_output(args,cwd=cwd,env=env,stderr=subprocess.PIPE,timeout=100)
            except subprocess.CalledProcessError as e:
                if args[0]==exe:print('Packaged diagnostic:',e.stderr.decode(errors='replace').splitlines()[-3:],flush=True)
                raise
        repo=root/'product';driver=root/'driver';driver.mkdir();cmd(['git','-c','credential.helper=','clone','-q','--no-checkout','https://github.com/typicode/demo.git',repo]);cmd(['git','-c','credential.helper=','fetch','-q','origin','refs/pull/1090/head'],repo)
        assert cmd(['git','rev-parse','FETCH_HEAD'],repo).decode().strip()==head,'Public PR moved: re-freeze expectations before rerun'
        blob=cmd(['git','show',head+':db.json'],repo); artifact='sha256:'+hashlib.sha256(blob).hexdigest();assert json.loads(blob)['employees'][0]['id']==15
        cmd(['git','init','-q'],driver);(driver/'philium/releases').mkdir(parents=True); spec={'release_records':True,'cases':[{'id':'first-id','document':'db.json','pointer':'/employees/0/id','expect':15}],'live_cases':[{'id':'live-id','path':'/first-id','pointer':'/id','expect':15}]};(driver/'philium/sql-contracts.json').write_text(json.dumps(spec))
        def commit():cmd(['git','add','--','philium'],driver);cmd(['git','-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','-qm','Fixture'],driver)
        commit(); pin=cmd(['git','rev-parse','HEAD'],driver).decode().strip()
        mode=['pass']; requests=[]
        class Service(http.server.BaseHTTPRequestHandler):
            def log_message(self,*args):pass
            def do_GET(self):
                requests.append(self.path)
                if self.path=='/.well-known/philium/deployment':body={'observed_ms':int(time.time()*1000),'complete':True,'replicas':[{'ready':True,'revision':head if mode[0]!='wrong-revision' else base,'digest':artifact}]}
                elif self.path=='/first-id': body={'id':json.loads(blob)['employees'][0]['id'] if mode[0]!='fail' else 0}
                else:self.send_error(404);return
                b=json.dumps(body).encode();self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(b)));self.send_header('X-Philium-Revision',head);self.send_header('X-Philium-Artifact',artifact);self.end_headers();self.wfile.write(b)
        with http.server.ThreadingHTTPServer(('127.0.0.1',0),Service) as srv:
            port=srv.server_port; thread=threading.Thread(target=srv.serve_forever,daemon=True);thread.start()
            try:
                cmd([exe,'--pin-review-driver',repo,driver,str(port)])
                record={'origin':'typicode/demo','pr':1090,'base':base,'head':head,'driver_pin':pin,'artifact':artifact};(driver/f'philium/releases/{head}.json').write_text(json.dumps(record));commit()
                def check():
                    out=json.loads(cmd([exe,'--review-url',repo,url])); assert out['head']==head and out['origin_current'] is True
                    print('Public PR',out['head'][:12],mode[0],out['contract_decision'],out['decision'],flush=True);return out
                p=check(); assert p['contract_decision']=='scoped_pass' and p['decision']=='incomplete' and p['receipt']['release_record_pin']!=pin
                # There is deliberately NO code-review or cleanup attestation about this public PR.
                mode[0]='fail';f=check();assert f['decision']=='not_ready' and f['generation']==p['generation']
                mode[0]='wrong-revision';n=requests.count('/first-id');s=check();assert s['decision']=='incomplete' and requests.count('/first-id')==n
                mode[0]='pass';n=requests.count('/first-id');ui=cmd([exe,'--pilot-check-review-form']);result=json.loads([line for line in ui.splitlines() if b'"native_review"' in line][-1]);assert result=={'native_review':'Release verification incomplete','head':head,'live_case':True} and requests.count('/first-id')==n+2;print('Native Review:',result,flush=True)
            finally:srv.shutdown();thread.join(3)
        assert not thread.is_alive();sock=socket.socket();assert sock.connect_ex(('127.0.0.1',port))!=0;sock.close()
        mode[0]='absent';last=check();assert last['decision']=='incomplete' and last['receipt']['deployment_identity']['state']=='unknown'
    assert not Path(tmp).exists();print('Owned service stopped, disposable checkout, independent driver and profile removed.')
if __name__=='__main__':run(Path(sys.argv[1]).resolve(strict=True))
