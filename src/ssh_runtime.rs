use crate::networking::PortForward;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    net::TcpListener,
    os::{
        fd::AsRawFd,
        unix::{
            fs::{OpenOptionsExt, PermissionsExt},
            net::{UnixListener, UnixStream},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const PROTOCOL_VERSION: u32 = 1;
pub const SCHEMA_VERSION: u32 = 1;
pub const SSH_BASE_PORT: u16 = 2222;
const CONTROL_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeRecord {
    pub schema_version: u32,
    pub id: String,
    pub startup_token: String,
    pub pid: u32,
    pub status: String,
    pub project_name: String,
    pub project_root: String,
    pub disk_path: String,
    pub host_port: u16,
    pub guest_port: u16,
    #[serde(default)]
    pub forwards: Vec<PortForward>,
    pub control_socket: String,
    pub started_at_unix_seconds: u64,
}

#[derive(Serialize, Deserialize)]
struct ControlRequest {
    protocol_version: u32,
    command: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ControlResponse {
    protocol_version: u32,
    ok: bool,
    #[serde(default)]
    id: String,
    #[serde(default)]
    startup_token: String,
    #[serde(default)]
    project_root: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    host_port: u16,
    #[serde(default)]
    error: String,
}

pub struct FileLock {
    _file: File,
}

impl FileLock {
    pub fn acquire(path: &Path, nonblocking: bool) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            create_private_dir(parent)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .open(path)?;
        let operation = libc::LOCK_EX | if nonblocking { libc::LOCK_NB } else { 0 };
        if unsafe { libc::flock(file.as_raw_fd(), operation) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { _file: file })
    }
}

pub fn acquire_start_lock(cache_dir: &Path) -> io::Result<FileLock> {
    FileLock::acquire(&cache_dir.join("ssh-start.lock"), false)
}

pub fn acquire_disk_lock(disk_path: &Path) -> Result<FileLock, Box<dyn std::error::Error>> {
    let name = disk_path
        .file_name()
        .ok_or("Disk path has no file name")?
        .to_string_lossy();
    let lock_path = disk_path.with_file_name(format!("{name}.lock"));
    FileLock::acquire(&lock_path, true).map_err(|err| {
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) || err.raw_os_error() == Some(libc::EAGAIN) {
            if let Some(record) = live_record_for_disk(disk_path) {
                format!("VM disk is already in use by SSH-managed VM {}. Reconnect with `vibe ssh` or stop it with `vibe ssh --stop {}`.", record.id, record.id).into()
            } else {
                format!("VM disk is already in use: {}. Stop the foreground VM before starting another process for this project.", disk_path.display()).into()
            }
        } else { err.into() }
    })
}

fn live_record_for_disk(disk_path: &Path) -> Option<RuntimeRecord> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".cache"))
        .join("vibe");
    read_records(&cache)
        .into_iter()
        .find(|record| Path::new(&record.disk_path) == disk_path && is_live(record))
}

pub fn running_dir(cache_dir: &Path) -> PathBuf {
    cache_dir.join("running")
}
pub fn record_path(cache_dir: &Path, id: &str) -> PathBuf {
    running_dir(cache_dir).join(format!("{id}.json"))
}
pub fn socket_path(cache_dir: &Path, id: &str) -> PathBuf {
    running_dir(cache_dir).join(format!("{id}.sock"))
}

pub fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

pub fn write_record(
    cache_dir: &Path,
    record: &RuntimeRecord,
) -> Result<(), Box<dyn std::error::Error>> {
    create_private_dir(&running_dir(cache_dir))?;
    let path = record_path(cache_dir, &record.id);
    let temp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        record.startup_token
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)?;
    let result = (|| {
        serde_json::to_writer(&mut file, record)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temp, &path)?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

fn read_records(cache_dir: &Path) -> Vec<RuntimeRecord> {
    let Ok(entries) = fs::read_dir(running_dir(cache_dir)) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            if entry.path().extension().and_then(|x| x.to_str()) != Some("json") {
                return None;
            }
            serde_json::from_slice::<RuntimeRecord>(&fs::read(entry.path()).ok()?).ok()
        })
        .filter(|r| r.schema_version == SCHEMA_VERSION)
        .collect()
}

