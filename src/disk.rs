use std::collections::{HashMap, HashSet};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::cmd;
use crate::util::{format_bytes, whole_disk_id};

/// Disks at or above this size get an extra "this may be a backup drive" prompt.
pub const LARGE_DISK_BYTES: u64 = 128 * 1024 * 1024 * 1024;

const DISKUTIL: &str = "/usr/sbin/diskutil";
const DISKUTIL_TIMEOUT: Duration = Duration::from_secs(30);
const MOUNT_TIMEOUT: Duration = Duration::from_secs(10);
const SCAN_ATTEMPTS: u32 = 3;

/// Identifies the physical device behind a `diskN` id, which macOS reuses when
/// drives are unplugged and replugged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    pub media_name: Option<String>,
    pub io_registry_name: Option<String>,
    pub device_tree_path: Option<String>,
    pub size_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct Disk {
    pub id: String,
    pub media_name: String,
    pub volume_names: Vec<String>,
    pub size_bytes: u64,
    pub protocol: String,
    pub removable: bool,
    pub fingerprint: Fingerprint,
}

impl Disk {
    pub fn node(&self) -> String {
        format!("/dev/{}", self.id)
    }

    pub fn raw_node(&self) -> String {
        format!("/dev/r{}", self.id)
    }

    pub fn is_large(&self) -> bool {
        self.size_bytes >= LARGE_DISK_BYTES
    }

