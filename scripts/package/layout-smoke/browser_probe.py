#!/usr/bin/env python3
"""Actual browser -> connectNative -> bundle host -> detached nonce daemon fixture.

Firefox uses Mozilla's own test-only dirsvc provider pattern, NOT profile/HOME
redirection of default macOS host registration. Never install into the real user
NativeMessagingHosts. This test hook is reported separately from product installation.
"""
import argparse
import base64
import hashlib
import json
import os
import pwd
from pathlib import Path
import re
import signal
import socket
import subprocess
import time
import urllib.request
from register_fixture import manifest as host_manifest

ROOT = Path(__file__).resolve().parents[3]

def wait_for(fn, timeout=15):
    end=time.monotonic()+timeout
    while time.monotonic()<end:
        value=fn()
        if value: return value
        time.sleep(.05)
    raise TimeoutError('browser fixture condition not observed')

def alive(pid):
    try:
        os.kill(pid,0)
        # Exclude zombies; macOS may briefly retain an adopted child.
        return subprocess.check_output(['/bin/ps','-p',str(pid),'-o','stat='],text=True).strip()[:1] != 'Z'
    except (ProcessLookupError, subprocess.CalledProcessError): return False

def acked(private):
    result={}
    for f in private.glob('events-*.txt'):
        match=re.search(r'client-ack daemon=(\d+)', f.read_text())
        if match: result[int(f.stem.split('-')[1])]=int(match[1])
    return result

def snapshot(paths):
    result={}
    for p in paths:
        result[str(p)]={'exists':p.exists()}
        if p.is_dir():
            result[str(p)]['files']={f.name: hashlib.sha256(f.read_bytes()).hexdigest()
                for f in p.iterdir() if f.is_file()}
        elif p.is_file(): result[str(p)]['sha256']=hashlib.sha256(p.read_bytes()).hexdigest()
    return result

class Marionette:
    def __init__(self, port):
        self.sock=socket.create_connection(('127.0.0.1',port),timeout=10)
        self.sock.settimeout(20);self.serial=0
        self.hello=self.read()
    def read(self):
        header=b''
        while not header.endswith(b':'): header+=self.sock.recv(1)
        n=int(header[:-1]);data=b''
        while len(data)<n:
            b=self.sock.recv(n-len(data))
            if not b: raise EOFError('Marionette closed')
            data+=b
        return json.loads(data)
    def call(self, method, params=None):
        self.serial+=1
        data=json.dumps([0,self.serial,method,params or {}]).encode()
        self.sock.sendall(str(len(data)).encode()+b':'+data)
        result=self.read()
        if result[2]: raise RuntimeError(result[2])
        return result[3]
    def script(self, script, args=None):
        return self.call('WebDriver:ExecuteScript', {'script':script,'args':args or [],
            'newSandbox':False, 'sandbox':'system','line':1,'filename':'cm34-fixture'})

