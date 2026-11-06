# Legacy DOI fixture provenance

`old_api.sql` contains **actual API output**, not invented legacy values. On 2026-11-07
(JST), base `42345ccf7f80c11d3b8a89e8ad1db5e0cafa881b` was extracted with `git archive`
into a private temporary scratch directory (no git checkout/worktree). `generate.rs` was
compiled there against the old pinned/locked workspace, and `PaperLibrary::create` created
six real on-disk records with retained raw resolver IDs. SQLite `iterdump()` supplied the
paper/external-ID INSERTs unchanged; papers are ordered before IDs for foreign keys.
UUIDs/timestamps are incidental generated values, not assertions or user data.

The fixture covers escaped SICI-like suffixes, query/fragment, double encoding, invalid
hex, invalid UTF-8, and URL-decoded whitespace. Old API stored the resolver suffix in ASCII
lowercase without decoding or removing queries/fragments. The regular regression creates
the current immutable schema, loads these historical INSERTs, closes it and reopens with
`LibraryDb::open`. No production migration or application startup code loads this SQL.

The same seven integration regressions were additionally run against whole copies of
the **actual old-API-created database** by passing its file path through
`CM_TEST_OLD_API_DATABASE`. Each test copies the closed/checkpointed synthetic fixture to
its own private temp library; the fixture source and all user libraries stay untouched.

To reproduce from the repository root:

```python
import io, os, pathlib, shutil, sqlite3, subprocess, tarfile, tempfile
repo = pathlib.Path.cwd()
scratch = pathlib.Path(tempfile.mkdtemp(prefix="pr41-old-api-")).resolve()
archive = subprocess.check_output(["git", "archive", "42345ccf7f80c11d3b8a89e8ad1db5e0cafa881b"])
with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
    tar.extractall(scratch, filter="data")
examples = scratch / "crates/storage/examples"
examples.mkdir()
shutil.copyfile(repo / "crates/storage/tests/fixtures/legacy_doi/generate.rs",
                examples / "legacy_doi_fixture.rs")
subprocess.run(["cargo", "run", "--locked", "--offline", "-p", "cm-storage",
                "--example", "legacy_doi_fixture", "--", str(scratch / "private-library")],
               cwd=scratch, check=True)
source = scratch / "private-library/library.sqlite"
env = os.environ.copy()
env["CM_TEST_OLD_API_DATABASE"] = str(source)
subprocess.run(["cargo", "test", "--locked", "-p", "cm-storage", "--test", "legacy_doi"],
               cwd=repo, env=env, check=True)
# Optional regeneration; inspect the resulting fixture diff (IDs/times will vary):
with sqlite3.connect(source) as db:
    inserts = list(db.iterdump())
    sql = "\n".join(line for table in ["papers", "paper_external_ids"]
                    for line in inserts if line.startswith(f'INSERT INTO "{table}"'))
print(sql)
```

Use only the private synthetic database produced by this generator for that test variable.
No migration IDs/checksums, dependency versions or library metadata are rewritten on open.