    pub fn summary(&self) -> String {
        let volumes = if self.volume_names.is_empty() {
            "no volumes".to_string()
        } else {
            self.volume_names.join(", ")
        };
        let removable = if self.removable { "removable" } else { "fixed" };
        format!(
            "{}  —  {}  —  {}  —  {} ({removable}, {})",
            self.id,
            self.media_name,
            format_bytes(self.size_bytes),
            volumes,
            self.protocol
        )
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct DiskInfo {
    #[serde(rename = "DeviceIdentifier")]
    pub device_identifier: Option<String>,
    #[serde(rename = "BusProtocol")]
    pub bus_protocol: Option<String>,
    #[serde(rename = "MediaName")]
    pub media_name: Option<String>,
    #[serde(rename = "IORegistryEntryName")]
    pub io_registry_entry_name: Option<String>,
    #[serde(rename = "DeviceTreePath")]
    pub device_tree_path: Option<String>,
    #[serde(rename = "VolumeName")]
    pub volume_name: Option<String>,
    #[serde(rename = "Size")]
    pub size: Option<u64>,
    #[serde(rename = "Internal")]
    pub internal: Option<bool>,
    #[serde(rename = "WholeDisk")]
    pub whole_disk: Option<bool>,
    #[serde(rename = "Removable")]
    pub removable: Option<bool>,
    #[serde(rename = "RemovableMedia")]
    pub removable_media: Option<bool>,
    #[serde(rename = "Writable")]
    pub writable: Option<bool>,
    #[serde(rename = "VirtualOrPhysical")]
    pub virtual_or_physical: Option<String>,
    #[serde(rename = "ParentWholeDisk")]
    pub parent_whole_disk: Option<String>,
    #[serde(rename = "APFSContainerReference")]
    pub apfs_container_reference: Option<String>,
    #[serde(rename = "BooterDeviceIdentifier")]
    pub booter_device_identifier: Option<String>,
    #[serde(rename = "RecoveryDeviceIdentifier")]
    pub recovery_device_identifier: Option<String>,
    #[serde(rename = "APFSPhysicalStores", default)]
    pub apfs_physical_stores: Vec<PhysicalStore>,
}

#[derive(Debug, Deserialize, Default)]
pub struct PhysicalStore {
    #[serde(rename = "APFSPhysicalStore")]
    pub apfs_physical_store: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DiskList {
    #[serde(rename = "AllDisks")]
    all_disks: Vec<String>,
    #[serde(rename = "AllDisksAndPartitions", default)]
    all_disks_and_partitions: Vec<PartitionNode>,
}

#[derive(Debug, Deserialize, Default)]
struct PartitionNode {
    #[serde(rename = "DeviceIdentifier")]
    device_identifier: Option<String>,
    #[serde(rename = "VolumeName")]
    volume_name: Option<String>,
    #[serde(rename = "Partitions", default)]
    partitions: Vec<PartitionNode>,
    #[serde(rename = "APFSVolumes", default)]
    apfs_volumes: Vec<PartitionNode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskVerdict {
    Candidate,
    Rejected(&'static str),
}

pub fn classify_disk(info: &DiskInfo, protected: &HashSet<String>) -> DiskVerdict {
    let id = match info.device_identifier.as_deref() {
        Some(id) if !id.is_empty() => id,
        _ => return DiskVerdict::Rejected("missing device identifier"),
    };

    if info.whole_disk != Some(true) {
        return DiskVerdict::Rejected("not a whole disk");
    }
    if protected.contains(id) || protected.contains(whole_disk_id(id)) {
        return DiskVerdict::Rejected("macOS boot / system disk");
    }
    if info.internal == Some(true) {
        return DiskVerdict::Rejected("internal disk");
    }
    if info
        .virtual_or_physical
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case("virtual"))
    {
        return DiskVerdict::Rejected("virtual disk or disk image");
    }
    let protocol = info.bus_protocol.as_deref().unwrap_or("");
    if protocol.eq_ignore_ascii_case("disk image") {
        return DiskVerdict::Rejected("disk image");
    }
    if !protocol.eq_ignore_ascii_case("usb") {
        return DiskVerdict::Rejected("not a USB device");
    }
    if info.writable == Some(false) {
        return DiskVerdict::Rejected("not writable");
    }
    DiskVerdict::Candidate
}

struct Scan {
    entries: Vec<(DiskInfo, DiskVerdict)>,
    volumes: HashMap<String, Vec<String>>,
}

fn scan() -> Result<Scan> {
    cmd::retry("Scanning disks", SCAN_ATTEMPTS, |_| {
        let protected = protected_disk_ids()?;
        let list = diskutil_plist::<DiskList>(&["list", "-plist"])?;
        let infos: Vec<DiskInfo> = list
            .all_disks
            .iter()
            .filter(|id| whole_disk_id(id) == id.as_str())
            .filter_map(|id| read_disk_info(id))
            .collect();

        let mut volumes = volume_names_by_whole(&list.all_disks_and_partitions);
        for info in &infos {
            let Some(container) = info.device_identifier.as_deref() else {
                continue;
            };
            for store in &info.apfs_physical_stores {
                if let Some(store_id) = store.apfs_physical_store.as_deref() {
                    merge_apfs_volumes(&mut volumes, container, store_id);
                }
            }
        }

        let entries = infos
            .into_iter()
            .map(|info| {
                let verdict = classify_disk(&info, &protected);
                (info, verdict)
            })
            .collect();
        Ok(Scan { entries, volumes })
    })
}

/// A disk can vanish or be mid-probe between `diskutil list` and `diskutil info`,
/// so one quiet retry before treating it as absent.
fn read_disk_info(id: &str) -> Option<DiskInfo> {
    diskutil_info(id).ok().or_else(|| {
        thread::sleep(Duration::from_millis(500));
        diskutil_info(id).ok()
    })
}

pub fn list_usb_candidates() -> Result<Vec<Disk>> {
    let scan = scan()?;
    let mut disks: Vec<Disk> = scan
        .entries
        .iter()
        .filter(|(_, verdict)| *verdict == DiskVerdict::Candidate)
        .map(|(info, _)| disk_from_info(info, &scan.volumes))
        .collect();
    disks.sort_by(|a, b| a.size_bytes.cmp(&b.size_bytes).then(a.id.cmp(&b.id)));
    Ok(disks)
}

pub fn require_candidate(id: &str) -> Result<Disk> {
    let scan = scan()?;
    let entry = scan
        .entries
        .iter()
        .find(|(info, _)| info.device_identifier.as_deref() == Some(id));
    match entry {
        None => bail!("{id} is no longer attached. Nothing was written."),
        Some((info, DiskVerdict::Candidate)) => Ok(disk_from_info(info, &scan.volumes)),
        Some((_, DiskVerdict::Rejected(reason))) => {
            bail!("refusing to use {id}: {reason}. Nothing was written.")
        }
    }
}

/// Same physical stick after a USB reset, when macOS may have changed `diskN` and the port path.
pub fn same_stick(selected: &Disk, current: &Disk) -> bool {
    selected.media_name == current.media_name
        && selected.size_bytes == current.size_bytes
        && selected.fingerprint.io_registry_name == current.fingerprint.io_registry_name
}

pub fn find_same_stick(selected: &Disk) -> Result<Option<Disk>> {
    let matches: Vec<Disk> = list_usb_candidates()?
        .into_iter()
        .filter(|disk| same_stick(selected, disk))
        .collect();
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.into_iter().next()),
        _ => bail!(
            "more than one USB looks like {} ({}). Unplug the other one and run SuitBoot again.",
            selected.media_name,
            format_bytes(selected.size_bytes)
        ),
    }
}

pub fn check_same_device(selected: &Disk, current: &Disk) -> Result<()> {
    if selected.fingerprint != current.fingerprint {
        bail!(
            "{} is now a different device ({}, {}) than the one you selected ({}, {}). \
             Nothing was written. Run SuitBoot again.",
            selected.id,
            current.media_name,
            format_bytes(current.size_bytes),
            selected.media_name,
            format_bytes(selected.size_bytes),
        );
    }
    Ok(())
}

pub fn mounted_slices(id: &str) -> Result<Vec<String>> {
    let output = cmd::run_ok("/sbin/mount", &[], MOUNT_TIMEOUT)?;
    Ok(mounted_slices_in(
        &String::from_utf8_lossy(&output.stdout),
        id,
    ))
}

fn mounted_slices_in(mount_output: &str, id: &str) -> Vec<String> {
    let prefix = format!("/dev/{id}");
    mount_output
        .lines()
        .filter_map(|line| {
            let device = line.split_whitespace().next()?;
            let rest = device.strip_prefix(&prefix)?;
            (rest.is_empty() || rest.starts_with('s')).then(|| device.to_string())
        })
        .collect()
}

pub fn diskutil_info(disk: &str) -> Result<DiskInfo> {
    diskutil_plist(&["info", "-plist", disk])
}

fn disk_from_info(info: &DiskInfo, volumes: &HashMap<String, Vec<String>>) -> Disk {
    let id = info.device_identifier.clone().unwrap_or_default();
    let mut volume_names = volumes.get(&id).cloned().unwrap_or_default();
    if let Some(name) = info.volume_name.as_deref().map(str::trim)
        && !name.is_empty()
        && !volume_names.iter().any(|existing| existing == name)
    {
        volume_names.insert(0, name.to_string());
    }
    let media_name = first_nonempty(&[
        info.media_name.as_deref(),
        info.io_registry_entry_name.as_deref(),
    ])
    .unwrap_or("USB disk")
    .to_string();
    let size_bytes = info.size.unwrap_or(0);
    Disk {
        id,
        media_name,
        volume_names,
        size_bytes,
        protocol: info
            .bus_protocol
            .clone()
            .unwrap_or_else(|| "USB".to_string()),
        removable: info.removable == Some(true) || info.removable_media == Some(true),
        fingerprint: Fingerprint {
            media_name: info.media_name.clone(),
            io_registry_name: info.io_registry_entry_name.clone(),
            device_tree_path: info.device_tree_path.clone(),
            size_bytes,
        },
    }
}

fn first_nonempty<'a>(values: &[Option<&'a str>]) -> Option<&'a str> {
    values
        .iter()
        .find_map(|value| value.map(str::trim).filter(|trimmed| !trimmed.is_empty()))
}

