#[cfg(not(target_os = "macos"))]
compile_error!("suitboot only runs on macOS (it uses diskutil and dd).");

mod cmd;
mod confirm;
mod disk;
mod flash;
mod iso;
mod ui;
mod util;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use dialoguer::{Select, theme::ColorfulTheme};
use owo_colors::OwoColorize;

use crate::disk::{Disk, list_usb_candidates, require_candidate};
use crate::iso::{IsoImage, downloads_dir, list_download_isos, load_iso};
use crate::util::format_bytes;

#[derive(Parser)]
#[command(
    name = "suitboot",
    about = "Erase a USB stick on this Mac and write a Linux ISO for ThinkPad startup.",
    long_about = "Run suitboot with no arguments. SuitBoot will:\n  \
        1. let you pick the target USB from a list\n  \
        2. let you pick an ISO from ~/Downloads\n  \
        3. erase that USB and write the ISO as a hybrid boot image\n\n\
        Afterward, boot the ThinkPad with F12 and pick the USB."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Show USB disks that this tool will allow
    List,
    /// Erase a USB and write the ISO (default)
    Flash,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{} {error:#}", "error:".red().bold());
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Flash) {
        Command::List => list_disks(),
        Command::Flash => flash_flow(),
    }
}

fn list_disks() -> Result<()> {
    ui::banner();
    ui::step("USB");
    ui::point("Looking for sticks");
    let disks = list_usb_candidates()?;
    if disks.is_empty() {
        ui::point("None found");
        ui::point("You: plug one in, then run suitboot list again");
        return Ok(());
    }
    for disk in &disks {
        print_disk_line(disk);
    }
    Ok(())
}

fn flash_flow() -> Result<()> {
    ui::banner();
    ui::step("Start");
    ui::point("Pick a USB, then an ISO");
    ui::point("SuitBoot erases the stick and writes it");
    ui::point("You: arrow keys, then Enter");

    let disk = pick_target_device()?;
    ui::step("Check");
    ui::point(&format!("{id} still attached", id = disk.id));
    let disk = require_candidate(&disk.id)?;
    ui::done(&disk.summary());

    let iso = pick_iso()?;
    let iso_name = iso
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("iso");
    ui::done(&format!(
        "{iso_name}  ·  {}  ·  {}",
        format_bytes(iso.size_bytes),
        iso.display_kind()
    ));

    confirm::confirm_erase(&disk, &iso)?;
    flash::erase_and_write(&disk, &iso)?;

    ui::step("Ready");
    ui::done("USB is bootable");
    confirm::thinkpad_boot_notes();
    Ok(())
}

fn pick_target_device() -> Result<Disk> {
    loop {
        ui::step("USB");
        ui::point("Looking for sticks");
        let disks = list_usb_candidates()?;
        if disks.is_empty() {
            ui::point("None found");
            ui::point("You: plug one in, then choose Rescan");
            let again = Select::with_theme(&ColorfulTheme::default())
                .with_prompt("Next")
                .items(["Rescan for USB devices", "Cancel"])
                .default(0)
                .interact()?;
            if again == 0 {
                continue;
            }
            bail!("aborted");
        }

        let mut labels: Vec<String> = disks.iter().map(device_menu_label).collect();
        labels.push("Rescan for USB devices".to_string());
        labels.push("Cancel".to_string());

        let selection = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("Select the USB device to erase")
            .items(&labels)
            .default(0)
            .interact()?;

        if selection == disks.len() {
            continue;
        }
        if selection == disks.len() + 1 {
            bail!("aborted");
        }
        return Ok(disks[selection].clone());
    }
}

fn pick_iso() -> Result<IsoImage> {
    let downloads = downloads_dir()?;
    loop {
        ui::step("ISO");
        ui::point(&format!("{}", downloads.display()));
        let files = list_download_isos()?;
        if files.is_empty() {
            ui::point("No .iso or .img files");
            ui::point("You: download one, then choose Rescan");
            let again = Select::with_theme(&ColorfulTheme::default())
                .with_prompt("Next")
                .items(["Rescan Downloads", "Cancel"])
                .default(0)
                .interact()?;
            if again == 0 {
                continue;
            }
            bail!("aborted");
        }

        let mut labels: Vec<String> = files
            .iter()
            .map(|file| {
                let name = file
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("unknown.iso");
                format!("{}   {}", name, format_bytes(file.size_bytes))
            })
            .collect();
        labels.push("Rescan Downloads".to_string());
        labels.push("Cancel".to_string());

        let selection = Select::with_theme(&ColorfulTheme::default())
            .with_prompt(format!("Select an ISO from {}", downloads.display()))
            .items(&labels)
            .default(0)
            .interact()?;

        if selection == files.len() {
            continue;
        }
        if selection == files.len() + 1 {
            bail!("aborted");
        }

        ui::point("Checking it can boot from USB");
        match load_iso(&files[selection].path) {
            Ok(iso) => return Ok(iso),
            Err(error) => {
                ui::point(&format!("{error:#}"));
                ui::point("You: pick another file, or Rescan");
            }
        }
    }
}

fn device_menu_label(disk: &Disk) -> String {
    let volumes = if disk.volume_names.is_empty() {
        "no volumes".to_string()
    } else {
        disk.volume_names.join(", ")
    };
    let warning = if disk.is_large() {
        "  ⚠ large — confirm carefully"
    } else {
        ""
    };
    format!(
        "{}   {}   {}   {}{warning}",
        disk.id,
        disk.media_name,
        format_bytes(disk.size_bytes),
        volumes
    )
}

fn print_disk_line(disk: &Disk) {
    if disk.is_large() {
        ui::alert(&format!("{}  ·  large", disk.summary()));
    } else {
        ui::point(&disk.summary());
    }
}
