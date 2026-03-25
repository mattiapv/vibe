use std::{
    borrow::Cow,
    collections::HashMap,
    env,
    ffi::OsString,
    fs,
    io::{self, Write},
    os::{
        fd::RawFd,
        unix::{
            fs::PermissionsExt,
            io::{AsRawFd, IntoRawFd, OwnedFd},
            net::UnixStream,
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use block2::RcBlock;
use dispatch2::DispatchQueue;
use lexopt::prelude::*;
use objc2::{AnyThread, rc::Retained, runtime::ProtocolObject};
use objc2_foundation::*;
use objc2_virtualization::*;

mod networking;
use networking::*;
mod ssh_runtime;
mod vm_registry;
const DEBIAN_COMPRESSED_DISK_URL: &str = "https://cloud.debian.org/images/cloud/trixie/20260112-2355/debian-13-nocloud-arm64-20260112-2355.tar.xz";
const DEBIAN_COMPRESSED_SHA: &str = "6ab9be9e6834adc975268367f2f0235251671184345c34ee13031749fdfbf66fe4c3aafd949a2d98550426090e9ac645e79009c51eb0eefc984c15786570bb38";
const DEBIAN_COMPRESSED_SIZE_BYTES: u64 = 280901576;
const SHARED_DIRECTORIES_TAG: &str = "shared";

const BYTES_PER_MB: u64 = 1024 * 1024;
const DEFAULT_CPU_COUNT: usize = 2;
const DEFAULT_RAM_MB: u64 = 2048;
const DEFAULT_RAM_BYTES: u64 = DEFAULT_RAM_MB * BYTES_PER_MB;
const DEFAULT_DISK_GB: u64 = 100;
const START_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_EXPECT_TIMEOUT: Duration = Duration::from_secs(30);
const LOGIN_EXPECT_TIMEOUT: Duration = Duration::from_secs(120);
const SCRIPT_ACTION_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
const LOGIN_ACTION_INPUT_CHUNK_BYTES: usize = 256;
const LOGIN_ACTION_INPUT_CHUNK_DELAY: Duration = Duration::from_millis(10);
const PROVISION_SUCCESS_MARKER: &str = "VIBE_PROVISION_SUCCESS";
const DEFAULT_IMAGE_NAME: &str = "default";
const INSTANCE_DIR_NAME: &str = ".vibe";
const INSTANCE_DISK_IMAGE_NAME: &str = "instance.raw";
include!(concat!(env!("OUT_DIR"), "/provisioning.rs"));
const AIEXCLUDE_MOUNTS_SCRIPT: &str = include_str!("aiexclude_mounts.sh");
const VIBE_GITIGNORE: &str = "# created by vibe automatically\n*\n";

#[derive(Clone)]
enum LoginAction {
    Expect { text: String, timeout: Duration },
    Send(String),
    Script { name: String, content: String },
}
use LoginAction::*;

#[derive(Clone)]
struct DirectoryShare {
    host: PathBuf,
    guest: PathBuf,
    read_only: bool,
}

impl DirectoryShare {
    fn new(
        host: PathBuf,
        mut guest: PathBuf,
        read_only: bool,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        if !host.exists() {
            return Err(format!("Host path does not exist: {}", host.display()).into());
        }
        if !guest.is_absolute() {
            guest = PathBuf::from("/root").join(guest);
        }
        Ok(Self {
            host,
            guest,
            read_only,
        })
    }

    fn from_mount_spec(spec: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let parts: Vec<&str> = spec.split(':').collect();
        if parts.len() < 2 || parts.len() > 3 {
            return Err(format!("Invalid mount spec: {spec}").into());
        }
        let host = PathBuf::from(parts[0]);
        let guest = PathBuf::from(parts[1]);
        let read_only = if parts.len() == 3 {
            match parts[2] {
                "read-only" => true,
                "read-write" => false,
                _ => {
                    return Err(format!(
                        "Invalid mount mode '{}'; expected read-only or read-write",
                        parts[2]
                    )
                    .into());
                }
            }
        } else {
            false
        };
        DirectoryShare::new(host, guest, read_only)
    }

    fn tag(&self) -> String {
        let path_str = self.host.to_string_lossy();
        let hash = path_str.bytes().fold(5381u64, |h, b| {
            h.wrapping_mul(33).wrapping_add(u64::from(b))
        });
        let base_name = self
            .host
            .file_name()
            .map_or("share".into(), |s| s.to_string_lossy());
        format!("{base_name}_{hash:016x}")
    }
}

fn provisioning_scripts_banner() -> String {
    let mut scripts: Vec<String> = BUILTIN_PROVISION_SCRIPTS
        .iter()
        .filter(|s| s.name != "base")
        .map(|s| format!("  @{:<10} {}", s.name, s.description))
        .collect();
    scripts.sort();

    format!(
        "Built-in provisioning scripts:

{}",
        scripts.join("\n")
    )
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = parse_cli()?;

    if args.version {
        println!("Vibe");
        println!("https://github.com/lynaghk/vibe/");
        println!("Git SHA: {}", env!("GIT_SHA"));
        println!("Built: {}", env!("BUILD_DATE"));

        std::process::exit(0);
    }

    if args.help {
        println!("Vibe is a quick way to spin up a Linux virtual machine on Mac to sandbox LLM agents.

vibe [OPTIONS] [LOGIN-ACTIONS ...] [path/to/disk.raw]
vibe provision [PROVISIONING_OPTIONS] [@built-in | path/to/script.sh ...]
vibe ssh [--forward HOST_PORT:GUEST_PORT ... | --list | --stop ID|all]
vibe ls

Options:

  --help                                                    Print this help message.
  --version                                                 Print the version (commit SHA and build date).
  --image NAME                                              Use this template image (ignored if `{INSTANCE_DIR_NAME}/{INSTANCE_DISK_IMAGE_NAME}` already exists)
  --no-default-mounts                                       Disable all default mounts, including .git and .vibe project subfolder masking.
  --env NAME                                                Export host environment variable NAME inside VM.
                                                            Errors if NAME is unset or empty.
  --mount HOST_PATH:GUEST_PATH[:read-only | :read-write]    Mount HOST_PATH inside VM at GUEST_PATH (default mode `:read-write`)
                                                            Errors if HOST_PATH does not exist.
  --network <nat|vznat>                                     Guest networking mode (default `nat`).
                                                            `nat` uses Vibe's bundled user-mode network stack.
                                                            `vznat` uses Apple's VZNATNetworkDeviceAttachment.
  --forward HOST_PORT:GUEST_PORT                             Forward a loopback-only TCP host port to the VM (repeatable; requires `--network nat`).
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

Commands

  ssh [--forward HOST_PORT:GUEST_PORT ...]                  Start or reconnect to a persistent VM over SSH, with optional extra port forwards.
  ssh --list                                                List currently running SSH-managed VMs.
  ssh --stop ID                                             Gracefully stop one SSH-managed VM.
  ssh --stop all                                            Gracefully stop all SSH-managed VMs.
  ls                                                        List tracked VMs from ~/.cache/vibe/vm_registry.json
                                                            and optionally delete selected entries and .vibe folders.

{}",
                 provisioning_scripts_banner()
        );
        std::process::exit(0);
    }

    let home = env::var("HOME").map(PathBuf::from)?;
    let cache_home = env::var("XDG_CACHE_HOME").map_or_else(|_| home.join(".cache"), PathBuf::from);
    let cache_dir = cache_home.join("vibe");
    if let CliCommand::Ssh(command) = &args.command {
        return match command {
            SshCommand::Connect(forwards) => {
                if !image_path(&cache_dir, DEFAULT_IMAGE_NAME).exists() {
                    return Err("vibe ssh requires a provisioned default image. Run `vibe` first, wait for provisioning to finish, then exit the VM and run `vibe ssh` again.".into());
                }
                require_vibe_ssh_identity(&home)?;
                ensure_signed();
                ssh_runtime::connect_command(&cache_dir, &home, &env::current_dir()?, forwards)
            }
            SshCommand::List => ssh_runtime::list_command(&cache_dir),
            SshCommand::Stop(id) => ssh_runtime::stop_command(&cache_dir, id),
            SshCommand::StopAll => ssh_runtime::stop_all_command(&cache_dir),
        };
    }

    if matches!(&args.command, CliCommand::List) {
        return run_vm_registry_ls(&cache_dir);
    }
    let guest_mise_cache = cache_dir.join(".guest-mise-cache");
    let basename_compressed = DEBIAN_COMPRESSED_DISK_URL.rsplit('/').next().unwrap();
    let base_compressed = cache_dir.join(basename_compressed);
    let base_raw = cache_dir.join(format!(
        "{}.raw",
        basename_compressed.trim_end_matches(".tar.xz")
    ));

    // Prepare system-wide directories
    fs::create_dir_all(&cache_dir)?;
    fs::create_dir_all(&guest_mise_cache)?;

    ensure_signed();

    let usernet_helper_path = cache_dir.join("vibe-usernet");
    let prepare_network_backend = |log_dir: Option<&Path>| {
        args.network_mode
            .prepare(&usernet_helper_path, &args.forwards, log_dir)
    };
    let prepare_provision_network_backend = |log_dir: Option<&Path>| {
        args.network_mode
            .prepare(&usernet_helper_path, &[], log_dir)
    };

    let mise_directory_share =
        DirectoryShare::new(guest_mise_cache, "/root/.local/share/mise".into(), false)?;

    match args.command {
        CliCommand::Provision {
            base,
            image,
            replace,
            scripts,
            cpu_count,
            ram_bytes,
        } => {
            let ssh_public_key = ensure_vibe_ssh_identity(&home)?;
            let base_raw = match base {
                Some(base) if base.contains('/') => PathBuf::from(base),
                Some(base) => image_path(&cache_dir, &base),
                None => {
                    ensure_base_image(&base_raw, &base_compressed)?;
                    base_raw
                }
            };
            if !base_raw.exists() {
                return Err(format!("Base image does not exist: {}", base_raw.display()).into());
            }
            provision_image(
                &base_raw,
                &image_path(&cache_dir, &image),
                replace,
                &scripts,
                std::slice::from_ref(&mise_directory_share),
                prepare_provision_network_backend,
                cpu_count,
                ram_bytes,
                &ssh_public_key,
            )
        }
        CliCommand::List => unreachable!(),
        CliCommand::Run {
            disk,
            image,
            supervisor,
        } => {
            let project_root = match &supervisor {
                Some(config) => config.project_root.clone(),
                None => env::current_dir()?.canonicalize()?,
            };
            let project_name = project_root
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();

            let instance_dir = project_root.join(INSTANCE_DIR_NAME);
            let instance_raw = instance_dir.join(INSTANCE_DISK_IMAGE_NAME);
            // Only persist the networking log for the managed instance disk in `.vibe/`.
            // For external `--disk` there's no natural per-instance directory to write to, so skip logging.
            let log_to_instance = disk.is_none();
            let disk_path = if let Some(path) = disk {
                if !path.exists() {
                    return Err(format!("Disk image does not exist: {}", path.display()).into());
                }
                path.clone()
            } else {
                if image != DEFAULT_IMAGE_NAME && instance_raw.exists() {
                    eprintln!("Ignoring --image {image}, using {}", instance_raw.display());
                }
                let template_raw = image_path(&cache_dir, &image);
                if image == DEFAULT_IMAGE_NAME {
                    ensure_default_image(
                        &base_raw,
                        &base_compressed,
                        &template_raw,
                        &home,
                        std::slice::from_ref(&mise_directory_share),
                        prepare_provision_network_backend,
                    )?;
                } else if !template_raw.exists() {
                    return Err(format!(
                        "Template image does not exist: {}",
                        template_raw.display()
                    )
                    .into());
                }
                ensure_instance_disk(&instance_raw, &template_raw)?;

                instance_raw
            };

            let mut login_actions = Vec::new();
            let mut directory_shares = Vec::new();

            if !args.no_default_mounts {
                login_actions.push(Send(format!(" cd '{}'", shell_single_quote(&project_name))));

                // Discourage read/write of project dir subfolders within the VM.
                // Note that this isn't secure, since the VM runs as root and could unmount this.
                // I couldn't find an alternative way to do this --- the MacOS sandbox doesn't apply to the Apple Virtualization system =(
                for subfolder in [".git", INSTANCE_DIR_NAME] {
                    if project_root.join(subfolder).exists() {
                        login_actions.push(Send(format!(r" mount -t tmpfs tmpfs {subfolder}")));
                    }
                }

                directory_shares.push(
                    DirectoryShare::new(
                        project_root.clone(),
                        PathBuf::from("/root/").join(project_name),
                        false,
                    )
                    .expect("Project directory must exist"),
                );

                directory_shares.push(mise_directory_share);
                // Activate mise if applicable.
                // This is in addition to the .bashrc, since mise activation must occur after the shared tool cache is mounted.
                login_actions.push(Send(
                    " if [ -x \"$HOME/.local/bin/mise\" ]; then eval \"$(\"$HOME/.local/bin/mise\" activate bash)\"; fi"
                        .to_string(),
                ));

                // Add default shares, if they exist
                for share in [
                    DirectoryShare::new(home.join(".m2"), "/root/.m2".into(), false),
                    DirectoryShare::new(
                        home.join(".cargo/registry"),
                        "/root/.cargo/registry".into(),
                        false,
                    ),
                    DirectoryShare::new(home.join(".codex"), "/root/.codex".into(), false),
                    DirectoryShare::new(home.join(".claude"), "/root/.claude".into(), false),
                    DirectoryShare::new(home.join(".gemini"), "/root/.gemini".into(), false),
                    DirectoryShare::new(home.join(".pi"), "/root/.pi".into(), false),
                ]
                .into_iter()
                .flatten()
                {
                    directory_shares.push(share);
                }
                // Bind-mount linux ripgrep over shared macos binary to ensure compatibility
                login_actions.push(Send(
                    " if [ -f /root/.gemini/tmp/bin/rg ] && [ -f /usr/bin/rg ]; then mount --bind /usr/bin/rg /root/.gemini/tmp/bin/rg; fi"
                        .to_string()
                ));
                login_actions.push(Send(
                    " if [ -x /root/.aiexclude_mounts.sh ] && [ -f .aiexclude ]; then /root/.aiexclude_mounts.sh .aiexclude; fi"
                        .to_string(),
                ));
            }

            for spec in &args.mounts {
                directory_shares.push(DirectoryShare::from_mount_spec(spec)?);
            }

            login_actions.extend(env_login_actions(&args.env));

            // Enable bash history
            login_actions.push(Send(" export HISTFILE=/root/.bash_history".to_string()));

            if let Some(motd_action) = motd_login_action(&directory_shares) {
                login_actions.push(motd_action);
            }

            // Any user-provided login actions must come after our system ones
            login_actions.extend(args.login_actions);

            if let Some(config) = supervisor {
                let disk_lock = ssh_runtime::acquire_disk_lock(&disk_path)?;
                vm_registry::record_vm_launch(&cache_dir, &project_root)?;
                let (control_tx, control_rx) = mpsc::channel();
                let runtime = ssh_runtime::SupervisorRuntime::start(
                    &cache_dir,
                    &project_root,
                    config.host_port,
                    config.startup_token.clone(),
                    config.forwards.clone(),
                    control_tx,
                )?;
                let result = run_vm(
                    &disk_path,
                    Some(instance_dir.as_path()),
                    &login_actions,
                    &directory_shares,
                    prepare_network_backend,
                    args.cpu_count,
                    args.ram_bytes,
                    VmIoMode::Headless,
                    Some(control_rx),
                    Some(config.startup_token),
                    DiskLockMode::Held(disk_lock),
                    |status| runtime.set_status(status),
                )
                .map(|_| ());
                if result.is_err() {
                    let _ = runtime.set_status("failed");
                }
                result
            } else {
                let disk_lock = ssh_runtime::acquire_disk_lock(&disk_path)?;
                vm_registry::record_vm_launch(&cache_dir, &project_root)?;
                run_vm(
                    &disk_path,
                    log_to_instance.then_some(instance_dir.as_path()),
                    &login_actions,
                    &directory_shares,
                    prepare_network_backend,
                    args.cpu_count,
                    args.ram_bytes,
                    VmIoMode::InteractiveConsole,
                    None,
                    None,
                    DiskLockMode::Held(disk_lock),
                    |_| Ok(()),
                )
                .map(|_| ())
            }
        }
        CliCommand::Ssh(_) => unreachable!(),
    }
}

struct CliArgs {
    command: CliCommand,
    version: bool,
    help: bool,
    no_default_mounts: bool,
    env: HashMap<String, String>,
    mounts: Vec<String>,
    login_actions: Vec<LoginAction>,
    network_mode: NetworkMode,
    forwards: Vec<PortForward>,
    cpu_count: usize,
    ram_bytes: u64,
}

enum CliCommand {
    Ssh(SshCommand),
    List,
    Run {
        disk: Option<PathBuf>,
        image: String,
        supervisor: Option<SupervisorArgs>,
    },
    Provision {
        base: Option<String>,
        image: String,
        replace: bool,
        scripts: Vec<ProvisionScript>,
        cpu_count: usize,
        ram_bytes: u64,
    },
}

enum SshCommand {
    Connect(Vec<PortForward>),
    List,
    Stop(String),
    StopAll,
}

struct SupervisorArgs {
    project_root: PathBuf,
    host_port: u16,
    startup_token: String,
    forwards: Vec<PortForward>,
}

fn parse_cli() -> Result<CliArgs, Box<dyn std::error::Error>> {
    fn os_to_string(value: OsString, flag: &str) -> Result<String, Box<dyn std::error::Error>> {
        value
            .into_string()
            .map_err(|_| format!("{flag} expects valid UTF-8").into())
    }

    fn parse_ram_size(
        parser: &mut lexopt::Parser,
    ) -> Result<u64, Box<dyn std::error::Error + 'static>> {
        let value: u64 = os_to_string(parser.value()?, "--ram")?.parse()?;
        if value == 0 {
            return Err("--ram must be >= 1".into());
        }
        Ok(value * BYTES_PER_MB)
    }

    fn parse_cpu_count(
        parser: &mut lexopt::Parser,
    ) -> Result<usize, Box<dyn std::error::Error + 'static>> {
        let value: usize = os_to_string(parser.value()?, "--cpus")?.parse()?;
        if value == 0 {
            return Err("--cpus must be >= 1".into());
        }
        Ok(value)
    }

    fn parse_provision_command(
        parser: &mut lexopt::Parser,
    ) -> Result<CliCommand, Box<dyn std::error::Error>> {
        let mut base = None;
        let mut image = DEFAULT_IMAGE_NAME.to_string();
        let mut image_seen = false;
        let mut replace = false;
        let mut scripts = Vec::new();
        let mut cpu_count = DEFAULT_CPU_COUNT;
        let mut ram_bytes = DEFAULT_RAM_BYTES;

        while let Some(arg) = parser.next()? {
            match arg {
                Long("base") => {
                    if base.is_some() {
                        return Err("Duplicate --base".into());
                    }
                    base = Some(os_to_string(parser.value()?, "--base")?);
                }
                Long("image") => {
                    if image_seen {
                        return Err("Duplicate --image".into());
                    }
                    image_seen = true;
                    image = os_to_string(parser.value()?, "--image")?;
                    assert_valid_image_name(&image);
                }
                Long("replace") => replace = true,
                Long("cpus") => cpu_count = parse_cpu_count(parser)?,
                Long("ram") => ram_bytes = parse_ram_size(parser)?,
                Value(value) => {
                    let name = os_to_string(value, "provisioning script")?;
                    let script = if let Some(name) = name.strip_prefix('@') {
                        BUILTIN_PROVISION_SCRIPTS
                            .iter()
                            .find(|script| script.name == name)
                            .cloned()
                            .ok_or_else(|| -> Box<dyn std::error::Error> {
                                format!(
                                    "Unknown built-in provisioning script: @{name}\n\n{}",
                                    provisioning_scripts_banner()
                                )
                                .into()
                            })?
                    } else {
                        ProvisionScript {
                            content: Cow::Owned(fs::read_to_string(&name).map_err(|err| {
                                format!("Failed to read provisioning script {name}: {err}")
                            })?),
                            name: Cow::Owned(name),
                            description: Cow::Borrowed(""),
                        }
                    };
                    scripts.push(script);
                }
                _ => return Err(arg.unexpected().into()),
            }
        }

        Ok(CliCommand::Provision {
            base,
            image,
            replace,
            scripts,
            cpu_count,
            ram_bytes,
        })
    }

    fn parse_ssh_command(
        parser: &mut lexopt::Parser,
    ) -> Result<CliCommand, Box<dyn std::error::Error>> {
        let mut command = SshCommand::Connect(Vec::new());
        while let Some(arg) = parser.next()? {
            match arg {
                Long("forward") if matches!(command, SshCommand::Connect(_)) => {
                    let value = os_to_string(parser.value()?, "--forward")?;
                    let forward = PortForward::parse(&value)?;
                    let SshCommand::Connect(forwards) = &mut command else {
                        unreachable!()
                    };
                    if forwards
                        .iter()
                        .any(|existing| existing.host_port == forward.host_port)
                    {
                        return Err(format!(
                            "Duplicate --forward host port: {}",
                            forward.host_port
                        )
                        .into());
                    }
                    forwards.push(forward);
                }
                Long("list") if matches!(&command, SshCommand::Connect(forwards) if forwards.is_empty()) => {
                    command = SshCommand::List
                }
                Long("stop") if matches!(&command, SshCommand::Connect(forwards) if forwards.is_empty()) =>
                {
                    let id = os_to_string(parser.value()?, "--stop")?;
                    if id == "all" {
                        command = SshCommand::StopAll;
                    } else {
                        if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
                            return Err(
                                "vibe ssh --stop requires a numeric ID from `vibe ssh --list` or `all`"
                                    .into(),
                            );
                        }
                        command = SshCommand::Stop(id);
                    }
                }
                Long("list") | Long("stop") if matches!(command, SshCommand::Connect(_)) => {
                    return Err(
                        "vibe ssh --forward cannot be combined with --list or --stop".into(),
                    );
                }
                Long("list") | Long("stop") => {
                    return Err("vibe ssh --list and --stop are mutually exclusive".into());
                }
                _ => {
                    return Err(format!(
                        "vibe ssh does not accept VM boot options: {}",
                        arg.unexpected()
                    )
                    .into());
                }
            }
        }
        Ok(CliCommand::Ssh(command))
    }

    fn parse_supervisor_command(
        parser: &mut lexopt::Parser,
    ) -> Result<CliCommand, Box<dyn std::error::Error>> {
        let mut project_root = None;
        let mut host_port = None;
        let mut startup_token = None;
        let mut forwards = Vec::new();
        while let Some(arg) = parser.next()? {
            match arg {
                Long("project-root") => project_root = Some(PathBuf::from(parser.value()?)),
                Long("host-port") => {
                    host_port = Some(os_to_string(parser.value()?, "--host-port")?.parse::<u16>()?)
                }
                Long("startup-token") => {
                    startup_token = Some(os_to_string(parser.value()?, "--startup-token")?)
                }
                Long("forward") => {
                    let value = os_to_string(parser.value()?, "--forward")?;
                    let forward = PortForward::parse(&value)?;
                    if forwards
                        .iter()
                        .any(|existing: &PortForward| existing.host_port == forward.host_port)
                    {
                        return Err(format!(
                            "Duplicate --forward host port: {}",
                            forward.host_port
                        )
                        .into());
                    }
                    forwards.push(forward);
                }
                _ => return Err(arg.unexpected().into()),
            }
        }
        let host_port = host_port.ok_or("Missing internal --host-port")?;
        if forwards
            .iter()
            .any(|forward| forward.host_port == host_port)
        {
            return Err(format!("--forward host port {host_port} conflicts with SSH port").into());
        }
        Ok(CliCommand::Run {
            disk: None,
            image: DEFAULT_IMAGE_NAME.into(),
            supervisor: Some(SupervisorArgs {
                project_root: project_root
                    .ok_or("Missing internal --project-root")?
                    .canonicalize()?,
                host_port,
                startup_token: startup_token.ok_or("Missing internal --startup-token")?,
                forwards,
            }),
        })
    }

    let mut parser = lexopt::Parser::from_env();
    let mut disk = None;
    let mut command = None;
    let mut image = DEFAULT_IMAGE_NAME.to_string();
    let mut image_seen = false;
    let mut version = false;
    let mut help = false;
    let mut no_default_mounts = false;
    let mut env_vars = HashMap::new();
    let mut mounts = Vec::new();
    let mut login_actions = Vec::new();
    let mut network_mode = NetworkMode::Nat;
    let mut forwards = Vec::new();
    let mut cpu_count = DEFAULT_CPU_COUNT;
    let mut ram_bytes = DEFAULT_RAM_BYTES;

    while let Some(arg) = parser.next()? {
        match arg {
            Long("version") => version = true,
            Long("help") | Short('h') => help = true,
            Long("image") => {
                if image_seen {
                    return Err("Duplicate --image".into());
                }
                image_seen = true;
                image = os_to_string(parser.value()?, "--image")?;
                assert_valid_image_name(&image);
            }
            Long("no-default-mounts") => no_default_mounts = true,
            Long("env") => {
                let name = os_to_string(parser.value()?, "--env")?;
                assert!(
                    !env_vars.contains_key(&name),
                    "Duplicate --env value: {name}"
                );
                let value = env::var(&name).unwrap_or_else(|_| {
                    panic!("--env {name} is not set or is not valid UTF-8 in the host environment")
                });
                assert!(
                    !value.is_empty(),
                    "--env {name} is empty in the host environment"
                );
                env_vars.insert(name, value);
            }
            Long("cpus") => cpu_count = parse_cpu_count(&mut parser)?,
            Long("ram") => ram_bytes = parse_ram_size(&mut parser)?,
            Long("mount") => {
                mounts.push(os_to_string(parser.value()?, "--mount")?);
            }
            Long("network") => {
                let value = os_to_string(parser.value()?, "--network")?;
                network_mode = NetworkMode::parse(&value)?;
            }
            Long("forward") => {
                let value = os_to_string(parser.value()?, "--forward")?;
                let forward = PortForward::parse(&value)?;
                if forwards
                    .iter()
                    .any(|existing: &PortForward| existing.host_port == forward.host_port)
                {
                    return Err(
                        format!("Duplicate --forward host port: {}", forward.host_port).into(),
                    );
                }
                forwards.push(forward);
            }
            Long("script") => {
                let path = os_to_string(parser.value()?, "--script")?;
                let content = fs::read_to_string(&path)
                    .map_err(|err| format!("Failed to read script {path}: {err}"))?;
                login_actions.push(Script {
                    name: path,
                    content,
                });
            }
            Long("send") => {
                login_actions.push(Send(os_to_string(parser.value()?, "--send")?));
            }
            Long("expect") => {
                let text = os_to_string(parser.value()?, "--expect")?;
                let timeout = match parser.optional_value() {
                    Some(value) => Duration::from_secs(os_to_string(value, "--expect")?.parse()?),
                    None => DEFAULT_EXPECT_TIMEOUT,
                };
                login_actions.push(Expect { text, timeout });
            }
            Value(value) => {
                let value = os_to_string(value, "argument")?;
                if disk.is_none() && command.is_none() && value == "ssh" {
                    command = Some(parse_ssh_command(&mut parser)?);
                    break;
                }
                if disk.is_none() && command.is_none() && value == "__ssh-supervisor" {
                    command = Some(parse_supervisor_command(&mut parser)?);
                    network_mode = NetworkMode::Nat;
                    if let Some(CliCommand::Run {
                        supervisor: Some(config),
                        ..
                    }) = command.as_ref()
                    {
                        forwards.extend(config.forwards.iter().cloned());
                        forwards.push(PortForward {
                            host_port: config.host_port,
                            guest_port: 22,
                        });
                    }
                    break;
                }
                if disk.is_none() && command.is_none() && value == "provision" {
                    command = Some(parse_provision_command(&mut parser)?);
                    break;
                }
                if disk.is_none() && command.is_none() && value == "ls" {
                    command = Some(CliCommand::List);
                    continue;
                }
                if matches!(command.as_ref(), Some(CliCommand::List)) {
                    return Err("Disk path cannot be provided with command 'ls'".into());
                }
                if disk.is_some() {
                    return Err("Only one disk path may be provided".into());
                }
                disk = Some(PathBuf::from(value));
            }
            _ => return Err(arg.unexpected().into()),
        }
    }

    if matches!(command.as_ref(), Some(CliCommand::Ssh(_)))
        && (no_default_mounts
            || image_seen
            || !env_vars.is_empty()
            || !mounts.is_empty()
            || !login_actions.is_empty()
            || !forwards.is_empty()
            || network_mode != NetworkMode::Nat
            || cpu_count != DEFAULT_CPU_COUNT
            || ram_bytes != DEFAULT_RAM_BYTES)
    {
        return Err("SSH commands cannot be combined with VM boot options".into());
    }

    if !forwards.is_empty() && network_mode == NetworkMode::VzNat {
        return Err("--forward requires --network nat".into());
    }

    if !forwards.is_empty() && matches!(command.as_ref(), Some(CliCommand::Provision { .. })) {
        return Err(
            "--forward is only available when running a VM, not provisioning an image".into(),
        );
    }

    if matches!(command.as_ref(), Some(CliCommand::List))
        && (image_seen
            || no_default_mounts
            || !env_vars.is_empty()
            || !mounts.is_empty()
            || !login_actions.is_empty()
            || !forwards.is_empty()
            || network_mode != NetworkMode::Nat
            || cpu_count != DEFAULT_CPU_COUNT
            || ram_bytes != DEFAULT_RAM_BYTES)
    {
        return Err("Command 'ls' cannot be combined with VM boot options".into());
    }

    Ok(CliArgs {
        command: match command {
            Some(command) => command,
            None => CliCommand::Run {
                disk,
                image,
                supervisor: None,
            },
        },
        version,
        help,
        no_default_mounts,
        env: env_vars,
        mounts,
        login_actions,
        network_mode,
        forwards,
        cpu_count,
        ram_bytes,
    })
}