fn request(record: &RuntimeRecord, command: &str) -> io::Result<ControlResponse> {
    let stream = UnixStream::connect(&record.control_socket)?;
    stream.set_read_timeout(Some(CONTROL_TIMEOUT))?;
    stream.set_write_timeout(Some(CONTROL_TIMEOUT))?;
    let mut writer = &stream;
    serde_json::to_writer(
        &mut writer,
        &ControlRequest {
            protocol_version: PROTOCOL_VERSION,
            command: command.into(),
        },
    )
    .map_err(io::Error::other)?;
    writer.write_all(b"\n")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    serde_json::from_str(&line).map_err(io::Error::other)
}

fn is_live(record: &RuntimeRecord) -> bool {
    request(record, "status").is_ok_and(|response| {
        response.ok
            && response.protocol_version == PROTOCOL_VERSION
            && response.id == record.id
            && response.startup_token == record.startup_token
            && response.project_root == record.project_root
    })
}

pub fn cleanup_and_list(
    cache_dir: &Path,
) -> Result<Vec<RuntimeRecord>, Box<dyn std::error::Error>> {
    create_private_dir(&running_dir(cache_dir))?;
    let mut live = Vec::new();
    let records = read_records(cache_dir);
    for record in records {
        if is_live(&record) {
            live.push(record);
        } else {
            let _ = fs::remove_file(record_path(cache_dir, &record.id));
            let _ = fs::remove_file(&record.control_socket);
        }
    }
    for entry in fs::read_dir(running_dir(cache_dir))?.flatten() {
        let path = entry.path();
        match path.extension().and_then(|x| x.to_str()) {
            Some("json")
                if fs::read(&path)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<RuntimeRecord>(&b).ok())
                    .is_none() =>
            {
                let _ = fs::remove_file(path);
            }
            Some("sock") if !live.iter().any(|r| Path::new(&r.control_socket) == path) => {
                let _ = fs::remove_file(path);
            }
            _ => {}
        }
    }
    live.sort_by_key(|r| (r.host_port, r.id.clone()));
    Ok(live)
}

pub fn random_token() -> io::Result<String> {
    let mut bytes = [0u8; 24];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

use std::io::Read;

pub fn allocate_port(
    live: &[RuntimeRecord],
    forwards: &[PortForward],
) -> Result<u16, Box<dyn std::error::Error>> {
    for port in SSH_BASE_PORT..=u16::MAX {
        if live.iter().any(|r| r.host_port == port) {
            continue;
        }
        if forwards.iter().any(|forward| forward.host_port == port) {
            continue;
        }
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return Ok(port);
        }
    }
    Err("No loopback SSH port is available from 2222 through 65535".into())
}

pub fn spawn_supervisor(
    cache_dir: &Path,
    project_root: &Path,
    port: u16,
    token: &str,
    forwards: &[PortForward],
) -> Result<(String, PathBuf), Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?;
    let instance_dir = project_root.join(".vibe");
    create_private_dir(&instance_dir)?;
    let log_path = instance_dir.join("vibe-ssh-supervisor.log");
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&log_path)?;
    let stderr = stdout.try_clone()?;
    let mut command = Command::new(executable);
    command
        .args(["__ssh-supervisor", "--project-root"])
        .arg(project_root)
        .args(["--host-port", &port.to_string(), "--startup-token", token]);
    for forward in forwards {
        command
            .arg(if forward.all_interfaces {
                "--forward-all"
            } else {
                "--forward"
            })
            .arg(format!("{}:{}", forward.host_port, forward.guest_port));
    }
    command.stdin(Stdio::null()).stdout(stdout).stderr(stderr);
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = command.spawn()?;
    let id = child.id().to_string();
    let deadline = Instant::now() + super::START_TIMEOUT + super::LOGIN_EXPECT_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            let details = fs::read_to_string(&log_path).ok().and_then(|text| {
                text.lines()
                    .rev()
                    .find(|line| !line.trim().is_empty())
                    .map(str::to_owned)
            });
            return Err(format!(
                "SSH VM supervisor exited during startup ({status}){}; see {}",
                details.map(|line| format!(": {line}")).unwrap_or_default(),
                log_path.display()
            )
            .into());
        }
        if let Ok(bytes) = fs::read(record_path(cache_dir, &id))
            && let Ok(record) = serde_json::from_slice::<RuntimeRecord>(&bytes)
            && record.startup_token == token
        {
            if record.status == "running" {
                return Ok((id, log_path));
            }
            if record.status == "failed" {
                return Err(format!("SSH VM startup failed; see {}", log_path.display()).into());
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "Timed out waiting for SSH VM startup; see {}",
                log_path.display()
            )
            .into());
        }
        thread::sleep(Duration::from_millis(200));
    }
}

