#!/usr/bin/env python3
"""Sign staged copies of Know Code archives, or verify their authenticated receipt.

Never changes the unsigned packages or producer outputs. Notarization is a separate
operation; this tool does not claim a ticket or normal-security installation pass.
"""
from pathlib import Path, PurePosixPath
import argparse
import copy
import gzip
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import zipfile

SCHEMA = 1
MAX_ENTRIES = 100_000
MAX_EXPANDED_BYTES = 1024 * 1024 * 1024
CODESIGN = '/usr/bin/codesign'
SECURITY = '/usr/bin/security'
OPENSSL = '/usr/bin/openssl'


class Invalid(RuntimeError):
    pass


def require(condition, message):
    if not condition:
        raise Invalid(message)


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def regular(path):
    require(stat.S_ISREG(path.lstat().st_mode), f'Expected regular file: {path}')
    return path


def read_json(path):
    regular(path)
    return json.loads(path.read_bytes())


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')


def safe_name(name):
    require(isinstance(name, str) and name and '\\' not in name and '\x00' not in name,
            f'Unsafe archive path: {name!r}')
    clean = name.rstrip('/')
    parts = clean.split('/')
    require(not name.startswith('/') and all(p not in ('', '.', '..') for p in parts),
            f'Unsafe archive path: {name!r}')
    return PurePosixPath(clean).as_posix()


def file_name(name):
    require(safe_name(name) == name and '/' not in name, f'Expected simple filename: {name!r}')
    return name


def run(argv, evidence):
    result = subprocess.run(list(map(str, argv)), capture_output=True, text=True, timeout=300)
    with (evidence / 'commands.jsonl').open('a') as log:
        log.write(json.dumps({'argv':list(map(str, argv)), 'returncode':result.returncode,
                              'stdout':result.stdout, 'stderr':result.stderr}) + '\n')
    require(result.returncode == 0, f'Command failed ({result.returncode}): {argv}\n{result.stderr}')
    return result.stdout, result.stderr


def package_set(directory):
    manifest = read_json(directory / 'manifest.json')
    require(manifest.get('unsigned_candidate') is True, 'Input must be an unsigned candidate')
    producer = read_json(directory / 'build-record.json')
    link = manifest['build_provenance']
    require(link['file'] == 'build-record.json' and link['sha256'] == digest(directory / link['file']),
            'Producer record digest mismatch')
    require(manifest.get('candidate') is True, 'Expected a candidate manifest')
    require(producer.get('status') == 'complete' and producer.get('schema_version') == link['schema_version'] == 1
            and producer.get('build_id') == link['build_id']
            and producer.get('profile') == manifest.get('profile')
            and manifest.get('profile') in ('debug', 'release'), 'Producer identity/profile mismatch')
    require(isinstance(producer.get('outputs'), dict), 'Missing producer output inventory')
    require(re.fullmatch(r'[A-Za-z0-9.+_-]+', manifest['version']) is not None, 'Unsafe version')
    artifacts = manifest['artifacts']
    require(len(artifacts) == 3, 'Expected exactly runtime, npm and NuGet archives')
    found = {}
    for artifact in artifacts:
        name = file_name(artifact['file'])
        kind = ('runtime' if name == f"know-code-runtime-{manifest['version']}-darwin-arm64.tar.gz"
                else 'node' if name.endswith('.tgz') and name.startswith('know-code-node-')
                else 'nuget' if name == f"KnowCode.{manifest['version']}.nupkg" else None)
        require(kind is not None and kind not in found, f'Unexpected/duplicate archive: {name}')
        path = regular(directory / name)
        require(path.stat().st_size == artifact['bytes'] and digest(path) == artifact['sha256'],
                f'Unsigned archive hash/size mismatch: {name}')
        found[kind] = artifact
    require(set(found) == {'runtime', 'node', 'nuget'}, 'Missing archive')
    return manifest, producer, found


def targets(kind, version, profile):
    root = f'know-code-{version}-darwin-arm64'
    return {
        'runtime': {
            f'{root}/bin/know-code': (f'target/{profile}/know-code', 'dev.bubstack.know-code'),
            f'{root}/lib/know-code/providers/swift/indexstore-reader':
                (f'.artifacts/build/{profile}/swift/indexstore-reader', 'dev.bubstack.know-code.indexstore-reader'),
        },
        'node': {'package/know_code.node': (f'target/{profile}/libknow_code_node.dylib', 'dev.bubstack.know-code.node')},
        'nuget': {'runtimes/osx-arm64/native/libknow_code_ffi.dylib':
                  (f'target/{profile}/libknow_code_ffi.dylib', 'dev.bubstack.know-code.ffi')},
    }[kind]