fn env_login_actions(env: &HashMap<String, String>) -> Vec<LoginAction> {
    env.iter()
        //leading space to keep env out of bash history; escape single quotes in value
        .map(|(name, value)| Send(format!(" export {name}='{}'", shell_single_quote(value))))
        .collect()
}

fn run_vm_registry_ls(cache_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    fs::create_dir_all(cache_dir)?;
    let records = vm_registry::list_vm_records(cache_dir)?;

    if records.is_empty() {
        println!("No tracked VMs in {}.", cache_dir.display());
        return Ok(());
    }

    println!("Tracked VMs:");
    println!("{:<4} {:<10} Folder", "ID", "Created At");
    println!("{:-<4} {:-<10} {:-<6}", "", "", "");
    for (idx, record) in records.iter().enumerate() {
        println!(
            "{:<4} {:<10} {}",
            idx + 1,
            record.created_at,
            record.folder_path
        );
    }

    println!();
    println!("Delete entries? Type indexes like '1 3', 'all'/'a', or press Enter to keep all:");
    print!("> ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim();
    if input.is_empty() {
        return Ok(());
    }

    let selected = if input.eq_ignore_ascii_case("all") || input.eq_ignore_ascii_case("a") {
        (0..records.len()).collect::<Vec<_>>()
    } else {
        parse_selection_indexes(input, records.len())?
    };

    let mut deleted_folders = Vec::with_capacity(selected.len());
    for idx in selected {
        let record = &records[idx];
        let project_dir = PathBuf::from(&record.folder_path);
        let vibe_dir = project_dir.join(".vibe");
        let instance_raw = vibe_dir.join("instance.raw");

        if !project_dir.exists() {
            println!(
                "Project folder missing, skipping filesystem delete: {}",
                project_dir.display()
            );
        } else if !project_dir.is_dir() {
            return Err(format!(
                "Refusing to operate on non-directory project path at {}",
                project_dir.display()
            )
            .into());
        } else if vibe_dir.is_dir() {
            if instance_raw.is_file() {
                // Keep the disk lock until deletion finishes so `vibe ls`
                // cannot remove a VM that is running or starting.
                let _disk_lock = match ssh_runtime::acquire_disk_lock(&instance_raw) {
                    Ok(lock) => lock,
                    Err(error) => {
                        println!(
                            "Skipping VM because its disk is in use: {} ({error})",
                            project_dir.display()
                        );
                        continue;
                    }
                };
                fs::remove_dir_all(&vibe_dir)?;
                println!("Deleted folder: {}", vibe_dir.display());
            } else {
                println!(
                    "Skipping folder delete (instance.raw missing): {}",
                    vibe_dir.display()
                );
            }
        } else if vibe_dir.exists() {
            return Err(format!(
                "Refusing to delete non-directory path at {}",
                vibe_dir.display()
            )
            .into());
        } else {
            println!("Folder already missing: {}", vibe_dir.display());
        }
        deleted_folders.push(record.folder_path.clone());
    }

    vm_registry::delete_vm_records(cache_dir, &deleted_folders)?;
    println!("Deleted {} registry entr(y/ies).", deleted_folders.len());
    Ok(())
}

fn parse_selection_indexes(
    input: &str,
    max: usize,
) -> Result<Vec<usize>, Box<dyn std::error::Error>> {
    let mut indexes = Vec::new();
    for token in input.split_whitespace() {
        let idx: usize = token
            .parse()
            .map_err(|_| format!("Invalid index '{}'", token))?;
        if idx == 0 || idx > max {
            return Err(format!("Index out of range: {}", idx).into());
        }
        let zero_based = idx - 1;
        if !indexes.contains(&zero_based) {
            indexes.push(zero_based);
        }
    }

    if indexes.is_empty() {
        return Err("No indexes were provided".into());
    }

    Ok(indexes)
}

fn shell_single_quote(value: &str) -> String {
    value.replace('\'', r"'\''")
}

fn ensure_vibe_ssh_identity(home: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let ssh_dir = home.join(".ssh");
    let private_key = ssh_dir.join("vibe_ed25519");
    let public_key = ssh_dir.join("vibe_ed25519.pub");

    match (private_key.is_file(), public_key.is_file()) {
        (true, true) => return read_vibe_ssh_public_key(&public_key),
        (true, false) | (false, true) => {
            return Err(format!(
                "Incomplete Vibe SSH identity: expected both {} and {}. Vibe will not replace either file automatically.",
                private_key.display(),
                public_key.display()
            )
            .into());
        }
        (false, false) => {}
    }

    if private_key.exists() || public_key.exists() {
        return Err(format!(
            "Vibe SSH identity paths must be regular files: {} and {}",
            private_key.display(),
            public_key.display()
        )
        .into());
    }

    if !Path::new("/usr/bin/ssh-keygen").is_file() {
        return Err("Required SSH key generator not found at /usr/bin/ssh-keygen".into());
    }
    if ssh_dir.exists() && !ssh_dir.is_dir() {
        return Err(format!(
            "SSH directory path is not a directory: {}",
            ssh_dir.display()
        )
        .into());
    }
    println!(
        "To use SSH, Vibe needs to create an Ed25519 key pair in {} ({} and {}).",
        ssh_dir.display(),
        private_key.display(),
        public_key.display()
    );
    print!("Continue? [y/N] ");
    io::stdout().flush()?;
    let mut response = String::new();
    io::stdin().read_line(&mut response)?;
    if !matches!(response.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        println!("Continuing without SSH support.");
        return Ok(String::new());
    }

    if !ssh_dir.exists() {
        fs::create_dir_all(&ssh_dir)?;
        fs::set_permissions(&ssh_dir, fs::Permissions::from_mode(0o700))?;
    }

    let status = Command::new("/usr/bin/ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "vibe ssh key", "-f"])
        .arg(&private_key)
        .status()?;
    if !status.success() {
        return Err("Failed to generate Vibe SSH identity".into());
    }
    fs::set_permissions(&private_key, fs::Permissions::from_mode(0o600))?;
    fs::set_permissions(&public_key, fs::Permissions::from_mode(0o644))?;

    read_vibe_ssh_public_key(&public_key)
}

fn require_vibe_ssh_identity(home: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let private_key = home.join(".ssh/vibe_ed25519");
    let public_key = home.join(".ssh/vibe_ed25519.pub");
    if !private_key.is_file() || !public_key.is_file() {
        return Err(format!(
            "vibe ssh requires an existing Vibe SSH identity at {} and {}. Existing images cannot be updated automatically; restore the matching key pair or recreate the default and project instance before running `vibe ssh`.",
            private_key.display(),
            public_key.display()
        )
        .into());
    }

    read_vibe_ssh_public_key(&public_key)
}

fn read_vibe_ssh_public_key(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let contents = fs::read_to_string(path)?;
    let line = contents.trim_end_matches(|character| character == '\r' || character == '\n');
    if line.is_empty() || line.contains('\r') || line.contains('\n') {
        return Err(format!(
            "Vibe SSH public key must contain exactly one key: {}",
            path.display()
        )
        .into());
    }

    let mut fields = line.split_whitespace();
    let key_type = fields.next();
    let key_data = fields.next();
    if key_type != Some("ssh-ed25519") || key_data.is_none() {
        return Err(format!(
            "Vibe SSH public key must be a valid Ed25519 key: {}",
            path.display()
        )
        .into());
    }
    let status = Command::new("/usr/bin/ssh-keygen")
        .args(["-l", "-f"])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(format!("Vibe SSH public key is invalid: {}", path.display()).into());
    }

    Ok(format!("ssh-ed25519 {} vibe ssh key", key_data.unwrap()))
}

fn script_command_and_status_marker(id: &str, script: &str) -> (String, String) {
    let marker = "VIBE_INSTALL_SCRIPT_EOF";
    let guest_dir = "/tmp/vibe-scripts";
    let guest_path = format!("{guest_dir}/{id}.sh");
    let status_marker = format!("VIBE_SCRIPT_STATUS_{id}");

    // Run the script with stdin from /dev/null so long-running tools spawned by
    // the script cannot consume queued wrapper lines before the parent shell
    // sees them. Print a leading newline before the marker so terminal echo and
    // control sequences cannot merge the echoed printf command with the marker
    // line that wait_for_line_after expects. Reactivate mise after each
    // successful script so later scripts can use newly installed commands.
    let command = format!(
        " mkdir -p {guest_dir}\n\
cat >{guest_path} <<'{marker}'\n\
{script}\n\
{marker}\n\
chmod +x {guest_path}\n\
{guest_path} </dev/null\n\
status=$?\n\
if [ \"$status\" -eq 0 ] && [ -x \"$HOME/.local/bin/mise\" ]; then\n\
eval \"$(\"$HOME/.local/bin/mise\" activate bash)\"\n\
fi\n\
printf '\\n%s\\n%s\\n' '{status_marker}' \"$status\""
    );

    (command, status_marker)
}

fn assert_valid_image_name(name: &str) {
    assert!(
        !name.is_empty()
            && name
                .chars()
                .all(|c| c == '-' || c == '_' || c.is_ascii_alphanumeric()),
        "Image name must be alphanumeric, dash, or underscore."
    );
}

fn image_path(cache_dir: &Path, name: &str) -> PathBuf {
    assert_valid_image_name(name);
    cache_dir.join(format!("{name}.raw"))
}

fn script_install_command_from_content(
    label: &str,
    script: &str,
    guest_path: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let marker = "VIBE_SCRIPT_EOF";
    if script.contains(marker) {
        return Err(
            format!("Script '{label}' contains marker '{marker}', cannot safely upload").into(),
        );
    }

    let command =
        format!("cat >{guest_path} <<'{marker}'\n{script}\n{marker}\nchmod +x {guest_path}");
    Ok(command)
}

fn motd_login_action(directory_shares: &[DirectoryShare]) -> Option<LoginAction> {
    if directory_shares.is_empty() {
        return Some(Send(" clear".into()));
    }

    let host_header = "Host";
    let guest_header = "Guest";
    let mode_header = "Mode";
    let mut host_width = host_header.len();
    let mut guest_width = guest_header.len();
    let mut mode_width = mode_header.len();
    let mut rows = Vec::with_capacity(directory_shares.len());

    for share in directory_shares {
        let host = share.host.to_string_lossy().into_owned();
        let guest = share.guest.to_string_lossy().into_owned();
        let mode = if share.read_only {
            "read-only"
        } else {
            "read-write"
        }
        .to_string();
        host_width = host_width.max(host.len());
        guest_width = guest_width.max(guest.len());
        mode_width = mode_width.max(mode.len());
        rows.push((host, guest, mode));
    }

    let mut output = String::new();
    output.push_str(
        "
░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░▒▓███████▓▒░░▒▓████████▓▒░ 
░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░        
 ░▒▓█▓▒▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░        
 ░▒▓█▓▒▒▓█▓▒░░▒▓█▓▒░▒▓███████▓▒░░▒▓██████▓▒░   
  ░▒▓█▓▓█▓▒░ ░▒▓█▓▒░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░        
  ░▒▓█▓▓█▓▒░ ░▒▓█▓▒░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░        
   ░▒▓██▓▒░  ░▒▓█▓▒░▒▓███████▓▒░░▒▓████████▓▒░

",
    );
    output.push_str(&format!(
        "{host_header:<host_width$}  {guest_header:<guest_width$}  {mode_header}\n"
    ));
    output.push_str(&format!(
        "{:-<host_width$}  {:-<guest_width$}  {:-<mode_width$}\n",
        "",
        "",
        "",
        host_width = host_width,
        guest_width = guest_width,
        mode_width = mode_width
    ));

    for (host, guest, mode) in rows {
        output.push_str(&format!(
            "{host:<host_width$}  {guest:<guest_width$}  {mode}\n"
        ));
    }

    let command = format!(" clear && cat <<'VIBE_MOTD'\n{output}\nVIBE_MOTD");
    Some(Send(command))
}

pub enum VmInput {
    Bytes(Vec<u8>),
    Shutdown,
}

enum VmOutput {
    LoginActionTimeout { action: String, timeout: Duration },
    LoginActionFailed { action: String, status: u8 },
    LoginActionsComplete,
}

#[derive(Default)]
pub struct OutputMonitor {
    buffer: Mutex<String>,
    condvar: Condvar,
    closed: AtomicBool,
}

impl OutputMonitor {
    fn push(&self, bytes: &[u8]) {
        self.buffer
            .lock()
            .unwrap()
            .push_str(&String::from_utf8_lossy(bytes));
        self.condvar.notify_all();
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
        self.condvar.notify_all();
    }

    fn wait_for_text(&self, needle: &str, timeout: Duration) -> bool {
        let mut found = false;
        let (_unused, _timeout_result) = self
            .condvar
            .wait_timeout_while(self.buffer.lock().unwrap(), timeout, |buf| {
                if let Some((_, remaining)) = buf.split_once(needle) {
                    *buf = remaining.to_string();
                    found = true;
                    false
                } else {
                    !self.closed.load(Ordering::Relaxed)
                }
            })
            .unwrap();

        found
    }

    fn wait_for_line_after(&self, marker: &str, timeout: Duration) -> Option<String> {
        let mut line = None;
        let (_unused, _timeout_result) = self
            .condvar
            .wait_timeout_while(self.buffer.lock().unwrap(), timeout, |buf| {
                let mut line_start = 0;
                let mut marker_seen = false;

                while let Some(line_end_offset) = buf[line_start..].find('\n') {
                    let line_end = line_start + line_end_offset;
                    let current_line = buf[line_start..line_end]
                        .strip_suffix('\r')
                        .unwrap_or(&buf[line_start..line_end]);

                    if marker_seen {
                        line = Some(current_line.to_string());
                        let remaining = buf[line_end + 1..].to_string();
                        *buf = remaining;
                        return false;
                    }

                    marker_seen = current_line == marker;
                    line_start = line_end + 1;
                }

                !self.closed.load(Ordering::Relaxed)
            })
            .unwrap();

        line
    }

    fn snapshot(&self) -> String {
        self.buffer.lock().unwrap().clone()
    }
}

fn ensure_base_image(
    base_raw: &Path,
    base_compressed: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if base_raw.exists() {
        return Ok(());
    }

    if !base_compressed.exists()
        || std::fs::metadata(base_compressed).map(|m| m.len())? < DEBIAN_COMPRESSED_SIZE_BYTES
    {
        println!("Downloading base image...");
        let status = Command::new("curl")
            .args([
                "--continue-at",
                "-",
                "--compressed",
                "--location",
                "--fail",
                "-o",
                &base_compressed.to_string_lossy(),
                DEBIAN_COMPRESSED_DISK_URL,
            ])
            .status()?;
        if !status.success() {
            return Err("Failed to download base image".into());
        }
    }

    // Check SHA
    {
        let input = format!("{}  {}\n", DEBIAN_COMPRESSED_SHA, base_compressed.display());

        let mut child = Command::new("/usr/bin/shasum")
            .args(["--algorithm", "512", "--check"])
            .stdin(Stdio::piped())
            .spawn()
            .expect("failed to spawn shasum");

        child
            .stdin
            .take()
            .expect("failed to open stdin")
            .write_all(input.as_bytes())
            .expect("failed to write to stdin");

        let status = child.wait().expect("failed to wait on child");
        if !status.success() {
            return Err(format!("SHA validation failed for {DEBIAN_COMPRESSED_DISK_URL}").into());
        }
    }

    println!("Decompressing base image...");
    let status = Command::new("tar")
        .args(["-xOf", &base_compressed.to_string_lossy(), "disk.raw"])
        .stdout(std::fs::File::create(base_raw)?)
        .status()?;

    if !status.success() {
        return Err("Failed to decompress base image".into());
    }

    Ok(())
}

fn ensure_default_image(
    base_raw: &Path,
    base_compressed: &Path,
    default_raw: &Path,
    home: &Path,
    directory_shares: &[DirectoryShare],
    prepare_network_backend: impl Fn(
        Option<&Path>,
    ) -> Result<PreparedNetworkBackend, Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    if default_raw.exists() {
        return Ok(());
    }

    let ssh_public_key = ensure_vibe_ssh_identity(home)?;
    ensure_base_image(base_raw, base_compressed)?;

    // Provision with everything, so folks who don't read README have a "it just works" experience.
    let mut default_provisioning_scripts: Vec<_> = BUILTIN_PROVISION_SCRIPTS
        .iter()
        .filter(|s| s.name != "base")
        .cloned()
        .collect();
    default_provisioning_scripts.push(ProvisionScript {
        name: Cow::Borrowed("aiexclude-mounts"),
        description: Cow::Borrowed("Install .aiexclude mount helper."),
        content: Cow::Owned(script_install_command_from_content(
            "aiexclude_mounts.sh",
            AIEXCLUDE_MOUNTS_SCRIPT,
            "/root/.aiexclude_mounts.sh",
        )?),
    });

    println!("Provisioning default VM with:");
    for s in &default_provisioning_scripts {
        println!("  @{}", &s.name);
    }
    println!("If you want a lighter default image, see `vibe provision`");

    provision_image(
        base_raw,
        default_raw,
        false,
        &default_provisioning_scripts[..],
        directory_shares,
        prepare_network_backend,
        DEFAULT_CPU_COUNT,
        DEFAULT_RAM_BYTES,
        &ssh_public_key,
    )
}

#[allow(clippy::too_many_arguments)]
fn provision_image(
    base_raw: &Path,
    image_raw: &Path,
    replace: bool,
    extra_scripts: &[ProvisionScript],
    directory_shares: &[DirectoryShare],
    prepare_network_backend: impl Fn(
        Option<&Path>,
    ) -> Result<PreparedNetworkBackend, Box<dyn std::error::Error>>,
    cpu_count: usize,
    ram_bytes: u64,
    ssh_public_key: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let image_name = image_raw
        .file_stem()
        .expect("image path has a file name")
        .to_string_lossy();
    if image_raw.exists() && !replace {
        return Err(format!(
            "Image '{image_name}' already exists at {}; pass --replace to overwrite it",
            image_raw.display()
        )
        .into());
    }

    let image_dir = image_raw
        .parent()
        .expect("image path has a parent directory");
    fs::create_dir_all(image_dir)?;

    let tmp_raw = image_dir.join(format!("{image_name}.tmp.{}", std::process::id()));
    let _ = fs::remove_file(&tmp_raw);

    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        println!("Provisioning image '{image_name}'");

        fs::copy(base_raw, &tmp_raw)?;

        let desired_size = DEFAULT_DISK_GB * 1024 * BYTES_PER_MB;
        let current_size = fs::metadata(&tmp_raw)?.len();
        if current_size < desired_size {
            fs::OpenOptions::new()
                .write(true)
                .open(&tmp_raw)?
                .set_len(desired_size)?;
        }

        let scripts: Vec<&ProvisionScript> = std::iter::once(
            BUILTIN_PROVISION_SCRIPTS
                .iter()
                .find(|script| script.name == "base")
                .expect("base provisioning script is bundled"),
        )
        .chain(extra_scripts.iter())
        .collect();

        let mut login_actions = Vec::new();

        let script_names = scripts
            .iter()
            .map(|script| script.name.as_ref())
            .collect::<Vec<_>>()
            .join("\n");

        login_actions.push(Send(format!(
            " export VIBE_PROVISION_IMAGE='{}'\n\
export VIBE_PROVISION_BASE='{}'\n\
export VIBE_GIT_SHA='{}'\n\
export VIBE_BUILD_DATE='{}'\n\
export VIBE_PROVISION_SCRIPTS='{}'\n\
export VIBE_SSH_PUBLIC_KEY='{}'",
            shell_single_quote(&image_name),
            shell_single_quote(&base_raw.to_string_lossy()),
            env!("GIT_SHA"),
            env!("BUILD_DATE"),
            shell_single_quote(&script_names),
            shell_single_quote(ssh_public_key)
        )));

        for script in scripts {
            login_actions.push(Script {
                name: script.name.to_string(),
                content: script.content.to_string(),
            });
        }

        login_actions.push(Send(format!(" echo {PROVISION_SUCCESS_MARKER}")));
        login_actions.push(Expect {
            text: PROVISION_SUCCESS_MARKER.to_string(),
            timeout: DEFAULT_EXPECT_TIMEOUT,
        });
        login_actions.push(Send(" systemctl poweroff; sleep 100".to_string()));

        run_vm(
            &tmp_raw,
            None,
            &login_actions,
            directory_shares,
            prepare_network_backend,
            cpu_count,
            ram_bytes,
            VmIoMode::InteractiveConsole,
            None,
            None,
            DiskLockMode::Skip,
            |_| Ok(()),
        )?;

        fs::rename(&tmp_raw, image_raw)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&tmp_raw);
    }

    result
}

