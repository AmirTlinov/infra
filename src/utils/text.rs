pub fn truncate_utf8_prefix(value: &str, max_bytes: usize) -> String {
    if max_bytes == 0 {
        return String::new();
    }
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

pub fn truncate_utf8_suffix(value: &str, max_bytes: usize) -> String {
    if max_bytes == 0 {
        return String::new();
    }
    let bytes = value.as_bytes();
    if bytes.len() <= max_bytes {
        return value.to_string();
    }
    let mut start = bytes.len().saturating_sub(max_bytes);
    while start < bytes.len() && !value.is_char_boundary(start) {
        start += 1;
    }
    value[start..].to_string()
}

#[cfg(test)]
mod tests {
    use super::{truncate_utf8_prefix, truncate_utf8_suffix};

    #[test]
    fn truncate_utf8_prefix_handles_ascii() {
        assert_eq!(truncate_utf8_prefix("hello", 3), "hel");
    }

    #[test]
    fn truncate_utf8_prefix_does_not_split_utf8() {
        assert_eq!(truncate_utf8_prefix("a😀b", 2), "a");
        assert_eq!(truncate_utf8_prefix("a😀b", 5), "a😀");
    }

    #[test]
    fn truncate_utf8_suffix_does_not_split_utf8() {
        assert_eq!(truncate_utf8_suffix("a😀b", 2), "b");
        assert_eq!(truncate_utf8_suffix("a😀b", 5), "😀b");
    }
}
