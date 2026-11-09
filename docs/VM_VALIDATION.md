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