def unpack(path, destination):
    """Extract only directories/regular files, keeping a finite entry inventory."""
    destination.mkdir(parents=True, exist_ok=False)
    inventory = {}
    total = 0

    def save(name, mode, size, source, directory):
        nonlocal total
        name = safe_name(name)
        require(name not in inventory, f'Duplicate archive entry: {name}')
        require(len(inventory) < MAX_ENTRIES and size >= 0, 'Archive entry limit/invalid size')
        total += size
        require(total <= MAX_EXPANDED_BYTES, 'Archive expanded-byte limit exceeded')
        require(mode & 0o7000 == 0, f'Special permission bits: {name}')
        target = destination.joinpath(*PurePosixPath(name).parts)
        if directory:
            require(not target.is_file(), f'File/directory collision: {name}')
            target.mkdir(parents=True, exist_ok=True)
            inventory[name] = {'kind':'directory', 'mode':mode}
        else:
            target.parent.mkdir(parents=True, exist_ok=True)
            with target.open('xb') as output:
                shutil.copyfileobj(source, output, length=1024 * 1024)
            require(target.stat().st_size == size, f'Archive size mismatch: {name}')
            # Scratch copies stay owner-readable/writable; repacking preserves
            # the original archive mode, including a read-only payload.
            target.chmod(mode | 0o600)
            inventory[name] = {'kind':'file', 'mode':mode, 'bytes':size, 'sha256':digest(target)}

    if path.name.endswith('.nupkg'):
        with zipfile.ZipFile(path) as archive:
            for item in archive.infolist():
                require(not item.flag_bits & 1, 'Encrypted ZIP entry')
                raw_mode = item.external_attr >> 16
                kind = stat.S_IFMT(raw_mode)
                require(kind in (0, stat.S_IFREG, stat.S_IFDIR), f'Nonregular ZIP entry: {item.filename}')
                directory = item.is_dir()
                require(kind != stat.S_IFDIR or directory, 'ZIP directory kind mismatch')
                require(not directory or item.file_size == 0, 'Nonempty ZIP directory')
                with archive.open(item) as source:
                    save(item.filename, stat.S_IMODE(raw_mode), item.file_size, source, directory)
    else:
        with tarfile.open(path, 'r:gz') as archive:
            for item in archive:
                require(item.isfile() or item.isdir(), f'Nonregular TAR entry: {item.name}')
                require(not item.isdir() or item.size == 0, 'Nonempty TAR directory')
                source = archive.extractfile(item) if item.isfile() else None
                try:
                    save(item.name, item.mode, item.size, source, item.isdir())
                finally:
                    if source is not None:
                        source.close()
    return inventory


def check_native_set(root, inventory, native):
    magic = {b'\xfe\xed\xfa\xce', b'\xce\xfa\xed\xfe', b'\xfe\xed\xfa\xcf', b'\xcf\xfa\xed\xfe',
             b'\xca\xfe\xba\xbe', b'\xbe\xba\xfe\xca', b'\xca\xfe\xba\xbf', b'\xbf\xba\xfe\xca'}
    found = set()
    for name, item in inventory.items():
        if item['kind'] == 'file':
            with root.joinpath(*PurePosixPath(name).parts).open('rb') as stream:
                if stream.read(4) in magic:
                    found.add(name)
    require(found == set(native), f'Unexpected/missing native payloads: {sorted(found ^ set(native))}')


def check_changes(before, after, native):
    require(before.keys() == after.keys(), 'Archive inventory changed')
    for name, original in before.items():
        changed = after[name]
        require(original['kind'] == changed['kind'] and original['mode'] == changed['mode'],
                f'Archive entry kind/mode changed: {name}')
        require(name in native or original == changed, f'Non-native archive payload changed: {name}')
    require(set(native) <= before.keys() and all(before[n]['kind'] == 'file' for n in native),
            'Missing native archive target')


def repack(original, extracted, output):
    if original.name.endswith('.nupkg'):
        with zipfile.ZipFile(original) as source, zipfile.ZipFile(output, 'w') as archive:
            archive.comment = source.comment
            for item in source.infolist():
                data = b'' if item.is_dir() else extracted.joinpath(*PurePosixPath(safe_name(item.filename)).parts).read_bytes()
                archive.writestr(copy.copy(item), data)
    else:
        with tarfile.open(original, 'r:gz') as source, output.open('xb') as stream, \
                gzip.GzipFile(fileobj=stream, mode='wb', filename='', mtime=0) as compressed, \
                tarfile.open(fileobj=compressed, mode='w') as archive:
            for item in source:
                item = copy.copy(item)
                if item.isfile():
                    path = extracted.joinpath(*PurePosixPath(safe_name(item.name)).parts)
                    item.size = path.stat().st_size
                    # PAX size overrides would otherwise preserve the unsigned size.
                    item.pax_headers = {k:v for k,v in item.pax_headers.items() if k != 'size'}
                    with path.open('rb') as data:
                        archive.addfile(item, data)
                else:
                    archive.addfile(item)


