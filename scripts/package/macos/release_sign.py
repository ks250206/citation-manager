#!/usr/bin/env python3
"""Developer ID sign -> notarize -> staple `Citation Manager.app` and its .dmg (#31).

Never pretends: without a Developer ID identity every stage reports
"UNVERIFIED: no Developer ID" and signs nothing; with an identity but no notary
credentials the result is signed but "UNVERIFIED: no notary credentials".

Secrets come only from the environment or the Keychain and are never printed or written:

  CM_SIGN_IDENTITY            "Developer ID Application: <Name> (<TEAMID>)" (in the Keychain)
  CM_SIGN_KEYCHAIN            optional keychain file holding it (CI temporary keychain)
  CM_NOTARY_KEYCHAIN_PROFILE  a `notarytool store-credentials` profile, or
  CM_NOTARY_KEY_PATH, CM_NOTARY_KEY_ID, CM_NOTARY_ISSUER   App Store Connect API key (.p8 path)

Stages (the PDFium dylib must be signed BEFORE the binaries are built, because the PDF
worker pins the signed copy's digest at compile time, see codesign.py):

  release_sign.py preflight [--status S.json] [--allow-unverified]
  release_sign.py pdfium SRC DEST              prints the signed copy's SHA-256
  CM_PDFIUM_SIGNED_SHA256=<digest> cargo build --release ...   (see docs/LICENSES.md)
  build_app.py ... --signed-pdfium DEST --signed-pdfium-sha256 <digest>
  release_sign.py app --app APP --dmg OUT.dmg [--status S.json]

`app` signs the helpers and the bundle with `--options runtime --timestamp`, checks the
dylib carries the same Team ID, verifies, notarizes the app (zip) and staples it, builds and
signs the .dmg, notarizes and staples it, and validates with stapler and `spctl`.
Exit: 0 done (or UNVERIFIED with --allow-unverified), 3 UNVERIFIED, 1 a step failed.
"""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parent))
import build_app  # noqa: E402
import codesign  # noqa: E402
import create_dmg  # noqa: E402

NO_IDENTITY = 'UNVERIFIED: no Developer ID'
NO_NOTARY = 'UNVERIFIED: no notary credentials'
NOT_RUN = 'not run'
EXIT_UNVERIFIED = 3
# Notarization or the final Gatekeeper assessment rejected the build: never publish it.
EXIT_NOT_NOTARIZED = 4


class NotarizationRejected(Exception):
    """Carries the status report so it is still written before the nonzero exit."""

    def __init__(self, status):
        super().__init__(status.get('release'))
        self.status = status


def notary_credentials(env):
    """`notarytool` authentication arguments from the environment, or None.

    Only names/paths reach the argument list (the .p8 key stays a file, the profile stays in
    the Keychain); nothing here is ever printed."""
    profile = env.get('CM_NOTARY_KEYCHAIN_PROFILE')
    if profile:
        return ['--keychain-profile', profile]
    key, key_id, issuer = (env.get(n) for n in ('CM_NOTARY_KEY_PATH', 'CM_NOTARY_KEY_ID', 'CM_NOTARY_ISSUER'))
    if key and key_id and issuer:
        return ['--key', key, '--key-id', key_id, '--issuer', issuer]
    return None


def identity_available(identity, find_identity_output):
    """Whether `security find-identity -v -p codesigning` lists this Developer ID identity."""
    return bool(identity) and identity.startswith('Developer ID Application:') and \
        f'"{identity}"' in find_identity_output


def team_id(identity):
    match = re.search(r'\(([A-Z0-9]{10})\)\s*$', identity or '')
    return match.group(1) if match else None


def initial_status():
    return {'developer_id_identity': NO_IDENTITY, 'pdfium_signature': NO_IDENTITY,
            'developer_id_signature': NO_IDENTITY, 'hardened_runtime': NOT_RUN,
            'notarization_app': NO_NOTARY, 'staple_app': NOT_RUN, 'dmg': NOT_RUN,
            'notarization_dmg': NO_NOTARY, 'staple_dmg': NOT_RUN, 'gatekeeper': NOT_RUN}


def listed_identities(env):
    keychain = env.get('CM_SIGN_KEYCHAIN')
    command = ['security', 'find-identity', '-v', '-p', 'codesigning'] + ([keychain] if keychain else [])
    return subprocess.run(command, capture_output=True, text=True).stdout


def preflight(env):
    """(status, identity or None). Prints nothing secret: only whether things are present."""
    status = initial_status()
    identity = env.get('CM_SIGN_IDENTITY', '')
    if not identity_available(identity, listed_identities(env)):
        return status, None
    status['developer_id_identity'] = 'present'
    if notary_credentials(env):
        status['notarization_app'] = status['notarization_dmg'] = 'credentials present, not run'
    return status, identity


class Runner:
    """Runs a step quietly; on failure prints only the tool's own output (never the environment)."""

    def __call__(self, command, **kwargs):
        result = subprocess.run(command, capture_output=True, text=True, **kwargs)
        if result.returncode != 0:
            sys.stderr.write(f'{Path(command[0]).name} {command[1] if len(command) > 1 else ""} failed '
                             f'(exit {result.returncode}):\n{result.stdout}{result.stderr}\n')
            raise SystemExit(1)
        return result


