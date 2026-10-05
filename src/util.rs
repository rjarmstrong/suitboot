pub fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const TIB: f64 = 1024.0 * 1024.0 * 1024.0 * 1024.0;
    let bytes_f = bytes as f64;
    if bytes_f >= TIB {
        format!("{:.1} TB", bytes_f / TIB)
    } else if bytes_f >= GIB {
        format!("{:.1} GB", bytes_f / GIB)
    } else if bytes_f >= MIB {
        format!("{:.1} MB", bytes_f / MIB)
    } else if bytes_f >= KIB {
        format!("{:.1} KB", bytes_f / KIB)
    } else {
        format!("{bytes} B")
    }
}

pub fn format_duration(seconds: u64) -> String {
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {secs}s")
    } else {
        format!("{secs}s")
    }
}

/// `disk4s1` / `disk12s2s1` -> `disk4` / `disk12`. Already-whole ids are unchanged.
pub fn whole_disk_id(id: &str) -> &str {
    let id = id
        .trim()
        .trim_start_matches("/dev/r")
        .trim_start_matches("/dev/");
    let Some(rest) = id.strip_prefix("disk") else {
        return id;
    };
    let digits = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    &id[..4 + digits]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_disk_id_strips_slices() {
        assert_eq!(whole_disk_id("disk0"), "disk0");
        assert_eq!(whole_disk_id("disk12s2s1"), "disk12");
        assert_eq!(whole_disk_id("/dev/rdisk4s1"), "disk4");
        assert_eq!(whole_disk_id("/dev/disk6"), "disk6");
    }

    #[test]
    fn format_duration_picks_a_readable_unit() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(45), "45s");
        assert_eq!(format_duration(75), "1m 15s");
        assert_eq!(format_duration(3700), "1h 1m");
    }

    #[test]
    fn format_bytes_uses_binary_units() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024 * 1024), "5.0 GB");
    }
}
