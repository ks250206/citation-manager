#!/usr/bin/env python3
"""Reproduce real PDFium + independent native PDFKit gates. Missing prerequisites fail, never skip.
Use Python from a venv installed with scripts/pdfium/requirements.txt. Output must be a new directory.
"""
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
from fetch import fetch
from verify import check
from inherited_boxes import inspect_structure

ROOT = Path(__file__).resolve().parents[2]

def run(argv, env=None):
    print('+', ' '.join(map(str,argv)), flush=True)
    subprocess.run(list(map(str,argv)),cwd=ROOT,env=env,check=True,timeout=600)

def main(out):
    if platform.system() != 'Darwin':
        raise RuntimeError('independent PDFKit gate requires macOS; not passed on this platform')
    if out.exists():
        raise ValueError('output directory already exists; choose a fresh path')
    out.mkdir(parents=True)
    library_dir=ROOT/'target/pdfium-poc'
    fetch(library_dir)
    env={**os.environ,'CM_PDFIUM_LIBRARY':str((library_dir/'lib/libpdfium.dylib').resolve())}
    structure = inspect_structure(ROOT/'fixtures/pdf/inherited.pdf')
    (out/'inherited-structure.json').write_text(json.dumps(structure,indent=2)+'\n')
    run([sys.executable,ROOT/'scripts/pdfium/test_inherited_boxes.py'])
    run(['cargo','run','-p','cm-pdf','--example','inherited_boxes_regression','--features','runtime','--locked','--','after',out/'inherited-after.json'],env)
    run(['cargo','test','-p','cm-pdf','--features','runtime','--locked'],env)
    run(['cargo','run','-p','cm-pdf','--example','pdfium_poc','--features','runtime','--locked','--',out],env)
    executable=library_dir/'inspect-pdfkit'
    run(['xcrun','swiftc',ROOT/'scripts/pdfium/inspect_pdfkit.swift','-o',executable])
    for directory, source in [(out, ROOT/'fixtures/pdf/corpus.pdf'), (out/'inherited', ROOT/'fixtures/pdf/inherited.pdf')]:
        run([sys.executable,ROOT/'scripts/pdfium/complete_appearances.py',directory/'pdfium-export.pdf',directory/'portable-export.pdf'])
        run(['cargo','run','-p','cm-pdf','--example','pdfium_poc','--features','runtime','--locked','--',directory/'pdfium-portable',directory/'portable-export.pdf',directory/'pdfium-report.json'],env)
        for input_pdf,name in [(source,'pdfkit-original'),(directory/'pdfium-export.pdf','pdfkit-native'),(directory/'portable-export.pdf','pdfkit-portable')]:
            run([executable,input_pdf,directory/name])
    # Package evidence for #34: fixed unsigned dylib, dependencies/install_name/minimum OS.
    with (out/'packaging.txt').open('w') as log:
        for argv in [['sw_vers'],['uname','-m'],['otool','-L',env['CM_PDFIUM_LIBRARY']],['otool','-l',env['CM_PDFIUM_LIBRARY']],['codesign','-dv','--verbose=4',env['CM_PDFIUM_LIBRARY']]]:
            result=subprocess.run(argv,text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,check=False)
            log.write('$ '+' '.join(argv)+'\n'+result.stdout+'\nexit='+str(result.returncode)+'\n')
    check(out)
    check(out/'inherited', inherited=True)
    (out/'run.json').write_text(json.dumps({'utc':datetime.now(timezone.utc).isoformat(),'platform':platform.platform(),'output':str(out),'pdfium':env['CM_PDFIUM_LIBRARY']},indent=2)+'\n')
    print('Runtime and independent verification passed:',out)

if __name__ == '__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out',type=Path,default=ROOT/'target/pdfium-validation'/datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ'))
    main(parser.parse_args().out.resolve())