def requirement(team, identifier):
    return (f'anchor apple generic and identifier "{identifier}" '
            'and certificate 1[field.1.2.840.113635.100.6.2.6] exists '
            'and certificate leaf[field.1.2.840.113635.100.6.1.13] exists '
            f'and certificate leaf[subject.OU] = "{team}"')


def verify_native(path, identifier, args, evidence, certificate_prefix):
    run([CODESIGN, '--verify', '--strict', '-R=' + requirement(args.team_id, identifier), path], evidence)
    stdout, stderr = run([CODESIGN, '--display', '--verbose=4', f'--extract-certificates={certificate_prefix}', path], evidence)
    display = stdout + stderr
    require(re.search(r'^CodeDirectory .*flags=.*\bruntime\b', display, re.M) is not None,
            f'Hardened runtime flag missing: {path}')
    require(re.search(r'^Timestamp=.+$', display, re.M) is not None,
            f'Secure signing timestamp missing: {path}')
    leaf = Path(str(certificate_prefix) + '0')
    require(digest(regular(leaf)) == args.signer_sha256, f'Native signing certificate does not match pinned signer: {path}')
    return {'requirement':requirement(args.team_id, identifier), 'display_sha256':hashlib.sha256(display.encode()).hexdigest()}


def verify_cms(directory, args, evidence):
    manifest = directory / 'manifest.json'
    signer_pem = evidence / 'cms-signer.pem'
    verified = evidence / 'cms-content.json'
    # -noverify disables only chain validation; signatures/content are checked here.
    # Certificate trust is checked separately by Security with code-signing policy.
    run([OPENSSL, 'cms', '-verify', '-binary', '-inform', 'DER', '-noverify',
         '-in', regular(directory / 'manifest.cms'), '-content', regular(manifest),
         '-signer', signer_pem, '-out', verified], evidence)
    require(verified.read_bytes() == manifest.read_bytes(), 'CMS verified content differs from exact manifest bytes')
    require(signer_pem.read_text().count('-----BEGIN CERTIFICATE-----') == 1, 'Expected exactly one CMS signer')
    signer_der = evidence / 'cms-signer.der'
    run([OPENSSL, 'x509', '-in', signer_pem, '-outform', 'DER', '-out', signer_der], evidence)
    require(digest(signer_der) == args.signer_sha256, 'CMS signer does not match externally pinned DER certificate SHA-256')
    run([SECURITY, 'verify-cert', '-c', signer_der, '-p', 'codeSign'], evidence)


