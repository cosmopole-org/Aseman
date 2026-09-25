//! Translation of `chain/common/median.go`.

/// Gets the median number in a slice of numbers.
pub fn median(input: &[i64]) -> i64 {
    // Start by sorting a copy of the slice.
    let mut s = input.to_vec();
    s.sort_unstable();

    // No math is needed if there are no numbers. For even counts we average
    // the two middle numbers; for odd counts we use the middle number.
    let l = s.len();
    if l == 0 {
        0
    } else if l % 2 == 0 {
        let mid = l / 2 - 1;
        (s[mid] + s[mid + 1]) / 2
    } else {
        s[l / 2]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Translation of median_test.go::TestMedian.
    #[test]
    fn test_median() {
        let cases: &[(&[i64], i64)] = &[(&[5, 3, 4, 2, 1], 3), (&[6, 3, 2, 4, 5, 1], 3), (&[1], 1)];
        for (input, out) in cases {
            let got = median(input);
            assert_eq!(got, *out, "Median({:?}) => {} != {}", input, got, out);
        }
        let m = median(&[]);
        assert_eq!(m, 0, "Empty slice should have returned 0");
    }
}
