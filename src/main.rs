#[cfg(not(target_os = "macos"))]
compile_error!("suitboot only runs on macOS (it uses diskutil and dd).");

mod cmd;
mod confirm;
mod disk;
mod flash;
mod iso;
mod ui;
mod util;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use owo_colors::OwoColorize;

use crate::disk::{Disk, list_usb_candidates, require_candidate};
use crate::iso::{IsoImage, downloads_dir, list_download_isos, load_iso};
use crate::ui::Ui;
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
    println!("{}", "SuitBoot".bold());
    let disks = list_usb_candidates()?;
    if disks.is_empty() {
        println!("No USB stick found. Plug one in, then run suitboot list again.");
        return Ok(());
    }
    for disk in &disks {
        if disk.is_large() {
            println!("  {}   large", disk.summary());
        } else {
            println!("  {}", disk.summary());
        }
    }
    Ok(())
}

fn flash_flow() -> Result<()> {
    let mut ui = Ui::open()?;

    let disk = pick_target_device(&mut ui)?;
    ui.show(
        "Checking the USB",
        &[format!("{} is still the selected disk", disk.id)],
    )?;
    let disk = require_candidate(&disk.id)?;

    let iso = pick_iso(&mut ui)?;

    confirm::confirm_erase(&mut ui, &disk, &iso)?;
    flash::erase_and_write(&mut ui, &disk, &iso)?;

    ui.clear_progress()?;
    ui.show("Ready", &confirm::thinkpad_notes())?;
    ui.set_action(&["Unplug the USB".to_string()])?;
    ui.wait_enter()?;
    Ok(())
}

fn pick_target_device(ui: &mut Ui) -> Result<Disk> {
    loop {
        ui.show("Looking for USB sticks", &[])?;
        let disks = list_usb_candidates()?;
        if disks.is_empty() {
            let choice = ui.choose(
                "No USB stick found",
                &["Internal disks stay hidden.".to_string()],
                &["Rescan".to_string(), "Cancel".to_string()],
            )?;
            if choice == Some(0) {
                continue;
            }
            bail!("aborted");
        }

        let mut labels: Vec<String> = disks.iter().map(device_menu_label).collect();
        labels.push("Rescan".to_string());
        labels.push("Cancel".to_string());
        let selection = ui
            .choose("Pick the stick to erase", &[], &labels)?
            .context_cancel()?;
        if selection == disks.len() {
            continue;
        }
        if selection == disks.len() + 1 {
            bail!("aborted");
        }
        return Ok(disks[selection].clone());
    }
}

fn pick_iso(ui: &mut Ui) -> Result<IsoImage> {
    let downloads = downloads_dir()?;
    loop {
        ui.show(&format!("Looking in {}", downloads.display()), &[])?;
        let files = list_download_isos()?;
        if files.is_empty() {
            let choice = ui.choose(
                "No ISO found",
                &[format!("Nothing in {}", downloads.display())],
                &["Rescan".to_string(), "Cancel".to_string()],
            )?;
            if choice == Some(0) {
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
                format!("{}    {}", name, format_bytes(file.size_bytes))
            })
            .collect();
        labels.push("Rescan".to_string());
        labels.push("Cancel".to_string());
        let selection = ui
            .choose("Pick an ISO", &[downloads.display().to_string()], &labels)?
            .context_cancel()?;
        if selection == files.len() {
            continue;
        }
        if selection == files.len() + 1 {
            bail!("aborted");
        }

        ui.show("Checking the file can boot from USB", &[])?;
        match load_iso(&files[selection].path) {
            Ok(iso) => return Ok(iso),
            Err(error) => {
                ui.show(&format!("{error:#}"), &["Pick another file".to_string()])?;
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
    let warning = if disk.is_large() { "    large" } else { "" };
    format!(
        "{}    {}    {}    {volumes}{warning}",
        disk.id,
        disk.media_name,
        format_bytes(disk.size_bytes),
    )
}

trait Cancel {
    fn context_cancel(self) -> Result<usize>;
}

impl Cancel for Option<usize> {
    fn context_cancel(self) -> Result<usize> {
        self.context("aborted")
    }
}