fn ensure_instance_disk(
    instance_raw: &Path,
    template_raw: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if instance_raw.exists() {
        return Ok(());
    }

    println!("Creating instance disk from {}...", template_raw.display());
    let instance_dir = instance_raw.parent().unwrap();
    std::fs::create_dir_all(instance_dir)?;
    fs::write(instance_dir.join(".gitignore"), VIBE_GITIGNORE)?;
    fs::copy(template_raw, instance_raw)?;
    Ok(())
}

pub struct IoContext {
    pub input_tx: Sender<VmInput>,
    wakeup_write: OwnedFd,
    stdin_thread: thread::JoinHandle<()>,
    mux_thread: thread::JoinHandle<()>,
    resize_thread: thread::JoinHandle<()>,
    stdout_thread: thread::JoinHandle<()>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum VmIoMode {
    InteractiveConsole,
    Headless,
}

enum DiskLockMode {
    Held(ssh_runtime::FileLock),
    Skip,
}

#[must_use]
pub fn create_pipe() -> (OwnedFd, OwnedFd) {
    let (read_stream, write_stream) = UnixStream::pair().expect("Failed to create socket pair");
    (read_stream.into(), write_stream.into())
}

fn spawn_vm_io(
    output_monitor: Arc<OutputMonitor>,
    vm_output_fd: OwnedFd,
    vm_input_fd: OwnedFd,
    resize_control_fd: OwnedFd,
    mode: VmIoMode,
) -> IoContext {
    let (input_tx, input_rx): (Sender<VmInput>, Receiver<VmInput>) = mpsc::channel();

    // raw_guard is set when we've put the user's terminal into raw mode because we've attached stdin/stdout to the VM.
    let raw_guard = Arc::new(Mutex::new(None));

    let (wakeup_read, wakeup_write) = create_pipe();

    enum PollResult<'a> {
        Ready(&'a [u8]),
        Spurious,
        Shutdown,
        Error,
    }

    fn poll_with_wakeup(main_fd: RawFd, wakeup_fd: RawFd, buf: &mut [u8]) -> PollResult<'_> {
        let mut fds = [
            libc::pollfd {
                fd: main_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wakeup_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];

        let ret = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if ret <= 0 || fds[1].revents & libc::POLLIN != 0 {
            PollResult::Shutdown
        } else if fds[0].revents & libc::POLLIN != 0 {
            let n = unsafe { libc::read(main_fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                PollResult::Error
            } else if n == 0 {
                PollResult::Shutdown
            } else {
                PollResult::Ready(&buf[..(n as usize)])
            }
        } else {
            PollResult::Spurious
        }
    }

    // Copies from stdin to the VM; also polls wakeup_read to exit the thread when it's time to shutdown.
    let stdin_thread = thread::spawn({
        let input_tx = input_tx.clone();
        let raw_guard = raw_guard.clone();
        let wakeup_read = wakeup_read.try_clone().unwrap();

        move || {
            if mode == VmIoMode::Headless {
                return;
            }
            let mut buf = [0u8; 1024];
            loop {
                match poll_with_wakeup(libc::STDIN_FILENO, wakeup_read.as_raw_fd(), &mut buf) {
                    PollResult::Shutdown | PollResult::Error => break,
                    PollResult::Spurious => continue,
                    PollResult::Ready(bytes) => {
                        // discard input if the VM hasn't booted up yielded output yet (which triggers us entering raw_mode)
                        if raw_guard.lock().unwrap().is_none() {
                            continue;
                        }
                        if input_tx.send(VmInput::Bytes(bytes.to_vec())).is_err() {
                            break;
                        }
                    }
                }
            }
        }
    });

    // Copies VM output to stdout; also polls wakeup_read to exit the thread when it's time to shutdown.
    let stdout_thread = thread::spawn({
        let raw_guard = raw_guard.clone();
        let wakeup_read = wakeup_read.try_clone().unwrap();
        let output_monitor = output_monitor.clone();

        move || {
            let mut stdout = std::io::stdout().lock();
            let mut buf = [0u8; 1024];
            loop {
                match poll_with_wakeup(vm_output_fd.as_raw_fd(), wakeup_read.as_raw_fd(), &mut buf)
                {
                    PollResult::Shutdown | PollResult::Error => break,
                    PollResult::Spurious => continue,
                    PollResult::Ready(bytes) => {
                        if mode == VmIoMode::InteractiveConsole {
                            // enable raw mode, if we haven't already
                            let mut raw_guard_inner = raw_guard.lock().unwrap();
                            if raw_guard_inner.is_none()
                                && let Ok(guard) = enable_raw_mode(libc::STDIN_FILENO)
                            {
                                *raw_guard_inner = Some(guard);
                            }
                            if let Err(e) = stdout.write_all(bytes) {
                                eprintln!("[stdout_thread] write failed: {e:?}");
                                break;
                            }
                            let _ = stdout.flush();
                        }
                        output_monitor.push(bytes);
                    }
                }
            }
            output_monitor.close();
        }
    });

    // Copies data from mpsc channel into VM, so vibe can "type" stuff and run scripts.
    let mux_thread = thread::spawn(move || {
        let mut vm_writer = std::fs::File::from(vm_input_fd);
        loop {
            match input_rx.recv() {
                Ok(VmInput::Bytes(data)) => {
                    if let Err(e) = vm_writer.write_all(&data) {
                        eprintln!("[mux] write failed: {e:?}");
                        break;
                    }
                }
                Ok(VmInput::Shutdown) => break,
                Err(_) => break,
            }
        }
    });

    let resize_thread = thread::spawn({
        let wakeup_read = wakeup_read.try_clone().unwrap();
        move || {
            let mut writer = std::fs::File::from(resize_control_fd);
            if mode == VmIoMode::Headless {
                let _ = writer.write_all(b"24 80\n");
                return;
            }
            let resize_fd = writer.as_raw_fd();
            let flags = unsafe { libc::fcntl(resize_fd, libc::F_GETFL) };
            if flags >= 0 {
                let _ = unsafe { libc::fcntl(resize_fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
            }

            loop {
                let mut pollfd = libc::pollfd {
                    fd: wakeup_read.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                let poll_result = unsafe { libc::poll(&raw mut pollfd, 1, 200) };
                if poll_result > 0 && (pollfd.revents & libc::POLLIN) != 0 {
                    break;
                }

                if let Some((rows, cols)) = terminal_size(libc::STDOUT_FILENO) {
                    let message = format!("{rows} {cols}\n");
                    let bytes = message.as_bytes();
                    match writer.write(bytes) {
                        Ok(n) if n == bytes.len() => {}
                        Ok(_) => {}
                        Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
                        Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                        Err(err) => {
                            eprintln!("[resize_thread] write failed: {err:?}");
                            break;
                        }
                    }
                }
            }
        }
    });

    IoContext {
        input_tx,
        wakeup_write,
        stdin_thread,
        mux_thread,
        resize_thread,
        stdout_thread,
    }
}

impl IoContext {
    pub fn shutdown(self) {
        let _ = self.input_tx.send(VmInput::Shutdown);
        unsafe { libc::write(self.wakeup_write.as_raw_fd(), b"x".as_ptr().cast(), 1) };
        let _ = self.stdin_thread.join();
        let _ = self.stdout_thread.join();
        let _ = self.mux_thread.join();
        let _ = self.resize_thread.join();
    }
}

#[allow(clippy::too_many_arguments)]
fn create_vm_configuration(
    disk_path: &Path,
    directory_shares: &[DirectoryShare],
    network_backend: &mut PreparedNetworkBackend,
    vm_reads_from_fd: OwnedFd,
    vm_writes_to_fd: OwnedFd,
    resize_reads_from_fd: OwnedFd,
    cpu_count: usize,
    ram_bytes: u64,
) -> Result<Retained<VZVirtualMachineConfiguration>, Box<dyn std::error::Error>> {
    unsafe {
        let platform =
            VZGenericPlatformConfiguration::init(VZGenericPlatformConfiguration::alloc());

        let boot_loader = VZEFIBootLoader::init(VZEFIBootLoader::alloc());
        let variable_store = load_efi_variable_store()?;
        boot_loader.setVariableStore(Some(&variable_store));

        let config = VZVirtualMachineConfiguration::new();
        config.setPlatform(&platform);
        config.setBootLoader(Some(&boot_loader));
        config.setCPUCount(cpu_count as NSUInteger);
        config.setMemorySize(ram_bytes);

        config.setNetworkDevices(&NSArray::from_retained_slice(&[{
            let network_device = VZVirtioNetworkDeviceConfiguration::new();
            match network_backend {
                PreparedNetworkBackend::VzNat => {
                    network_device.setAttachment(Some(&VZNATNetworkDeviceAttachment::new()));
                }
                PreparedNetworkBackend::Usernet { vm_socket_fd, .. } => {
                    let mac_address = VZMACAddress::initWithString(
                        VZMACAddress::alloc(),
                        &NSString::from_str(USERNET_MAC_ADDRESS),
                    )
                    .ok_or_else(|| io::Error::other("invalid usernet MAC address"))?;
                    network_device.setMACAddress(&mac_address);

                    let network_fd = vm_socket_fd
                        .take()
                        .ok_or_else(|| io::Error::other("vibe-usernet socket already consumed"))?;
                    let file_handle = NSFileHandle::initWithFileDescriptor_closeOnDealloc(
                        NSFileHandle::alloc(),
                        network_fd.into_raw_fd(),
                        true,
                    );
                    let attachment = VZFileHandleNetworkDeviceAttachment::initWithFileHandle(
                        VZFileHandleNetworkDeviceAttachment::alloc(),
                        &file_handle,
                    );
                    network_device.setAttachment(Some(&attachment));
                }
            }
            Retained::into_super(network_device)
        }]));

        config.setEntropyDevices(&NSArray::from_retained_slice(&[Retained::into_super(
            VZVirtioEntropyDeviceConfiguration::new(),
        )]));

        ////////////////////////////
        // Disks
        {
            let disk_attachment = VZDiskImageStorageDeviceAttachment::initWithURL_readOnly_cachingMode_synchronizationMode_error(
                VZDiskImageStorageDeviceAttachment::alloc(),
                &nsurl_from_path(disk_path).unwrap(),
                false,
                VZDiskImageCachingMode::Cached,
                VZDiskImageSynchronizationMode::Full,
            ).unwrap();

            let disk_device = VZVirtioBlockDeviceConfiguration::initWithAttachment(
                VZVirtioBlockDeviceConfiguration::alloc(),
                &disk_attachment,
            );

            let storage_devices: Retained<NSArray<_>> =
                NSArray::from_retained_slice(&[Retained::into_super(disk_device)]);

            config.setStorageDevices(&storage_devices);
        };

        ////////////////////////////
        // Directory shares

        if !directory_shares.is_empty() {
            let directories: Retained<NSMutableDictionary<NSString, VZSharedDirectory>> =
                NSMutableDictionary::new();

            for share in directory_shares {
                assert!(
                    share.host.is_dir(),
                    "path does not exist or is not a directory: {:?}",
                    share.host
                );

                let url = nsurl_from_path(&share.host)?;
                let shared_directory = VZSharedDirectory::initWithURL_readOnly(
                    VZSharedDirectory::alloc(),
                    &url,
                    share.read_only,
                );

                let key = NSString::from_str(&share.tag());
                directories.setObject_forKey(&*shared_directory, ProtocolObject::from_ref(&*key));
            }

            let multi_share = VZMultipleDirectoryShare::initWithDirectories(
                VZMultipleDirectoryShare::alloc(),
                &directories,
            );

            let device = VZVirtioFileSystemDeviceConfiguration::initWithTag(
                VZVirtioFileSystemDeviceConfiguration::alloc(),
                &NSString::from_str(SHARED_DIRECTORIES_TAG),
            );
            device.setShare(Some(&multi_share));

            let share_devices = NSArray::from_retained_slice(&[device.into_super()]);
            config.setDirectorySharingDevices(&share_devices);
        }

        ////////////////////////////
        // Serial ports
        {
            let ns_read_handle = NSFileHandle::initWithFileDescriptor_closeOnDealloc(
                NSFileHandle::alloc(),
                vm_reads_from_fd.into_raw_fd(),
                true,
            );

            let ns_write_handle = NSFileHandle::initWithFileDescriptor_closeOnDealloc(
                NSFileHandle::alloc(),
                vm_writes_to_fd.into_raw_fd(),
                true,
            );

            let serial_attach =
                VZFileHandleSerialPortAttachment::initWithFileHandleForReading_fileHandleForWriting(
                    VZFileHandleSerialPortAttachment::alloc(),
                    Some(&ns_read_handle),
                    Some(&ns_write_handle),
                );
            let serial_port = VZVirtioConsoleDeviceSerialPortConfiguration::new();
            serial_port.setAttachment(Some(&serial_attach));

            let resize_read_handle = NSFileHandle::initWithFileDescriptor_closeOnDealloc(
                NSFileHandle::alloc(),
                resize_reads_from_fd.into_raw_fd(),
                true,
            );
            let resize_attach =
                VZFileHandleSerialPortAttachment::initWithFileHandleForReading_fileHandleForWriting(
                    VZFileHandleSerialPortAttachment::alloc(),
                    Some(&resize_read_handle),
                    None,
                );
            let resize_port = VZVirtioConsoleDeviceSerialPortConfiguration::new();
            resize_port.setAttachment(Some(&resize_attach));

            let serial_ports: Retained<NSArray<_>> = NSArray::from_retained_slice(&[
                Retained::into_super(serial_port),
                Retained::into_super(resize_port),
            ]);

            config.setSerialPorts(&serial_ports);
        }

        ////////////////////////////
        // Validate
        config.validateWithError().map_err(|e| {
            io::Error::other(format!(
                "Invalid VM configuration: {:?}",
                e.localizedDescription()
            ))
        })?;

        Ok(config)
    }
}

fn load_efi_variable_store() -> Result<Retained<VZEFIVariableStore>, Box<dyn std::error::Error>> {
    unsafe {
        let temp_dir = std::env::temp_dir();
        let temp_path = temp_dir.join(format!("efi_variable_store_{}.efivars", std::process::id()));
        let url = nsurl_from_path(&temp_path)?;
        let options = VZEFIVariableStoreInitializationOptions::AllowOverwrite;
        let store = VZEFIVariableStore::initCreatingVariableStoreAtURL_options_error(
            VZEFIVariableStore::alloc(),
            &url,
            options,
        )?;
        Ok(store)
    }
}

fn spawn_login_actions_thread(
    login_actions: Vec<LoginAction>,
    output_monitor: Arc<OutputMonitor>,
    input_tx: mpsc::Sender<VmInput>,
    vm_output_tx: mpsc::Sender<VmOutput>,
    marker_scope: Option<String>,
) -> thread::JoinHandle<()> {
    fn send_text(input_tx: &mpsc::Sender<VmInput>, mut text: String) {
        // Type the newline so the command is actually submitted.
        text.push('\n');
        // Send text to VM in chunks so that we don't overrun its input buffer.
        for chunk in text.as_bytes().chunks(LOGIN_ACTION_INPUT_CHUNK_BYTES) {
            input_tx
                .send(VmInput::Bytes(chunk.to_vec()))
                .expect("failed to send login action to VM input thread");
            thread::sleep(LOGIN_ACTION_INPUT_CHUNK_DELAY);
        }
    }

    thread::spawn(move || {
        for (index, a) in login_actions.into_iter().enumerate() {
            match a {
                Expect { text, timeout } => {
                    if !output_monitor.wait_for_text(&text, timeout) {
                        let _ = vm_output_tx.send(VmOutput::LoginActionTimeout {
                            action: format!("expect '{text}'"),
                            timeout,
                        });
                        return;
                    }
                }
                Send(text) => {
                    send_text(&input_tx, text);
                }
                Script { name, content } => {
                    let id = format!(
                        "{}_script_{index}",
                        marker_scope.as_deref().unwrap_or("interactive")
                    );
                    let (command, status_marker) = script_command_and_status_marker(&id, &content);
                    send_text(&input_tx, command);
                    match output_monitor.wait_for_line_after(&status_marker, SCRIPT_ACTION_TIMEOUT)
                    {
                        None => {
                            let _ = vm_output_tx.send(VmOutput::LoginActionTimeout {
                                action: format!("script '{name}'"),
                                timeout: SCRIPT_ACTION_TIMEOUT,
                            });
                            return;
                        }
                        Some(status) => {
                            let status = status.trim().parse().unwrap_or(u8::MAX);
                            if status != 0 {
                                let _ = vm_output_tx.send(VmOutput::LoginActionFailed {
                                    action: format!("script '{name}'"),
                                    status,
                                });
                                return;
                            }
                        }
                    }
                }
            }
        }
        let _ = vm_output_tx.send(VmOutput::LoginActionsComplete);
    })
}

fn run_vm(
    disk_path: &Path,
    network_log_dir: Option<&Path>,
    login_actions: &[LoginAction],
    directory_shares: &[DirectoryShare],
    prepare_network_backend: impl Fn(
        Option<&Path>,
    ) -> Result<PreparedNetworkBackend, Box<dyn std::error::Error>>,
    cpu_count: usize,
    ram_bytes: u64,
    io_mode: VmIoMode,
    control_rx: Option<Receiver<ssh_runtime::VmControl>>,
    marker_scope: Option<String>,
    disk_lock: DiskLockMode,
    mut state_changed: impl FnMut(&str) -> Result<(), Box<dyn std::error::Error>>,
) -> Result<String, Box<dyn std::error::Error>> {
    let _disk_lock = match disk_lock {
        DiskLockMode::Held(lock) => Some(lock),
        DiskLockMode::Skip => None,
    };
    let (vm_reads_from, we_write_to) = create_pipe();
    let (we_read_from, vm_writes_to) = create_pipe();
    let (resize_reads_from, we_write_resize_to) = create_pipe();
    let mut prepared_network_backend = prepare_network_backend(network_log_dir)?;
    let config = create_vm_configuration(
        disk_path,
        directory_shares,
        &mut prepared_network_backend,
        vm_reads_from,
        vm_writes_to,
        resize_reads_from,
        cpu_count,
        ram_bytes,
    )?;

    let queue = DispatchQueue::main();

    let vm = unsafe {
        VZVirtualMachine::initWithConfiguration_queue(VZVirtualMachine::alloc(), &config, queue)
    };

    let (tx, rx) = mpsc::channel::<Result<(), String>>();
    let completion_handler = RcBlock::new(move |error: *mut NSError| {
        if error.is_null() {
            let _ = tx.send(Ok(()));
        } else {
            let err = unsafe { &*error };
            let _ = tx.send(Err(format!("{:?}", err.localizedDescription())));
        }
    });

    unsafe {
        vm.startWithCompletionHandler(&completion_handler);
    }

    let start_deadline = Instant::now() + START_TIMEOUT;
    while Instant::now() < start_deadline {
        unsafe {
            NSRunLoop::mainRunLoop().runMode_beforeDate(
                NSDefaultRunLoopMode,
                &NSDate::dateWithTimeIntervalSinceNow(0.1),
            )
        };

        match rx.try_recv() {
            Ok(result) => {
                result.map_err(|e| format!("Failed to start VM: {e}"))?;
                break;
            }
            Err(mpsc::TryRecvError::Empty) => continue,
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err("VM start channel disconnected".into());
            }
        }
    }

    if Instant::now() >= start_deadline {
        return Err("Timed out waiting for VM to start".into());
    }

    println!("VM booting...");

    let output_monitor = Arc::new(OutputMonitor::default());
    let io_ctx = spawn_vm_io(
        output_monitor.clone(),
        we_read_from,
        we_write_to,
        we_write_resize_to,
        io_mode,
    );

    let mut all_login_actions = vec![
        Expect {
            text: "login: ".to_string(),
            timeout: LOGIN_EXPECT_TIMEOUT,
        },
        Send("root".to_string()),
        Expect {
            text: "~#".to_string(),
            timeout: LOGIN_EXPECT_TIMEOUT,
        },
        // Temporarily disable bash history and set commands starting with space to be ignored
        Send(" export HISTCONTROL=ignorespace".to_string()),
        Send(" unset HISTFILE".to_string()),
        // Our terminal is connected via /dev/hvc0 which Debian apparently keeps barebones.
        // We want sane terminal defaults like icrnl (translating carriage returns into newlines)
        Send(" stty -F /dev/hvc0 sane".to_string()),
        // In background, continuously read host terminal resizes sent over hvc1 and update hvc0.
        Send({
            // sorry for this nonsense, the string is so long it angers rustfmt =(
            const S: &str = " sh -c '(while IFS=\" \" read -r rows cols; do stty -F /dev/hvc0 rows \"$rows\" cols \"$cols\"; done) < /dev/hvc1 >/dev/null 2>&1 &'";
            S.to_string()
        }),
    ];

    if !directory_shares.is_empty() {
        all_login_actions.push(Send(" mkdir -p /mnt/shared".into()));
        all_login_actions.push(Send(format!(
            " mount -t virtiofs {SHARED_DIRECTORIES_TAG} /mnt/shared"
        )));

        for share in directory_shares {
            let staging = format!("/mnt/shared/{}", share.tag());
            let guest = share.guest.to_string_lossy();
            let quoted_staging = format!("'{}'", shell_single_quote(&staging));
            let quoted_guest = format!("'{}'", shell_single_quote(&guest));
            all_login_actions.push(Send(format!(" mkdir -p {quoted_guest}")));
            all_login_actions.push(Send(format!(
                " mount --bind {quoted_staging} {quoted_guest}"
            )));
        }
    }

    for a in login_actions {
        all_login_actions.push(a.clone());
    }

    if io_mode == VmIoMode::Headless {
        // The base image intentionally powers off from /root/.bash_logout so
        // exiting the foreground serial shell stops an ordinary `vibe` VM.
        // SSH logout must not do that. Mask the file for this boot only; an
        // explicit guest poweroff still stops the supervisor normally.
        let mut setup = String::from(
            "set -e\nif [ -f /root/.bash_logout ]; then mount --bind /dev/null /root/.bash_logout; fi\n",
        );
        for action in all_login_actions.drain(3..) {
            match action {
                Send(command) => {
                    setup.push_str(&command);
                    setup.push('\n');
                }
                _ => return Err("Headless SSH setup only supports tracked setup commands".into()),
            }
        }
        all_login_actions.push(Script {
            name: "SSH setup".into(),
            content: setup,
        });
    }

    let (vm_output_tx, vm_output_rx) = mpsc::channel::<VmOutput>();
    let login_actions_thread = spawn_login_actions_thread(
        all_login_actions,
        output_monitor.clone(),
        io_ctx.input_tx.clone(),
        vm_output_tx,
        marker_scope,
    );

    let mut last_state = None;
    let mut exit_result = Ok(());
    let request_stop = || unsafe {
        if vm.canRequestStop() {
            if let Err(err) = vm.requestStopWithError() {
                eprintln!("Failed to request VM stop: {err:?}");
            }
        } else if vm.canStop() {
            let handler = RcBlock::new(|_error: *mut NSError| {});
            vm.stopWithCompletionHandler(&handler);
        }
    };
    let mut stopping_since = None;
    let mut fallback_stop_requested = false;
    loop {
        unsafe {
            NSRunLoop::mainRunLoop().runMode_beforeDate(
                NSDefaultRunLoopMode,
                &NSDate::dateWithTimeIntervalSinceNow(0.2),
            )
        };

        let state = unsafe { vm.state() };
        if last_state != Some(state) {
            //eprintln!("[state] {:?}", state);
            last_state = Some(state);
        }
        match vm_output_rx.try_recv() {
            Ok(VmOutput::LoginActionTimeout { action, timeout }) => {
                exit_result = Err(format!(
                    "Login action ({action}) timed out after {timeout:?}; shutting down."
                ));
                request_stop();
                stopping_since = Some(Instant::now());
            }
            Ok(VmOutput::LoginActionFailed { action, status }) => {
                exit_result = Err(format!(
                    "Login action ({action}) failed with exit code {status}"
                ));
                request_stop();
                stopping_since = Some(Instant::now());
            }
            Ok(VmOutput::LoginActionsComplete) => {
                if io_mode == VmIoMode::Headless {
                    state_changed("running")?;
                }
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {}
        }
        if let Some(receiver) = &control_rx
            && matches!(receiver.try_recv(), Ok(ssh_runtime::VmControl::Stop))
            && stopping_since.is_none()
        {
            state_changed("stopping")?;
            request_stop();
            stopping_since = Some(Instant::now());
        }
        if stopping_since.is_some_and(|started| started.elapsed() >= Duration::from_secs(10))
            && unsafe { vm.canStop() }
            && !fallback_stop_requested
        {
            let handler = RcBlock::new(|_error: *mut NSError| {});
            unsafe {
                vm.stopWithCompletionHandler(&handler);
            }
            fallback_stop_requested = true;
        }
        if state != objc2_virtualization::VZVirtualMachineState::Running {
            //eprintln!("VM stopped with state: {:?}", state);
            break;
        }
    }

    output_monitor.close();
    let _ = login_actions_thread.join();

    io_ctx.shutdown();

    let output = output_monitor.snapshot();
    exit_result?;
    Ok(output)
}

fn nsurl_from_path(path: &Path) -> Result<Retained<NSURL>, Box<dyn std::error::Error>> {
    let abs_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()?.join(path)
    };
    let ns_path = NSString::from_str(
        abs_path
            .to_str()
            .ok_or("Non-UTF8 path encountered while building NSURL")?,
    );
    Ok(NSURL::fileURLWithPath(&ns_path))
}

fn terminal_size(fd: i32) -> Option<(u16, u16)> {
    let mut winsize: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut winsize) } != 0 {
        return None;
    }
    if winsize.ws_row == 0 || winsize.ws_col == 0 {
        return None;
    }
    Some((winsize.ws_row, winsize.ws_col))
}

fn enable_raw_mode(fd: i32) -> io::Result<RawModeGuard> {
    let mut attributes: libc::termios = unsafe { std::mem::zeroed() };

    if unsafe { libc::tcgetattr(fd, &raw mut attributes) } != 0 {
        return Err(io::Error::last_os_error());
    }

    let original = attributes;

    // Disable translation of carriage return to newline on input
    attributes.c_iflag &= !(libc::ICRNL);
    // Disable canonical mode (line buffering), echo, and signal generation
    attributes.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG);
    attributes.c_cc[libc::VMIN] = 0;
    attributes.c_cc[libc::VTIME] = 1;

    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw const attributes) } != 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(RawModeGuard { fd, original })
}