def verify_packages(directory, args, evidence):
    verify_cms(directory, args, evidence)  # Authenticate before trusting its inventory.
    final = read_json(directory / 'manifest.json')
    binding = final['signing']
    require(binding['schema_version'] == SCHEMA and binding['team_id'] == args.team_id
            and binding['signer_sha256'] == args.signer_sha256
            and final.get('unsigned_candidate') is False, 'Signing policy/format mismatch')
    require(binding['receipt']['file'] == 'signing-transform.json', 'Unexpected receipt path')
    require(digest(regular(directory / 'signing-transform.json')) == binding['receipt']['sha256'], 'Signing receipt digest mismatch')
    receipt = read_json(directory / 'signing-transform.json')
    unsigned = directory / 'unsigned'
    manifest, producer, artifacts = package_set(unsigned)
    require(digest(regular(directory / 'build-record.json')) == digest(unsigned / 'build-record.json'), 'Producer copy differs')
    require(receipt['schema_version'] == SCHEMA and receipt['profile'] == manifest['profile']
            and receipt['build_id'] == producer['build_id']
            and receipt['unsigned_manifest_sha256'] == digest(unsigned / 'manifest.json')
            and receipt['producer_sha256'] == digest(unsigned / 'build-record.json')
            and receipt['script_sha256'] == binding['script_sha256']
            and receipt['source_revision'] == producer.get('source_revision')
            and receipt['team_id'] == args.team_id and receipt['signer_sha256'] == args.signer_sha256,
            'Receipt producer/script identity mismatch')
    require(digest(regular(directory / 'signing-tool.py')) == receipt['script_sha256'], 'Retained signing script digest mismatch')
    require(final['build_provenance'] == manifest['build_provenance'] and final['profile'] == manifest['profile'], 'Final producer identity differs')
    final_artifacts = {file_name(item['file']):item for item in final['artifacts']}
    require(len(final['artifacts']) == 3 and len(final_artifacts) == 3 and set(final_artifacts) == {a['file'] for a in artifacts.values()}, 'Final archive set differs')
    rows = receipt['archives']
    require(len(rows) == 3 and {r['kind'] for r in rows} == set(artifacts), 'Receipt archive set differs')
    for row in rows:
        kind = row['kind']
        source = artifacts[kind]
        require(row['file'] == source['file'] and row['unsigned_sha256'] == source['sha256'], 'Receipt input archive differs')
        final_path = regular(directory / source['file'])
        actual = digest(final_path)
        require(row['signed_sha256'] == final_artifacts[source['file']]['sha256'] == actual
                and final_artifacts[source['file']]['bytes'] == final_path.stat().st_size, 'Signed archive digest/size mismatch')
        before = unpack(unsigned / source['file'], evidence / (kind + '-unsigned'))
        after_root = evidence / (kind + '-signed')
        after = unpack(final_path, after_root)
        native = targets(kind, manifest['version'], manifest['profile'])
        check_native_set(evidence / (kind + '-unsigned'), before, native)
        check_native_set(after_root, after, native)
        check_changes(before, after, native)
        require(row['unsigned_inventory'] == before and row['signed_inventory'] == after, 'Receipt inventory differs')
        transforms = row['native']
        require(len(transforms) == len(native) and {t['path'] for t in transforms} == set(native), 'Native transform set differs')
        for index, item in enumerate(transforms):
            name = item['path']
            producer_key, identifier = native[name]
            require(item['producer_output'] == producer_key and item['identifier'] == identifier
                    and before[name]['sha256'] == producer['outputs'].get(producer_key) == item['unsigned_sha256']
                    and after[name]['sha256'] == item['signed_sha256'], 'Native producer/transform mismatch')
            verify_native(after_root.joinpath(*PurePosixPath(name).parts), identifier, args, evidence,
                          evidence / f'{kind}-certificate-{index}-')
    expected = copy.deepcopy(manifest)
    expected.update(unsigned_candidate=False, notarization={'status':'not_performed'},
                    signing=binding, artifacts=final['artifacts'])
    require(final == expected, 'Unexpected unsigned-to-signed manifest changes')
    require(final.get('notarization') == {'status':'not_performed'}, 'This verifier does not certify notarization')
    return {'verified':True, 'manifest_sha256':digest(directory / 'manifest.json'),
            'producer_sha256':receipt['producer_sha256'], 'team_id':args.team_id,
            'signer_sha256':args.signer_sha256, 'notarization':'not_performed'}


def sign_packages(args):
    source = args.packages.resolve(strict=True)
    output = args.out.resolve()
    require(source != output and source not in output.parents and output not in source.parents,
            'Output must be a new directory outside the unsigned package tree')
    manifest, producer, artifacts = package_set(source)
    original = {'manifest.json':digest(source / 'manifest.json'), 'build-record.json':digest(source / 'build-record.json'),
                **{item['file']:item['sha256'] for item in artifacts.values()}}
    output.mkdir(parents=True, exist_ok=False)
    unsigned = output / 'unsigned'
    unsigned.mkdir()
    for name, expected in original.items():
        shutil.copyfile(regular(source / name), unsigned / name)
        require(digest(unsigned / name) == expected, f'Input changed during copy: {name}')
    shutil.copyfile(unsigned / 'build-record.json', output / 'build-record.json')
    shutil.copyfile(regular(Path(__file__).resolve()), output / 'signing-tool.py')
    evidence = output / 'signing-evidence'
    evidence.mkdir()
    receipt = {'schema_version':SCHEMA, 'profile':manifest['profile'], 'build_id':producer['build_id'],
               'source_revision':producer.get('source_revision'), 'producer_sha256':original['build-record.json'],
               'unsigned_manifest_sha256':original['manifest.json'], 'script_sha256':digest(output / 'signing-tool.py'),
               'team_id':args.team_id, 'signer_sha256':args.signer_sha256, 'archives':[]}
    for kind, item in artifacts.items():
        extracted = evidence / (kind + '-staged')
        before = unpack(unsigned / item['file'], extracted)
        native = targets(kind, manifest['version'], manifest['profile'])
        require(set(native) <= before.keys(), f'Missing expected native target in {kind}')
        check_native_set(extracted, before, native)
        transforms = []
        for index, (name, (producer_key, identifier)) in enumerate(sorted(native.items())):
            require(before[name]['kind'] == 'file' and before[name]['sha256'] == producer['outputs'].get(producer_key),
                    f'Unsigned native differs from producer: {name}')
            path = extracted.joinpath(*PurePosixPath(name).parts)
            run([CODESIGN, '--force', '--sign', args.identity, '--options', 'runtime', '--timestamp',
                 '--identifier', identifier, path], evidence)
            verification = verify_native(path, identifier, args, evidence, evidence / f'{kind}-staged-certificate-{index}-')
            transforms.append({'path':name, 'producer_output':producer_key, 'identifier':identifier,
                               'unsigned_sha256':before[name]['sha256'], 'signed_sha256':digest(path),
                               'verification':verification})
        repack(unsigned / item['file'], extracted, output / item['file'])
        after = unpack(output / item['file'], evidence / (kind + '-repacked'))
        check_changes(before, after, native)
        receipt['archives'].append({'kind':kind, 'file':item['file'], 'unsigned_sha256':item['sha256'],
                                    'signed_sha256':digest(output / item['file']), 'native':transforms,
                                    'unsigned_inventory':before, 'signed_inventory':after})
    require(all(digest(regular(source / name)) == value for name, value in original.items()), 'Original package inputs changed during signing')
    write_json(output / 'signing-transform.json', receipt)
    final = copy.deepcopy(manifest)
    final.update(unsigned_candidate=False, notarization={'status':'not_performed'},
                 signing={'schema_version':SCHEMA, 'team_id':args.team_id, 'signer_sha256':args.signer_sha256,
                          'script_sha256':receipt['script_sha256'],
                          'receipt':{'file':'signing-transform.json', 'sha256':digest(output / 'signing-transform.json')}})
    final['artifacts'] = [dict(item, bytes=(output / item['file']).stat().st_size,
                              sha256=digest(output / item['file'])) for item in manifest['artifacts']]
    write_json(output / 'manifest.json', final)
    run([SECURITY, 'cms', '-S', '-T', '-H', 'SHA256', '-u', '6', '-N', args.identity,
         '-i', output / 'manifest.json', '-o', output / 'manifest.cms'], evidence)
    verification = output / 'verification-evidence'
    verification.mkdir()
    result = verify_packages(output, args, verification)
    write_json(output / 'verification.json', result)
    names = [*original.keys(), 'signing-transform.json', 'manifest.cms', 'signing-tool.py']
    (output / 'SHA256SUMS').write_text(''.join(f'{digest(output / name)}  {name}\n' for name in names))
    return result


