#!/usr/bin/env python3
"""Supplemental direct-spawn checks. This NEVER counts as actual browser evidence."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import signal
import select
import struct
import subprocess
import time

spec=importlib.util.spec_from_file_location('browser_probe',Path(__file__).with_name('browser_probe.py'))
helper=importlib.util.module_from_spec(spec);spec.loader.exec_module(helper)

def read_exact(stream,n):
    data=b'';end=time.monotonic()+5
    while len(data)<n:
        remaining=end-time.monotonic()
        if remaining<=0 or not select.select([stream],[],[],remaining)[0]: raise TimeoutError('native reply timed out')
        chunk=os.read(stream.fileno(),n-len(data))
        if not chunk: raise EOFError('native host closed')
        data+=chunk
    return data

def send(p,message):
    data=json.dumps(message,separators=(',',':')).encode()
    p.stdin.write(struct.pack('=I',len(data))+data);p.stdin.flush()

def ping(p,nonce):
    send(p,{'nonce':nonce,'op':'ping'})
    n=struct.unpack('=I',read_exact(p.stdout,4))[0]
    assert n<4096
    result=json.loads(read_exact(p.stdout,n));assert result['nonce']==nonce
    send(p,{'nonce':nonce,'op':'ack','host_pid':result['host_pid'],'daemon_pid':result['daemon_pid']})
    return result

def run(meta_path,out):
    meta=json.loads(meta_path.read_text());bundle=Path(meta['bundle']).resolve();private=Path(meta['private'])
    if (private/'probe.sock').exists(): raise RuntimeError('a previous test is running')
    for f in private.glob('events-*.txt'): f.unlink()
    out.mkdir(mode=0o700);nonce=meta['nonce'];hosts=[];pids=set();report={'mode':'direct-spawn-only','browser_tested':False}
    def start():
        p=subprocess.Popen([str(bundle/'Contents/MacOS/cm34-probe-host')],cwd='/',env={},
            stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
        hosts.append(p);return p
    try:
        a,b=start(),start()
        # Write both requests before reading to exercise competing startup paths.
        import concurrent.futures
        with concurrent.futures.ThreadPoolExecutor(2) as pool:
            futures=[pool.submit(ping,p,nonce) for p in (a,b)]
            first=[f.result(timeout=10) for f in futures]
        pids.update(r['daemon_pid'] for r in first);assert len(pids)==1
        daemon=next(iter(pids));report['concurrent']=first
        time.sleep(2.4);assert helper.alive(daemon)
        report['active_past_idle']=True
        a.kill();a.wait(timeout=5);c=start();recovered=ping(c,nonce)
        assert recovered['daemon_pid']==daemon;report['host_death_recovery']=recovered
        b.stdin.close();c.stdin.close();b.wait(timeout=5);c.wait(timeout=5)
        assert helper.alive(daemon);report['eof_daemon_survived']=True
        helper.wait_for(lambda:not helper.alive(daemon),7)
        d=start();restart=ping(d,nonce);pids.add(restart['daemon_pid'])
        assert restart['daemon_pid']!=daemon;report['lazy_restart']=restart
        d.stdin.close();d.wait(timeout=5);helper.wait_for(lambda:not helper.alive(restart['daemon_pid']),7)
        # Unknown nonce/operation and truncated/oversized frames fail before daemon spawn.
        rejected=[]
        invalid_messages=[{'nonce':'0'*24,'op':'ping'}, {'nonce':nonce,'op':'exec','command':'not permitted'}]
        frames=[struct.pack('=I',4097),struct.pack('=I',8)+b'{}',struct.pack('=I',2)+b'{}']
        for message in invalid_messages:
            data=json.dumps(message,separators=(',',':')).encode();frames.append(struct.pack('=I',len(data))+data)
        for frame in frames:
            p=start();stdout,stderr=p.communicate(frame,timeout=5)
            assert p.returncode!=0 and not stdout
            rejected.append({'exit':p.returncode,'stderr':stderr.decode()})
        report['rejected_frames']=rejected
        report['passed']=True
    finally:
        for p in hosts:
            if p.poll() is None: p.kill();p.wait(timeout=5)
        for pid in pids:
            if helper.alive(pid): os.kill(pid,signal.SIGTERM)
        report['events']={f.name:f.read_text() for f in private.glob('events-*.txt')}
        report['no_lingering_hosts']=all(p.poll() is not None for p in hosts)
        report['no_lingering_daemons']=all(not helper.alive(p) for p in pids)
        report['socket_removed']=not (private/'probe.sock').exists()
        (out/'report.json').write_text(json.dumps(report,indent=2,ensure_ascii=False)+'\n')
    return report

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--bundle-meta',type=Path,required=True);p.add_argument('--out',type=Path,required=True)
    a=p.parse_args();r=run(a.bundle_meta,a.out);print(json.dumps({k:r[k] for k in ['mode','browser_tested','passed','no_lingering_hosts','no_lingering_daemons','socket_removed']},indent=2))
