use std::fs::File;
use std::io::{self, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

use crate::cmd;
use crate::disk::{self, Disk};
use crate::iso::IsoImage;
use crate::ui::Ui;
use crate::util::{format_bytes, format_duration};

const SUDO: &str = "/usr/bin/sudo";
const DD: &str = "/bin/dd";
const DISKUTIL: &str = "/usr/sbin/diskutil";

const CHUNK: usize = 1024 * 1024;
/// Written last. Until then the USB has no partition table, so macOS won't mount it mid-write.
const BOOT_HEAD: u64 = CHUNK as u64;
const RECONNECT_WAIT: Duration = Duration::from_secs(30);
const SECTOR: u64 = 512;

const DISKUTIL_TIMEOUT: Duration = Duration::from_secs(60);
const SUDO_CHECK_TIMEOUT: Duration = Duration::from_secs(10);
const SYNC_TIMEOUT: Duration = Duration::from_secs(60);
/// Longest a write or read-back may go without moving a byte before it's treated as hung.
const STALL_TIMEOUT: Duration = Duration::from_secs(180);

const UNMOUNT_ATTEMPTS: u32 = 5;
const WRITE_ATTEMPTS: u32 = 3;
const EJECT_ATTEMPTS: u32 = 3;

pub fn erase_and_write(ui: &mut Ui, selected: &Disk, iso: &IsoImage) -> Result<()> {
    ui.show(
        "Write",
        &[
            "Leave the USB plugged in".to_string(),
            "Keep the Mac awake".to_string(),
        ],
    )?;
    ui.set_action(&[
        "If macOS says the disk is unreadable, click Ignore".to_string(),
        "Not Eject. Not Initialize.".to_string(),
    ])?;
    ensure_sudo(ui, true)?;
    let disk = confirm_same_device(ui, selected)?;
    unmount(ui, &disk)?;
    write_image(ui, &disk, iso)?;
    flush(ui)?;
    verify_image(ui, &disk, iso)?;
    eject(ui, &disk)?;
    ui.clear_action()?;
    Ok(())
}

/// Asks for the admin password up front so a bad password can't be mistaken for a
/// failed write, and so the later non-interactive `sudo -n` calls can't block on a prompt.
fn ensure_sudo(ui: &mut Ui, announce: bool) -> Result<()> {
    let cached = cmd::run(SUDO, &["-n", "-v"], SUDO_CHECK_TIMEOUT)
        .map(|output| output.status.success())
        .unwrap_or(false);
    if cached {
        if announce {
            ui.show("Password", &["macOS already granted access".to_string()])?;
        }
        return Ok(());
    }
    ui.show("Password", &["Needed to write the USB".to_string()])?;
    let status = ui.suspend(|| {
        println!();
        println!("  Password required to write the USB");
        println!();
        Command::new(SUDO).args(["-v", "-p", "Password: "]).status()
    });
    let status = status.context("failed to run sudo")?;
    if !status.success() {
        bail!(
            "the password was not accepted, or this account isn't an admin. Nothing was written."
        );
    }
    ui.show("Password", &["Accepted".to_string()])?;
    Ok(())
}

fn confirm_same_device(ui: &mut Ui, selected: &Disk) -> Result<Disk> {
    ui.show(
        "Checking the USB",
        &[format!(
            "{}  ·  {}  ·  {}",
            selected.id,
            selected.media_name,
            format_bytes(selected.size_bytes)
        )],
    )?;
    let current = disk::require_candidate(&selected.id)?;
    disk::check_same_device(selected, &current)?;
    Ok(current)
}

fn unmount(ui: &mut Ui, disk: &Disk) -> Result<()> {
    let node = disk.node();
    ui.show("Unmount", std::slice::from_ref(&node))?;
    cmd::retry(&format!("Unmounting {node}"), UNMOUNT_ATTEMPTS, |attempt| {
        if attempt > 1 {
            let _ = ui.show("Unmount", &[format!("{node}  ·  retry {attempt}")]);
        }
        cmd::run_ok(DISKUTIL, &["unmountDisk", "force", &node], DISKUTIL_TIMEOUT)?;
        let still_mounted = disk::mounted_slices(&disk.id)?;
        if !still_mounted.is_empty() {
            bail!("still mounted: {}", still_mounted.join(", "));
        }
        Ok(())
    })
    .with_context(|| {
        format!(
            "could not unmount {node}. Nothing was written. Close Finder windows on the USB, then run SuitBoot again."
        )
    })?;
    ui.show("Unmount", &["Unmounted".to_string()])?;
    Ok(())
}

#[derive(Debug)]
enum WriteFailure {
    /// macOS remounted the USB.
    Busy,
    /// The USB dropped off the bus. `written` is how much of this attempt had landed.
    Gone {
        written: u64,
    },
    Fatal(anyhow::Error),
}

impl From<anyhow::Error> for WriteFailure {
    fn from(error: anyhow::Error) -> Self {
        WriteFailure::Fatal(error)
    }
}

fn write_image(ui: &mut Ui, selected: &Disk, iso: &IsoImage) -> Result<()> {
    let mut disk = selected.clone();
    let mut attempt = 1;
    loop {
        match write_once(ui, &disk, iso) {
            Ok(()) => return Ok(()),
            Err(WriteFailure::Busy) if attempt < WRITE_ATTEMPTS => {
                attempt += 1;
                ui.show(
                    "Retry",
                    &[format!(
                        "macOS remounted the USB. Writing again ({attempt}/{WRITE_ATTEMPTS})"
                    )],
                )?;
                thread::sleep(Duration::from_secs(2));
                disk = confirm_same_device(ui, &disk)?;
                unmount(ui, &disk)?;
            }
            Err(WriteFailure::Busy) => bail!(
                "macOS kept remounting the USB. It is not bootable. Unplug it, plug it back in, and run SuitBoot again."
            ),
            Err(WriteFailure::Gone { written }) if attempt < WRITE_ATTEMPTS => {
                attempt += 1;
                ui.show(
                    "Disconnected",
                    &[
                        format!(
                            "{} of {} written. Not bootable yet.",
                            format_bytes(written),
                            format_bytes(iso.size_bytes)
                        ),
                        format!(
                            "Waiting {}s. Leave it plugged in.",
                            RECONNECT_WAIT.as_secs()
                        ),
                    ],
                )?;
                disk = wait_for_return(ui, &disk)?;
                ui.show(
                    "Disconnected",
                    &[format!("{} is back. Writing again.", disk.id)],
                )?;
                unmount(ui, &disk)?;
            }
            Err(WriteFailure::Gone { written }) => bail!(
                "the USB disconnected again after {} of {}. It is not bootable. Use a port on the Mac, not a hub, then run SuitBoot again.",
                format_bytes(written),
                format_bytes(iso.size_bytes)
            ),
            Err(WriteFailure::Fatal(error)) => return Err(error),
        }
    }
}

fn wait_for_return(ui: &mut Ui, selected: &Disk) -> Result<Disk> {
    let deadline = Instant::now() + RECONNECT_WAIT;
    let mut next_notice = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(disk) = disk::find_same_stick(selected)? {
            return Ok(disk);
        }
        if Instant::now() >= deadline {
            bail!(
                "the {} USB did not come back. It is not bootable. Unplug it, plug it back in, and run SuitBoot again.",
                selected.media_name
            );
        }
        if Instant::now() >= next_notice {
            let left = deadline.saturating_duration_since(Instant::now()).as_secs();
            let _ = ui.show(
                "Disconnected",
                &[format!(
                    "Still waiting for {} · {left}s. Leave it plugged in.",
                    selected.media_name
                )],
            );
            next_notice = Instant::now() + Duration::from_secs(5);
        }
        thread::sleep(Duration::from_secs(1));
    }
}

