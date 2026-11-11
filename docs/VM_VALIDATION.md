# macOS 15 VM validation feasibility

The user accepts a macOS 15 validation requirement only if a suitable VM can be obtained freely; no separate macOS 15 machine is available. Current-host testing remains required. Buying hardware, paid hosting or a paid virtualization product is not part of this acceptance condition.

## Available paths

[Tart's quick start](https://tart.run/quick-start/) lists `ghcr.io/cirruslabs/macos-sequoia-vanilla:latest`, a macOS 15 ARM64 image. Its [licensing page](https://tart.run/licensing/) explicitly describes royalty-free personal-workstation use. Public registry requests on 2026-11-09 retrieved its manifest and small configuration layers without a paid account. The listed compressed layers total 23,710,376,601 bytes, approximately 23.7 GB decimal. The VM disk has not been downloaded.

[UTM's GitHub distribution is free and open source](https://docs.getutm.app/installation/macos/). Its [macOS guest documentation](https://docs.getutm.app/guest-support/macos/) describes Apple Silicon virtualization and IPSW installation. This is another free tooling path; its latest-compatible automatic image download does not itself select macOS 15.

Apple's [Virtualization framework installation guide](https://developer.apple.com/documentation/virtualization/installing-macos-on-a-virtual-machine) describes restore-image/hardware-model compatibility checks. Free image availability is distinct from a booted, validated guest.

## Checks on the current host

The host reports macOS 27.2/ARM64. A small Swift probe loads the image's serialized hardware model through Apple's Virtualization framework. Outside the agent sandbox, the host reports:

```json
{"hardwareModelSupported":true,"imageArchitecture":"arm64","minimumMemoryBytes":4294967296,"virtualizationSupported":true}
```

`kern.hv_support` is also 1. The same probe inside the sandbox reported virtualization unavailable, while preserving hardware-model support; the supported unsandboxed read-only check distinguishes that restriction from hardware incompatibility. No virtualization settings were changed, VM was created or software installed. The 4 GiB minimum belongs to the guest-image configuration, not Know Code's hardware requirements or its accepted 2 GiB background-analysis budget.

Durable evidence is in `.artifacts/vm-research/`: registry manifest, fetched small layers, the probe source/binary, and sandboxed/unsandboxed compatibility JSON. The registry tag is mutable; select and record its actual manifest digest when provisioning the final validation environment.

## Candidate validation checkpoint

A free image and compatible hardware model are established; actual macOS 15 guest installation/boot and Know Code execution remain unverified. Provision the VM during the platform-validation checkpoint, with its storage/resources tracked separately from the product's own resource measurements. Retain VM work in a durable project location.

Test the exact release candidate with the declared Node/.NET prerequisites: install/login lifecycle, TS/C# compiler graphs, both in-process SDKs, CLI/MCP, saved-file updates, stale/failure behavior and uninstall/reinstall. A full source build or optional Swift/Rust toolchain is not inferred as a user prerequisite. Record the guest OS/build, VM configuration, source/artifact identities and results. VM compatibility tests do not replace the real-encoding performance gate on the user's workstation.

If a viable free VM cannot actually be run, retain the user's condition and report macOS 15 as unverified. Do not substitute a paid machine or label the current-host run as minimum-OS evidence. Public compatibility claims must distinguish the deployment target from tested versions.


## Pinned provisioning plan — checked 2026-11-11, not executed

The current official release is [Tart 2.41.1](https://github.com/openai/tart/releases/tag/2.41.1); the former `cirruslabs/tart` repository redirects there. Its 22,959,557-byte `tart.tar.gz` asset has GitHub's published SHA-256 `e680a734febccfce7d4e910c1f7f13fc3b00f19a181c945e2d746de3214c32c2`. The [licensing page](https://tart.run/licensing/) states personal-workstation use is royalty-free; [this pinned release's FSL license](https://github.com/openai/tart/blob/2.41.1/LICENSE) explicitly permits internal use. Tart is a validation tool, not an Apache-2.0 dependency to bundle in Know Code.

A fresh public-registry metadata request resolved `ghcr.io/cirruslabs/macos-sequoia-vanilla:latest` to **`sha256:57a83b1b52f928f5b11b0963927178fd6c4cac52c315279f227c27cc5ab81587`**. This is the macOS 15 image listed by the [official image catalog](https://tart.run/quick-start/#vm-images). Its config is ARM64, minimum 2 CPUs/4 GiB, initially configured for 4 CPUs/8 GiB. Compressed layers total 23,710,376,601 bytes; disk layer annotations describe a 50,000,000,000-byte guest disk. Check available volume space for the image cache, clone and subsequent guest growth before provisioning. These are VM storage costs, not Know Code requirements. Raw release/registry responses, the small image config, pinned command sources and hashes are retained in `.artifacts/vm-research/2026-11-11/summary.json`; no archive or disk layer was downloaded.

Tart [supports `TART_HOME`](https://github.com/openai/tart/blob/2.41.1/Sources/tart/Config.swift) for its VM/cache/temporary storage. Its [set command](https://github.com/openai/tart/blob/2.41.1/Sources/tart/Commands/Set.swift) converts `--memory 4096` to exactly 4 GiB; [clone](https://github.com/openai/tart/blob/2.41.1/Sources/tart/Commands/Clone.swift) supports bounded download concurrency. The following is a future provisioning command sequence, **not a log of executed actions**:

```sh
VM_ROOT="$PWD/.artifacts/vm/macos15-final"
test ! -e "$VM_ROOT"
mkdir -p "$VM_ROOT/tool" "$VM_ROOT/input" "$VM_ROOT/evidence"
curl --fail --location --output "$VM_ROOT/tool/tart.tar.gz" \
  https://github.com/openai/tart/releases/download/2.41.1/tart.tar.gz
printf '%s  %s\n' \
  e680a734febccfce7d4e910c1f7f13fc3b00f19a181c945e2d746de3214c32c2 \
  "$VM_ROOT/tool/tart.tar.gz" | shasum -a 256 -c -
tar -xzf "$VM_ROOT/tool/tart.tar.gz" -C "$VM_ROOT/tool"
TART="$VM_ROOT/tool/tart.app/Contents/MacOS/tart"
tart_vm() {
  env TART_HOME="$VM_ROOT/tart-home" TART_NO_AUTO_PRUNE=1 "$TART" "$@"
}
tart_vm --version > "$VM_ROOT/evidence/tart-version.txt"
df -h "$VM_ROOT" > "$VM_ROOT/evidence/host-capacity.txt"
tart_vm clone --concurrency 2 \
  ghcr.io/cirruslabs/macos-sequoia-vanilla@sha256:57a83b1b52f928f5b11b0963927178fd6c4cac52c315279f227c27cc5ab81587 \
  macos15-final
tart_vm set macos15-final --cpu 2 --memory 4096
tart_vm list --format json > "$VM_ROOT/evidence/vms.json"
cp "$VM_ROOT/tart-home/vms/macos15-final/config.json" "$VM_ROOT/evidence/vm-config.json"
# Stage only the final signed candidate, prerequisite archives and validation kit
# under input before this command. Do not share the source checkout or host HOME.
tart_vm run --no-clipboard --no-audio --no-usb-accessories \
  --dir "candidate:$VM_ROOT/input:ro" --dir "evidence:$VM_ROOT/evidence" macos15-final
```

Use the full app-contained executable, as required by Tart's manual-install instructions; no Homebrew/global installation, credential creation, host network reconfiguration or `HOME` reassignment is part of this plan. `TART_NO_AUTO_PRUNE` prevents automatic pruning even within this dedicated store. Use the graphical guest for the first normal-security install; do not change host keychain/security settings to work around a boot failure. The default shared NAT suffices; no bridged interface or host port exposure is planned. Mount syntax and guest paths are documented by [Tart's shared-directory guide](https://tart.run/quick-start/#mounting-directories).

## Guest validation handoff

Before boot, stage a SHA-256 inventory of the exact final **signed/notarized** runtime/npm/NuGet archives and signing receipt, separately downloaded official Node 24 ARM64 and .NET 10 SDK ARM64 archives with their upstream checksums, and a minimal verification kit. The current kit needs `LICENSE`, `tests/package_install.py`, `tests/workspace_client.mjs`, `bindings/dotnet/Smoke/Program.cs`, and `scripts/sign_packages.py`; recheck its file dependencies against the frozen candidate. Python 3 is a test-harness tool only, not a Know Code prerequisite. If absent from the vanilla guest, provision it separately for this verification kit; do not install the full Know Code source build or optional Swift/Rust toolchains.

Inside the guest, record `sw_vers`, `uname -m`, resource settings and prerequisite versions to the writable evidence share. Require guest macOS major 15 and ARM64 before counting this gate. Extract the staged Node/.NET archives into a dedicated guest test directory, add their bins to that terminal's `PATH`, and set `DOTNET_ROOT` to that SDK directory. Leave the real guest `HOME` intact. Copy the verification kit to `~/know-code-vm-validation/kit`; keep the installed-test prefix outside it. The signed installed/offline gate can then reuse the existing harness, including actual native .NET/Node consumers, CLI/MCP, saved changes and checkout/network-denial controls:

```sh
python3 "$HOME/know-code-vm-validation/kit/tests/package_install.py" \
  "/Volumes/My Shared Files/candidate/signed-packages" \
  --prefix "$HOME/know-code-vm-validation/installed" \
  --signed-team-id 3FMAPDQDGP --signed-signer-sha256 FINAL_PINNED_SIGNER_DER_SHA256
```

Replace the signer placeholder from the independently pinned release signer, not from untrusted package contents. In addition, perform the final normal-security/Gatekeeper and login lifecycle checks from the release checklist in this disposable guest: successful signature verification alone is not notarization or a normal-security install pass. Copy the raw harness result, commands, guest OS/build, archive/signer/harness hashes and lifecycle observations to the evidence share, shut the guest down normally, then retain `tart_vm list --format json` showing it stopped. Retain the VM disk and failed-attempt evidence under `.artifacts/vm/`; no automatic deletion is planned.

Actual Tart installation, image download, boot, guest prerequisite setup and all guest product checks remain **unverified**. The current host's earlier hardware-model result does not certify this sequence. A free image remains available, but the conditional minimum-OS gate closes only after a successful recorded guest run.
