//! Readback overlap index for root-UAV ranges. This does not change the
//! page-level policy: a page touching a precise range is left to that
//! range's existing byte-for-byte copy-back.

pub(super) struct PreciseRanges {
    // Sorted starts with the maximum end among this entry and all earlier
    // entries. Nested ranges and repeated starts require the prefix max,
    // not just the end of the last range preceding a query.
    prefix: Vec<(u64, u64)>,
}

impl PreciseRanges {
    pub(super) fn new(ranges: &[(u64, u64)]) -> Self {
        let mut prefix: Vec<_> = ranges
            .iter()
            .map(|&(start, len)| (start, start.saturating_add(len)))
            .collect();
        prefix.sort_unstable_by_key(|&(start, _)| start);
        let mut max_end = 0;
        for (_, end) in &mut prefix {
            max_end = max_end.max(*end);
            *end = max_end;
        }
        Self { prefix }
    }

    /// For a nonempty page [from, to), the original test was
    /// start < to && from < start + len. In particular, preserve its
    /// treatment of zero-length declarations strictly inside a page.
    pub(super) fn overlaps(&self, from: u64, to: u64) -> bool {
        let count = self.prefix.partition_point(|&(start, _)| start < to);
        count != 0 && self.prefix[count - 1].1 > from
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(ranges: &[(u64, u64)], from: u64, to: u64) -> bool {
        ranges.iter().any(|&(a, n)| a < to && from < a.saturating_add(n))
    }

    #[test]
    fn nested_ranges_keep_the_earlier_long_end() {
        let index = PreciseRanges::new(&[(0, 200), (10, 1), (20, 1), (20, 2)]);
        assert!(index.overlaps(100, 101));
        assert!(!index.overlaps(200, 201));
    }

    #[test]
    fn boundaries_and_zero_length_match_the_old_predicate() {
        let ranges = [(64, 64), (200, 0), (300, 8)];
        let index = PreciseRanges::new(&ranges);
        for (from, to) in [(0, 64), (64, 128), (128, 192), (192, 256), (200, 201), (199, 200), (308, 309)] {
            assert_eq!(index.overlaps(from, to), reference(&ranges, from, to));
        }
        assert!(!PreciseRanges::new(&[]).overlaps(0, 65536));
        assert!(PreciseRanges::new(&[(u64::MAX - 2, 8)]).overlaps(u64::MAX - 1, u64::MAX));
    }

    #[test]
    fn exhaustive_small_ranges_and_queries() {
        let ranges: Vec<_> = (0..8u64)
            .flat_map(|a| (0..=8 - a).map(move |n| (a, n)))
            .collect();
        for &a in &ranges {
            for &b in &ranges {
                let input = [a, b];
                let index = PreciseRanges::new(&input);
                for from in 0..8 {
                    for to in from + 1..=8 {
                        assert_eq!(index.overlaps(from, to), reference(&input, from, to));
                    }
                }
            }
        }
    }

    #[test]
    fn many_unsorted_declarations_across_page_boundaries() {
        let mut seed = 0x1234_abcd_u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for size in [0, 1, 2, 8, 32, 128, 512, 2048] {
            let ranges: Vec<_> = (0..size)
                .map(|_| (next() % (1 << 32), next() % (1 << 18)))
                .collect();
            let index = PreciseRanges::new(&ranges);
            for _ in 0..4096 {
                let from = (next() % (1 << 16)) << 16;
                assert_eq!(index.overlaps(from, from + 65536), reference(&ranges, from, from + 65536));
            }
        }
    }
}