BRIDGE_NATIVE = {'sourcekit-xcode-bsp':'dev.bubstack.know-code.xcode-bridge',
                 'SWBBuildServiceBundle':'dev.bubstack.know-code.xcode-build-service'}
BRIDGE_MANIFEST = 'know-code-xcode-bridge.json'


def bridge_names(recipe_id):
    require(isinstance(recipe_id, str) and re.fullmatch(r'[A-Za-z0-9._-]+', recipe_id), 'Unsafe bridge recipe ID')
    root = f'know-code-xcode-bridge-{recipe_id}-darwin-arm64'
    return root, root + '.tar.gz'


def bridge_inventory(runtime):
    """The separately built tool has no links or special files in its qualified layout."""
    require(runtime.is_dir() and not runtime.is_symlink(), 'Expected a physical bridge runtime directory')
    result, total = {}, 0
    for path in sorted(runtime.rglob('*')):
        name, info = safe_name(path.relative_to(runtime).as_posix()), path.lstat()
        require(len(result) < MAX_ENTRIES and not info.st_mode & 0o7000, 'Bridge entry/mode limit')
        require(stat.S_ISDIR(info.st_mode) or stat.S_ISREG(info.st_mode), f'Nonregular bridge input: {name}')
        item = {'kind':'directory' if stat.S_ISDIR(info.st_mode) else 'file', 'mode':stat.S_IMODE(info.st_mode)}
        if item['kind'] == 'file':
            total += info.st_size
            require(total <= MAX_EXPANDED_BYTES, 'Bridge expanded-byte limit exceeded')
            item.update(bytes=info.st_size, sha256=digest(path))
        result[name] = item
    return result