fn volume_names_by_whole(nodes: &[PartitionNode]) -> HashMap<String, Vec<String>> {
    let mut map = HashMap::new();
    for node in nodes {
        collect_volume_names(node, &mut map);
    }
    map
}

fn merge_apfs_volumes(
    volumes: &mut HashMap<String, Vec<String>>,
    container_id: &str,
    physical_store: &str,
) {
    let parent = whole_disk_id(physical_store).to_string();
    let Some(container_volumes) = volumes.get(container_id).cloned() else {
        return;
    };
    let dest = volumes.entry(parent).or_default();
    for name in container_volumes {
        if !dest.contains(&name) {
            dest.push(name);
        }
    }
}

fn collect_volume_names(node: &PartitionNode, map: &mut HashMap<String, Vec<String>>) {
    if let Some(id) = node.device_identifier.as_deref()
        && let Some(name) = node.volume_name.as_deref().map(str::trim)
        && !name.is_empty()
        && name != "-"
    {
        map.entry(whole_disk_id(id).to_string())
            .or_default()
            .push(name.to_string());
    }
    for child in node.partitions.iter().chain(node.apfs_volumes.iter()) {
        collect_volume_names(child, map);
    }
}

fn protected_disk_ids() -> Result<HashSet<String>> {
    let mut ids = HashSet::new();
    let root = diskutil_info("/")?;
    add_protected(&mut ids, root.device_identifier.as_deref());
    add_protected(&mut ids, root.parent_whole_disk.as_deref());
    add_protected(&mut ids, root.apfs_container_reference.as_deref());
    add_protected(&mut ids, root.booter_device_identifier.as_deref());
    add_protected(&mut ids, root.recovery_device_identifier.as_deref());
    for store in &root.apfs_physical_stores {
        add_protected(&mut ids, store.apfs_physical_store.as_deref());
    }
    if ids.is_empty() {
        bail!("could not identify the macOS boot disk");
    }
    Ok(ids)
}

