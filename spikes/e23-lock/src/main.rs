//! E23 part 1: the S14 sidecar lock on Linux, std only. The lock is `File::try_lock` (flock(2)) on the
//! sidecar, taken before anything else is built, never unlinked; the sidecar holds the 32-byte card key.
//! Modes: hold, check, killtest, childtest, sleepchild.
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn now_ns() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// S14 init: take a non-blocking exclusive lock on the sidecar; held returns None.
fn take_lock(path: &Path) -> std::io::Result<Option<File>> {
    let f = File::open(path)?;
    match f.try_lock() {
        Ok(()) => Ok(Some(f)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

fn create_sidecar(path: &Path, fsync: bool) -> std::io::Result<[u8; 32]> {
    let mut key = [0u8; 32];
    File::open("/dev/urandom")?.read_exact(&mut key)?;
    let mut f = OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    f.write_all(&key)?;
    if fsync {
        f.sync_all()?;
        if let Some(d) = path.parent() {
            File::open(d)?.sync_all()?;
        }
    }
    Ok(key)
}

fn read_key(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut v = vec![];
    File::open(path)?.read_to_end(&mut v)?;
    Ok(v)
}

fn hold(dir: &Path, fsync: bool) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let sidecar = dir.join("card.key");
    let created = if sidecar.exists() {
        None
    } else {
        let t = now_ns();
        let k = create_sidecar(&sidecar, fsync)?;
        Some((hex(&k), t))
    };
    match take_lock(&sidecar)? {
        None => {
            println!("CARD_HELD t={}", now_ns());
            std::process::exit(3);
        }
        Some(lock) => {
            if let Some((h, t)) = created {
                println!("CREATED fsync={fsync} hex={h} t={t}");
            }
            println!("LOCKED t={}", now_ns());
            let mut j = OpenOptions::new().create(true).append(true).open(dir.join("journal"))?;
            let mut i = 0u64;
            let _keep = lock;
            loop {
                i += 1;
                j.write_all(format!("{i} {}\n", now_ns()).as_bytes())?;
                if i % 100 == 0 {
                    println!("TICK {i} t={}", now_ns());
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

/// What a new instance would see and do after the cut.
fn check(dir: &Path, expect: &str) -> std::io::Result<()> {
    let sidecar = dir.join("card.key");
    let t0 = Instant::now();
    match std::fs::metadata(&sidecar) {
        Err(e) => println!("SIDECAR missing err={e}"),
        Ok(m) => {
            let k = read_key(&sidecar)?;
            let h = hex(&k);
            let state = if k.is_empty() {
                "empty"
            } else if h == expect {
                "intact"
            } else if k.len() != 32 {
                "short_or_long"
            } else if k.iter().all(|b| *b == 0) {
                "zero_filled"
            } else {
                "other_content"
            };
            println!("SIDECAR state={state} len={} meta_len={} match={}", k.len(), m.len(), h == expect);
        }
    }
    match std::fs::read(dir.join("journal")) {
        Err(e) => println!("JOURNAL missing err={e}"),
        Ok(b) => {
            let nul = b.iter().filter(|x| **x == 0).count();
            let ends_nl = b.last() == Some(&b'\n');
            let s = String::from_utf8_lossy(&b);
            let lines: Vec<&str> = s.split('\n').collect();
            let last = lines.iter().rev().find(|l| !l.is_empty() && !l.contains('\0')).copied().unwrap_or("");
            let lastn: u64 = last.split(' ').next().and_then(|x| x.parse().ok()).unwrap_or(0);
            println!("JOURNAL bytes={} nul_bytes={nul} ends_with_newline={ends_nl} last_counter={lastn} last_line_ok={}", b.len(), last.split(' ').count() == 2);
        }
    }
    match take_lock(&sidecar) {
        Ok(Some(_lock)) => {
            println!("LOCK free first_try_us={}", t0.elapsed().as_micros());
            let k = read_key(&sidecar).unwrap_or_default();
            if k.len() == 32 {
                println!("START_OK key_len=32");
            } else {
                println!("START_FAIL reason=sidecar_len_{}", k.len());
            }
        }
        Ok(None) => println!("LOCK held"),
        Err(e) => println!("LOCK error={e}"),
    }
    Ok(())
}

fn spawn_hold(exe: &Path, dir: &Path) -> std::io::Result<std::process::Child> {
    let mut c = Command::new(exe).arg("hold").arg(dir).stdout(Stdio::piped()).spawn()?;
    // wait for LOCKED
    let out = c.stdout.take().ok_or(std::io::Error::other("no stdout"))?;
    let mut r = std::io::BufReader::new(out);
    let mut line = String::new();
    loop {
        line.clear();
        use std::io::BufRead;
        if r.read_line(&mut line)? == 0 {
            return Err(std::io::Error::other("holder exited"));
        }
        if line.starts_with("LOCKED") {
            break;
        }
    }
    // keep draining stdout so the holder never blocks on a full pipe
    std::thread::spawn(move || {
        let mut sink = [0u8; 4096];
        let mut raw = r.into_inner();
        while raw.read(&mut sink).map(|n| n > 0).unwrap_or(false) {}
    });
    Ok(c)
}

fn killtest(exe: &Path, base: &Path, n: usize) -> std::io::Result<()> {
    for i in 0..n {
        let dir = base.join(format!("kill{i}"));
        let _ = std::fs::remove_dir_all(&dir);
        let mut h = spawn_hold(exe, &dir)?;
        let sidecar = dir.join("card.key");
        // control (a): a second instance is refused, by another process and by another open file in this process
        let second = Command::new(exe).arg("hold").arg(&dir).output()?;
        let refused_proc = second.status.code() == Some(3) && String::from_utf8_lossy(&second.stdout).contains("CARD_HELD");
        let refused_same_proc = take_lock(&sidecar)?.is_none();
        // (b) SIGKILL, then spin on try_lock from the moment kill() returns
        let t = Instant::now();
        h.kill()?;
        let kill_ret = t.elapsed();
        let mut first = None;
        let mut tries = 0u64;
        while first.is_none() && t.elapsed() < Duration::from_secs(2) {
            tries += 1;
            if take_lock(&sidecar)?.is_some() {
                first = Some(t.elapsed());
            }
        }
        h.wait()?;
        let after_wait = take_lock(&sidecar)?.is_some();
        // restart: a full second instance takes the lock and runs
        let mut h2 = spawn_hold(exe, &dir)?;
        let restart_ok = h2.try_wait()?.is_none();
        h2.kill()?;
        h2.wait()?;
        println!(
            "KILLTEST run={i} refused_other_process={refused_proc} refused_same_process_second_open={refused_same_proc} kill_returned_us={} lock_free_after_kill_us={:?} tries={tries} free_after_wait={after_wait} restart_started={restart_ok}",
            kill_ret.as_micros(),
            first.map(|d| d.as_micros())
        );
    }
    Ok(())
}

fn childtest(exe: &Path, base: &Path, n: usize) -> std::io::Result<()> {
    // holder forks a child with std::process::Command (O_CLOEXEC on the lock fd): is the lock freed on holder SIGKILL while the child lives?
    for i in 0..n {
        let dir = base.join(format!("child{i}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let sidecar = dir.join("card.key");
        create_sidecar(&sidecar, false)?;
        let mut holder = Command::new(exe).arg("holdchild").arg(&dir).stdout(Stdio::piped()).spawn()?;
        let out = holder.stdout.take().ok_or(std::io::Error::other("no stdout"))?;
        let mut r = std::io::BufReader::new(out);
        let mut line = String::new();
        use std::io::BufRead;
        let mut child_pid = 0u32;
        loop {
            line.clear();
            if r.read_line(&mut line)? == 0 {
                break;
            }
            if let Some(p) = line.strip_prefix("CHILD ") {
                child_pid = p.trim().parse().unwrap_or(0);
                break;
            }
        }
        let refused = take_lock(&sidecar)?.is_none();
        holder.kill()?;
        holder.wait()?;
        std::thread::sleep(Duration::from_millis(50));
        let free = take_lock(&sidecar)?.is_some();
        let alive = Path::new(&format!("/proc/{child_pid}")).exists();
        println!("CHILDTEST run={i} child_pid={child_pid} child_alive_after_holder_kill={alive} refused_while_held={refused} lock_free_after_holder_kill={free}");
        let _ = Command::new("kill").arg("-9").arg(child_pid.to_string()).status();
    }
    Ok(())
}

fn holdchild(dir: &Path) -> std::io::Result<()> {
    let sidecar = dir.join("card.key");
    let lock = take_lock(&sidecar)?.ok_or(std::io::Error::other("held"))?;
    let c = Command::new("sleep").arg("60").stdin(Stdio::null()).stdout(Stdio::null()).spawn()?;
    println!("CHILD {}", c.id());
    std::io::stdout().flush()?;
    let _keep = lock;
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn main() -> std::io::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let exe = std::env::current_exe()?;
    let dir = PathBuf::from(a.get(2).map_or(".", String::as_str));
    match a.get(1).map(String::as_str) {
        Some("hold") => hold(&dir, a.iter().any(|x| x == "--fsync")),
        Some("check") => check(&dir, a.get(3).map_or("", String::as_str)),
        Some("killtest") => killtest(&exe, &dir, a.get(3).and_then(|x| x.parse().ok()).unwrap_or(5)),
        Some("childtest") => childtest(&exe, &dir, a.get(3).and_then(|x| x.parse().ok()).unwrap_or(5)),
        Some("holdchild") => holdchild(&dir),
        _ => {
            eprintln!("usage: e23-lock hold|check|killtest|childtest <dir> [..]");
            std::process::exit(2);
        }
    }
}
