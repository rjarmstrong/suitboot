<h1>SuitBoot</h1>

<img width="150" height="150" alt="image" src="https://github.com/user-attachments/assets/b46cf117-aa26-41e9-a5fa-3fac2d4b9d45" />


<h2>Quick, easy way to make a bootable Linux USB on your Mac.</h2>

Plug in a USB stick, run:

```sh
$ suitboot
```

and pick a Linux ISO from Downloads. SuitBoot writes it ready to boot.

## Download

Grab the Apple silicon build from the [0.0.1 release](https://github.com/rjarmstrong/suitboot/releases/tag/v0.0.1) and move it to `/usr/local/bin/suitboot`.

macOS may refuse to run it, because this build is not notarized. Clear that with `xattr -d com.apple.quarantine /usr/local/bin/suitboot`. `SHA256SUMS` on the release page is there if you want to check the download.

## What you get

- **Fast setup.** Pick the stick and the ISO. No Disk Utility.
- **Safe targets.** Internal disks, disk images, and the Mac’s boot disk are refused.
- **A finished stick.** SuitBoot writes the ISO, reads it back, then ejects it.

## One dialog to ignore

macOS will say the disk is not readable.

- **Click Ignore.**
- **Do not** click Eject or Initialize.

## On the target machine

- Plug in the USB.
- Power on and open the boot menu.
- Choose the USB.

## Build

SuitBoot builds on macOS with a current stable Rust toolchain.

```sh
cargo build --release
```

The binary is `target/release/suitboot`. Install it with a new signature. macOS rejects a binary that was copied on top of an older one.

```sh
sudo rm -f /usr/local/bin/suitboot
sudo cp target/release/suitboot /usr/local/bin/suitboot
sudo codesign --force --sign - /usr/local/bin/suitboot
```
