Vibe is a quick, zero-configuration way to spin up a Linux virtual machine on Mac to sandbox LLM agents:

```
$ cd my-project
$ vibe

░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░▒▓███████▓▒░░▒▓████████▓▒░
░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░
 ░▒▓█▓▒▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░
 ░▒▓█▓▒▒▓█▓▒░░▒▓█▓▒░▒▓███████▓▒░░▒▓██████▓▒░
  ░▒▓█▓▓█▓▒░ ░▒▓█▓▒░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░
  ░▒▓█▓▓█▓▒░ ░▒▓█▓▒░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░
   ░▒▓██▓▒░  ░▒▓█▓▒░▒▓███████▓▒░░▒▓████████▓▒░

Host                                      Guest                    Mode
----------------------------------------  -----------------------  ----------
/Users/dev/work/my-project                /root/my-project         read-write
/Users/dev/.cache/vibe/.guest-mise-cache  /root/.local/share/mise  read-write
/Users/dev/.cache/vibe/.guest-mise-config /root/.config/mise       read-write
/Users/dev/.cache/vibe/.guest-claude-config /root/.claude-config   read-write
/Users/dev/.m2                            /root/.m2                read-write
/Users/dev/.cargo/registry                /root/.cargo/registry    read-write
/Users/dev/.codex                         /root/.codex             read-write
/Users/dev/.claude                        /root/.claude            read-write
/Users/dev/.gemini                        /root/.gemini            read-write
/Users/dev/.pi                            /root/.pi                read-write

root@vibe:~/my-project#
```

On my M1 MacBook Air it takes ~10 seconds to boot.


Dependencies:

- An ARM-based Mac running MacOS 13 (Ventura) or higher.
- A network connection is required on the first run to download and configure the Debian Linux base image.
- That's it!


## Why use Vibe?

- LLM agents are more fun to use with `--yolo`, since they're not always interrupting you to approve their commands.
- Sandboxing the agent in a VM lets it install/remove whatever tools its lil' transformer heart desires, *without* wrecking your actual machine.
- You control what the agent (and thus the upstream LLM provider) can actually see, by controlling exactly what's shared into the VM sandbox.
  (This project was inspired by me running `codex` *without* `--yolo` and seeing it reading files outside of the directory I started it in --- not cool, bro.)

I'm using virtual machines rather than containers because:

- Virtualization is more secure against malicious escapes than containers or the MacOS sandbox framework.
- Containers on MacOS require spinning up a virtual machine anyway.

Finally, as a matter of taste and style:

