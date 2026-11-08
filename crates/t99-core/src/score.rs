// Code in this module PARTIALLY or FULLY utilized AI Coding Agent

//! Arena 积分（PTS）：名次底座 + 击杀加成。
//! 规则见 docs/rules/scoring.md。
//!
//! 内部一律用 1/100 分：1000 分记为 100000。这样百分比加成恒为整数，不出现小数。

/// 名次底座分，单位 1/100 分。第 100 名及以后为 0。
pub fn placement_points(rank: u32) -> u32 {
    let pts = match rank {
        1 => 1000,
        2 => 850,
        3 => 700,
        4 => 650,
        5 => 600,
        6..=10 => 500 - 5 * (rank - 6),
        11..=20 => 450 - 4 * (rank - 11),
        21..=30 => 400 - 3 * (rank - 21),
        31..=50 => 350 - 2 * (rank - 31),
        51..=75 => 300 - 8 * (rank - 51),
        76..=99 => 100 - 4 * (rank - 76),
        _ => 0,
    };
    pts * 100
}

/// 单局总积分，单位 1/100 分：
/// 底座 + 每杀 5 分 + 首杀 50 分 + 底座 ×（1% × 击杀数 + 10% × 首杀）。
/// 百分比按底座计算；内部单位是分的百分之一，所以结果恒为整数。
pub fn match_points(rank: u32, ko: u32, first_ko: bool) -> u32 {
    let base = placement_points(rank) as u64;
    let kills = ko as u64;
    let fk = if first_ko { 1u64 } else { 0 };
    (base + 500 * kills + 5000 * fk + base * (kills + 10 * fk) / 100) as u32
}

/// 把 1/100 分的整数格式化为两位小数，例如 145000 → "1450.00"
pub fn format_points(units: u32) -> String {
    format!("{}.{:02}", units / 100, units % 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_curve_matches_table() {
        let want = [
            (1, 1000),
            (2, 850),
            (3, 700),
            (4, 650),
            (5, 600),
            (6, 500),
            (7, 495),
            (10, 480),
            (11, 450),
            (20, 414),
            (21, 400),
            (30, 373),
            (31, 350),
            (50, 312),
            (51, 300),
            (75, 108),
            (76, 100),
            (99, 8),
        ];
        for (rank, pts) in want {
            assert_eq!(placement_points(rank), pts * 100, "rank {rank}");
        }
        assert_eq!(placement_points(0), 0);
        assert_eq!(placement_points(100), 0);
    }

    #[test]
    fn placement_curve_is_monotone() {
        for r in 1..99 {
            assert!(
                placement_points(r) > placement_points(r + 1),
                "rank {r} 应高于 {}",
                r + 1
            );
        }
    }

    #[test]
    fn kill_bonuses_add_up() {
        assert_eq!(match_points(1, 0, false), 100_000);
        // 1 名 + 20 杀 + 首杀：1000 + (100 + 50) + 1000×30%
        assert_eq!(match_points(1, 20, true), 145_000);
        // 末位无击杀
        assert_eq!(match_points(99, 0, false), 800);
        // 首杀也计入普通击杀：50 + 5 + 10% + 1%
        assert_eq!(match_points(50, 1, true), 312 * 100 + 5_500 + 312 * 11);
        assert_eq!(format_points(match_points(1, 20, true)), "1450.00");
        assert_eq!(format_points(match_points(99, 0, false)), "8.00");
    }
}