def bridge_payload(root, inventory, producer, builder):
    manifest = read_json(root / BRIDGE_MANIFEST)
    recipe = read_json(root / 'source-recipe.json')
    expected_sources = {name:item['distributed_sha256'] for name, item in recipe['changed_files'].items()}
    expected_sources.update({'Package.swift':recipe['package_manifest_sha256'], 'Package.resolved':recipe['package_resolved_sha256']})
    dependencies = {name:item['revision'] for name, item in recipe['dependencies'].items()}
    require(producer.get('status') == 'staged_build_only' and producer.get('exit_code') == 0,
            'Bridge requires a successful staged build record')
    require(re.fullmatch(r'[0-9a-f]{64}', producer.get('builder_sha256', '')) is not None,
            'Bridge producer has no builder digest; rebuild with the recording builder, never retrofit the old record')
    require(digest(regular(builder)) == producer['builder_sha256'], 'Bridge builder source differs from producer')
    require(producer['manifest_sha256'] == digest(root / BRIDGE_MANIFEST), 'Bridge unsigned manifest differs from producer')
    require(manifest.get('schema_version') == 1 and manifest.get('tool') == 'know-code-xcode-bridge'
            and manifest['recipe_id'] == producer['recipe_id'] == recipe['recipe_id']
            and manifest['cli_identity'] == recipe['cli_identity']
            and manifest['upstream_revision'] == producer['upstream']['revision'] == recipe['upstream']['revision']
            and manifest['swift_build_revision'] == dependencies['swift-build']
            and manifest['recipe_sha256'] == producer['recipe_sha256'] == digest(root / 'source-recipe.json')
            and manifest['patch_sha256'] == producer['patch_sha256'] == recipe['patch_sha256'] == digest(root / 'know-code.patch')
            and manifest['package_resolved_sha256'] == recipe['package_resolved_sha256'] == digest(root / 'Package.resolved')
            and manifest['source_hashes'] == producer['source_hashes'] == expected_sources
            and manifest['dependency_revisions'] == producer['dependencies_before'] == producer['dependencies_after'] == dependencies,
            'Bridge source/recipe/dependency identity differs')
    files = {name:{'bytes':item['bytes'], 'sha256':item['sha256']} for name, item in inventory.items()
             if item['kind'] == 'file' and name != BRIDGE_MANIFEST}
    require(files == manifest['files'], 'Bridge runtime inventory differs from its manifest')
    check_native_set(root, inventory, BRIDGE_NATIVE)
    return manifest


def check_bridge_changes(before, after, unsigned_manifest, signed_manifest, prefix):
    native = {f'{prefix}/{name}' for name in BRIDGE_NATIVE}
    check_changes(before, after, native | {f'{prefix}/{BRIDGE_MANIFEST}'})
    expected = copy.deepcopy(unsigned_manifest)
    for name in BRIDGE_NATIVE:
        row = after[f'{prefix}/{name}']
        expected['files'][name] = {'bytes':row['bytes'], 'sha256':row['sha256']}
    require(signed_manifest == expected, 'Bridge manifest changed beyond the two signed native inventory rows')


