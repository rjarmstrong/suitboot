# SuitBoot

Interactive macOS tool that erases a USB stick and writes a Linux ISO so a ThinkPad can boot from it.

Plug in a USB stick, run `suitboot`, and pick the stick and an ISO from Downloads. SuitBoot refuses internal disks, disk images, and the Mac’s boot disk. It then writes the image, reads it back, and ejects the stick.

macOS will open a dialog saying the disk is not readable. Click **Ignore**. Do not click Eject or Initialize.

On the ThinkPad, power on and tap F12 to choose the USB.