pub fn list_command(cache_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let _lock = acquire_start_lock(cache_dir)?;
    let records = cleanup_and_list(cache_dir)?;
    if records.is_empty() {
        println!("No running SSH-managed VMs.");
        return Ok(());
    }
    println!(
        "{:<8} {:<10} {:<6} {:<18} FOLDER",
        "ID", "STATUS", "PORT", "NAME"
    );
    for r in records {
        println!(
            "{:<8} {:<10} {:<6} {:<18} {}",
            r.id, r.status, r.host_port, r.project_name, r.project_root
        );
    }
    Ok(())
}

pub fn stop_command(cache_dir: &Path, id: &str) -> Result<(), Box<dyn std::error::Error>> {
    let _lock = acquire_start_lock(cache_dir)?;
    let records = cleanup_and_list(cache_dir)?;
    let record = records
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| format!("No running SSH-managed VM with ID {id}"))?;
    println!("Stopping {} ({})...", record.project_name, record.id);
    let response = request(&record, "stop")?;
    if !response.ok {
        return Err(response.error.into());
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if !is_live(&record) && !record_path(cache_dir, id).exists() {
            println!("stopped.");
            return Ok(());
        }
        thread::sleep(Duration::from_millis(200));
    }
    Err(format!(
        "Timed out stopping VM {id}; see {}/.vibe/vibe-ssh-supervisor.log",
        record.project_root
    )
    .into())
}

pub fn stop_all_command(cache_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let _lock = acquire_start_lock(cache_dir)?;
    let records = cleanup_and_list(cache_dir)?;
    if records.is_empty() {
        println!("No running SSH-managed VMs.");
        return Ok(());
    }

    let total = records.len();
    let mut stopping = Vec::new();
    let mut errors = Vec::new();
    for record in records {
        println!("Stopping {} ({})...", record.project_name, record.id);
        match request(&record, "stop") {
            Ok(response) if response.ok => stopping.push(record),
            Ok(response) => errors.push(format!(
                "Failed to stop VM {}: {}",
                record.id, response.error
            )),
            Err(error) => errors.push(format!("Failed to stop VM {}: {error}", record.id)),
        }
    }

    let deadline = Instant::now() + Duration::from_secs(30);
    while !stopping.is_empty() && Instant::now() < deadline {
        stopping.retain(|record| is_live(record) || record_path(cache_dir, &record.id).exists());
        if !stopping.is_empty() {
            thread::sleep(Duration::from_millis(200));
        }
    }
    for record in stopping {
        errors.push(format!(
            "Timed out stopping VM {}; see {}/.vibe/vibe-ssh-supervisor.log",
            record.id, record.project_root
        ));
    }

    if errors.is_empty() {
        println!("Stopped {total} SSH-managed VM(s).");
        Ok(())
    } else {
        Err(errors.join("\n").into())
    }
}

