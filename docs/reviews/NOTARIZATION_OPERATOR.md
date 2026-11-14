# Final notarization and normal-security installation

Operator recipe, checked 2026-11-11; **not executed**. This review used repository source, installed command help and Apple documentation. It did not access credentials, sign, upload, build, run product tests, provision a VM or change the live installation. The earlier signed Debug diagnostic is not the final candidate. Freeze and validate the final post-npm-10 candidate first.

## One distribution container

Use one signed, notarized, stapled UDIF disk image, `know-code-0.0.1-darwin-arm64.dmg`. Keep the existing three signed runtime/npm/NuGet archives unchanged. Put their expanded payloads directly in the image as well, so all four native code items are submitted without relying on scanning a nested TAR or interpreting a NuGet ZIP. Use the existing installer from the expanded runtime. This needs no installer-package certificate, new application wrapper or product installation framework.

Apple supports a signed disk image for direct distribution. A disk image signature covers its contents. See [Apple's packaging instructions](https://developer.apple.com/documentation/xcode/packaging-mac-software-for-distribution). Standalone binaries cannot receive stapled tickets; the image carries the ticket. A ZIP containing only opaque package archives is therefore an unsuitable assumption for this release. See [Apple's custom workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow).

The image layout is fixed:

```text
runtime/know-code-0.0.1-darwin-arm64/    expanded runtime, including CLI and Swift helper
sdk/node/package/                     expanded signed npm archive, including add-on
sdk/nuget/                            expanded signed NuGet archive, including FFI
packages/                             exact three signed archives
proof/manifest.json                   unchanged authenticated signing-stage manifest
proof/manifest.cms
proof/signing-transform.json
proof/build-record.json
```

Do not include `unsigned/`, producer output trees, signing/replay evidence directories, certificate/key material or unrelated artifacts. The expanded trees are product payloads, not replacement archive builds. Their native bytes must match the corresponding archive inventory exactly. The read-only image is the offline-first installation route; independent first installation from a standalone archive has no stapled ticket and requires its own validation before claiming offline-first Gatekeeper acceptance.

## Prepare the image

Run from the frozen checkout. `SIGNED` names the final directory produced by `scripts/sign_packages.py`, retaining its `unsigned/` proof locally. `WORK` must be a new durable directory. `IDENTITY` is the installed Developer ID Application identity name; do not put passwords or key exports in the environment or command line. Team ID is `3FMAPDQDGP`; the independently pinned certificate DER SHA-256 is `f9a80b2743c868aa5579cd1ac2b9cf5a8925a6784cb2a1c0bb367eb0ffbfcf75`.

```sh
export SIGNED="$PWD/.artifacts/pretag-wave/packages/FINAL_SIGNED_DIRECTORY"
export WORK="$PWD/.artifacts/notarization/final-01"
export IDENTITY='INSTALLED DEVELOPER ID APPLICATION IDENTITY NAME'
test ! -e "$WORK"
mkdir -p "$WORK/evidence" "$WORK/release"
python3 scripts/sign_packages.py verify --packages "$SIGNED" \
  --team-id 3FMAPDQDGP \
  --signer-sha256 f9a80b2743c868aa5579cd1ac2b9cf5a8925a6784cb2a1c0bb367eb0ffbfcf75 \
  --work-dir "$WORK/replay-before"
```

Stop if verification fails. Then stage from the authenticated inventory, using the existing bounded safe extractor and exact native target list rather than a second extraction implementation:

