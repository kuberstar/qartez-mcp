/// Stable replacement for the nightly-only `str::floor_char_boundary`.
/// Returns the largest byte index `<= index` that is a valid UTF-8 char boundary.
/// If `index >= s.len()`, returns `s.len()`.
pub(crate) fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        s.len()
    } else {
        let mut i = index;
        while !s.is_char_boundary(i) {
            i -= 1;
        }
        i
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Basic behavior ---

    #[test]
    fn empty_string_index_zero() {
        assert_eq!(floor_char_boundary("", 0), 0);
    }

    #[test]
    fn empty_string_index_nonzero() {
        assert_eq!(floor_char_boundary("", 5), 0);
    }

    #[test]
    fn ascii_exact_boundary() {
        let s = "hello world";
        assert_eq!(floor_char_boundary(s, 5), 5);
        assert_eq!(&s[..5], "hello");
    }

    #[test]
    fn ascii_index_zero() {
        assert_eq!(floor_char_boundary("hello", 0), 0);
    }

    #[test]
    fn ascii_at_len() {
        let s = "hello";
        assert_eq!(floor_char_boundary(s, s.len()), s.len());
    }

    #[test]
    fn ascii_past_end() {
        let s = "hello";
        assert_eq!(floor_char_boundary(s, 100), s.len());
        assert_eq!(floor_char_boundary(s, usize::MAX), s.len());
    }

    // --- 2-byte UTF-8 (Cyrillic, Latin extended) ---

    #[test]
    fn cyrillic_on_boundary() {
        // "аб" = [0xD0,0xB0, 0xD0,0xB1] = 4 bytes
        let s = "аб";
        assert_eq!(floor_char_boundary(s, 0), 0); // start of 'а'
        assert_eq!(floor_char_boundary(s, 2), 2); // start of 'б'
        assert_eq!(floor_char_boundary(s, 4), 4); // past end = len
    }

    #[test]
    fn cyrillic_mid_char() {
        let s = "аб";
        // index 1 is inside 'а' (bytes 0..2), should round down to 0
        assert_eq!(floor_char_boundary(s, 1), 0);
        // index 3 is inside 'б' (bytes 2..4), should round down to 2
        assert_eq!(floor_char_boundary(s, 3), 2);
    }

    #[test]
    fn mixed_ascii_cyrillic() {
        // "aбв" = 'a'(1) + 'б'(2) + 'в'(2) = 5 bytes
        let s = "aбв";
        assert_eq!(floor_char_boundary(s, 0), 0); // 'a'
        assert_eq!(floor_char_boundary(s, 1), 1); // start of 'б'
        assert_eq!(floor_char_boundary(s, 2), 1); // mid 'б' -> back to 1
        assert_eq!(floor_char_boundary(s, 3), 3); // start of 'в'
        assert_eq!(floor_char_boundary(s, 4), 3); // mid 'в' -> back to 3
        assert_eq!(floor_char_boundary(s, 5), 5); // len
    }

    // --- 3-byte UTF-8 (CJK, most emoji) ---

    #[test]
    fn cjk_on_boundary() {
        // "你好" = 6 bytes (3 per char)
        let s = "你好";
        assert_eq!(floor_char_boundary(s, 0), 0);
        assert_eq!(floor_char_boundary(s, 3), 3);
        assert_eq!(floor_char_boundary(s, 6), 6);
    }

    #[test]
    fn cjk_mid_char() {
        let s = "你好";
        assert_eq!(floor_char_boundary(s, 1), 0);
        assert_eq!(floor_char_boundary(s, 2), 0);
        assert_eq!(floor_char_boundary(s, 4), 3);
        assert_eq!(floor_char_boundary(s, 5), 3);
    }

    // --- 4-byte UTF-8 (emoji, supplementary planes) ---

    #[test]
    fn emoji_on_boundary() {
        // Each emoji is 4 bytes
        let s = "\u{1F600}\u{1F601}"; // two emoji
        assert_eq!(s.len(), 8);
        assert_eq!(floor_char_boundary(s, 0), 0);
        assert_eq!(floor_char_boundary(s, 4), 4);
        assert_eq!(floor_char_boundary(s, 8), 8);
    }

    #[test]
    fn emoji_mid_char() {
        let s = "\u{1F600}"; // 4 bytes
        assert_eq!(floor_char_boundary(s, 1), 0);
        assert_eq!(floor_char_boundary(s, 2), 0);
        assert_eq!(floor_char_boundary(s, 3), 0);
    }

    // --- Real-world truncation scenarios matching actual call sites ---

    #[test]
    fn truncate_long_ascii_signature() {
        let sig = "fn very_long_function_name(".to_string() + &"x".repeat(300);
        assert!(sig.len() > 200);
        let boundary = floor_char_boundary(&sig, 200);
        assert_eq!(boundary, 200); // all ASCII, boundary == index
        let truncated = &sig[..boundary];
        assert_eq!(truncated.len(), 200);
    }

    #[test]
    fn truncate_signature_with_unicode_at_boundary() {
        // Simulate a signature like: "fn func(парам: Тип)" padded to cross 200 bytes
        let prefix = "fn func(";
        let cyrillic_fill = "а".repeat(100); // 200 bytes of Cyrillic
        let sig = format!("{}{}", prefix, cyrillic_fill);
        assert!(sig.len() > 200);

        let boundary = floor_char_boundary(&sig, 200);
        // The result must be a valid char boundary
        assert!(sig.is_char_boundary(boundary));
        assert!(boundary <= 200);
        // The slice must not panic
        let _truncated = &sig[..boundary];
    }

    #[test]
    fn truncate_path_short_no_change() {
        let path = "src/main.rs";
        let boundary = floor_char_boundary(path, 50);
        assert_eq!(boundary, path.len()); // index >= len returns len
    }

    #[test]
    fn truncate_path_with_unicode_dirs() {
        let path = "/home/user/проект/src/файл.rs";
        let boundary = floor_char_boundary(path, 20);
        assert!(path.is_char_boundary(boundary));
        assert!(boundary <= 20);
        let _truncated = &path[..boundary];
    }

    // --- Edge cases ---

    #[test]
    fn single_multibyte_char() {
        let s = "д"; // 2 bytes
        assert_eq!(floor_char_boundary(s, 0), 0);
        assert_eq!(floor_char_boundary(s, 1), 0);
        assert_eq!(floor_char_boundary(s, 2), 2);
    }

    #[test]
    fn all_4byte_chars_various_indices() {
        let s = "\u{10000}\u{10001}\u{10002}"; // 12 bytes total
        for i in 0..=s.len() + 5 {
            let b = floor_char_boundary(s, i);
            assert!(s.is_char_boundary(b), "index {i} -> boundary {b} not valid");
            assert!(b <= i.min(s.len()));
        }
    }

    #[test]
    fn exhaustive_boundary_check_mixed() {
        // Mix of 1, 2, 3, 4 byte chars
        let s = "a\u{00E9}\u{4E16}\u{1F600}b"; // a(1) + e-acute(2) + CJK(3) + emoji(4) + b(1) = 11
        assert_eq!(s.len(), 11);
        for i in 0..=s.len() + 10 {
            let b = floor_char_boundary(s, i);
            assert!(s.is_char_boundary(b), "index {i} -> boundary {b} not valid");
            assert!(b <= i.min(s.len()));
            // The slice must always be valid
            let _slice = &s[..b];
        }
    }

    #[test]
    fn result_always_valid_for_slicing() {
        // The main contract: &s[..floor_char_boundary(s, n)] must never panic
        let cases = vec![
            "",
            "hello",
            "привет мир",
            "你好世界",
            "\u{1F600}\u{1F601}\u{1F602}",
            "mixed: hello привет 你好 \u{1F600}",
        ];
        for s in &cases {
            for i in 0..=s.len() + 5 {
                let b = floor_char_boundary(s, i);
                let _slice = &s[..b]; // must not panic
            }
        }
    }
}