struct RawModeGuard {
    fd: i32,
    original: libc::termios,
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &raw const self.original);
        }
    }
}

// Ensure the running binary has com.apple.security.virtualization entitlements by checking and, if not, signing and relaunching.
pub fn ensure_signed() {
    let exe = std::env::current_exe().expect("failed to get current exe path");
    let exe_str = exe.to_str().expect("exe path not valid utf-8");

    let has_required_entitlements = {
        let output = Command::new("codesign")
            .args(["-d", "--entitlements", "-", "--xml", exe.to_str().unwrap()])
            .output();

        match output {
            Ok(o) if o.status.success() => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                stdout.contains("com.apple.security.virtualization")
            }
            _ => false,
        }
    };

    if has_required_entitlements {
        return;
    }

    const ENTITLEMENTS: &str = include_str!("entitlements.plist");
    let entitlements_path = std::env::temp_dir().join("entitlements.plist");
    std::fs::write(&entitlements_path, ENTITLEMENTS).expect("failed to write entitlements");

    let status = Command::new("codesign")
        .args([
            "--sign",
            "-",
            "--force",
            "--entitlements",
            entitlements_path.to_str().unwrap(),
            exe_str,
        ])
        .status();

    let _ = std::fs::remove_file(&entitlements_path);

    match status {
        Ok(s) if s.success() => {
            let err = Command::new(&exe).args(std::env::args_os().skip(1)).exec();
            eprintln!("failed to re-exec after signing: {err}");
            std::process::exit(1);
        }
        Ok(s) => {
            eprintln!("codesign failed with status: {s}");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("failed to run codesign: {e}");
            std::process::exit(1);
        }
    }
}