fn write_once(ui: &mut Ui, disk: &Disk, iso: &IsoImage) -> Result<(), WriteFailure> {
    ensure_sudo(ui, false)?;
    let name = iso
        .path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    if iso.size_bytes <= BOOT_HEAD {
        ui.show(
            "Write",
            &[format!("{name}  ·  {}", format_bytes(iso.size_bytes))],
        )?;
        return stream_range(ui, disk, iso, 0, iso.size_bytes, &[], "Writing");
    }

    ui.show("Clear", &["Wiping the old partition table".to_string()])?;
    ui.set_action(&[
        "If macOS says the disk is unreadable, click Ignore".to_string(),
        "Not Eject. Not Initialize.".to_string(),
    ])?;
    zero_head(disk)?;
    ui.show(
        "Write",
        &[
            format!("{name}  ·  {}", format_bytes(iso.size_bytes)),
            "Boot sector is written last".to_string(),
        ],
    )?;
    stream_range(
        ui,
        disk,
        iso,
        BOOT_HEAD,
        iso.size_bytes - BOOT_HEAD,
        &["seek=1"],
        "Writing",
    )?;
    ui.show(
        "Boot sector",
        &["Writing the start of the disk".to_string()],
    )?;
    stream_range(ui, disk, iso, 0, BOOT_HEAD, &[], "Boot sector")
}