```sh
python3 - <<'PY'
import importlib.util, json, os, shutil
from pathlib import Path
spec = importlib.util.spec_from_file_location('sign_packages', 'scripts/sign_packages.py')
s = importlib.util.module_from_spec(spec)
spec.loader.exec_module(s)
source, work = Path(os.environ['SIGNED']), Path(os.environ['WORK'])
m = s.read_json(source / 'manifest.json')
receipt = s.read_json(source / 'signing-transform.json')
assert m['version'] == '0.0.1' and m['profile'] == 'release'
assert s.digest(Path('scripts/sign_packages.py')) == m['signing']['script_sha256']
rows = {row['file']: row for row in receipt['archives']}
image = work / 'image-root'
image.mkdir()
for name in ('packages', 'proof'):
    (image / name).mkdir()
native = []
for artifact in m['artifacts']:
    row = rows[artifact['file']]
    archive = source / artifact['file']
    assert s.digest(archive) == artifact['sha256'] == row['signed_sha256']
    shutil.copyfile(archive, image / 'packages' / artifact['file'])
    shutil.copyfile(archive, work / 'release' / artifact['file'])
    assert s.digest(image / 'packages' / artifact['file']) == s.digest(archive)
    assert s.digest(work / 'release' / artifact['file']) == s.digest(archive)
    destination = image / {'runtime':'runtime', 'node':'sdk/node', 'nuget':'sdk/nuget'}[row['kind']]
    inventory = s.unpack(archive, destination)
    assert inventory == row['signed_inventory']
    targets = s.targets(row['kind'], m['version'], m['profile'])
    s.check_native_set(destination, inventory, targets)
    for name, item in inventory.items():
        (destination / name).chmod(item['mode'])
    for entry in row['native']:
        path = destination / entry['path']
        assert s.digest(path) == entry['signed_sha256']
        native.append({'image_path':path.relative_to(image).as_posix(),
                       'sha256':s.digest(path), 'identifier':entry['identifier']})
assert len(native) == 4
for name in ('manifest.json', 'manifest.cms', 'signing-transform.json', 'build-record.json'):
    for destination in (image / 'proof', work / 'release'):
        shutil.copyfile(source / name, destination / name)
        assert s.digest(destination / name) == s.digest(source / name)
s.write_json(work / 'evidence' / 'native-submission-inventory.json', native)
PY
/usr/bin/hdiutil create -srcfolder "$WORK/image-root" -volname KnowCode-0.0.1 \
  -fs HFS+ -format UDRO "$WORK/release/know-code-0.0.1-darwin-arm64.dmg"
/usr/bin/codesign --sign "$IDENTITY" --timestamp \
  --identifier dev.bubstack.know-code.distribution \
  "$WORK/release/know-code-0.0.1-darwin-arm64.dmg"
/usr/bin/codesign --verify --strict \
  '-R=anchor apple generic and identifier "dev.bubstack.know-code.distribution" and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = "3FMAPDQDGP"' \
  "$WORK/release/know-code-0.0.1-darwin-arm64.dmg"
/usr/bin/codesign --display --verbose=4 \
  --extract-certificates="$WORK/evidence/image-certificate-" \
  "$WORK/release/know-code-0.0.1-darwin-arm64.dmg" \
  > "$WORK/evidence/image-signature.stdout" 2> "$WORK/evidence/image-signature.stderr"
/usr/bin/shasum -a 256 "$WORK/evidence/image-certificate-0"
/usr/bin/hdiutil verify "$WORK/release/know-code-0.0.1-darwin-arm64.dmg"
cp "$WORK/release/know-code-0.0.1-darwin-arm64.dmg" "$WORK/submitted.dmg"
```

Require the extracted leaf-certificate digest to equal the external pin above, and a secure timestamp in the display output. Do not request hardened runtime for the disk image itself; the four native payloads already require it. Before submission, mount the image read-only for inspection, recheck all archive hashes and native payload hashes/signatures against the staged inventory, record each native `CDHash` with `codesign --display --verbose=4`, and detach. Record the disk-image `CDHash` too. Do not execute any code during this inspection. These checks cover the actual submitted filesystem, rather than trusting only the image construction command.

`hdiutil create` remains available on the current host but prints a deprecation notice there; its conventional syntax is also available on macOS 15. This recipe does not change the selected Xcode or install tools.

## Submit, inspect, staple