fn add_protected(ids: &mut HashSet<String>, value: Option<&str>) {
    if let Some(id) = value.map(str::trim).filter(|id| !id.is_empty()) {
        ids.insert(id.to_string());
        ids.insert(whole_disk_id(id).to_string());
    }
}

fn diskutil_plist<T: for<'de> Deserialize<'de>>(args: &[&str]) -> Result<T> {
    let output = cmd::run_ok(DISKUTIL, args, DISKUTIL_TIMEOUT)?;
    plist::from_bytes(&output.stdout)
        .with_context(|| format!("could not parse diskutil {}", args.join(" ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usb_stick() -> DiskInfo {
        DiskInfo {
            device_identifier: Some("disk8".into()),
            bus_protocol: Some("USB".into()),
            media_name: Some("SanDisk Ultra".into()),
            size: Some(32 * 1024 * 1024 * 1024),
            internal: Some(false),
            whole_disk: Some(true),
            removable: Some(true),
            removable_media: Some(true),
            writable: Some(true),
            virtual_or_physical: Some("Physical".into()),
            device_tree_path: Some("IODeviceTree:/arm-io/usb-drd1/port1".into()),
            ..DiskInfo::default()
        }
    }

    #[test]
    fn accepts_physical_usb_whole_disk() {
        let protected = HashSet::from(["disk0".into(), "disk3".into()]);
        assert_eq!(
            classify_disk(&usb_stick(), &protected),
            DiskVerdict::Candidate
        );
    }

    #[test]
    fn rejects_internal_and_boot_disks() {
        let protected = HashSet::from(["disk0".into()]);
        let mut internal = usb_stick();
        internal.device_identifier = Some("disk0".into());
        internal.internal = Some(true);
        internal.bus_protocol = Some("Apple Fabric".into());
        assert_eq!(
            classify_disk(&internal, &protected),
            DiskVerdict::Rejected("macOS boot / system disk")
        );

        let mut not_usb = usb_stick();
        not_usb.bus_protocol = Some("SATA".into());
        assert_eq!(
            classify_disk(&not_usb, &HashSet::new()),
            DiskVerdict::Rejected("not a USB device")
        );
    }

    #[test]
    fn rejects_disk_images_and_partitions() {
        let mut image = usb_stick();
        image.virtual_or_physical = Some("Virtual".into());
        image.bus_protocol = Some("Disk Image".into());
        assert_eq!(
            classify_disk(&image, &HashSet::new()),
            DiskVerdict::Rejected("virtual disk or disk image")
        );

        let mut partition = usb_stick();
        partition.device_identifier = Some("disk8s1".into());
        partition.whole_disk = Some(false);
        assert_eq!(
            classify_disk(&partition, &HashSet::new()),
            DiskVerdict::Rejected("not a whole disk")
        );
    }

    #[test]
    fn collects_nested_volume_names() {
        let nodes = vec![PartitionNode {
            device_identifier: Some("disk6".into()),
            volume_name: None,
            partitions: vec![PartitionNode {
                device_identifier: Some("disk6s2".into()),
                volume_name: Some("my_files Backup".into()),
                ..PartitionNode::default()
            }],
            apfs_volumes: vec![PartitionNode {
                device_identifier: Some("disk7s2".into()),
                volume_name: Some("Photos".into()),
                ..PartitionNode::default()
            }],
        }];
        let map = volume_names_by_whole(&nodes);
        assert!(map["disk6"].contains(&"my_files Backup".to_string()));
        assert!(map["disk7"].contains(&"Photos".to_string()));
    }

    #[test]
    fn merges_apfs_container_volumes_onto_physical_usb() {
        let mut volumes = HashMap::from([("disk7".into(), vec!["my_files Backup".into()])]);
        merge_apfs_volumes(&mut volumes, "disk7", "disk6s2");
        assert!(volumes["disk6"].contains(&"my_files Backup".to_string()));
    }

    #[test]
    fn same_device_passes_and_swapped_device_is_refused() {
        let volumes = HashMap::new();
        let selected = disk_from_info(&usb_stick(), &volumes);
        let same = disk_from_info(&usb_stick(), &volumes);
        assert!(check_same_device(&selected, &same).is_ok());

        let mut other = usb_stick();
        other.media_name = Some("Kingston DataTraveler".into());
        other.device_tree_path = Some("IODeviceTree:/arm-io/usb-drd2/port1".into());
        let swapped = disk_from_info(&other, &volumes);
        let error = check_same_device(&selected, &swapped).expect_err("swap must be refused");
        assert!(error.to_string().contains("Nothing was written"));

        let mut resized = usb_stick();
        resized.size = Some(64 * 1024 * 1024 * 1024);
        assert!(check_same_device(&selected, &disk_from_info(&resized, &volumes)).is_err());
    }

    #[test]
    fn same_stick_allows_a_new_disk_id_after_reconnect() {
        let volumes = HashMap::new();
        let selected = disk_from_info(&usb_stick(), &volumes);
        let mut reconnected = usb_stick();
        reconnected.device_identifier = Some("disk9".into());
        reconnected.device_tree_path = Some("IODeviceTree:/arm-io/usb-drd1/port2".into());
        assert!(same_stick(
            &selected,
            &disk_from_info(&reconnected, &volumes)
        ));

        let mut other = usb_stick();
        other.media_name = Some("Kingston DataTraveler".into());
        assert!(!same_stick(&selected, &disk_from_info(&other, &volumes)));
    }

    #[test]
    fn finds_mounted_slices_without_matching_similar_ids() {
        let mount = "\
/dev/disk3s1s1 on / (apfs, sealed, local, read-only, journaled)
/dev/disk8s1 on /Volumes/NIXOS (msdos, local, nodev, nosuid, noowners)
/dev/disk8s2 on /Volumes/EFI (msdos, local)
/dev/disk80s1 on /Volumes/Other (apfs, local)
map auto_home on /System/Volumes/Data/home (autofs, automounted, nobrowse)
";
        assert_eq!(
            mounted_slices_in(mount, "disk8"),
            ["/dev/disk8s1", "/dev/disk8s2"]
        );
        assert!(mounted_slices_in(mount, "disk9").is_empty());
    }
}
