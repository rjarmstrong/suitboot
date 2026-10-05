use anyhow::{Result, bail};
use dialoguer::{Input, theme::ColorfulTheme};

use crate::disk::Disk;
use crate::iso::IsoImage;
use crate::ui;
use crate::util::format_bytes;

pub fn confirm_erase(disk: &Disk, iso: &IsoImage) -> Result<()> {
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

    ui::step("Erase");
    ui::alert("This destroys the whole USB");
    ui::point(&format!("{}  ·  {}", disk.id, disk.media_name));
    ui::point(&format!("{}  ·  {volumes}", format_bytes(disk.size_bytes)));
    ui::point(&format!("{iso_name}  ·  {}", format_bytes(iso.size_bytes)));
    ui::point(iso.display_kind());

    if iso.size_bytes > disk.size_bytes {
        bail!(
            "ISO ({}) is larger than the USB ({}). Use a bigger stick.",
            format_bytes(iso.size_bytes),
            format_bytes(disk.size_bytes)
        );
    }

    if disk.is_large() {
        ui::step("Large disk");
        ui::alert("128 GB or bigger");
        ui::point("This may be a backup drive");
        ui::point("You: type ERASE");
        let typed: String = Input::with_theme(&ColorfulTheme::default())
            .with_prompt("Type ERASE to continue")
            .interact_text()?;
        if typed.trim() != "ERASE" {
            bail!("aborted");
        }
    }

    ui::point(&format!("You: type {}", disk.id));
    ui::point("SuitBoot waits until you do");
    let typed: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt(format!("Type {} to erase it and write the ISO", disk.id))
        .interact_text()?;
    if typed.trim() != disk.id {
        bail!("aborted (identifier did not match)");
    }
    Ok(())
}

pub fn thinkpad_boot_notes() {
    ui::step("ThinkPad");
    ui::point("Unplug the USB and plug it into the laptop");
    ui::point("Power on, tap F12");
    ui::point("Fn+F12 on some models");
    ui::point("Choose the USB entry");
    ui::point("Missing? BIOS with F1");
    ui::point("USB boot: on");
    ui::point("Secure Boot: fine for Ubuntu and Fedora");
    ui::point("Other distros: turn Secure Boot off");
    ui::point("Old models: enable Legacy / CSM");
    ui::point("Fast Boot: off");
    ui::point("Prefer a USB-A port");
    println!();
}