def bridge_sign(args):
    source, output = args.bridge_build.resolve(strict=True), args.out.resolve()
    require(source != output and source not in output.parents and output not in source.parents,
            'Bridge output must be a new directory outside its build tree')
    runtime = source / 'runtime'
    producer = read_json(source / 'build-record.json')
    producer_digest = digest(source / 'build-record.json')
    original = bridge_inventory(runtime)
    builder = Path(__file__).resolve().parents[1] / 'tooling/xcode-bridge/build.py'
    manifest = bridge_payload(runtime, original, producer, builder)
    require(digest(runtime / 'source-recipe.json') == digest(builder.parent / 'recipe.json'), 'Bridge is not the currently reviewed recipe')
    prefix, filename = bridge_names(manifest['recipe_id'])
    output.mkdir(parents=True, exist_ok=False)
    unsigned, evidence = output / 'unsigned', output / 'signing-evidence'
    unsigned.mkdir()
    evidence.mkdir()
    for origin, name in [(source / 'build-record.json', 'build-record.json'), (builder, 'bridge-builder.py')]:
        shutil.copyfile(regular(origin), unsigned / name)
    shutil.copyfile(unsigned / 'build-record.json', output / 'build-record.json')
    shutil.copyfile(regular(Path(__file__).resolve()), output / 'signing-tool.py')
    archive = unsigned / filename
    with archive.open('xb') as stream, gzip.GzipFile(fileobj=stream, mode='wb', filename='', mtime=0) as compressed, \
            tarfile.open(fileobj=compressed, mode='w') as tar:
        for name in ['', *original]:
            path = runtime / name
            info = tar.gettarinfo(str(path), arcname=prefix + ('/' + name if name else ''))
            require(info.isdir() or info.isfile(), 'Bridge input changed kind during archival')
            info.uid = info.gid = info.mtime = 0
            info.uname = info.gname = ''
            if info.isfile():
                with path.open('rb') as data:
                    tar.addfile(info, data)
            else:
                tar.addfile(info)
    staged = evidence / 'staged'
    before = unpack(archive, staged)
    nested = {name[len(prefix) + 1:]:row for name, row in before.items() if name != prefix}
    bridge_payload(staged / prefix, nested, read_json(unsigned / 'build-record.json'), unsigned / 'bridge-builder.py')
    transforms = []
    for index, (name, identifier) in enumerate(sorted(BRIDGE_NATIVE.items())):
        path = staged / prefix / name
        run([CODESIGN, '--force', '--sign', args.identity, '--options', 'runtime', '--timestamp', '--identifier', identifier, path], evidence)
        verification = verify_native(path, identifier, args, evidence, evidence / f'bridge-staged-{index}-')
        transforms.append({'path':f'{prefix}/{name}', 'identifier':identifier,
                           'unsigned_sha256':before[f'{prefix}/{name}']['sha256'], 'signed_sha256':digest(path), 'verification':verification})
    signed_manifest = copy.deepcopy(manifest)
    for name in BRIDGE_NATIVE:
        path = staged / prefix / name
        signed_manifest['files'][name] = {'bytes':path.stat().st_size, 'sha256':digest(path)}
    write_json(staged / prefix / BRIDGE_MANIFEST, signed_manifest)
    repack(archive, staged, output / filename)
    signed_root = evidence / 'repacked'
    after = unpack(output / filename, signed_root)
    check_native_set(signed_root, after, {f'{prefix}/{name}' for name in BRIDGE_NATIVE})
    check_bridge_changes(before, after, manifest, read_json(signed_root / prefix / BRIDGE_MANIFEST), prefix)
    require(bridge_inventory(runtime) == original and digest(source / 'build-record.json') == producer_digest,
            'Original bridge inputs changed during signing')
    receipt = {'schema_version':1, 'tool':'know-code-xcode-bridge', 'recipe_id':manifest['recipe_id'],
               'producer_sha256':digest(unsigned / 'build-record.json'), 'builder_sha256':producer['builder_sha256'],
               'script_sha256':digest(output / 'signing-tool.py'), 'team_id':args.team_id, 'signer_sha256':args.signer_sha256,
               'unsigned_manifest_sha256':producer['manifest_sha256'], 'signed_manifest_sha256':digest(signed_root / prefix / BRIDGE_MANIFEST),
               'file':filename, 'unsigned_sha256':digest(archive), 'signed_sha256':digest(output / filename),
               'unsigned_inventory':before, 'signed_inventory':after, 'native':transforms}
    write_json(output / 'signing-transform.json', receipt)
    final = {'schema_version':1, 'tool':'know-code-xcode-bridge', 'recipe_id':manifest['recipe_id'], 'unsigned_candidate':False,
             'notarization':{'status':'not_performed'}, 'producer_sha256':receipt['producer_sha256'],
             'artifact':{'file':filename, 'bytes':(output / filename).stat().st_size, 'sha256':receipt['signed_sha256']},
             'signing':{'schema_version':1, 'team_id':args.team_id, 'signer_sha256':args.signer_sha256,
                        'script_sha256':receipt['script_sha256'], 'receipt':{'file':'signing-transform.json', 'sha256':digest(output / 'signing-transform.json')}}}
    write_json(output / 'manifest.json', final)
    run([SECURITY, 'cms', '-S', '-T', '-H', 'SHA256', '-u', '6', '-N', args.identity,
         '-i', output / 'manifest.json', '-o', output / 'manifest.cms'], evidence)
    verification = output / 'verification-evidence'
    verification.mkdir()
    result = bridge_verify(output, args, verification)
    write_json(output / 'verification.json', result)
    names = [filename, 'manifest.json', 'manifest.cms', 'build-record.json', 'signing-transform.json', 'signing-tool.py']
    (output / 'SHA256SUMS').write_text(''.join(f'{digest(output / name)}  {name}\n' for name in names))
    return result


