#!/usr/bin/env python3
"""Read-only Mach-O, signatures, dependencies and exact file hashes, never patch/sign."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

def inspect(bundle):
    result={'bundle':str(bundle.resolve()),'commands':[],'sha256':{}}
    files=[bundle/'Contents/MacOS'/n for n in ['citation-manager','cm34-probe-host','cm34-probe-daemon']]
    files.append(bundle/'Contents/Frameworks/libpdfium.dylib')
    for path in files:
        result['sha256'][str(path)]=hashlib.sha256(path.read_bytes()).hexdigest()
        commands=[['/usr/bin/file',str(path)],['/usr/bin/lipo','-archs',str(path)],
          ['/usr/bin/otool','-L',str(path)],['/usr/bin/otool','-D',str(path)],
          ['/usr/bin/otool','-l',str(path)],
          ['/usr/bin/codesign','--display','--verbose=4',str(path)],
          ['/usr/bin/codesign','--verify','--strict','--verbose=2',str(path)]]
        for args in commands:
            p=subprocess.run(args,capture_output=True,text=True,check=False)
            result['commands'].append({'argv':args,'status':p.returncode,'stdout':p.stdout,'stderr':p.stderr})
    for args in [['/usr/bin/security','find-identity','-v','-p','codesigning'],
      ['/usr/bin/codesign','--verify','--deep','--strict','--verbose=2',str(bundle)],
      ['/usr/bin/sw_vers']]:
        p=subprocess.run(args,capture_output=True,text=True,check=False)
        result['commands'].append({'argv':args,'status':p.returncode,'stdout':p.stdout,'stderr':p.stderr})
    return result

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--bundle',type=Path,required=True);p.add_argument('--out',type=Path,required=True)
    a=p.parse_args();a.out.write_text(json.dumps(inspect(a.bundle),indent=2)+'\n')