fn zero_head(disk: &Disk) -> Result<(), WriteFailure> {
    let raw = disk.raw_node();
    let output = cmd::run(
        SUDO,
        &[
            "-n",
            DD,
            "if=/dev/zero",
            &format!("of={raw}"),
            "bs=1m",
            "count=1",
        ],
        Duration::from_secs(60),
    )?;
    if output.status.success() {
        Ok(())
    } else {
        Err(dd_failure(&raw, &cmd::output_text(&output), 0))
    }
}

fn stream_range(
    ui: &mut Ui,
    disk: &Disk,
    iso: &IsoImage,
    skip: u64,
    len: u64,
    dd_options: &[&str],
    label: &str,
) -> Result<(), WriteFailure> {
    let raw = disk.raw_node();
    let of = format!("of={raw}");
    let mut args = vec!["-n", DD, of.as_str(), "bs=1m"];
    args.extend_from_slice(dd_options);

    let mut child = Command::new(SUDO)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start dd")?;
    let stderr = cmd::drain(child.stderr.take());
    let stdin = child.stdin.take().context("dd stdin was not captured")?;

    let progress = Arc::new(AtomicU64::new(0));
    let feeder = {
        let progress = Arc::clone(&progress);
        let path = iso.path.clone();
        thread::spawn(move || feed(&path, stdin, &progress, skip, len))
    };

    let status = supervise(ui, &mut child, &progress, len, label)?;
    let fed = feeder
        .join()
        .map_err(|_| anyhow!("the ISO reader thread panicked"))?;
    let dd_output = String::from_utf8_lossy(&cmd::collect(stderr))
        .trim()
        .to_string();
    let written = progress.load(Ordering::Relaxed);

    if !status.success() {
        return Err(dd_failure(&raw, &dd_output, written));
    }
    match fed {
        Ok(got) if got == padded_len(len) => Ok(()),
        Ok(got) => Err(anyhow!(
            "only {} of {} reached the USB",
            format_bytes(got),
            format_bytes(len)
        )
        .into()),
        // dd already reported why it stopped; a broken pipe here is that same failure.
        Err(_) if !dd_output.is_empty() => Err(dd_failure(&raw, &dd_output, written)),
        Err(error) => Err(anyhow!(error)
            .context(format!("could not read {}", iso.path.display()))
            .into()),
    }
}

fn dd_failure(raw: &str, output: &str, written: u64) -> WriteFailure {
    if output.contains("Resource busy") {
        WriteFailure::Busy
    } else if output.contains("Device not configured") || output.contains("Input/output error") {
        WriteFailure::Gone { written }
    } else {
        let detail = output
            .lines()
            .find(|line| !line.trim().is_empty() && !line.contains("records"))
            .unwrap_or("dd failed");
        WriteFailure::Fatal(anyhow!("could not write {raw}: {detail}"))
    }
}

/// Streams `len` bytes from `skip` into `sink`, padding the tail to a whole sector
/// because raw devices reject partial-sector writes.
fn feed(
    path: &Path,
    mut sink: impl Write,
    progress: &AtomicU64,
    skip: u64,
    len: u64,
) -> io::Result<u64> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(skip))?;
    let mut buf = vec![0u8; CHUNK];
    let mut remaining = len;
    let mut written = 0u64;
    while remaining > 0 {
        let want = remaining.min(CHUNK as u64) as usize;
        let read = read_full(&mut file, &mut buf[..want])?;
        if read == 0 {
            break;
        }
        let out_len = padded_len(read as u64) as usize;
        buf[read..out_len].fill(0);
        sink.write_all(&buf[..out_len])?;
        written += out_len as u64;
        remaining -= read as u64;
        progress.store(written, Ordering::Relaxed);
        if read < want {
            break;
        }
    }
    sink.flush()?;
    Ok(written)
}

