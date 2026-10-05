use owo_colors::OwoColorize;

const TEAL: (u8, u8, u8) = (45, 212, 191);

pub fn banner() {
    println!();
    println!("  {}", "SuitBoot".truecolor(TEAL.0, TEAL.1, TEAL.2).bold());
    println!("  {}", "▰".repeat(16).truecolor(TEAL.0, TEAL.1, TEAL.2));
}

pub fn step(title: &str) {
    println!();
    println!(
        "  {}  {}",
        "▸".truecolor(TEAL.0, TEAL.1, TEAL.2).bold(),
        title.bold()
    );
}

pub fn point(text: &str) {
    println!("     {}  {text}", "·".truecolor(TEAL.0, TEAL.1, TEAL.2));
}

pub fn done(text: &str) {
    println!(
        "     {}  {}",
        "✓".truecolor(TEAL.0, TEAL.1, TEAL.2).bold(),
        text.truecolor(TEAL.0, TEAL.1, TEAL.2)
    );
}

pub fn alert(text: &str) {
    println!(
        "     {}  {}",
        "·".truecolor(251, 191, 36).bold(),
        text.truecolor(251, 191, 36).bold()
    );
}

/// Shown before the write, and again when the partition table is wiped.
/// That is when macOS opens the "disk is not readable" dialog.
pub fn ignore_dialog() {
    step("macOS dialog");
    point("A window will say the disk is unreadable");
    alert("Click Ignore");
    point("Not Eject");
    point("Not Initialize");
}
