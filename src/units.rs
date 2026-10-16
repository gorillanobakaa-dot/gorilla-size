//! Sizes and ages, read and written the way a person types and reads them.
//!
//! Sizes are binary (1 KiB = 1024 bytes) and are SHOWN with the binary unit names (KiB, MiB, GiB, TiB), so a number
//! never claims more precision than it has. Input accepts both spellings: `2G`, `2GB` and `2GiB` all mean 2 x 1024^3,
//! because that is what Explorer, du and dust mean by "GB".

use std::time::Duration;

const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];

/// 1536 -> "1.5 KiB"; under 1 KiB the exact byte count.
pub fn human(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if v >= 100.0 {
        format!("{v:.0} {}", UNITS[i])
    } else if v >= 10.0 {
        format!("{v:.1} {}", UNITS[i])
    } else {
        format!("{v:.2} {}", UNITS[i])
    }
}

/// "10K", "1.5M", "2GB", "3 GiB", "512" (bytes) -> bytes.
pub fn parse_size(s: &str) -> Result<u64, String> {
    let t = s.trim().replace(' ', "");
    let split = t.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let n: f64 = num.parse().map_err(|_| format!("not a size: {s:?} (examples: 500M, 2G, 1.5T)"))?;
    let mult: u64 = match unit.to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1 << 10,
        "m" | "mb" | "mib" => 1 << 20,
        "g" | "gb" | "gib" => 1 << 30,
        "t" | "tb" | "tib" => 1 << 40,
        "p" | "pb" | "pib" => 1 << 50,
        _ => return Err(format!("unknown size unit in {s:?} (use B, K, M, G, T or P)")),
    };
    Ok((n * mult as f64).round() as u64)
}

/// "90s", "30m", "12h", "7d", "2w", "1y" -> a duration.
pub fn parse_age(s: &str) -> Result<Duration, String> {
    let t = s.trim();
    let split = t.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let n: f64 = num.parse().map_err(|_| format!("not an age: {s:?} (examples: 12h, 7d, 2w, 1y)"))?;
    let secs = match unit {
        "s" => 1.0,
        "m" | "min" => 60.0,
        "h" => 3600.0,
        "d" | "" => 86400.0,
        "w" => 7.0 * 86400.0,
        "y" => 365.0 * 86400.0,
        _ => return Err(format!("unknown age unit in {s:?} (use s, m, h, d, w or y)")),
    };
    Ok(Duration::from_secs_f64(n * secs))
}

/// 1234567 -> "1,234,567"
pub fn count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_both_spellings_as_binary() {
        assert_eq!(parse_size("2G").unwrap(), 2 << 30);
        assert_eq!(parse_size("2GB").unwrap(), 2 << 30);
        assert_eq!(parse_size("2 GiB").unwrap(), 2 << 30);
        assert_eq!(parse_size("1.5k").unwrap(), 1536);
        assert_eq!(parse_size("512").unwrap(), 512);
        assert!(parse_size("2X").is_err());
        assert!(parse_size("G").is_err());
    }

    #[test]
    fn sizes_print_with_binary_names() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(1023), "1023 B");
        assert_eq!(human(1536), "1.50 KiB");
        assert_eq!(human(4_724_464_025), "4.40 GiB");
        assert_eq!(human(150 << 30), "150 GiB");
    }

    #[test]
    fn ages_and_counts() {
        assert_eq!(parse_age("7d").unwrap(), Duration::from_secs(7 * 86400));
        assert_eq!(parse_age("12h").unwrap(), Duration::from_secs(12 * 3600));
        assert!(parse_age("3q").is_err());
        assert_eq!(count(482113), "482,113");
        assert_eq!(count(7), "7");
    }
}