fn read_full(reader: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

fn padded_len(size: u64) -> u64 {
    size.div_ceil(SECTOR) * SECTOR
}

/// Waits for `child` while showing progress. If no bytes move for [`STALL_TIMEOUT`],
/// the child is terminated and an error returned, so a dead stick can't hang the app.
fn supervise(
    ui: &mut Ui,
    child: &mut Child,
    progress: &AtomicU64,
    total: u64,
    label: &str,
) -> Result<ExitStatus> {
    let started = Instant::now();
    let mut last_bytes = 0;
    let mut last_change = Instant::now();
    let mut last_render: Option<Instant> = None;
    loop {
        if ui.ctrl_c() {
            cmd::terminate(child);
            bail!("{label} stopped");
        }
        if let Some(status) = child
            .try_wait()
            .with_context(|| format!("failed to poll the {label} process"))?
        {
            show_progress(
                ui,
                label,
                progress.load(Ordering::Relaxed),
                total,
                started.elapsed(),
                Duration::ZERO,
            )?;
            return Ok(status);
        }

        let bytes = progress.load(Ordering::Relaxed);
        if bytes != last_bytes {
            last_bytes = bytes;
            last_change = Instant::now();
        }
        if last_change.elapsed() >= STALL_TIMEOUT {
            cmd::terminate(child);
            bail!(
                "{label} stalled: nothing moved for {}s at {} of {}. The USB stick or port may be \
                 faulty — try another port or stick.",
                STALL_TIMEOUT.as_secs(),
                format_bytes(bytes),
                format_bytes(total)
            );
        }

        if last_render.is_none_or(|at| at.elapsed() >= Duration::from_millis(500)) {
            show_progress(
                ui,
                label,
                bytes,
                total,
                started.elapsed(),
                last_change.elapsed(),
            )?;
            last_render = Some(Instant::now());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn show_progress(
    ui: &mut Ui,
    label: &str,
    done: u64,
    total: u64,
    elapsed: Duration,
    idle: Duration,
) -> Result<()> {
    let ratio = if total == 0 {
        1.0
    } else {
        (done.min(total) as f64) / (total as f64)
    };
    ui.set_progress(
        ratio,
        &format!(
            "{label}  {} / {}  {}",
            format_bytes(done),
            format_bytes(total),
            progress_detail(done, total, elapsed, idle)
        ),
    )
}

fn progress_detail(done: u64, total: u64, elapsed: Duration, idle: Duration) -> String {
    let percent = (done.min(total) * 100).checked_div(total).unwrap_or(100);
    let secs = elapsed.as_secs_f64();
    let rate = if secs > 0.0 {
        (done as f64 / secs) as u64
    } else {
        0
    };
    let eta = if total == 0 || done >= total {
        "done".to_string()
    } else if rate == 0 || elapsed < Duration::from_secs(2) {
        "estimating time left".to_string()
    } else {
        let left = ((total - done) as f64 / rate as f64).ceil() as u64;
        format!("about {} left", format_duration(left))
    };
    let waiting = if idle >= Duration::from_secs(5) && done < total {
        format!("  no data for {}", format_duration(idle.as_secs()))
    } else {
        String::new()
    };
    format!("({percent}%)  {}/s  {eta}{waiting}", format_bytes(rate))
}

fn flush(ui: &mut Ui) -> Result<()> {
    ui.clear_progress()?;
    ui.show(
        "Flush",
        &[
            "Pushing cached writes onto the USB".to_string(),
            "This can sit still. It has not stalled.".to_string(),
        ],
    )?;
    if let Err(error) = cmd::run_ok("/bin/sync", &[], SYNC_TIMEOUT) {
        ui.show("Flush", &[format!("Sync did not finish: {error:#}")])?;
    } else {
        ui.show("Flush", &["Flushed".to_string()])?;
    }
    Ok(())
}

fn verify_image(ui: &mut Ui, disk: &Disk, iso: &IsoImage) -> Result<()> {
    ensure_sudo(ui, false)?;
    let raw = disk.raw_node();
    let blocks = iso.size_bytes.div_ceil(CHUNK as u64);
    ui.show(
        "Verify",
        &["Reading the USB back. About as long as the write.".to_string()],
    )?;

    let mut child = Command::new(SUDO)
        .args([
            "-n",
            DD,
            &format!("if={raw}"),
            "bs=1m",
            &format!("count={blocks}"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start dd for verification")?;
    let stderr = cmd::drain(child.stderr.take());
    let device = child.stdout.take().context("dd stdout was not captured")?;

    let progress = Arc::new(AtomicU64::new(0));
    let checker = {
        let progress = Arc::clone(&progress);
        let path: PathBuf = iso.path.clone();
        let size = iso.size_bytes;
        thread::spawn(move || compare(&path, size, device, &progress))
    };

    let status = supervise(ui, &mut child, &progress, iso.size_bytes, "Verifying")?;
    let result = checker
        .join()
        .map_err(|_| anyhow!("the verification thread panicked"))?;
    if !status.success() {
        let dd_output = String::from_utf8_lossy(&cmd::collect(stderr))
            .trim()
            .to_string();
        bail!("could not read back {raw} to verify it: {dd_output}");
    }
    match result {
        Ok(None) => {
            ui.clear_progress()?;
            ui.show("Verify", &["USB matches the ISO".to_string()])?;
            Ok(())
        }
        Ok(Some(offset)) => bail!(
            "verification failed at byte {offset}. The USB is not bootable. Run SuitBoot again, or try another stick."
        ),
        Err(error) => Err(anyhow!(error).context("verification failed while reading the USB")),
    }
}

/// Returns the first differing byte offset, or `None` if the first `size` bytes match.
/// Always drains `device` to EOF so dd never blocks on a full pipe.
fn compare(
    path: &Path,
    size: u64,
    mut device: impl Read,
    progress: &AtomicU64,
) -> io::Result<Option<u64>> {
    let mut file = File::open(path)?;
    let mut expected = vec![0u8; CHUNK];
    let mut actual = vec![0u8; CHUNK];
    let mut offset = 0u64;
    let mut mismatch = None;
    while offset < size {
        let want = (size - offset).min(CHUNK as u64) as usize;
        file.read_exact(&mut expected[..want])?;
        device.read_exact(&mut actual[..want])?;
        if let Some(index) = expected[..want]
            .iter()
            .zip(&actual[..want])
            .position(|(a, b)| a != b)
        {
            mismatch = Some(offset + index as u64);
            break;
        }
        offset += want as u64;
        progress.store(offset, Ordering::Relaxed);
    }
    io::copy(&mut device, &mut io::sink())?;
    Ok(mismatch)
}

fn eject(ui: &mut Ui, disk: &Disk) -> Result<()> {
    let node = disk.node();
    ui.clear_progress()?;
    ui.show("Eject", std::slice::from_ref(&node))?;
    let result = cmd::retry(&format!("Ejecting {node}"), EJECT_ATTEMPTS, |_| {
        cmd::run_ok(DISKUTIL, &["eject", &node], DISKUTIL_TIMEOUT).map(|_| ())
    });
    if let Err(error) = result {
        ui.show(
            "Eject",
            &[
                format!("{error:#}"),
                "Written and verified. Eject it from Finder before unplugging.".to_string(),
            ],
        )?;
    } else {
        ui.show("Eject", &["Ejected".to_string()])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(bytes: &[u8]) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("suitboot-flash-{}-{nanos}.bin", std::process::id()));
        std::fs::write(&path, bytes).expect("temp file");
        path
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn progress_detail_shows_eta_then_a_stall() {
        let total = 1000;
        let early = progress_detail(0, total, Duration::from_millis(500), Duration::ZERO);
        assert!(early.contains("estimating time left"), "{early}");

        let moving = progress_detail(250, total, Duration::from_secs(10), Duration::from_secs(1));
        assert!(moving.contains("(25%)"), "{moving}");
        assert!(moving.contains("about 30s left"), "{moving}");
        assert!(!moving.contains("no data"), "{moving}");

        let stalled = progress_detail(250, total, Duration::from_secs(20), Duration::from_secs(12));
        assert!(stalled.contains("no data for 12s"), "{stalled}");

        let done = progress_detail(total, total, Duration::from_secs(40), Duration::ZERO);
        assert!(done.contains("done"), "{done}");
    }

    #[test]
    fn padded_len_rounds_up_to_sectors() {
        assert_eq!(padded_len(0), 0);
        assert_eq!(padded_len(1), 512);
        assert_eq!(padded_len(512), 512);
        assert_eq!(padded_len(1000), 1024);
    }

    #[test]
    fn feed_streams_whole_file_and_pads_tail() {
        let data = pattern(CHUNK * 2 + 1000);
        let path = temp_file(&data);
        let progress = AtomicU64::new(0);
        let mut sink = Vec::new();
        let written = feed(&path, &mut sink, &progress, 0, data.len() as u64).expect("feed");

        assert_eq!(written, padded_len(data.len() as u64));
        assert_eq!(sink.len() as u64, written);
        assert_eq!(&sink[..data.len()], &data[..]);
        assert!(sink[data.len()..].iter().all(|&b| b == 0));
        assert_eq!(progress.load(Ordering::Relaxed), written);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn feed_can_skip_the_boot_sector() {
        let data = pattern(CHUNK * 2 + 1000);
        let path = temp_file(&data);
        let progress = AtomicU64::new(0);
        let mut sink = Vec::new();
        let written = feed(
            &path,
            &mut sink,
            &progress,
            CHUNK as u64,
            data.len() as u64 - CHUNK as u64,
        )
        .expect("feed");
        let expected = &data[CHUNK..];
        assert_eq!(written, padded_len(expected.len() as u64));
        assert_eq!(&sink[..expected.len()], expected);
        assert!(sink[expected.len()..].iter().all(|&b| b == 0));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn dd_failure_hides_the_record_count() {
        let raw =
            "dd: /dev/rdisk5: Device not configured\n30176+0 records in\n1885+0 records out\n";
        match dd_failure("/dev/rdisk5", raw, 1_800_000_000) {
            WriteFailure::Gone { written } => assert_eq!(written, 1_800_000_000),
            other => panic!("expected Gone, got {other:?}"),
        }
        let busy = dd_failure("/dev/rdisk5", "dd: /dev/rdisk5: Resource busy", 0);
        assert!(matches!(busy, WriteFailure::Busy));
        match dd_failure(
            "/dev/rdisk5",
            "dd: /dev/rdisk5: Permission denied\n1+0 records in\n",
            0,
        ) {
            WriteFailure::Fatal(error) => {
                let text = error.to_string();
                assert!(text.contains("Permission denied"), "{text}");
                assert!(!text.contains("records"), "{text}");
            }
            other => panic!("expected Fatal, got {other:?}"),
        }
    }

    #[test]
    fn compare_accepts_match_including_trailing_device_bytes() {
        let data = pattern(CHUNK + 4096);
        let path = temp_file(&data);
        let mut device = data.clone();
        device.extend(vec![0xEE; CHUNK]);
        let progress = AtomicU64::new(0);
        let result = compare(&path, data.len() as u64, &device[..], &progress).expect("compare");
        assert_eq!(result, None);
        assert_eq!(progress.load(Ordering::Relaxed), data.len() as u64);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn compare_reports_first_differing_byte() {
        let data = pattern(CHUNK + 4096);
        let path = temp_file(&data);
        let mut device = data.clone();
        device[CHUNK + 10] ^= 0xFF;
        let progress = AtomicU64::new(0);
        let result = compare(&path, data.len() as u64, &device[..], &progress).expect("compare");
        assert_eq!(result, Some((CHUNK + 10) as u64));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn compare_errors_when_device_is_short() {
        let data = pattern(8192);
        let path = temp_file(&data);
        let progress = AtomicU64::new(0);
        assert!(compare(&path, data.len() as u64, &data[..4096], &progress).is_err());
        let _ = std::fs::remove_file(path);
    }
}
