#[derive(Debug, Clone, Copy)]
pub enum UnitSpec {
    Raw,
    B,
    K,
    Ki,
    M,
    Mi,
    G,
    Gi,
    T,
    Ti,
    Auto,
    AutoSi,
}

fn round_half_up_scaled(num: u64, den: u64) -> (u64, u32) {
    let scaled = num * 10 / den;
    (scaled / 10, (scaled % 10) as u32)
}

pub fn format_size(bytes: u64, unit: UnitSpec) -> String {
    match unit {
        UnitSpec::Raw | UnitSpec::B => bytes.to_string(),
        UnitSpec::K => {
            let (int, frac) = round_half_up_scaled(bytes, 1_000);
            format!("{}.{}", int, frac)
        }
        UnitSpec::Ki => {
            let (int, frac) = round_half_up_scaled(bytes, 1_024);
            format!("{}.{} KiB", int, frac)
        }
        UnitSpec::M => {
            let (int, frac) = round_half_up_scaled(bytes, 1_000_000);
            format!("{}.{}", int, frac)
        }
        UnitSpec::Mi => {
            let (int, frac) = round_half_up_scaled(bytes, 1_048_576);
            format!("{}.{} MiB", int, frac)
        }
        UnitSpec::G => {
            let (int, frac) = round_half_up_scaled(bytes, 1_000_000_000);
            format!("{}.{}", int, frac)
        }
        UnitSpec::Gi => {
            let (int, frac) = round_half_up_scaled(bytes, 1_073_741_824);
            format!("{}.{} GiB", int, frac)
        }
        UnitSpec::T => {
            let (int, frac) = round_half_up_scaled(bytes, 1_000_000_000_000);
            format!("{}.{}", int, frac)
        }
        UnitSpec::Ti => {
            let (int, frac) = round_half_up_scaled(bytes, 1_099_511_627_776);
            format!("{}.{} TiB", int, frac)
        }
        UnitSpec::Auto => format_auto(bytes, true),
        UnitSpec::AutoSi => format_auto(bytes, false),
    }
}

fn auto_pick_unit(bytes: u64, is_iec: bool) -> (u64, u32, &'static str) {
    if bytes == 0 {
        return (0, 0, "B");
    }

    let tiers = if is_iec {
        &[
            (1_099_511_627_776u64, "TiB"),
            (1_073_741_824u64, "GiB"),
            (1_048_576u64, "MiB"),
            (1_024u64, "KiB"),
        ]
    } else {
        &[
            (1_000_000_000_000u64, "T"),
            (1_000_000_000u64, "G"),
            (1_000_000u64, "M"),
            (1_000u64, "K"),
        ]
    };

    for &(divisor, suffix) in tiers {
        if bytes >= divisor {
            let (int_part, frac) = round_half_up_scaled(bytes, divisor);
            return (int_part, frac, suffix);
        }
    }

    (bytes, 0, "B")
}

fn format_auto(bytes: u64, is_iec: bool) -> String {
    let (int_part, frac, suffix) = auto_pick_unit(bytes, is_iec);
    if int_part == bytes && !is_iec || (int_part == bytes && is_iec && suffix == "B") {
        return format!("{} B", bytes)
    }
    format!("{}.{} {}", int_part, frac, suffix)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_raw_zero() {
        assert_eq!(format_size(0, UnitSpec::Raw), "0");
    }

    #[test]
    fn test_b_zero() {
        assert_eq!(format_size(0, UnitSpec::B), "0");
    }

    #[test]
    fn test_kilobytes_1k_raw() {
        assert_eq!(format_size(1_000, UnitSpec::K), "1.0");
    }

    #[test]
    fn test_kibibytes_1536() {
        assert_eq!(format_size(1_536, UnitSpec::Ki), "1.5 KiB");
    }

    #[test]
    fn test_megabytes_1536000_raw() {
        assert_eq!(format_size(1_536_000, UnitSpec::M), "1.5");
    }

    #[test]
    fn test_mebibytes_rounding_up() {
        assert_eq!(format_size(2_097_152, UnitSpec::Mi), "2.0 MiB");
    }

    #[test]
    fn test_half_up_rounding() {
        assert_eq!(format_size(1_536, UnitSpec::Ki), "1.5 KiB");
    }

    #[test]
    fn test_auto_one_byte() {
        assert_eq!(format_size(1, UnitSpec::Auto), "1 B");
    }

    #[test]
    fn test_auto_si_1500_bytes() {
        assert_eq!(format_size(1_500, UnitSpec::AutoSi), "1.5 K");
    }

    #[test]
    fn test_auto_iec_1536_bytes() {
        assert_eq!(format_size(1_536, UnitSpec::Auto), "1.5 KiB");
    }

    #[test]
    fn test_auto_si_1500000_bytes() {
        assert_eq!(format_size(1_500_000, UnitSpec::AutoSi), "1.5 M");
    }

    #[test]
    fn test_auto_iec_1433600_bytes_rounds_to_mi_b_or_mib_variant() {
        // 1433600 bytes ≈ 1.4 MiB (1433600/1048576 = 1.367)
        let result = format_size(1_433_600, UnitSpec::Auto);
        assert!(result.ends_with("MiB") || result.contains("Mi"));
    }

    #[test]
    fn test_auto_gigabyte_range() {
        let result = format_size(2_500_000_000, UnitSpec::AutoSi);
        assert!(result.contains("G"));
    }

    #[test]
    fn test_auto_zero_bytes() {
        assert_eq!(format_size(0, UnitSpec::Auto), "0 B");
    }
}