- I wrote the entire README myself, 100% with my human brain.
- The entire implementation is ~2000 lines of Rust.
- The only Rust dependencies are the [Objc2](https://github.com/madsmtm/objc2) interop crates and the [lexopt](https://github.com/blyxxyz/lexopt) argument parser.
- There are no emoji anywhere in this repository.


## Install

Vibe is a single binary built with Rust, with a [bundled networking helper](/helpers/vibe-usernet) written in Go.

Download [the latest binary built by GitHub actions](https://github.com/lynaghk/vibe/releases/tag/latest) and put it somewhere on your `$PATH`:

    curl -LO https://github.com/lynaghk/vibe/releases/download/latest/vibe-macos-arm64.zip
    unzip vibe-macos-arm64.zip
    mkdir -p ~/.local/bin
    mv vibe ~/.local/bin
    export PATH="$HOME/.local/bin:$PATH"

If you use [mise-en-place](https://mise.jdx.dev/):

    mise use github:lynaghk/vibe@latest

I'm not making formal releases or keeping a change log.
I recommend reading the commit history and pinning to a specific version.

If you're building from a checkout, use mise to get the Rust and Go compilers:

    mise install --locked
    cargo build --locked


## Using Vibe

Vibe only does two things:

1. **Runs** VMs from a raw disk image file
2. **Provisions** VMs by booting a base raw disk image, running scripts in it, then saving the resulting raw disk image as a template

When you run `vibe` in a project directory, it copies the default template (`~/.cache/vibe/default.raw`) to `.vibe/instance.raw`, boots it up, and attaches your terminal to this VM.

If a `.aiexclude` file exists in the project root, Vibe applies masks inside the VM at startup:

- Lines starting with `#` and empty lines are ignored.
- Absolute paths are used as-is.
- Relative paths are resolved from the `.aiexclude` file directory.
- Entries containing `/` are path-based (e.g. `server/secretfolder`).
- Bare entries without `/` are matched recursively (gitignore-style filename matching):
  - `.env` matches `.env` files in root and subfolders.
  - `.env*` matches `.env.production`, `.env.local`, etc. in root and subfolders.
  - `secretfolder` matches folders/files named `secretfolder` in root and subfolders.
- Recursive matching skips common heavy folders:
  `.git`, `node_modules`, `target`, `__pycache__`, `.venv`, `venv`, `env`, `.tox`, `.nox`,
  `.pytest_cache`, `.mypy_cache`, `.ruff_cache`, `.cache`, `dist`, `build`, `.next`, `.nuxt`, `.svelte-kit`.

When you `exit` this shell, the VM is shutdown.
The disk state persists until you delete it.

For a persistent, reconnectable VM, use SSH mode:

vibe ssh [--main] [--forward HOST_PORT:GUEST_PORT | --forward-all HOST_PORT:GUEST_PORT ...]

`vibe ssh` requires an existing `~/.cache/vibe/default.raw`. On a new
installation, run `vibe` first, wait for provisioning to finish, then exit the
VM before starting SSH mode.

This starts the current project's VM in a detached supervisor and connects with
the host `/usr/bin/ssh` client. Logging out, pressing Ctrl-D, losing the
connection, or closing the terminal leaves the VM running. Running `vibe ssh`
again from the same canonical project folder reconnects to that VM. Different
projects receive separate loopback ports starting at 2222.

Use `vibe ssh --main` to start or reconnect to one project-independent VM. Its
disk and logs are stored in `~/.cache/vibe/main/`, and it does not create a
`.vibe` directory in the current folder. On first use, Vibe creates
`~/.cache/vibe/main/mounted-folders.txt` and adds the canonical current folder.
Each absolute host folder path in the file is mounted read-write at
`/root/FOLDER_NAME`; blank lines and lines starting with `#` are ignored. Paths
must exist, and two paths with the same folder name are rejected because they
would have the same guest destination.

Each `vibe ssh --main` invocation adds the canonical current folder when it is
not already covered by a listed parent folder. If the main VM is stopped, the
new folder is mounted during startup. If it is already running, the entry is
saved for the next boot and Vibe reports that the VM must be stopped and
restarted before the folder becomes available. The running VM is never
restarted automatically.

When `vibe ssh --main` is invoked from a listed folder or one of its
descendants, the SSH shell starts at the corresponding path below
`/root/FOLDER_NAME`. Otherwise, it starts in `/root`. If listed folders are
nested, the most specific matching folder is used. Any `.vibe` directory in a
listed folder is masked inside the VM.

Add repeatable TCP forwards alongside SSH. `--forward` binds only to `127.0.0.1`; `--forward-all` binds to `0.0.0.0`:

    vibe ssh --forward 8080:80 --forward 3000:3000

    vibe ssh --forward-all 8080:80

Port forwards are fixed when supervisor starts. To change them for running VM,
stop VM first, then restart it with desired `--forward` values.

List and stop live SSH-managed VMs with:

    vibe ssh --list
    vibe ssh --stop ID
    vibe ssh --stop all

`vibe ssh --list` reports live processes only. A guest `poweroff` also ends its
supervisor and removes the live record. Stale live records left by a crash or
host reboot are cleaned by the next SSH command.

SSH mode uses the fixed host identity `~/.ssh/vibe_ed25519`. When provisioning
an image for the first time, Vibe asks permission to create this Ed25519
identity and its `.pub` file with no passphrase. `vibe ssh` requires that pair
to exist and never generates or installs keys into an existing image. Every
image created by `vibe provision`, including the automatic default image,
receives the public key and OpenSSH configuration through the always-run base
provisioning script when key creation is accepted. Declining creates a
console-only image and continues provisioning normally. Existing template and
instance disks are not updated. The private key is never copied into the guest. Supervisor
diagnostics are written to `.vibe/vibe-ssh-supervisor.log`; networking
diagnostics remain in `.vibe/vibe-usernet.log`.

Where does this `default.raw` raw disk image come from?

When you first run `vibe`, a Debian Linux base image is downloaded and [all of the provisioning scripts](/provisioning/) are run against it.

As a thoughtful person who reads the README, you'll probably appreciate the ability to create custom template images by running:

    vibe provision --image my-template @rust @codex my-custom-script.sh

(Omitting `--image` provisions the default template.)
Scripts are run in order, and those prefixed with `@` are resolved against the built-in scripts shipped with Vibe.

In `my-custom-script.sh` I set up my tmux keybindings, favorite shell customizations, etc.

In a project directory, you can then run `vibe --image my-template` to use this template image (this flag is ignored if `.vibe/instance.raw` already exists).

The [base provisioning script](/provisioning/base.sh) is always run when provisioning to install basic tools like gcc, [mise-en-place](https://mise.jdx.dev/), ripgrep, etc.
If you don't want this, you can make your own `.raw` disk images and copy them into `~/.cache/vibe/` to use them as templates.


```
vibe [OPTIONS] [LOGIN-ACTIONS ...] [path/to/disk.raw]
vibe provision [PROVISIONING_OPTIONS] [@built-in | path/to/script.sh ...]
vibe ssh [--main] [--forward HOST_PORT:GUEST_PORT ... | --list | --stop ID|all]

Options:

  --help                                                    Print this help message.
  --version                                                 Print the version (commit SHA and build date).
  --image NAME                                              Use this template image (ignored if `.vibe/instance.raw` already exists)
  --no-default-mounts                                       Disable all default mounts, including .git and .vibe project subfolder masking.
  --env NAME                                                Export host environment variable NAME inside VM.
                                                            Errors if NAME is unset or empty.
  --mount HOST_PATH:GUEST_PATH[:read-only | :read-write]    Mount HOST_PATH inside VM at GUEST_PATH (default mode `:read-write`)
                                                            Errors if HOST_PATH does not exist.
  --network <nat|vznat>                                     Guest networking mode (default `nat`).
                                                            `nat` uses Vibe's bundled user-mode network stack.
                                                            `vznat` uses Apple's VZNATNetworkDeviceAttachment.
  --forward HOST_PORT:GUEST_PORT                             Forward a loopback-only TCP host port to the VM (repeatable; requires `--network nat`).
  --forward-all HOST_PORT:GUEST_PORT                         Forward a TCP host port on all interfaces to the VM (repeatable; requires `--network nat`).
  --cpus COUNT                                              Number of virtual CPUs (default 2).
  --ram MEGABYTES                                           RAM size in megabytes (default 2048).

Login actions (executed in order after root login, repeatable):

  --script PATH_TO_SCRIPT                                   Run script in VM; stop if it exits non-zero.
  --send SOME_COMMAND                                       Type SOME_COMMAND followed by newline into the VM.
  --expect STRING [timeout-seconds]                         Wait for STRING to appear in console output before executing next login action.
                                                            If STRING does not appear within timeout (default 30 seconds), shutdown VM with error.

Provisioning creates a new named image by running (built-in) scripts. Options:

  --base NAME_OR_PATH                                       Use this existing image or path/to/image.raw as base for new image (default Debian Stable).
  --image NAME                                              Name for new image (default `default`).
  --replace                                                 Replace existing image with NAME, if one exists.
  --cpus COUNT                                              Number of virtual CPUs for the provisioning VM (default 2).
  --ram MEGABYTES                                           RAM size in megabytes for the provisioning VM (default 2048).
```

## Other notes

- Vibe VMs can reach the host at `192.168.5.2`.
  VMs cannot reach each other. The host can reach an explicitly forwarded TCP port, such as `vibe --forward 8080:80`, at `127.0.0.1:8080`.
  DNS is handled by the host resolver, so VMs get VPN and split-DNS compatibility.

- This networking is based on a bundled gVisor/Lima-style user-mode network helper process, `vibe-usernet`, which is spawned automatically when you run `vibe`.
  I ended up with this solution because Apple's [VZNATNetworkDeviceAttachment](https://developer.apple.com/documentation/virtualization/vznatnetworkdeviceattachment) lost packets and VMs got wrecked whenever host networking changed (e.g., switching between wifi/ethernet/VPN). [VZBridgedNetworkDeviceAttachment](https://developer.apple.com/documentation/virtualization/vzbridgednetworkdeviceattachment) requires kowtowing to acquire the restricted [com.apple.vm.networking](https://developer.apple.com/documentation/BundleResources/Entitlements/com.apple.vm.networking) entitlement, which I'm not interested in doing.
  If you have suggestions for how to improve networking capabilities, please open an issue or PR!

- The default VM disk is 100 GiB, but since Apple Filesystem is copy-on-write and doesn't count zeros, disk space is only used when you actually write new blocks.
  You can use `du -h` to see how much space is actually consumed:

      $ ls -lah .vibe/instance.raw
      -rw-r--r--  1 dev  staff    100G Feb 11 21:57 .vibe/instance.raw

      $ du -h .vibe/instance.raw
      2.5G    .vibe/instance.raw

  If you need even more space within the VM, e.g., 500 GiB, run `truncate -s 500G .vibe/instance.raw` on your Mac and then within the VM run `growpart /dev/vda 1 && resize2fs /dev/vda1`.

- MacOS only lets binaries signed with the `com.apple.security.virtualization` entitlement run virtual machines, so `vibe` checks itself on startup and, if necessary, signs itself using `codesign`. SeCuRiTy!

- Debian "nocloud" is used as a base image because it boots directly to a root prompt.
  The other images use [cloudinit](https://cloudinit.readthedocs.io/en/latest/), which I found much more complex:
  - Network requests are made during the boot process, and if you're offline they take several *minutes* to timeout before the login prompt is reached (thanks, `systemd-networkd-wait-online.service`).
  - Subsequent boots are much slower (at least, I couldn't easily figure out how to remove the associated cloud init machinery).

- Claude Code keeps credentials and session state in `.claude.json`. Vibe stores this file in its persistent guest share at `~/.cache/vibe/.guest-claude-config`, so logging in from the VM preserves it without reading or overwriting the host's `~/.claude.json`.


## Alternatives

Here's what I tried before writing this solution:

- [Sandboxtron](https://github.com/lynaghk/sandboxtron/) - My own little wrapper around Mac's `sandbox-exec`.
Turns out both Claude Code and Codex rely on this as well, and MacOS doesn't allow creating a sandbox from within a sandbox.
I considered writing my own sandboxing rules and running the agents `--yolo`, but didn't like the risk of configuration typos and/or Mac sandbox escapes (there are a lot --- I'm not an expert, but from [this HN discussion](https://news.ycombinator.com/item?id=42084588) I figured virtualization would be safer).

- [Lima](https://github.com/lima-vm/lima/), quick Linux VMs on Mac. I wanted to like this, ran into too many issues in first 30 minutes to trust it:
  - The recommended Debian image took 8 seconds to get to a login prompt, even after the VM was already running.
  - The CLI flags *mutate hidden state*. E.g., If you `limactl start --mount foo` and then later `limactl start --mount bar`, both `foo` and `bar` will be mounted.
  - Some capabilities are only available via yaml. E.g., the `--mount` CLI flag always mounts at the same path in the guest. If you want to mount at a different path, you have to do that via YAML.
  - There are many layers of inheritance/defaults, so even if you do write YAML, you can't see the full configuration.

- [Vagrant](https://developer.hashicorp.com/vagrant/) - I fondly remember using this back in the early 2010's, but based on this [2025 Reddit discussion](https://www.reddit.com/r/devops/comments/1axws75/vagrant_doesnt_support_mac_m1/) it seemed like running it on an ARM-based Mac was A Project and so I figured it'd be easier to roll my own thing.

- [Tart](https://tart.run/) - I found this via some positive HN comments, but unfortunately wasn't able to run the release binary from their GitHub because it's not signed.
They apparently hack around that when installing with homebrew, but I don't use homebrew either.
I tried cloning the repo and compiling myself, but the build failed with lots of language syntax errors despite the repo SHA is the same as one of their releases.
I assume this is a Swift problem and not Tart's, since this sort of mess happens most times when I try to build Swift. `¯\_(ツ)_/¯`

- [OrbStack](https://orbstack.dev/) - This looked nice, but seems mostly geared towards container stuff.
It runs a single VM, and I couldn't figure out how to have this VM run *without* my entire disk mounted inside of it.
I didn't want to run agents via containers, since containers aren't security boundaries.

- [Apple Container Framework](https://github.com/apple/container) - This looks technically promising, as it runs every container within a lightweight VM.
Unfortunately it requires MacOS 26 Tahoe, which wrecks [window resizing](https://news.ycombinator.com/item?id=46579864), adds [useless icons everywhere](https://news.ycombinator.com/item?id=46497712), and otherwise seems to be a mess.
Sorry excellent Apple programmers and hardware designers, I hope your management can reign in the haute couture folks before we all have to switch to Linux for professional computing.

- [QEMU](https://wiki.qemu.org/) - The first prototype of this app was [a single bash script](https://github.com/lynaghk/vibe/blob/1c82fd3b9fabf93abba2680fc856458e97a105cd/qemu.sh) wrapping `qemu`. This worked swimmingly, except for host/guest directory sharing, which ended up being a show-stopper. This is because QEMU doesn't support [virtiofs](https://virtio-fs.gitlab.io/) on Mac hosts, it only supports "9p", which is way slower ---  e.g., `mise use node@latest` takes > 10 minutes on 9p and 5 seconds on virtiofs.


## Roadmap / Collaboration

I wrote this software for myself, and I'm open to pull requests and otherwise collaborating on features that I'd personally use:

- resizing disk images
- forwarding ports from the host to a guest
- running `vibe` against a disk image that's already running should connect to the already-running VM
  - the VM shouldn't shutdown until all host terminals have logged out
- if not the above, at least a check and throw a nice error message when you try to start a VM that's already running
- a way to make faster-booting even more minimal Linux virtual machines
  - this should be bootstrappable on Mac; i.e., if the only way to make a small Linux image is with Linux-only tools, the entire process should still be runnable on MacOS via intermediate VMs
- propagate an exit code from within VM to the `vibe` command
- don't propagate user typing until all provided `--expect` and `--send` actions have completed
- CPU core / memory / networking configuration, possibly via flags or via extended attributes on the disk image file
- a `--plan` flag which pretty-prints a CLI invocation with all of the default arguments shown
  - to keep ourselves honest, we should use the same codepath for the actual execution (maybe we can `exec` into the generated command?)
  - Being fully "explicit" is tricky due to flag interactions.
    E.g., the friendly `--mount` would need to be decomposed into two flags: One that exposes the host directory in the guest's staging area at `/mnt/shared/` and another flag `--send 'mount --bind ...'`to bind this to the desired guest location.

I'm not sure about (but open to discussing proposals via GitHub issues):

- running VMs in the background
- supporting Linux hosts
- supporting guests beyond Debian Linux
- using SSH as a login mechanism; this would eliminate the current stdin/stdout-to-console plumbing (yay!) but require additional setup/configuration (boo!)

I'm not interested in:

- anything related to Docker / containers / Kubernetes / distributed systems


When opening PRs, please ensure all commits have been formatted and pass tests. Run:

    /scripts/format.sh
    /scripts/test.sh