def notarize(run, path, credentials):
    output = run(['xcrun', 'notarytool', 'submit', str(path), '--wait', '--output-format', 'json']
                 + credentials).stdout
    result = json.loads(output)
    return result.get('status'), result.get('id')


def sign_app(app, dmg, env, identity, run=None):
    run = run or Runner()
    status, _ = preflight(env) if identity is None else (initial_status(), identity)
    status['developer_id_identity'] = 'present'
    keychain = env.get('CM_SIGN_KEYCHAIN')
    extra = ['--keychain', keychain] if keychain else []
    details = run(['codesign', '-dv', '--verbose=2', str(app / 'Contents' / build_app.PDFIUM)]).stderr
    if f'TeamIdentifier={team_id(identity)}' not in details:
        sys.stderr.write('the bundled PDFium is not signed by this Team ID; run the `pdfium` stage '
                         'and rebuild with CM_PDFIUM_SIGNED_SHA256 first\n')
        raise SystemExit(1)
    status['pdfium_signature'] = 'Developer ID (same Team ID)'
    for step in codesign.plan(app, identity=identity, hardened_runtime=True, timestamp=True):
        run(step[:-1] + extra + step[-1:])
    run(codesign.verify_command(app))
    status['developer_id_signature'] = 'signed (codesign --verify --deep --strict passed)'
    status['hardened_runtime'] = 'enabled (--options runtime)'
    credentials = notary_credentials(env)
    if credentials:
        with tempfile.TemporaryDirectory(prefix='cm-notary-') as scratch:
            archive = Path(scratch) / 'app.zip'
            run(['ditto', '-c', '-k', '--keepParent', str(app), str(archive)])
            state, submission = notarize(run, archive, credentials)
        status['notarization_app'] = f'{state} (submission {submission})'
        if state != 'Accepted':
            status['release'] = f'FAILED: app notarization {state}'
            raise NotarizationRejected(status)
        run(['xcrun', 'stapler', 'staple', str(app)])
        status['staple_app'] = 'stapled'
    create_dmg.build(app, dmg)
    run(['codesign', '--force', '--sign', identity, '--timestamp'] + extra + [str(dmg)])
    status['dmg'] = f'{Path(dmg).name} signed'
    if credentials:
        state, submission = notarize(run, dmg, credentials)
        status['notarization_dmg'] = f'{state} (submission {submission})'
        if state != 'Accepted':
            status['release'] = f'FAILED: dmg notarization {state}'
            raise NotarizationRejected(status)
        run(['xcrun', 'stapler', 'staple', str(dmg)])
        run(['xcrun', 'stapler', 'validate', str(dmg)])
        status['staple_dmg'] = 'stapled and validated'
        gatekeeper = subprocess.run(['spctl', '--assess', '--type', 'open', '--context',
                                     'context:primary-signature', '--verbose=2', str(dmg)],
                                    capture_output=True, text=True)
        status['gatekeeper'] = (gatekeeper.stdout + gatekeeper.stderr).strip()
        if gatekeeper.returncode != 0:
            # spctl exits nonzero when the assessment rejects the image: never publish it.
            status['release'] = f'FAILED: Gatekeeper assessment (exit {gatekeeper.returncode})'
            raise NotarizationRejected(status)
    return status


def report(status, path):
    text = json.dumps(status, indent=2)
    print(text)
    if path:
        Path(path).write_text(text + '\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest='command', required=True)
    check = commands.add_parser('preflight')
    pdfium = commands.add_parser('pdfium')
    pdfium.add_argument('source', type=Path)
    pdfium.add_argument('destination', type=Path)
    app = commands.add_parser('app')
    app.add_argument('--app', type=Path, required=True)
    app.add_argument('--dmg', type=Path, required=True)
    for sub in (check, app):
        sub.add_argument('--status', type=Path)
        sub.add_argument('--allow-unverified', action='store_true',
                         help='exit 0 when no Developer ID is available (the status still says UNVERIFIED)')
    args = parser.parse_args()
    if sys.platform != 'darwin':
        parser.error('macOS only')
    status, identity = preflight(os.environ)
    if identity is None:
        if args.command != 'pdfium':
            report(status, args.status)
        print(NO_IDENTITY, file=sys.stderr)
        sys.exit(0 if getattr(args, 'allow_unverified', False) else EXIT_UNVERIFIED)
    if args.command == 'preflight':
        report(status, args.status)
    elif args.command == 'pdfium':
        print(codesign.sign_pdfium_copy(args.source, args.destination, identity=identity, timestamp=True,
                                        keychain=os.environ.get('CM_SIGN_KEYCHAIN')))
    else:
        try:
            report(sign_app(args.app, args.dmg, os.environ, identity), args.status)
        except NotarizationRejected as rejected:
            report(rejected.status, args.status)
            print(rejected.status['release'], file=sys.stderr)
            sys.exit(EXIT_NOT_NOTARIZED)


if __name__ == '__main__':
    main()
