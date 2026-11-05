#!/bin/sh
# Prepare the Tuta SDK the helper builds against, in native/tuta/prepared/.
# Run it wherever sdk-prepare.py used to be run; a repeat run is a quick no-op.
#
# 1. TutaBridge's upstream/scripts/sdk-prepare.py prepares and verifies its
#    pinned SDK in upstream/sdk/generated, which stays untouched.
# 2. prepared/ gets a copy of that SDK with Hylki's own patches from
#    sdk-patches/ applied in name order, beside a copy of TutaBridge's
#    tutabridge-tuta crate. Cargo.toml builds both from there.
#
# Why a copy: tutabridge-tuta's build script refuses an SDK whose content
# differs from sdk/EXPECTED, so patching upstream/sdk/generated in place would
# break the build. prepared/ mirrors the layout the build script checks --
# crates/tuta next to sdk/{BASE,EXPECTED,patches,generated} -- with Hylki's
# patches listed (as hylki-<name>) and EXPECTED's content digest recomputed
# over the patched tree, so the check still pins exactly what is built.
# EXPECTED's tree= stays TutaBridge's: the git tree the patched SDK starts
# from.
#
# Unchanged files keep their mtimes, so a repeat run does not make Cargo
# rebuild the SDK; changed ones are written with the current time.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
python3 "$here/upstream/scripts/sdk-prepare.py"

exec python3 - "$here" <<'PY'
import hashlib
import importlib.util
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

here = Path(sys.argv[1])
upstream = here / 'upstream'
out = here / 'prepared'

spec = importlib.util.spec_from_file_location('sdk_prepare', upstream / 'scripts/sdk-prepare.py')
sdk = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sdk)


def fail(message):
    sys.exit(f'prepare.sh: {message}')


def regular_patches(folder):
    patches = sorted(folder.glob('*.patch'), key=lambda p: p.name.encode())
    for patch in patches:
        if patch.is_symlink() or not patch.is_file() or not re.fullmatch(r'[A-Za-z0-9_.-]+', patch.name):
            fail(f'{patch.name} is not a plain patch file')
    return patches


def git_apply(patch, tree, ceiling):
    # The ceiling keeps git from treating Hylki's own repository above the
    # copy as the work tree; callers' repository variables go for the same
    # reason, and so do user and system config.
    env = {k: v for k, v in os.environ.items()
           if k not in ('GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_COMMON_DIR',
                        'GIT_CONFIG_COUNT', 'GIT_CONFIG_PARAMETERS')}
    env.update(GIT_CEILING_DIRECTORIES=str(ceiling), GIT_CONFIG_NOSYSTEM='1',
               GIT_CONFIG_GLOBAL=os.devnull)
    result = subprocess.run(['git', 'apply', '--whitespace=nowarn', str(patch)],
                            cwd=tree, env=env, capture_output=True)
    if result.returncode != 0:
        fail(f'{patch.name} does not apply to the prepared SDK; review it against '
             'upstream/sdk/EXPECTED')


def same(a, b):
    return b.is_file() and not b.is_symlink() and a.stat().st_size == b.stat().st_size \
        and a.read_bytes() == b.read_bytes()


def sync(source, target):
    if target.is_symlink() or (target.exists() and not target.is_dir()):
        target.unlink()
    target.mkdir(exist_ok=True)
    wanted = {p.name: p for p in source.iterdir()}
    for existing in target.iterdir():
        model = wanted.get(existing.name)
        if model is None or model.is_dir() != (existing.is_dir() and not existing.is_symlink()):
            if existing.is_dir() and not existing.is_symlink():
                shutil.rmtree(existing)
            else:
                existing.unlink()
    for name, model in wanted.items():
        destination = target / name
        if model.is_dir():
            sync(model, destination)
        elif not same(model, destination):
            os.replace(model, destination)
            os.utime(destination)


ours = regular_patches(here / 'sdk-patches')
expected = sdk.read_expected(upstream)
stage = Path(tempfile.mkdtemp(prefix='.prepared-', dir=here))
try:
    generated = stage / 'sdk/generated'
    shutil.copytree(upstream / 'sdk/generated', generated, symlinks=True)
    for patch in ours:
        git_apply(patch, generated, stage / 'sdk')
    (stage / 'sdk/patches').mkdir()
    for patch in regular_patches(upstream / 'sdk/patches'):
        shutil.copyfile(patch, stage / 'sdk/patches' / patch.name)
    for patch in ours:
        shutil.copyfile(patch, stage / 'sdk/patches' / f'hylki-{patch.name}')
    shutil.copyfile(upstream / 'sdk/BASE', stage / 'sdk/BASE')
    expected['content'] = sdk.content_digest(generated)
    expected['patches'] = sorted(
        ((p.name, hashlib.sha256(p.read_bytes()).hexdigest())
         for p in (stage / 'sdk/patches').iterdir()),
        key=lambda entry: entry[0].encode())
    sdk.write_expected(stage, expected)
    shutil.copytree(upstream / 'crates/tuta', stage / 'crates/tuta', symlinks=True,
                    ignore=shutil.ignore_patterns('target'))
    sync(stage, out)
finally:
    shutil.rmtree(stage, ignore_errors=True)

if sdk.content_digest(out / 'sdk/generated') != expected['content']:
    fail('prepared/ differs from what was just written; is another prepare.sh running?')
print(f'Hylki SDK prepared in {out.relative_to(here)} with {len(ours)} patch(es)')
PY
