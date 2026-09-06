//! Portable filename policy shared by bundle declarations and script filesystem APIs.

/// Validate one segment. Callers enforce total length, extensions and root access.
pub(crate) fn valid_segment(part: &str) -> bool {
    if part.is_empty()
        || matches!(part, "." | "..")
        || part.ends_with(['.', ' '])
        || part.chars().any(|ch| {
            ch.is_control() || matches!(ch, '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*')
        })
    {
        return false;
    }
    let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
    let device = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|n| {
            matches!(
                n,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    });
    !device
}