def bridge_verify(directory, args, evidence):
    verify_cms(directory, args, evidence)
    final = read_json(directory / 'manifest.json')
    binding = final['signing']
    require(final.get('schema_version') == 1 and final.get('tool') == 'know-code-xcode-bridge'
            and final.get('unsigned_candidate') is False and final.get('notarization') == {'status':'not_performed'}
            and binding['schema_version'] == 1 and binding['team_id'] == args.team_id and binding['signer_sha256'] == args.signer_sha256
            and binding['receipt']['file'] == 'signing-transform.json', 'Bridge signing policy/format differs')
    require(digest(regular(directory / 'signing-transform.json')) == binding['receipt']['sha256'], 'Bridge receipt digest mismatch')
    receipt, unsigned = read_json(directory / 'signing-transform.json'), directory / 'unsigned'
    producer = read_json(unsigned / 'build-record.json')
    prefix, filename = bridge_names(final['recipe_id'])
    require(receipt['schema_version'] == 1 and receipt['tool'] == final['tool'] and receipt['recipe_id'] == final['recipe_id']
            and receipt['file'] == final['artifact']['file'] == filename
            and receipt['producer_sha256'] == final['producer_sha256'] == digest(unsigned / 'build-record.json') == digest(regular(directory / 'build-record.json'))
            and receipt['builder_sha256'] == producer['builder_sha256']
            and receipt['script_sha256'] == binding['script_sha256'] == digest(regular(directory / 'signing-tool.py'))
            and receipt['team_id'] == args.team_id and receipt['signer_sha256'] == args.signer_sha256
            and receipt['unsigned_sha256'] == digest(regular(unsigned / filename))
            and receipt['signed_sha256'] == final['artifact']['sha256'] == digest(regular(directory / filename))
            and final['artifact']['bytes'] == (directory / filename).stat().st_size, 'Bridge archive/producer/script binding differs')
    before_root, after_root = evidence / 'unsigned', evidence / 'signed'
    before, after = unpack(unsigned / filename, before_root), unpack(directory / filename, after_root)
    require(all(name == prefix or name.startswith(prefix + '/') for name in before), 'Unexpected bridge archive root')
    nested = {name[len(prefix) + 1:]:row for name, row in before.items() if name != prefix}
    manifest = bridge_payload(before_root / prefix, nested, producer, unsigned / 'bridge-builder.py')
    signed_manifest = read_json(after_root / prefix / BRIDGE_MANIFEST)
    check_bridge_changes(before, after, manifest, signed_manifest, prefix)
    check_native_set(after_root, after, {f'{prefix}/{name}' for name in BRIDGE_NATIVE})
    require(receipt['unsigned_inventory'] == before and receipt['signed_inventory'] == after
            and receipt['unsigned_manifest_sha256'] == digest(before_root / prefix / BRIDGE_MANIFEST)
            and receipt['signed_manifest_sha256'] == digest(after_root / prefix / BRIDGE_MANIFEST), 'Bridge manifest/inventory proof differs')
    rows = receipt['native']
    require(len(rows) == 2 and {row['path'] for row in rows} == {f'{prefix}/{name}' for name in BRIDGE_NATIVE}, 'Expected exactly two bridge native transforms')
    for index, row in enumerate(rows):
        name = row['path']
        identifier = BRIDGE_NATIVE[name[len(prefix) + 1:]]
        require(row['identifier'] == identifier and row['unsigned_sha256'] == before[name]['sha256']
                and row['signed_sha256'] == after[name]['sha256'], 'Bridge native transform differs')
        verify_native(after_root / name, identifier, args, evidence, evidence / f'bridge-certificate-{index}-')
    return {'verified':True, 'tool':final['tool'], 'recipe_id':final['recipe_id'], 'manifest_sha256':digest(directory / 'manifest.json'),
            'producer_sha256':receipt['producer_sha256'], 'team_id':args.team_id,
            'signer_sha256':args.signer_sha256, 'notarization':'not_performed', 'archive_root':prefix,
            'runtime_inventory':{name[len(prefix) + 1:]:row for name, row in after.items() if name != prefix}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest='operation', required=True)
    for operation in ('sign', 'verify', 'bridge-sign', 'bridge-verify'):
        command = subparsers.add_parser(operation)
        command.add_argument('--bridge-build' if operation == 'bridge-sign' else '--packages', type=Path, required=True)
        command.add_argument('--team-id', required=True)
        command.add_argument('--signer-sha256', required=True, help='Externally pinned signer certificate DER SHA-256')
        if operation in ('sign', 'bridge-sign'):
            command.add_argument('--out', type=Path, required=True)
            command.add_argument('--identity', required=True, help='Installed keychain signing identity name; no private-key export')
        else:
            command.add_argument('--work-dir', type=Path, required=True, help='New durable directory for extracted verification evidence')
    args = parser.parse_args()
    require(re.fullmatch(r'[A-Z0-9]{10}', args.team_id) is not None, 'Expected ten-character Apple Team ID')
    args.signer_sha256 = args.signer_sha256.lower()
    require(re.fullmatch(r'[0-9a-f]{64}', args.signer_sha256) is not None, 'Expected SHA-256 of signer DER certificate')
    if args.operation in ('sign', 'bridge-sign'):
        result = bridge_sign(args) if args.operation == 'bridge-sign' else sign_packages(args)
    else:
        packages = args.packages.resolve(strict=True)
        evidence = args.work_dir.resolve()
        require(evidence != packages and packages not in evidence.parents and evidence not in packages.parents,
                'Verification work directory must be outside the signed packages')
        evidence.mkdir(parents=True, exist_ok=False)
        result = bridge_verify(packages, args, evidence) if args.operation == 'bridge-verify' else verify_packages(packages, args, evidence)
        write_json(evidence / 'verification.json', result)
    print(json.dumps(result, sort_keys=True))


if __name__ == '__main__':
    try:
        main()
    except (Invalid, OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError, tarfile.TarError,
            zipfile.BadZipFile) as error:
        print(f'Signing verification failed: {error}', file=sys.stderr)
        sys.exit(1)
