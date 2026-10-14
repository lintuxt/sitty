/// Map a key name (case-insensitive) to the bytes a terminal sends for it.
pub fn key_bytes(name: &str) -> Option<Vec<u8>> {
    let n = name.to_ascii_lowercase();
    let fixed: &[u8] = match n.as_str() {
        "enter" | "return" => b"\r",
        "tab" => b"\t",
        "esc" | "escape" => b"\x1b",
        "backspace" => b"\x7f",
        "space" => b" ",
        "up" => b"\x1b[A",
        "down" => b"\x1b[B",
        "right" => b"\x1b[C",
        "left" => b"\x1b[D",
        "home" => b"\x1b[H",
        "end" => b"\x1b[F",
        "pageup" => b"\x1b[5~",
        "pagedown" => b"\x1b[6~",
        "delete" => b"\x1b[3~",
        _ => {
            let c = n.strip_prefix("ctrl-")?;
            let mut chars = c.chars();
            let ch = chars.next()?;
            if chars.next().is_some() || !ch.is_ascii_lowercase() {
                return None;
            }
            return Some(vec![ch as u8 - b'a' + 1]);
        }
    };
    Some(fixed.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_keys() {
        assert_eq!(key_bytes("enter").unwrap(), b"\r");
        assert_eq!(key_bytes("tab").unwrap(), b"\t");
        assert_eq!(key_bytes("esc").unwrap(), b"\x1b");
        assert_eq!(key_bytes("backspace").unwrap(), b"\x7f");
        assert_eq!(key_bytes("up").unwrap(), b"\x1b[A");
        assert_eq!(key_bytes("ctrl-c").unwrap(), b"\x03");
        assert_eq!(key_bytes("CTRL-D").unwrap(), b"\x04");
        assert!(key_bytes("ctrl-").is_none());
        assert!(key_bytes("hyper-x").is_none());
    }
}