class CDP:
    """Minimal RFC6455 client only for Browser.close on our own ephemeral endpoint."""
    def __init__(self,url):
        from urllib.parse import urlsplit
        u=urlsplit(url);self.sock=socket.create_connection((u.hostname,u.port),timeout=10)
        key=base64.b64encode(os.urandom(16)).decode()
        self.sock.sendall((f'GET {u.path} HTTP/1.1\r\nHost: {u.hostname}:{u.port}\r\n'
          f'Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n'
          'Sec-WebSocket-Version: 13\r\n\r\n').encode())
        data=b''
        while b'\r\n\r\n' not in data: data+=self.sock.recv(1)
        if not data.startswith(b'HTTP/1.1 101'): raise RuntimeError(data.decode())
        expected=base64.b64encode(hashlib.sha1((key+'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest())
        if expected not in data: raise RuntimeError('invalid websocket accept')
    def close_browser(self):
        data=b'{"id":1,"method":"Browser.close"}';mask=os.urandom(4)
        self.sock.sendall(bytes([0x81,0x80|len(data)])+mask+bytes(b^mask[i%4] for i,b in enumerate(data)))
        self.sock.close()

def fixture(extension, meta, kind, host_name):
    extension.mkdir()
    config='const CONFIG = '+json.dumps({'host':host_name,'nonce':meta['nonce']})+';\n'
    (extension/'background.js').write_text(config+(ROOT/'tests/fixtures/lifecycle-probe/background.js').read_text())
    manifest={'name':'Citation manager #34 isolated nonce fixture','version':'1.0',
        'description':'Only nonce ping; no web pages/network/user data','permissions':['nativeMessaging']}
    if kind=='chromium':
        # Public test key fixes the fixture ID across bundle moves; not a credential.
        key=base64.b64encode(b'citation-manager-34-test-fixture-public-key').decode()
        manifest.update(manifest_version=3,key=key,background={'service_worker':'background.js'})
        raw=hashlib.sha256(base64.b64decode(key)).digest()[:16]
        extension_id=''.join(chr(97+int(c,16)) for c in raw.hex())
    else:
        extension_id='cm34-probe@fixture.invalid'
        manifest.update(manifest_version=2,background={'scripts':['background.js']},
            browser_specific_settings={'gecko':{'id':extension_id}})
    (extension/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
    return extension_id

def run(kind,browser,bundle_meta,out):
    # Outer Seatbelt conflicts with current browser self-sandboxing. CfT also
    # relaunched without profile flags after the failed trial. Do not repeat that
    # on an interactive user's account. A separate OS account/VM is now required.
    if not pwd.getpwuid(os.getuid()).pw_name.startswith('cm34test-'):
        raise RuntimeError('Browser gate BLOCKED: run only in a disposable macOS OS account named cm34test-*. A browser profile alone does not isolate macOS registrations/Crashpad; do not bypass browser sandboxing.')
    meta=json.loads(bundle_meta.read_text());bundle=Path(meta['bundle']).resolve()
    private=Path(meta['private']);out=out.resolve();out.mkdir(mode=0o700)
    nonce=meta['nonce'];host_name='test.citationmanager.probe_'+nonce
    # No overlapping test runs for this bundle/private socket domain.
    if (private/'probe.sock').exists(): raise RuntimeError('probe already running')
    for p in private.glob('events-*.txt'): p.unlink()
    profile=out/'profile';profile.mkdir(mode=0o700)
    extension=out/'extension';extension_id=fixture(extension,meta,kind,host_name)
    manifest=host_manifest(bundle,kind,host_name,extension_id)
    # Serialize structured values again so Unicode/spaces cannot break JSON.
    if kind=='chromium':
        manifests=profile/'NativeMessagingHosts'
    else:
        manifests=out/'native-root/NativeMessagingHosts'
    manifests.mkdir(parents=True,mode=0o700)
    registration=manifests/(host_name+'.json')
    registration.write_text(json.dumps(manifest,indent=2)+'\n')
    real_support=Path.home()/'Library/Application Support'
    paths=[real_support/'Google/Chrome/NativeMessagingHosts',
        real_support/'Google/ChromeForTesting/NativeMessagingHosts',
        real_support/'Mozilla/NativeMessagingHosts',
        real_support/'Firefox/profiles.ini',real_support/'Firefox/installs.ini']
    before=snapshot(paths)
    # Extra denial: browser may not write the real user's settings/profile/manifests.
    # This adds restrictions. Normal browser extension signing/security stays enabled.
    sandbox=out/'isolation.sb'
    sandbox.write_text('(version 1)\n(allow default)\n'+''.join(
        '(deny file-write* (subpath '+json.dumps(str(Path.home()/'Library'/p))+'))\n'
        for p in ['Application Support','Preferences']))
    log=(out/'browser.log').open('wb')
    marionette=None; cdp=None;proc=None;known_hosts=set();known_daemons=set()
    report={'browser':kind,'browser_executable':str(browser),'bundle':str(bundle),
        'manifest':manifest,'manifest_file':str(registration),'profile':str(profile),
        'extension_id':extension_id,'permissions':['nativeMessaging'],'before':before,
        'firefox_dirsvc_override':kind=='firefox','default_registration_tested':kind=='chromium'}
    try:
        if kind=='chromium':
            args=[str(browser),f'--user-data-dir={profile}',f'--load-extension={extension}',
                '--no-first-run','--no-default-browser-check','--remote-debugging-port=0','about:blank']
        else:
            # Bind ephemeral Marionette loopback port; no persistent/global prefs.
            with socket.socket() as s: s.bind(('127.0.0.1',0)); port=s.getsockname()[1]
            (profile/'user.js').write_text(f'user_pref("marionette.port", {port});\n'
                'user_pref("browser.shell.checkDefaultBrowser", false);\n'
                'user_pref("browser.startup.homepage_override.mstone", "ignore");\n'
                'user_pref("datareporting.policy.dataSubmissionPolicyBypassNotification", true);\n')
            args=[str(browser),'--no-remote','--profile',str(profile),'--marionette',
                '--remote-allow-system-access','about:blank']
        report['arguments']=args
        proc=subprocess.Popen(['/usr/bin/sandbox-exec','-f',str(sandbox),*args],
            cwd='/',stdin=subprocess.DEVNULL,stdout=log,stderr=subprocess.STDOUT)
        report['browser_pid']=proc.pid
        if kind=='chromium':
            devtools=profile/'DevToolsActivePort'
            wait_for(lambda:devtools.exists())
            lines=devtools.read_text().splitlines()
            cdp=CDP(f'ws://127.0.0.1:{lines[0]}{lines[1]}')
            report['browser_version']=json.load(urllib.request.urlopen(
                f'http://127.0.0.1:{lines[0]}/json/version'))['Browser']
        else:
            def get_marionette():
                try: return Marionette(port)
                except OSError: return None
            marionette=wait_for(get_marionette,30)
            session=marionette.call('WebDriver:NewSession',{'capabilities':{'alwaysMatch':{}}})
            report['session_capabilities']=session
            marionette.call('Marionette:SetContext',{'value':'chrome'})
            # Mirrors Mozilla head_native_messaging.js. Own process only; no global install,
            # no signature prefs disabled. Both user and system test lookups point to temp.
            report['path_selection']=marionette.script('''
              const root = arguments[0];
              const before = ["XREUserNativeManifests", "XRESysNativeManifests"].map(k => Services.dirsvc.get(k, Ci.nsIFile).path);
              globalThis.cm34Provider = {getFile(k) {
                if (!["XREUserNativeManifests", "XRESysNativeManifests"].includes(k)) return null;
                let f = Cc["@mozilla.org/file/local;1"].createInstance(Ci.nsIFile); f.initWithPath(root); return f;
              }};
              Services.dirsvc.registerProvider(globalThis.cm34Provider);
              for (const k of ["XREUserNativeManifests", "XRESysNativeManifests"]) Services.dirsvc.undefine(k);
              return {before, after:["XREUserNativeManifests", "XRESysNativeManifests"].map(k => Services.dirsvc.get(k, Ci.nsIFile).path)};
            ''',[str(manifests.parent)])
            marionette.call('Addon:Install',{'path':str(extension),'temporary':True})
        first=wait_for(lambda: acked(private) if len(acked(private))>=2 else None,25)
        report['concurrent_browser_acks']=first;known_hosts.update(first);known_daemons.update(first.values())
        assert len(set(first.values()))==1,'concurrent clients started duplicate daemon'
        daemon=next(iter(first.values()))
        # An active client keeps it alive beyond idle timeout.
        time.sleep(2.4)
        assert alive(daemon),'daemon exited with actual browser clients connected'
        report['active_over_idle_seconds']=2.4
        killed=next(iter(first))
        os.kill(killed,signal.SIGKILL)
        recovered=wait_for(lambda:{h:d for h,d in acked(private).items() if h not in first})
        assert set(recovered.values())=={daemon},'host-death recovery lost live daemon'
        report['host_death']={'killed_host':killed,'replacement_acks':recovered,'daemon_survived':alive(daemon)}
        known_hosts.update(recovered)
        wait_for(lambda:not alive(daemon),12)
        report['disconnect_idle_exit']={'daemon_pid':daemon,'exited':True,
            'events':{f.name:f.read_text() for f in private.glob('events-*.txt')}}
        restart=wait_for(lambda:{h:d for h,d in acked(private).items() if d!=daemon},10)
        assert len(set(restart.values()))==1
        new_daemon=next(iter(restart.values()));known_hosts.update(restart);known_daemons.add(new_daemon)
        report['lazy_restart_acks']=restart
        if cdp: cdp.close_browser()
        if marionette:
            # Remove the test provider before process quit. Cached NativeManifests lookup
            # only belongs to this process and is discarded on quit.
            marionette.script('Services.dirsvc.unregisterProvider(globalThis.cm34Provider); delete globalThis.cm34Provider; return true;')
            marionette.call('Marionette:Quit',{'flags':['eForceQuit']})
        proc.wait(timeout=15)
        # Host/browser teardown must not directly kill the independent daemon.
        survived=alive(new_daemon)
        report['browser_quit']={'browser_returncode':proc.returncode,'daemon_survived_immediately':survived}
        assert survived,'browser quit killed independent daemon'
        wait_for(lambda:not alive(new_daemon),10)
        report['browser_quit']['daemon_idle_exited']=True
        report['passed']=True
    except Exception as error:
        report['passed']=False;report['error']=repr(error)
        raise
    finally:
        if proc and proc.poll() is None:
            proc.terminate()
            try: proc.wait(timeout=10)
            except subprocess.TimeoutExpired: proc.kill();proc.wait()
        log.close()
        # Only PIDs identified by our nonce-scoped fixture; never broad pkill.
        known_hosts.update(acked(private));known_daemons.update(acked(private).values())
        lingering=[pid for pid in known_hosts|known_daemons if alive(pid)]
        report['lingering_before_cleanup']=lingering
        for pid in lingering: os.kill(pid,signal.SIGTERM)
        registration.unlink(missing_ok=True)
        after=snapshot(paths);report['after']=after
        report['default_locations_unchanged']=before==after
        report['events']={f.name:f.read_text() for f in private.glob('events-*.txt')}
        report['manifest_removed']=not registration.exists()
        report['socket_removed']=not (private/'probe.sock').exists()
        report['elapsed_cleanup_note']='Own test profile/extension/log retained under output for inspection; no user registration written.'
        (out/'report.json').write_text(json.dumps(report,indent=2,ensure_ascii=False)+'\n')
        if before!=after: raise RuntimeError('real-user native manifests/settings changed')
    return report

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('browser_kind',choices=['chromium','firefox']);p.add_argument('--browser',type=Path,required=True)
    p.add_argument('--bundle-meta',type=Path,required=True);p.add_argument('--out',type=Path,required=True)
    a=p.parse_args();r=run(a.browser_kind,a.browser,a.bundle_meta,a.out)
    print(json.dumps({'passed':r['passed'],'report':str(a.out/'report.json')},indent=2))