pub fn connect_command(
    cache_dir: &Path,
    home: &Path,
    project_root: &Path,
    forwards: &[PortForward],
) -> Result<(), Box<dyn std::error::Error>> {
    let identity = home.join(".ssh/vibe_ed25519");
    if !identity.is_file() {
        return Err(format!(
            "SSH identity is missing or not a regular file: {}. Recreate the image after creating the Vibe SSH identity.",
            identity.display()
        )
        .into());
    }
    if !Path::new("/usr/bin/ssh").is_file() {
        return Err("Required SSH client not found at /usr/bin/ssh".into());
    }
    let canonical = project_root.canonicalize()?;
    let (record, log_path) = {
        let _lock = acquire_start_lock(cache_dir)?;
        let live = cleanup_and_list(cache_dir)?;
        if let Some(record) = live
            .iter()
            .find(|r| Path::new(&r.project_root) == canonical)
            .cloned()
        {
            if !forwards.is_empty()
                && (forwards.len() != record.forwards.len()
                    || forwards
                        .iter()
                        .any(|forward| !record.forwards.contains(forward)))
            {
                return Err(format!(
                    "SSH-managed VM {} is already running with different port forwards. Stop it with `vibe ssh --stop {}` before changing --forward values.",
                    record.id, record.id
                )
                .into());
            }
            (record, canonical.join(".vibe/vibe-ssh-supervisor.log"))
        } else {
            let instance_raw = canonical
                .join(super::INSTANCE_DIR_NAME)
                .join(super::INSTANCE_DISK_IMAGE_NAME);
            let default_raw = cache_dir.join(format!("{}.raw", super::DEFAULT_IMAGE_NAME));
            super::ensure_instance_disk(&instance_raw, &default_raw)?;
            let port = allocate_port(&live, forwards)?;
            let token = random_token()?;
            println!(
                "Starting SSH VM for {} on 127.0.0.1:{port}...",
                canonical.display()
            );
            let (id, log) = spawn_supervisor(cache_dir, &canonical, port, &token, forwards)?;
            let record = cleanup_and_list(cache_dir)?
                .into_iter()
                .find(|r| r.id == id)
                .ok_or("Supervisor became unavailable after startup")?;
            (record, log)
        }
    };
    println!("Waiting for SSH...");
    let deadline = Instant::now() + super::START_TIMEOUT;
    while Instant::now() < deadline {
        let status = Command::new("/usr/bin/ssh")
            .args(ssh_common_args(&identity, record.host_port))
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=2",
                "root@127.0.0.1",
                "true",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if status.is_ok_and(|s| s.success()) {
            break;
        }
        if !is_live(&record) {
            return Err(format!(
                "SSH VM stopped before authentication succeeded; see {}",
                log_path.display()
            )
            .into());
        }
        thread::sleep(Duration::from_millis(500));
    }
    if Instant::now() >= deadline {
        return Err(format!(
            "SSH did not become ready. Existing images are not updated automatically; recreate this image if its authorized key does not match {}.",
            identity.display()
        )
        .into());
    }
    println!("Connected to {} (ID {}).", record.project_name, record.id);
    let guest_path = format!("/root/{}", record.project_name);
    let remote = format!(
        "cd '{}' && exec \"${{SHELL:-/bin/bash}}\" -l",
        guest_path.replace('\'', "'\\''")
    );
    let status = Command::new("/usr/bin/ssh")
        .args(ssh_common_args(&identity, record.host_port))
        .args(["-t", "root@127.0.0.1", &remote])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        std::process::exit(status.code().unwrap_or(255))
    }
}

fn ssh_common_args(identity: &Path, port: u16) -> Vec<String> {
    vec![
        "-i".into(),
        identity.to_string_lossy().into_owned(),
        "-o".into(),
        "IdentitiesOnly=yes".into(),
        "-o".into(),
        "StrictHostKeyChecking=no".into(),
        "-o".into(),
        "UserKnownHostsFile=/dev/null".into(),
        "-o".into(),
        "LogLevel=ERROR".into(),
        "-p".into(),
        port.to_string(),
    ]
}

#[derive(Clone, Copy, Debug)]
pub enum VmControl {
    Stop,
}

