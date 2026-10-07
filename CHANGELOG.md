# Changelog

## 0.0.1 — 2026-10-07

First release. SuitBoot writes a Linux ISO to a USB stick from a Mac.

- Pick the USB stick and an ISO from Downloads. No Disk Utility.
- Internal disks, the Mac boot disk, disk images, and partitions are refused. A disk of 128 GB or more needs an extra confirmation.
- The image is written, read back, and the stick is ejected. The boot sector is written last, and a stick that resets is retried.
- Each step stays on one screen. The Mac password is typed there and sent with the disk write.
- If macOS says the disk is unreadable, click Ignore. Do not click Eject or Initialize.
