use anyhow::{Result, bail};

use crate::disk::Disk;
use crate::iso::IsoImage;
use crate::ui::Ui;
use crate::util::format_bytes;

pub fn confirm_erase(ui: &mut Ui, disk: &Disk, iso: &IsoImage) -> Result<()> {
    if iso.size_bytes > disk.size_bytes {
        bail!(
            "ISO ({}) is larger than the USB ({}). Use a bigger stick.",
            format_bytes(iso.size_bytes),
            format_bytes(disk.size_bytes)
        );
    }

    let volumes = if disk.volume_names.is_empty() {
        "no volumes".to_string()
    } else {
        disk.volume_names.join(", ")
    };
    let iso_name = iso
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("iso");
    let notes = vec![
        format!("{}  ·  {}", disk.id, disk.media_name),
        format!("{}  ·  {volumes}", format_bytes(disk.size_bytes)),
        format!("{iso_name}  ·  {}", format_bytes(iso.size_bytes)),
        iso.display_kind().to_string(),
        "The whole USB is erased".to_string(),
    ];

    if disk.is_large() {
        loop {
            let typed = ui.prompt(
                "Large disk",
                &[
                    notes[0].clone(),
                    "128 GB or larger. This may be a backup drive.".to_string(),
                ],
                &["Type ERASE".to_string()],
            )?;
            match typed.as_deref() {
                None => bail!("aborted"),
                Some("ERASE") => break,
                Some(_) => continue,
            }
        }
    }

    loop {
        let typed = ui.prompt("Erase", &notes, &[format!("Type {}", disk.id)])?;
        match typed.as_deref() {
            None => bail!("aborted"),
            Some(value) if value == disk.id => return Ok(()),
            Some(_) => continue,
        }
    }
}

pub fn thinkpad_notes() -> Vec<String> {
    [
        "Unplug the USB and plug it into the laptop",
        "Power on and tap F12",
        "Fn+F12 on some models",
        "Choose the USB entry",
        "Missing? BIOS with F1, USB boot on",
        "Secure Boot is fine for Ubuntu and Fedora",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}
