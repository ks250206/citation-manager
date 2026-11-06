#!/usr/bin/env python3
"""Real app executable negative tests; each failure must precede GPUI and DB creation."""
import argparse
import json
from pathlib import Path
import subprocess


def run(bundle, out):
    bundle=bundle.resolve();out.mkdir(mode=0o700)
    contents=bundle/'Contents';exe=contents/'MacOS/citation-manager'
    cases=[('missing-pdfium','Frameworks/libpdfium.dylib','missing','No such file'),
      ('altered-pdfium','Frameworks/libpdfium.dylib','altered','LibraryChecksum'),
      ('missing-resource','Resources/layout-fixture.pdf','missing','No such file'),
      ('altered-resource','Resources/layout-fixture.pdf','altered','resource checksum mismatch'),
      ('missing-helper','MacOS/cm34-probe-host','missing','No such file'),
      ('symlink-resource','Resources/layout-fixture.pdf','symlink','symlink in bundle inventory')]
    reports=[]
    for name,relative,mode,expected in cases:
        target=contents/relative;original=target.read_bytes();permissions=target.stat().st_mode
        backup=target.with_name(target.name+'.negative-test-backup')
        if backup.exists(): raise ValueError('stale negative backup')
        target.rename(backup)
        try:
            if mode=='altered': target.write_bytes(original+b'altered by disposable fixture')
            if mode=='symlink': target.symlink_to(backup)
            result_dir=out/name;result_dir.mkdir(mode=0o700)
            result=subprocess.run([str(exe),'--layout-smoke',str(result_dir)],cwd='/',
                env={'PATH':'/usr/bin:/bin','CM_PDFIUM_LIBRARY':str(backup)},
                capture_output=True,text=True,timeout=25)
            report={'case':name,'exit_code':result.returncode,'stderr':result.stderr,
                'output_empty':not any(result_dir.iterdir())}
            reports.append(report)
            assert result.returncode!=0 and expected in result.stderr,report
            assert report['output_empty'],'negative case opened/wrote output'
        finally:
            target.unlink(missing_ok=True);backup.rename(target)
            assert target.read_bytes()==original and target.stat().st_mode==permissions
    (out/'report.json').write_text(json.dumps(reports,indent=2)+'\n')
    return reports

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--bundle',type=Path,required=True);p.add_argument('--out',type=Path,required=True)
    a=p.parse_args();r=run(a.bundle,a.out);print(f'{len(r)} actual executable fail-closed cases passed')