pub struct SupervisorRuntime {
    cache_dir: PathBuf,
    pub record: Arc<Mutex<RuntimeRecord>>,
    shutdown: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl SupervisorRuntime {
    pub fn start(
        cache_dir: &Path,
        project_root: &Path,
        port: u16,
        token: String,
        forwards: Vec<PortForward>,
        control_tx: Sender<VmControl>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        create_private_dir(&running_dir(cache_dir))?;
        let id = std::process::id().to_string();
        let socket = socket_path(cache_dir, &id);
        let _ = fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket)?;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let project_name = project_root
            .file_name()
            .ok_or("Project root has no basename")?
            .to_string_lossy()
            .into_owned();
        let record = RuntimeRecord {
            schema_version: SCHEMA_VERSION,
            id,
            startup_token: token,
            pid: std::process::id(),
            status: "starting".into(),
            project_name,
            project_root: project_root.to_string_lossy().into_owned(),
            disk_path: project_root
                .join(".vibe/instance.raw")
                .to_string_lossy()
                .into_owned(),
            host_port: port,
            guest_port: 22,
            forwards,
            control_socket: socket.to_string_lossy().into_owned(),
            started_at_unix_seconds: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        };
        write_record(cache_dir, &record)?;
        let shared = Arc::new(Mutex::new(record));
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread = thread::spawn({
            let shared = shared.clone();
            let shutdown = shutdown.clone();
            move || {
                while !shutdown.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => handle_connection(stream, &shared, &control_tx),
                        Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(50))
                        }
                        Err(_) => break,
                    }
                }
            }
        });
        Ok(Self {
            cache_dir: cache_dir.into(),
            record: shared,
            shutdown,
            thread: Some(thread),
        })
    }

    pub fn set_status(&self, status: &str) -> Result<(), Box<dyn std::error::Error>> {
        let mut record = self.record.lock().unwrap();
        record.status = status.into();
        write_record(&self.cache_dir, &record)
    }
}

fn handle_connection(
    mut stream: UnixStream,
    record: &Arc<Mutex<RuntimeRecord>>,
    control_tx: &Sender<VmControl>,
) {
    let _ = stream.set_read_timeout(Some(CONTROL_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CONTROL_TIMEOUT));
    let mut line = String::new();
    let parsed = BufReader::new(&stream)
        .read_line(&mut line)
        .ok()
        .and_then(|_| serde_json::from_str::<ControlRequest>(&line).ok());
    let current = record.lock().unwrap().clone();
    let response = match parsed {
        Some(r) if r.protocol_version != PROTOCOL_VERSION => ControlResponse {
            protocol_version: PROTOCOL_VERSION,
            ok: false,
            error: "unsupported protocol version".into(),
            id: String::new(),
            startup_token: String::new(),
            project_root: String::new(),
            status: String::new(),
            host_port: 0,
        },
        Some(r) if r.command == "status" => response_for(&current),
        Some(r) if r.command == "stop" => {
            let _ = control_tx.send(VmControl::Stop);
            let mut r = response_for(&current);
            r.status = "stopping".into();
            r
        }
        _ => ControlResponse {
            protocol_version: PROTOCOL_VERSION,
            ok: false,
            error: "unsupported command".into(),
            id: String::new(),
            startup_token: String::new(),
            project_root: String::new(),
            status: String::new(),
            host_port: 0,
        },
    };
    let _ = serde_json::to_writer(&mut stream, &response);
    let _ = stream.write_all(b"\n");
}

fn response_for(r: &RuntimeRecord) -> ControlResponse {
    ControlResponse {
        protocol_version: PROTOCOL_VERSION,
        ok: true,
        id: r.id.clone(),
        startup_token: r.startup_token.clone(),
        project_root: r.project_root.clone(),
        status: r.status.clone(),
        host_port: r.host_port,
        error: String::new(),
    }
}

impl Drop for SupervisorRuntime {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let record = self.record.lock().unwrap();
        let _ = fs::remove_file(record_path(&self.cache_dir, &record.id));
        let _ = fs::remove_file(&record.control_socket);
    }
}
