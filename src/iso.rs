use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use std::io::ErrorKind;

use anyhow::{Context, Result, anyhow, bail};

const ISO_PVD_OFFSET: u64 = 32768;
const ISO_MAGIC: &[u8] = b"CD001";
const MBR_SIGNATURE: [u8; 2] = [0x55, 0xAA];

#[derive(Debug, Clone)]
pub struct IsoImage {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub kind: IsoKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsoKind {
    /// ISO 9660 plus an MBR boot sector: boots when written raw to USB.
    HybridIso,
    /// A raw disk image (`.img`) with a partition table.
    DiskImage,
}

impl IsoImage {
    pub fn display_kind(&self) -> &'static str {
        match self.kind {
            IsoKind::HybridIso => "hybrid ISO (boots from USB)",
            IsoKind::DiskImage => "raw disk image with a partition table",
        }
    }
}

pub fn load_iso(path: &Path) -> Result<IsoImage> {
    let path = path
        .canonicalize()
        .with_context(|| format!("ISO not found: {}", path.display()))?;
    if !path.is_file() {
        bail!("{} is not a file", path.display());
    }
    let size_bytes = path
        .metadata()
        .with_context(|| format!("could not stat {}", path.display()))?
        .len();
    if size_bytes < 64 * 1024 {
        bail!(
            "{} is too small to be a Linux installer ISO",
            path.display()
        );
    }

    let kind = inspect_iso(&path, size_bytes)?;
    Ok(IsoImage {
        path,
        size_bytes,
        kind,
    })
}

#[derive(Debug, Clone)]
pub struct DownloadIso {
    pub path: PathBuf,
    pub size_bytes: u64,
}

pub fn downloads_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join("Downloads"))
}

pub fn list_download_isos() -> Result<Vec<DownloadIso>> {
    list_isos_in(&downloads_dir()?)
}

pub fn list_isos_in(dir: &Path) -> Result<Vec<DownloadIso>> {
    let mut found = Vec::new();
    let entries = fs::read_dir(dir).map_err(|error| {
        if error.kind() == ErrorKind::PermissionDenied {
            anyhow!(
                "macOS blocked {}. You: System Settings → Privacy & Security → Files and Folders, allow your terminal, then run SuitBoot again.",
                dir.display()
            )
        } else {
            anyhow!(error).context(format!("could not read {}", dir.display()))
        }
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if extension != "iso" && extension != "img" {
            continue;
        }
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if !metadata.is_file() || metadata.len() < 64 * 1024 {
            continue;
        }
        found.push(DownloadIso {
            path,
            size_bytes: metadata.len(),
        });
    }
    found.sort_by(|a, b| {
        let a_name = a.path.file_name().unwrap_or_default();
        let b_name = b.path.file_name().unwrap_or_default();
        a_name.cmp(b_name)
    });
    Ok(found)
}

fn inspect_iso(path: &Path, size_bytes: u64) -> Result<IsoKind> {
    let mut file =
        File::open(path).with_context(|| format!("could not open {}", path.display()))?;

    let mut has_iso9660 = false;
    if size_bytes > ISO_PVD_OFFSET + 6 {
        let mut pvd = [0u8; 6];
        file.seek(SeekFrom::Start(ISO_PVD_OFFSET))
            .context("could not seek ISO primary volume descriptor")?;
        file.read_exact(&mut pvd)
            .context("could not read ISO primary volume descriptor")?;
        has_iso9660 = pvd[0] == 1 && &pvd[1..6] == ISO_MAGIC;
    }

    let mut mbr_sig = [0u8; 2];
    file.seek(SeekFrom::Start(510))
        .context("could not seek MBR signature")?;
    file.read_exact(&mut mbr_sig)
        .context("could not read MBR signature")?;
    let has_mbr = mbr_sig == MBR_SIGNATURE;

    match (has_iso9660, has_mbr) {
        (true, true) => Ok(IsoKind::HybridIso),
        (false, true) => Ok(IsoKind::DiskImage),
        (true, false) => bail!(
            "{} is not a hybrid ISO. It will not boot from USB. Windows ISOs do this.",
            path.display()
        ),
        (false, false) => bail!("{} has no ISO header and no boot sector.", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(bytes: &[u8]) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("suitboot-test-{}-{nanos}.iso", std::process::id()));
        let mut file = File::create(&path).expect("temp iso");
        file.write_all(bytes).expect("write temp iso");
        path
    }

    fn image(iso9660: bool, mbr: bool) -> Vec<u8> {
        let mut bytes = vec![0u8; 70_000];
        if iso9660 {
            bytes[32768] = 1;
            bytes[32769..32774].copy_from_slice(b"CD001");
        }
        if mbr {
            bytes[510] = 0x55;
            bytes[511] = 0xAA;
        }
        bytes
    }

    #[test]
    fn accepts_hybrid_iso() {
        let path = write_temp(&image(true, true));
        let iso = load_iso(&path).expect("hybrid iso should load");
        assert_eq!(iso.kind, IsoKind::HybridIso);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn accepts_raw_disk_image() {
        let path = write_temp(&image(false, true));
        let iso = load_iso(&path).expect("disk image should load");
        assert_eq!(iso.kind, IsoKind::DiskImage);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rejects_non_hybrid_iso() {
        let path = write_temp(&image(true, false));
        let error = load_iso(&path).expect_err("non-hybrid iso must be refused");
        assert!(error.to_string().contains("not a hybrid ISO"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rejects_tiny_or_random_files() {
        let path = write_temp(&[1, 2, 3, 4]);
        assert!(load_iso(&path).is_err());
        let _ = std::fs::remove_file(&path);

        let path = write_temp(&vec![0u8; 70_000]);
        assert!(load_iso(&path).is_err());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn lists_iso_files_and_skips_junk() {
        let dir = std::env::temp_dir().join(format!(
            "suitboot-downloads-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("temp downloads");
        fs::write(dir.join("notes.txt"), b"hello").expect("notes");
        fs::write(dir.join("tiny.iso"), [0u8; 16]).expect("tiny iso");
        let iso = image(true, true);
        fs::write(dir.join("ubuntu.iso"), &iso).expect("ubuntu iso");
        fs::write(dir.join("fedora.img"), &iso).expect("fedora img");

        let listed = list_isos_in(&dir).expect("list");
        let names: Vec<_> = listed
            .iter()
            .filter_map(|item| item.path.file_name()?.to_str())
            .collect();
        assert_eq!(names, ["fedora.img", "ubuntu.iso"]);
        let _ = fs::remove_dir_all(dir);
    }
}