The installed `asc notarization` help was inspected for every command below. It uses the direct [Apple Notary API](https://developer.apple.com/documentation/notaryapi/submitting-software-for-notarization-over-the-web), with the existing configured ASC authentication. A nonexistent `notarytool` keychain profile is unnecessary. Do not inspect or export that authentication. Submit once, retain its returned submission ID, and poll that ID; a delayed response is not a reason for duplicate uploads.

```sh
asc notarization submit --file "$WORK/release/know-code-0.0.1-darwin-arm64.dmg" \
  --output json > "$WORK/evidence/submission.json"
# Set this from the actual returned submission ID; do not guess a JSON layout.
export SUBMISSION_ID='ACTUAL_RETURNED_SUBMISSION_ID'
asc notarization status --id "$SUBMISSION_ID" --output json \
  > "$WORK/evidence/status.json"
asc notarization log --id "$SUBMISSION_ID" --fetch \
  --path "$WORK/release/notarization-log.json" --output json \
  > "$WORK/evidence/log-fetch-receipt.json"
```

The submit command hashes and uploads a supported ZIP/DMG/PKG; without `--wait` it does not certify completion. `status` reports `In Progress`, `Accepted`, `Invalid` or `Rejected`. Poll while in progress. Require `Accepted`, then fetch and review the developer log even on success. `log` without `--fetch` returns a temporary URL, not the log. `--fetch --path` saves the raw JSON and returns a separate download receipt; it refuses overwrite unless explicitly requested. Retain upload/status/download receipts locally, not in release assets.

Require the log's submission identity and SHA-256 to match the actual pre-staple image. Match all four mounted native CDHashes and the signed image CDHash against `ticketContents`, including each shipped architecture. Extra duplicate entries from scanning archive copies are harmless; a missing native hash is a release failure even if overall status is accepted. Apple's [ticket explanation](https://developer.apple.com/forums/thread/720093) describes why the code-directory hashes are the meaningful identities. Do not substitute filename matches for hash matches.

After those checks:

```sh
asc notarization staple --file "$WORK/release/know-code-0.0.1-darwin-arm64.dmg" \
  --confirm --output json > "$WORK/evidence/staple.json"
asc notarization validate --file "$WORK/release/know-code-0.0.1-darwin-arm64.dmg" \
  --output json > "$WORK/evidence/staple-validation.json"
/usr/bin/codesign --verify --strict \
  "$WORK/release/know-code-0.0.1-darwin-arm64.dmg"
```

`staple` changes the image in place and immediately validates it. `validate` is read-only. Both are macOS operations backed by Apple's stapler and may need networking. Never staple or re-sign the standalone archives or their native contents after the signing manifest was authenticated. Recheck the same image/native CDHashes, mounted payload hashes and all three archive hashes after stapling. Retain the exact pre-staple `submitted.dmg` locally and compute a new final image SHA-256; neither hash replaces the other.

## Final inventory and asset allowlist

Keep the signing-stage `manifest.json` and its CMS bytes unchanged, including its truthful `notarization.status: not_performed`. That manifest describes the earlier signing stage; editing it would invalidate its signature and the existing verifier. Add one **outer** `release-index.json` only after stapling, then authenticate its exact bytes using the existing keychain-backed detached CMS procedure. The index records:

- Version, final source revision, build ID/profile and exact producer digest from the original verified record; require optimized `release` profile for this candidate.
- Expected Team ID, externally pinned signer DER SHA-256 and image code identifier.
- SHA-256 and byte length of every public file below other than the index, its signature and checksums.
- Submission ID, accepted status, pre-staple image SHA-256, post-staple image SHA-256, image CDHash, and the four path/architecture/native SHA-256/CDHash rows checked against the raw log. Distinguish native-file SHA-256 from CDHash.

Authenticate and verify `release-index.json` by the same strict CMS checks used in `scripts/sign_packages.py::verify_cms`: cryptographic signature/content verification, exactly one signer, the independent certificate pin, and Security's code-signing certificate trust. A decode-only `security cms -D` result or checksum comparison is insufficient. This index must not claim an installation pass before that gate completes; the final candidate index may authenticate the artifacts before the separately hash-bound test evidence exists.

```sh
/usr/bin/security cms -S -T -H SHA256 -u 6 -N "$IDENTITY" \
  -i "$WORK/release/release-index.json" -o "$WORK/release/release-index.cms"
```

Publish exactly this top-level asset set, after all gates pass:

```text
know-code-0.0.1-darwin-arm64.dmg
know-code-runtime-0.0.1-darwin-arm64.tar.gz
know-code-node-0.0.1.tgz
KnowCode.0.0.1.nupkg
manifest.json
manifest.cms
signing-transform.json
build-record.json
notarization-log.json
release-index.json
release-index.cms
SHA256SUMS
```

Require the npm filename to equal the actual authenticated manifest entry; stop and correct this explicit allowlist if the pinned package name produces another filename. Generate the final `SHA256SUMS` over these files except itself. Do not copy the signing directory's earlier checksum file unchanged. Verify the public asset set exactly; do not upload the entire local signing directory.

Local-only evidence includes `unsigned/`, retained `signing-tool.py`, all replay/staging/native/signing evidence, `submitted.dmg`, ASC command receipts, certificate extracts, and VM disks/results. The unchanged public transform/producer inventory establishes the claimed byte transitions; full unsigned-to-signed replay uses those retained local originals and the script whose digest the signed manifest already records. Source supplies the verifier. No private key, credential or temporary signed upload URL belongs in any release asset.

The chain stays acyclic: signed producer/transform/archives are covered by the immutable signing manifest and CMS; the final image contains those exact inputs; the outer index binds their hashes, the signed/stapled image and raw Apple log; its detached CMS authenticates that claim. The Apple ticket is separate platform trust evidence. Neither JSON inventory hashes itself or its own CMS signature.

## Disposable macOS 15 gate

Reuse the VM/image download already in progress; do not start another download or reuse host trust caches. Record guest OS/build/ARM64 and toolchain versions. Stage prerequisites and fixtures before the test. Use a fresh guest snapshot with no earlier access to this product's CDHashes. Record Gatekeeper status without changing it. This gate must exercise the final **stapled** image whose hash the outer index authenticates.

1. Obtain the image through a guest browser from the controlled candidate source so it receives normal quarantine. For a prepublication shared-file transfer, explicitly apply and record quarantine on the copied guest image as a test fixture, rather than treating a nonquarantined copy as a real download. Require a visible `com.apple.quarantine` attribute before first open. Do not clear it.
2. Disconnect the guest's network **before the first image assessment/open or native execution**; record the network-denial proof. Keep host networking available for unrelated work. Do not run online `stapler validate` or native loading in this guest first, because that can contaminate ticket availability.
3. Open the image in Finder/LaunchServices, preserving normal security prompts, then run its expanded runtime installer. `hdiutil attach` alone is not proof of ticket ingestion; [Apple DTS describes that distinction](https://developer.apple.com/forums/thread/756027). Record the opening outcome and `spctl --assess --type open --context context:primary-signature --verbose=4 IMAGE.dmg` output as supporting evidence. Actual user-flow success is required too.
4. From that same image, use the existing installation instructions and independently restored Node 24/.NET 10 prerequisites. Install the SDK archives and load both native SDKs; run the CLI, analysis, saved-file changes, MCP, login lifecycle, uninstall/reinstall and existing exact-archive offline checks. Retain first-execution prompts/failures and quarantine attributes on copied native payloads before running them. A copy routine that silently drops quarantine must not become the only security exercise; explicitly assess the original quarantined runtime path too. Ordinary open confirmation is fine; Open Anyway, control-click overrides, disabled Gatekeeper and quarantine removal are failures for this gate.
5. Bind raw results to final index/CMS/image/archive hashes, pinned signer, guest snapshot and harness hashes. The existing signed archive harness still requires the local unsigned replay directory; stage that proof separately outside the public image if running it unchanged. Its product/offline checks, including the added relocated npm 10 policy cases, do not automatically certify Gatekeeper. Repeat the normal-security route on the current supported host in an isolated environment; do not replace the active development runtime.

If Finder ticket ingestion, the CLI's first quarantined execution, an SDK's native load, or the image's offline assessment fails, preserve evidence and stop the release. An online rerun is diagnostic only. The fallback is a concrete packaging/installation change with new final artifact hashes and repeat validation, not a security bypass or a claim that notarization alone satisfied the gate.

## Separate Xcode bridge image — source recipe, not executed

The user selected a separate bridge download **and** separate bridge DMG. Keep
the main image and its three core archives unchanged. This route wraps the
single archive from `sign_packages.py bridge-sign`; it neither builds nor
automatically installs the bridge. Require a new matching tool build recording
`builder_sha256`, successful bridge signing verification and relevant installed
language checks. Earlier unsigned research binaries are not substitutes.

Use the same externally pinned Team ID/certificate and installed identity as
above. Set these paths to the actual frozen signed stage and a **new** durable
work directory; stop on any failed command:

```sh
set -eu
export BRIDGE_SIGNED="$PWD/.artifacts/xcode-tooling/ACTUAL_SIGNED_STAGE"
export BRIDGE_WORK="$PWD/.artifacts/notarization/bridge-final-01"
export BRIDGE_DMG='know-code-xcode-bridge-index-only.7-notice1-darwin-arm64.dmg'
export IDENTITY='INSTALLED DEVELOPER ID APPLICATION IDENTITY NAME'
test ! -e "$BRIDGE_WORK"
mkdir -p "$BRIDGE_WORK/evidence" "$BRIDGE_WORK/release"
python3 scripts/sign_packages.py bridge-verify --packages "$BRIDGE_SIGNED" \
  --work-dir "$BRIDGE_WORK/replay-before" --team-id 3FMAPDQDGP \
  --signer-sha256 f9a80b2743c868aa5579cd1ac2b9cf5a8925a6784cb2a1c0bb367eb0ffbfcf75 \
  > "$BRIDGE_WORK/evidence/bridge-verification.json"
python3 - <<'PY'
import importlib.util, os, shutil
from pathlib import Path
spec = importlib.util.spec_from_file_location('sign_packages', 'scripts/sign_packages.py')
s = importlib.util.module_from_spec(spec)
spec.loader.exec_module(s)
source, work = Path(os.environ['BRIDGE_SIGNED']), Path(os.environ['BRIDGE_WORK'])
verification = s.read_json(work / 'replay-before/verification.json')
manifest = s.read_json(source / 'manifest.json')
receipt = s.read_json(source / 'signing-transform.json')
assert verification['verified'] and verification['recipe_id'] == 'index-only.7-notice1'
assert s.digest(Path('scripts/sign_packages.py')) == manifest['signing']['script_sha256']
image = work / 'image-root'
image.mkdir()
for name in ('packages', 'proof'):
    (image / name).mkdir()
archive = source / manifest['artifact']['file']
assert s.digest(archive) == manifest['artifact']['sha256'] == receipt['signed_sha256']
inventory = s.unpack(archive, image / 'tool')
assert inventory == receipt['signed_inventory']
for name, item in inventory.items():
    (image / 'tool' / name).chmod(item['mode'])
root = image / 'tool' / verification['archive_root']
assert s.bridge_inventory(root) == verification['runtime_inventory']
for destination in (image / 'packages', work / 'release'):
    shutil.copyfile(archive, destination / archive.name)
    assert s.digest(destination / archive.name) == manifest['artifact']['sha256']
for name in ('manifest.json', 'manifest.cms', 'signing-transform.json', 'build-record.json'):
    for destination in (image / 'proof', work / 'release'):
        shutil.copyfile(source / name, destination / name)
        assert s.digest(destination / name) == s.digest(source / name)
shutil.copyfile('tooling/xcode-bridge/README.md', image / 'INSTALLATION.md')
s.write_json(work / 'evidence/native-submission-inventory.json', [
    {'image_path':(Path('tool') / row['path']).as_posix(),
     'sha256':row['signed_sha256'], 'identifier':row['identifier']}
    for row in receipt['native']])
assert len(receipt['native']) == 2
PY
/usr/bin/hdiutil create -srcfolder "$BRIDGE_WORK/image-root" \
  -volname KnowCode-Xcode-Bridge-index-only.7 -fs HFS+ -format UDRO \
  "$BRIDGE_WORK/release/$BRIDGE_DMG"
/usr/bin/codesign --sign "$IDENTITY" --timestamp \
  --identifier dev.bubstack.know-code.xcode-bridge.distribution \
  "$BRIDGE_WORK/release/$BRIDGE_DMG"
/usr/bin/hdiutil verify "$BRIDGE_WORK/release/$BRIDGE_DMG"
```

The image layout is `tool/<archive-topdir>/`, `packages/<single archive>`,
`proof/{manifest.json,manifest.cms,signing-transform.json,build-record.json}`
and `INSTALLATION.md`. Keep `unsigned/`, builder/signing evidence, certificate
extracts and checkout/cache trees local. Do not re-sign either expanded native
file or edit the adjacent `know-code-xcode-bridge.json` after archive signing.

Before submission, repeat the image signature requirement, secure timestamp,
certificate extraction/pin, read-only mounting and exact inventory checks from
the main-image procedure, using identifier
`dev.bubstack.know-code.xcode-bridge.distribution`. Compare the mounted expanded
tool with `verification.runtime_inventory` through `bridge_inventory`, and
mounted proof/archive hashes with the verified stage. Record `codesign
--display --verbose=4` output and actual architecture/CDHash for **both** native
files and the image; no product execution belongs in this inspection. Preserve
the exact signed pre-staple image as `$BRIDGE_WORK/submitted.dmg`.

Reuse the submit/status/log/staple/validate commands above with this bridge
image and a separate actual submission ID. Require Accepted and review the raw
Apple log. Its submission SHA-256 must match `submitted.dmg`; ticket contents
must cover the two actual native CDHashes/architectures and image CDHash.
Filename matches and overall Accepted status alone are insufficient. After
stapling, recheck the same native bytes, both native signatures/CDHashes, image
signature/CDHash, full mounted runtime inventory and unchanged archive/proof
hashes. Record the final image SHA-256 separately from the submitted hash.

Retain the inner `manifest.json`/CMS unchanged, including its truthful
`notarization: {status: not_performed}`. Build a separate outer bridge
`release-index.json` only from the actual verified evidence. Record recipe ID,
CLI identity, upstream/SwiftBuild revisions, tool build-record digest, recorded
builder digest, signing-script digest, frozen Know Code source revision and
operator-document digest; pinned signer/Team ID; every public file's size and
SHA-256; actual submission ID/status; pre/post-staple hashes; and the two
native plus image architecture/CDHash rows checked against the raw log. The
index does not inventory itself or its CMS signature. Authenticate its exact
bytes with the existing `security cms -S -T -H SHA256 -u 6` procedure. For replay,
copy the index and its detached signature into a **new evidence directory** as
`manifest.json`/`manifest.cms` and call existing `verify_cms` with the external
certificate pin; also check the index's actual artifact hashes. This reuses
the strict content/signature/signer/trust helper without a second verifier.

The separate public asset allowlist is this DMG, its single bridge TAR, unchanged
inner manifest/CMS, signing-transform and tool build-record, the actual raw
notarization log, outer release-index/CMS and final SHA256SUMS. Do not upload the
entire local signing directory. Before publication, exercise first quarantined
image open and installation under normal security, offline ticket ingestion,
and actual installed device/simulator indexing outside the checkout, using the
existing installed Xcode fixture and authenticated stage. Keep the bridge
installation explicit and separate from the common runtime. No execution or
successful notarization/platform/installation claim accompanies this recipe.


## 0.0.3 bridge assets in the shared release

On 2026-11-13 the user selected one GitHub 0.0.3 release containing the common
runtime and the separate bridge downloads/DMGs. The bridge's public proof
files use the following names to avoid collisions with common assets:

- `xcode-bridge-manifest.json` and `xcode-bridge-manifest.cms`
- `xcode-bridge-signing-transform.json` and `xcode-bridge-build-record.json`
- `xcode-bridge-notarization-log.json`
- `xcode-bridge-release-index.json` and `xcode-bridge-release-index.cms`
- `xcode-bridge-SHA256SUMS`

The bridge TAR and DMG retain their recipe-specific names. The image's `proof/`
files retain the original generic names and exactly the authenticated signing
bytes. The separate outer index inventories actual public filenames and hashes;
its own bytes/signature are excluded from that inventory. For existing strict
CMS verification, copy the exact outer index and signature into a new evidence
directory as `manifest.json` and `manifest.cms`; do not edit either content.
The original signed stage and local unsigned replay proof remain unchanged.

The retained 0.0.2 operator is adapted under
`.artifacts/implementation-0.0.3/drivers/`: a common 0.0.3 operator and a narrow
bridge preparation adapter share image, native, Apple-log and CMS verification.
Pure/static checks passed 27 cases with OS subprocess execution blocked. That
is tooling evidence only; it does not certify image creation, notarization or
installation. Actual executions and failed attempts belong in the 0.0.3
implementation ledger and artifact-bound receipts.
